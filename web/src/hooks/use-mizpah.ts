import { useCallback, useEffect, useRef, useState } from "react"

import {
  fetchLogs,
  fetchProperties,
  fetchServices,
  fetchStats,
} from "@/lib/api"
import type {
  FilterChip,
  LogEntry,
  PropertyInfo,
  Stats,
  WsEvent,
} from "@/lib/types"

const MAX_CLIENT_ENTRIES = 50_000

function sendSubscribe(ws: WebSocket, filters: FilterChip[]) {
  if (ws.readyState !== WebSocket.OPEN) return
  ws.send(
    JSON.stringify({
      type: "subscribe",
      service: "*",
      filters,
    })
  )
}

export function useMizpah(filters: FilterChip[]) {
  const [entries, setEntries] = useState<LogEntry[]>([])
  const [services, setServices] = useState<string[]>([])
  const [properties, setProperties] = useState<PropertyInfo[]>([])
  const [stats, setStats] = useState<Stats | null>(null)
  const [connected, setConnected] = useState(false)
  const [loading, setLoading] = useState(true)
  const filtersRef = useRef(filters)
  const wsRef = useRef<WebSocket | null>(null)

  filtersRef.current = filters

  const reload = useCallback(async () => {
    setLoading(true)
    try {
      const [logs, svc, props, st] = await Promise.all([
        fetchLogs({
          limit: 500,
          filters,
        }),
        fetchServices(),
        fetchProperties(),
        fetchStats(),
      ])
      setEntries(logs.entries)
      setServices(svc)
      setProperties(props)
      setStats(st)
    } finally {
      setLoading(false)
    }
  }, [filters])

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
      sendSubscribe(ws, filtersRef.current)
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
    sendSubscribe(ws, filters)
  }, [filters])

  return {
    entries,
    services,
    properties,
    stats,
    connected,
    loading,
    reload,
  }
}
