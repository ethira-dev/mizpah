export type LogEntry = {
  id: number
  receivedAt: string
  service: string
  data: Record<string, unknown>
}

export type PropertyValueInfo = {
  value: string
  count: number
}

export type PropertyInfo = {
  path: string
  types: string[]
  sampleValues: string[]
  /** Entries in the buffer that have this field (from REST; 0 on WS snapshots). */
  count?: number
  /** Sample values with occurrence counts (from REST search/list). */
  values?: PropertyValueInfo[]
}

export type ServiceInfo = {
  name: string
  count: number
}

export type Stats = {
  count: number
  approxBytes: number
  maxBytes: number
  services: ServiceInfo[]
}

export type ActivityBucket = {
  start: string
  end: string
  count: number
}

/** Inclusive `from`, exclusive `to` (RFC3339). Omit `to` for an open upper bound (through now). */
export type TimeRange = {
  from: string
  to?: string
}

export type WsEvent =
  | { type: "log"; entry: LogEntry }
  | { type: "evicted"; ids: number[] }
  | { type: "services"; names: string[]; blocked?: string[] }
  | { type: "properties"; paths: PropertyInfo[] }
  | { type: "pong" }
  | { type: "lagged"; skipped: number }

export type ServicesList = {
  services: string[]
  blocked: string[]
}

export type InvestigateTarget = "claude" | "cursor"

export type UpdateChannel = "homebrew" | "direct"

export type UpdateStatus = {
  installedVersion: string
  latestVersion?: string
  updateAvailable: boolean
  channel: UpdateChannel
  busy: boolean
}

export type UpdateEvent = {
  step: string
  progress: number
  error?: string
  restarting?: boolean
}
