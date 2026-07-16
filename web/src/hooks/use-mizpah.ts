import { useCallback, useEffect, useRef, useState } from "react"

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
  WsEvent,
} from "@/lib/types"

const MAX_CLIENT_ENTRIES = 50_000
const HEARTBEAT_MS = 1_000
const PONG_TIMEOUT_MS = 2_500
const RECONNECT_BASE_MS = 500
const RECONNECT_MAX_MS = 8_000

function sendSubscribe(ws: WebSocket, q: string, timeRange: TimeRange | null) {
  if (ws.readyState !== WebSocket.OPEN) return
  ws.send(
    JSON.stringify({
      type: "subscribe",
      service: "*",
      q,
      from: timeRange?.from,
      to: timeRange?.to,
    })
  )
}

function sendPing(ws: WebSocket) {
  if (ws.readyState !== WebSocket.OPEN) return
  ws.send(JSON.stringify({ type: "ping" }))
}

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
  const [connected, setConnected] = useState(false)
  const [loading, setLoading] = useState(true)
  const [queryError, setQueryError] = useState<string | null>(null)
  const queryRef = useRef(query)
  const timeRangeRef = useRef(timeRange)
  const timeZoomRef = useRef(timeZoom)
  const wsRef = useRef<WebSocket | null>(null)
  const awaitingPongRef = useRef(false)
  const reconnectAttemptRef = useRef(0)

  useEffect(() => {
    queryRef.current = query
  }, [query])

  useEffect(() => {
    timeRangeRef.current = timeRange
  }, [timeRange])

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
    // Defer so the effect body does not synchronously setState (react-hooks lint).
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

  // WebSocket with reconnect + 1s ping/pong liveness.
  useEffect(() => {
    let cancelled = false
    let heartbeatId: number | undefined
    let reconnectId: number | undefined
    let pongWatchId: number | undefined

    function clearTimers() {
      if (heartbeatId != null) window.clearInterval(heartbeatId)
      if (reconnectId != null) window.clearTimeout(reconnectId)
      if (pongWatchId != null) window.clearTimeout(pongWatchId)
      heartbeatId = undefined
      reconnectId = undefined
      pongWatchId = undefined
    }

    function scheduleReconnect() {
      if (cancelled) return
      const attempt = reconnectAttemptRef.current
      const delay = Math.min(
        RECONNECT_MAX_MS,
        RECONNECT_BASE_MS * 2 ** Math.min(attempt, 4)
      )
      reconnectAttemptRef.current = attempt + 1
      reconnectId = window.setTimeout(() => {
        connect()
      }, delay)
    }

    function connect() {
      if (cancelled) return
      clearTimers()

      const proto = window.location.protocol === "https:" ? "wss" : "ws"
      const ws = new WebSocket(`${proto}://${window.location.host}/ws`)
      wsRef.current = ws
      awaitingPongRef.current = false

      ws.onopen = () => {
        if (cancelled || wsRef.current !== ws) return
        reconnectAttemptRef.current = 0
        setConnected(true)
        sendSubscribe(ws, queryRef.current, timeRangeRef.current)

        heartbeatId = window.setInterval(() => {
          if (wsRef.current !== ws || ws.readyState !== WebSocket.OPEN) return
          if (awaitingPongRef.current) {
            // Missed a pong — treat as offline and force reconnect.
            setConnected(false)
            ws.close()
            return
          }
          awaitingPongRef.current = true
          sendPing(ws)
          pongWatchId = window.setTimeout(() => {
            if (!awaitingPongRef.current || wsRef.current !== ws) return
            setConnected(false)
            ws.close()
          }, PONG_TIMEOUT_MS)
        }, HEARTBEAT_MS)
      }

      ws.onclose = () => {
        if (wsRef.current === ws) wsRef.current = null
        awaitingPongRef.current = false
        setConnected(false)
        clearTimers()
        if (!cancelled) scheduleReconnect()
      }

      ws.onerror = () => {
        setConnected(false)
      }

      ws.onmessage = (ev) => {
        let event: WsEvent
        try {
          event = JSON.parse(String(ev.data)) as WsEvent
        } catch {
          return
        }

        if (event.type === "pong") {
          awaitingPongRef.current = false
          if (pongWatchId != null) {
            window.clearTimeout(pongWatchId)
            pongWatchId = undefined
          }
          setConnected(true)
          return
        }
        if (event.type === "services") {
          setServices(event.names)
          setBlocked(event.blocked ?? [])
          return
        }
        if (event.type === "properties") {
          setProperties(event.paths)
          setPropertiesRevision((n) => n + 1)
          return
        }
        if (event.type === "evicted") {
          const ids = new Set(event.ids)
          setEntries((prev) => prev.filter((e) => !ids.has(e.id)))
          return
        }
        if (event.type === "log") {
          const entry = event.entry
          setEntries((prev) => {
            const next = [entry, ...prev]
            if (next.length > MAX_CLIENT_ENTRIES) next.length = MAX_CLIENT_ENTRIES
            return next
          })
        }
      }
    }

    connect()

    return () => {
      cancelled = true
      clearTimers()
      const ws = wsRef.current
      wsRef.current = null
      if (ws) ws.close()
    }
  }, [])

  useEffect(() => {
    const ws = wsRef.current
    if (!ws) return
    sendSubscribe(ws, query, timeRange)
  }, [query, timeRange])

  const onDisconnectService = useCallback(
    async (service: string) => {
      await disconnectService(service)
      // WS events update services/blocked/entries; refresh stats promptly.
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
