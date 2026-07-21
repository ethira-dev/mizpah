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

/** Same-origin fetch with cookies; redirects to OIDC login on 401. */
export async function apiFetch(
  input: RequestInfo | URL,
  init?: RequestInit
): Promise<Response> {
  const res = await fetch(input, { ...init, credentials: "include" })
  if (res.status === 401 && typeof window !== "undefined") {
    const path = window.location.pathname
    if (!path.startsWith("/api/auth/")) {
      window.location.assign("/api/auth/login")
    }
  }
  return res
}

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
  const res = await apiFetch(`/api/logs?${params}`)
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `logs: ${res.status}`)
  }
  return res.json()
}

export type AggregateRow = {
  keys: string[]
  count: number
  sum?: number
  avg?: number
  min?: number
  max?: number
}

export async function fetchAggregate(opts: {
  q?: string
  service?: string
  groupBy?: string
  limit?: number
}): Promise<AggregateRow[]> {
  const params = new URLSearchParams()
  if (opts.q?.trim()) params.set("q", opts.q.trim())
  if (opts.service) params.set("service", opts.service)
  if (opts.groupBy) params.set("groupBy", opts.groupBy)
  if (opts.limit != null) params.set("limit", String(opts.limit))
  const res = await apiFetch(`/api/aggregate?${params}`)
  if (!res.ok) throw new Error(`aggregate: ${res.status}`)
  const data = (await res.json()) as { rows: AggregateRow[] }
  return data.rows
}

export async function fetchTrace(
  opid: string,
  limit = 100
): Promise<LogEntry[]> {
  const res = await apiFetch(
    `/api/trace/${encodeURIComponent(opid)}?limit=${limit}`
  )
  if (!res.ok) throw new Error(`trace: ${res.status}`)
  const data = (await res.json()) as { entries: LogEntry[] }
  return data.entries
}

export async function fetchNavLevel(opts: {
  fromId: number
  direction: "next" | "prev"
  levels?: string
  service?: string
  q?: string
  from?: string
  to?: string
}): Promise<LogEntry | null> {
  const params = new URLSearchParams()
  params.set("fromId", String(opts.fromId))
  params.set("direction", opts.direction)
  if (opts.levels) params.set("levels", opts.levels)
  if (opts.service && opts.service !== "*") params.set("service", opts.service)
  if (opts.q?.trim()) params.set("q", opts.q.trim())
  if (opts.from) params.set("from", opts.from)
  if (opts.to) params.set("to", opts.to)
  const res = await apiFetch(`/api/nav/level?${params}`)
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `nav: ${res.status}`)
  }
  const data = (await res.json()) as { entry: LogEntry | null }
  return data.entry
}

export type AnnotatedEntry = {
  id: number
  annotation: {
    marked: boolean
    tags: string[]
    comment?: string
  }
}

export async function fetchBookmarks(): Promise<AnnotatedEntry[]> {
  const res = await apiFetch("/api/bookmarks")
  if (!res.ok) throw new Error(`bookmarks: ${res.status}`)
  const data = (await res.json()) as { bookmarks: AnnotatedEntry[] }
  return data.bookmarks
}

export async function setBookmark(opts: {
  id: number
  marked?: boolean
  tags?: string[]
  comment?: string
}): Promise<void> {
  const res = await apiFetch("/api/bookmarks", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(opts),
  })
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `bookmark: ${res.status}`)
  }
}

export type SqlResult = {
  columns: string[]
  rows: unknown[][]
  rowCount: number
  truncated: boolean
}

export async function runSql(sql: string, limit = 100): Promise<SqlResult> {
  const res = await apiFetch("/api/sql", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ sql, limit }),
  })
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `sql: ${res.status}`)
  }
  return res.json()
}

export type SpectrogramResult = {
  fieldPath: string
  from: string
  to: string
  timeStarts: string[]
  valueLabels: string[]
  counts: number[][]
}

export type Keymap = {
  nextError: string
  prevError: string
  down: string
  up: string
  quit: string
  showTrace: string
}

