import type { LogEntry, PropertyInfo, Stats } from "./types"

export async function fetchLogs(opts: {
  service?: string
  cursor?: number
  limit?: number
  q?: string
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
  const res = await fetch(`/api/logs?${params}`)
  if (!res.ok) {
    const body = await res.text().catch(() => "")
    throw new Error(body || `logs: ${res.status}`)
  }
  return res.json()
}

export async function fetchServices(): Promise<string[]> {
  const res = await fetch("/api/services")
  if (!res.ok) throw new Error(`services: ${res.status}`)
  const data = (await res.json()) as { services: string[] }
  return data.services
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

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`
}

export function summarizeLog(data: Record<string, unknown>): string {
  for (const key of ["msg", "message", "error", "event", "_raw"]) {
    const v = data[key]
    if (typeof v === "string" && v.trim()) return v
  }
  try {
    const s = JSON.stringify(data)
    return s.length > 160 ? `${s.slice(0, 160)}…` : s
  } catch {
    return "(unserializable)"
  }
}

export function levelOf(data: Record<string, unknown>): string | null {
  for (const key of ["level", "severity", "lvl"]) {
    const v = data[key]
    if (typeof v === "string") return v.toLowerCase()
    if (typeof v === "number") return String(v)
  }
  return null
}
