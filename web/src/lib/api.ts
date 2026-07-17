import type {
  ActivityBucket,
  InvestigateTarget,
  LogEntry,
  PropertyInfo,
  ServicesList,
  Stats,
  UpdateEvent,
  UpdateStatus,
} from "./types"

export type {
  InvestigateTarget,
  ServicesList,
  UpdateChannel,
  UpdateEvent,
  UpdateStatus,
} from "./types"

export async function fetchLogs(opts: {
  service?: string
  cursor?: number
  limit?: number
  q?: string
  from?: string
  to?: string
}): Promise<{ entries: LogEntry[]; hasMore: boolean }> {
  const params = new URLSearchParams()
  if (opts.service && opts.service !== "*") {
    params.set("service", opts.service)
  }
  if (opts.cursor != null) params.set("cursor", String(opts.cursor))
  if (opts.limit != null) params.set("limit", String(opts.limit))
  if (opts.q?.trim()) {
    params.set("q", opts.q.trim())
  }
  if (opts.from) params.set("from", opts.from)
  if (opts.to) params.set("to", opts.to)
  const res = await fetch(`/api/logs?${params}`)
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `logs: ${res.status}`)
  }
  return res.json()
}

export async function fetchActivity(opts?: {
  hours?: number
  bucketMinutes?: number
}): Promise<ActivityBucket[]> {
  const params = new URLSearchParams()
  if (opts?.hours != null) params.set("hours", String(opts.hours))
  if (opts?.bucketMinutes != null) {
    params.set("bucketMinutes", String(opts.bucketMinutes))
  }
  const qs = params.toString()
  const res = await fetch(qs ? `/api/activity?${qs}` : "/api/activity")
  if (!res.ok) throw new Error(`activity: ${res.status}`)
  const data = (await res.json()) as { buckets: ActivityBucket[] }
  return data.buckets
}

export async function fetchServices(): Promise<ServicesList> {
  const res = await fetch("/api/services")
  if (!res.ok) throw new Error(`services: ${res.status}`)
  const data = (await res.json()) as { services: string[]; blocked?: string[] }
  return {
    services: data.services,
    blocked: data.blocked ?? [],
  }
}

export async function disconnectService(service: string): Promise<void> {
  const res = await fetch("/api/services/disconnect", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ service }),
  })
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `disconnect: ${res.status}`)
  }
}

export async function reconnectService(service: string): Promise<void> {
  const res = await fetch("/api/services/reconnect", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ service }),
  })
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `reconnect: ${res.status}`)
  }
}

export async function fetchProperties(opts?: {
  service?: string
  q?: string
}): Promise<PropertyInfo[]> {
  const params = new URLSearchParams()
  if (opts?.service && opts.service !== "*") {
    params.set("service", opts.service)
  }
  if (opts?.q?.trim()) {
    params.set("q", opts.q.trim())
  }
  const qs = params.toString()
  const res = await fetch(qs ? `/api/properties?${qs}` : "/api/properties")
  if (!res.ok) throw new Error(`properties: ${res.status}`)
  const data = (await res.json()) as { properties: PropertyInfo[] }
  return data.properties
}

export async function fetchStats(): Promise<Stats> {
  const res = await fetch("/api/stats")
  if (!res.ok) throw new Error(`stats: ${res.status}`)
  return res.json()
}

export async function startInvestigate(
  target: InvestigateTarget,
  id: number
): Promise<void> {
  const res = await fetch("/api/investigate", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ target, id }),
  })
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `investigate: ${res.status}`)
  }
}

export async function fetchUpdateStatus(): Promise<UpdateStatus> {
  const res = await fetch("/api/update")
  if (!res.ok) throw new Error(`update status: ${res.status}`)
  return res.json()
}

/** Stream POST /api/update SSE events via fetch (EventSource is GET-only). */
export async function streamUpdate(
  onEvent: (ev: UpdateEvent) => void,
  signal?: AbortSignal
): Promise<void> {
  const res = await fetch("/api/update", { method: "POST", signal })
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `update: ${res.status}`)
  }
  if (!res.body) {
    throw new Error("update: empty response body")
  }

  const reader = res.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ""

  while (true) {
    const { done, value } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })

    // SSE frames separated by blank line
    let splitAt: number
    while ((splitAt = buffer.indexOf("\n\n")) >= 0) {
      const frame = buffer.slice(0, splitAt)
      buffer = buffer.slice(splitAt + 2)
      for (const line of frame.split("\n")) {
        if (!line.startsWith("data:")) continue
        const raw = line.slice(5).trim()
        if (!raw) continue
        try {
          onEvent(JSON.parse(raw) as UpdateEvent)
        } catch {
          // ignore malformed chunks
        }
      }
    }
  }
}