export async function fetchKeymap(): Promise<Keymap> {
  const res = await apiFetch("/api/keymap")
  if (!res.ok) throw new Error(`keymap: ${res.status}`)
  return res.json()
}

export type Theme = {
  name: string
  background: string
  foreground: string
  accent: string
  muted: string
  error: string
  warn: string
}

export async function fetchTheme(name?: string): Promise<{
  themes: string[]
  theme: Theme
}> {
  const params = new URLSearchParams()
  if (name) params.set("name", name)
  const qs = params.toString()
  const res = await apiFetch(qs ? `/api/themes?${qs}` : "/api/themes")
  if (!res.ok) throw new Error(`themes: ${res.status}`)
  return res.json()
}

export async function fetchSpectrogram(opts: {
  field?: string
  from?: string
  to?: string
  timeBuckets?: number
  valueBuckets?: number
}): Promise<SpectrogramResult> {
  const params = new URLSearchParams()
  if (opts.field) params.set("field", opts.field)
  if (opts.from) params.set("from", opts.from)
  if (opts.to) params.set("to", opts.to)
  if (opts.timeBuckets != null) {
    params.set("timeBuckets", String(opts.timeBuckets))
  }
  if (opts.valueBuckets != null) {
    params.set("valueBuckets", String(opts.valueBuckets))
  }
  const qs = params.toString()
  const res = await apiFetch(qs ? `/api/spectrogram?${qs}` : "/api/spectrogram")
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `spectrogram: ${res.status}`)
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
  const res = await apiFetch(qs ? `/api/activity?${qs}` : "/api/activity")
  if (!res.ok) throw new Error(`activity: ${res.status}`)
  const data = (await res.json()) as { buckets: ActivityBucket[] }
  return data.buckets
}

export async function fetchServices(): Promise<ServicesList> {
  const res = await apiFetch("/api/services")
  if (!res.ok) throw new Error(`services: ${res.status}`)
  const data = (await res.json()) as { services: string[]; blocked?: string[] }
  return {
    services: data.services,
    blocked: data.blocked ?? [],
  }
}

export async function disconnectService(service: string): Promise<void> {
  const res = await apiFetch("/api/services/disconnect", {
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
  const res = await apiFetch("/api/services/reconnect", {
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
  const res = await apiFetch(qs ? `/api/properties?${qs}` : "/api/properties")
  if (!res.ok) throw new Error(`properties: ${res.status}`)
  const data = (await res.json()) as { properties: PropertyInfo[] }
  return data.properties
}

export async function fetchStats(): Promise<Stats> {
  const res = await apiFetch("/api/stats")
  if (!res.ok) throw new Error(`stats: ${res.status}`)
  return res.json()
}

export type IncidentSummary = {
  minutes: number
  total: number
  byLevel: { level: string; count: number }[]
  topServices: { service: string; count: number }[]
  topMessages: {
    msg: string
    count: number
    sampleId?: number | null
  }[]
  topTraces: { opid: string; errorCount: number }[]
  notes: string[]
}

/** `GET /api/incident?minutes=` — "what broke?" summary. */
export async function fetchIncident(minutes = 15): Promise<IncidentSummary> {
  const params = new URLSearchParams()
  params.set("minutes", String(minutes))
  const res = await apiFetch(`/api/incident?${params}`)
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `incident: ${res.status}`)
  }
  return res.json()
}

export async function startInvestigate(
  target: InvestigateTarget,
  id: number
): Promise<void> {
  const res = await apiFetch("/api/investigate", {
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
  const res = await apiFetch("/api/update")
  if (!res.ok) throw new Error(`update status: ${res.status}`)
  return res.json()
}

/** Stream POST /api/update SSE events via fetch (EventSource is GET-only). */
export async function streamUpdate(
  onEvent: (ev: UpdateEvent) => void,
  signal?: AbortSignal
): Promise<void> {
  const res = await apiFetch("/api/update", { method: "POST", signal })
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
