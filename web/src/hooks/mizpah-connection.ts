import { useEffect, useRef, useState } from "react"

import type { TimeRange, WsEvent } from "@/lib/types"

const HEARTBEAT_MS = 1_000
const PONG_TIMEOUT_MS = 2_500
const RECONNECT_BASE_MS = 500
const RECONNECT_MAX_MS = 8_000

export type WsHandlers = {
  onLog: (entry: WsEvent & { type: "log" }) => void
  onEvicted: (ids: number[]) => void
  onServices: (names: string[], blocked: string[]) => void
  onProperties: (paths: import("@/lib/types").PropertyInfo[]) => void
  onLagged: () => void
}

export function sendSubscribe(
  ws: WebSocket,
  q: string,
  timeRange: TimeRange | null
) {
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

/** Own WebSocket lifecycle (reconnect, ping/pong). Returns connection flag + ws ref. */
export function useHubConnection(
  query: string,
  timeRange: TimeRange | null,
  handlers: WsHandlers
) {
  const [connected, setConnected] = useState(false)
  const queryRef = useRef(query)
  const timeRangeRef = useRef(timeRange)
  const handlersRef = useRef(handlers)
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
    handlersRef.current = handlers
  }, [handlers])

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

        const h = handlersRef.current
        if (event.type === "pong") {
          awaitingPongRef.current = false
          if (pongWatchId != null) {
            window.clearTimeout(pongWatchId)
            pongWatchId = undefined
          }
          setConnected(true)
          return
        }
        if (event.type === "lagged") {
          h.onLagged()
          return
        }
        if (event.type === "services") {
          h.onServices(event.names, event.blocked ?? [])
          return
        }
        if (event.type === "properties") {
          h.onProperties(event.paths)
          return
        }
        if (event.type === "evicted") {
          h.onEvicted(event.ids)
          return
        }
        if (event.type === "log") {
          h.onLog(event)
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

  return { connected, wsRef }
}
