import { useCallback, useEffect, useRef, useState } from "react"

import {
  fetchLogs,
  fetchProperties,
  fetchServices,
  fetchStats,
} from "@/lib/api"
import type { LogEntry, PropertyInfo, Stats, WsEvent } from "@/lib/types"

const MAX_CLIENT_ENTRIES = 50_000

function sendSubscribe(ws: WebSocket, q: string) {
  if (ws.readyState !== WebSocket.OPEN) return
  ws.send(
    JSON.stringify({
      type: "subscribe",
      service: "*",
      q,
    })
  )
}

export function useMizpah(query: string) {
  const [entries, setEntries] = useState<LogEntry[]>([])
  const [services, setServices] = useState<string[]>([])
  const [properties, setProperties] = useState<PropertyInfo[]>([])
  const [propertiesRevision, setPropertiesRevision] = useState(0)
  const [stats, setStats] = useState<Stats | null>(null)
  const [connected, setConnected] = useState(false)
  const [loading, setLoading] = useState(true)
  const [queryError, setQueryError] = useState<string | null>(null)
  const queryRef = useRef(query)
  const wsRef = useRef<WebSocket | null>(null)

  queryRef.current = query

  const reload = useCallback(async () => {
    setLoading(true)
    try {
      const [logsResult, svc, props, st] = await Promise.all([
        fetchLogs({
          limit: 500,
          q: query,
        })
          .then((logs) => ({ ok: true as const, logs }))
          .catch((err: unknown) => ({
            ok: false as const,
            message: err instanceof Error ? err.message : "Failed to load logs",
          })),
        fetchServices(),
        fetchProperties(),
        fetchStats(),
      ])
      setServices(svc)
      setProperties(props)
      setPropertiesRevision((n) => n + 1)
      setStats(st)
      if (logsResult.ok) {
        setEntries(logsResult.logs.entries)
        setQueryError(null)
      } else {
        setQueryError(logsResult.message)
      }
    } finally {
      setLoading(false)
    }
  }, [query])

  useEffect(() => {
    void reload()
  }, [reload])

  useEffect(() => {
    const id = window.setInterval(() => {
      void fetchStats().then(setStats).catch(() => undefined)
    }, 2000)
    return () => window.clearInterval(id)
  }, [])

  useEffect(() => {
    const proto = window.location.protocol === "https:" ? "wss" : "ws"
    const ws = new WebSocket(`${proto}://${window.location.host}/ws`)
    wsRef.current = ws

    ws.onopen = () => {
      setConnected(true)
      sendSubscribe(ws, queryRef.current)
    }
    ws.onclose = () => {
      setConnected(false)
      if (wsRef.current === ws) wsRef.current = null
    }
    ws.onerror = () => setConnected(false)

    ws.onmessage = (ev) => {
      let event: WsEvent
      try {
        event = JSON.parse(String(ev.data)) as WsEvent
      } catch {
        return
      }

      if (event.type === "services") {
        setServices(event.names)
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

    return () => {
      ws.close()
      if (wsRef.current === ws) wsRef.current = null
    }
  }, [])

  useEffect(() => {
    const ws = wsRef.current
    if (!ws) return
    sendSubscribe(ws, query)
  }, [query])

  return {
    entries,
    services,
    properties,
    propertiesRevision,
    stats,
    connected,
    loading,
    queryError,
    reload,
  }
}
