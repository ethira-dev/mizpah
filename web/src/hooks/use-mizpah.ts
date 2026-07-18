import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import { useHubConnection } from "@/hooks/mizpah-connection"
import {
  disconnectService,
  fetchActivity,
  fetchLogs,
  fetchProperties,
  fetchServices,
  fetchStats,
  reconnectService,
} from "@/lib/api"
import type { TimeZoomLevel } from "@/lib/time-zoom"
import type {
  ActivityBucket,
  LogEntry,
  PropertyInfo,
  Stats,
  TimeRange,
} from "@/lib/types"

const MAX_CLIENT_ENTRIES = 50_000
const PAGE_SIZE = 100

export function useMizpah(
  query: string,
  timeRange: TimeRange | null,
  timeZoom: TimeZoomLevel
) {
  const [entries, setEntries] = useState<LogEntry[]>([])
  const [hasMore, setHasMore] = useState(false)
  const [loadingMore, setLoadingMore] = useState(false)
  const [services, setServices] = useState<string[]>([])
  const [blocked, setBlocked] = useState<string[]>([])
  const [properties, setProperties] = useState<PropertyInfo[]>([])
  const [propertiesRevision, setPropertiesRevision] = useState(0)
  const [stats, setStats] = useState<Stats | null>(null)
  const [activity, setActivity] = useState<ActivityBucket[]>([])
  const [loading, setLoading] = useState(true)
  const [queryError, setQueryError] = useState<string | null>(null)
  const timeZoomRef = useRef(timeZoom)
  const reloadRef = useRef<() => Promise<void>>(async () => {})
  const loadingMoreRef = useRef(false)
  const entriesRef = useRef(entries)
  const hasMoreRef = useRef(hasMore)
  const queryRef = useRef(query)
  const timeRangeRef = useRef(timeRange)

  useEffect(() => {
    timeZoomRef.current = timeZoom
  }, [timeZoom])

  useEffect(() => {
    entriesRef.current = entries
  }, [entries])

  useEffect(() => {
    hasMoreRef.current = hasMore
  }, [hasMore])

  useEffect(() => {
    queryRef.current = query
  }, [query])

  useEffect(() => {
    timeRangeRef.current = timeRange
  }, [timeRange])

  const reloadActivity = useCallback(async () => {
    const zoom = timeZoomRef.current
    try {
      const act = await fetchActivity({
        hours: zoom.windowHours,
        bucketMinutes: zoom.bucketMinutes,
      })
      setActivity(act)
    } catch {
      /* keep previous */
    }
  }, [])

  const reload = useCallback(async () => {
    setLoading(true)
    try {
      const [logsResult, svc, props, st, act] = await Promise.all([
        fetchLogs({
          limit: PAGE_SIZE,
          q: query,
          from: timeRange?.from,
          to: timeRange?.to,
        })
          .then((logs) => ({ ok: true as const, logs }))
          .catch((err: unknown) => ({
            ok: false as const,
            message: err instanceof Error ? err.message : "Failed to load logs",
          })),
        fetchServices(),
        fetchProperties(),
        fetchStats(),
        fetchActivity({
          hours: timeZoom.windowHours,
          bucketMinutes: timeZoom.bucketMinutes,
        }).catch(() => [] as ActivityBucket[]),
      ])
      setServices(svc.services)
      setBlocked(svc.blocked)
      setProperties(props)
      setPropertiesRevision((n) => n + 1)
      setStats(st)
      setActivity(act)
      if (logsResult.ok) {
        setEntries(logsResult.logs.entries)
        setHasMore(logsResult.logs.hasMore)
        setQueryError(null)
      } else {
        setQueryError(logsResult.message)
      }
    } finally {
      setLoading(false)
    }
  }, [query, timeRange, timeZoom])

  const loadMore = useCallback(async () => {
    if (loadingMoreRef.current || !hasMoreRef.current) return
    const oldest = entriesRef.current[entriesRef.current.length - 1]
    if (!oldest) return

    loadingMoreRef.current = true
    setLoadingMore(true)
    try {
      const range = timeRangeRef.current
      const logs = await fetchLogs({
        limit: PAGE_SIZE,
        cursor: oldest.id,
        q: queryRef.current,
        from: range?.from,
        to: range?.to,
      })
      const prev = entriesRef.current
      const seen = new Set(prev.map((e) => e.id))
      const appended = logs.entries.filter((e) => !seen.has(e.id))
      const next = [...prev, ...appended]
      const capped = next.length > MAX_CLIENT_ENTRIES
      if (capped) next.length = MAX_CLIENT_ENTRIES
      setEntries(next)
      setHasMore(capped ? false : logs.hasMore)
    } catch {
      /* keep previous page; user can scroll again */
    } finally {
      loadingMoreRef.current = false
      setLoadingMore(false)
    }
  }, [])

  useEffect(() => {
    reloadRef.current = reload
  }, [reload])

  useEffect(() => {
    const id = window.setTimeout(() => {
      void reload()
    }, 0)
    return () => window.clearTimeout(id)
  }, [reload])

  useEffect(() => {
    const id = window.setInterval(() => {
      void fetchStats().then(setStats).catch(() => undefined)
      void reloadActivity()
    }, 2000)
    return () => window.clearInterval(id)
  }, [reloadActivity])

  const wsHandlers = useMemo(
    () => ({
      onLog: (event: { type: "log"; entry: LogEntry }) => {
        const entry = event.entry
        setEntries((prev) => {
          const next = [entry, ...prev]
          if (next.length > MAX_CLIENT_ENTRIES) next.length = MAX_CLIENT_ENTRIES
          return next
        })
      },
      onEvicted: (ids: number[]) => {
        const idSet = new Set(ids)
        setEntries((prev) => prev.filter((e) => !idSet.has(e.id)))
      },
      onServices: (names: string[], blockedNames: string[]) => {
        setServices(names)
        setBlocked(blockedNames)
      },
      onProperties: (paths: PropertyInfo[]) => {
        setProperties(paths)
        setPropertiesRevision((n) => n + 1)
      },
      onLagged: () => {
        void reloadRef.current()
      },
    }),
    []
  )

  const { connected } = useHubConnection(query, timeRange, wsHandlers)

  const onDisconnectService = useCallback(
    async (service: string) => {
      await disconnectService(service)
      void fetchStats().then(setStats).catch(() => undefined)
      void reloadActivity()
    },
    [reloadActivity]
  )

  const onReconnectService = useCallback(async (service: string) => {
    await reconnectService(service)
    void fetchStats().then(setStats).catch(() => undefined)
  }, [])

  return {
    entries,
    hasMore,
    loadingMore,
    loadMore,
    services,
    blocked,
    properties,
    propertiesRevision,
    stats,
    activity,
    connected,
    loading,
    queryError,
    reload,
    onDisconnectService,
    onReconnectService,
  }
}
