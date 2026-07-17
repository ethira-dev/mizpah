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

export function useMizpah(
  query: string,
  timeRange: TimeRange | null,
  timeZoom: TimeZoomLevel
) {
  const [entries, setEntries] = useState<LogEntry[]>([])
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

  useEffect(() => {
    timeZoomRef.current = timeZoom
  }, [timeZoom])

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
          limit: 500,
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
        setQueryError(null)
      } else {
        setQueryError(logsResult.message)
      }
    } finally {
      setLoading(false)
    }
  }, [query, timeRange, timeZoom])

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
