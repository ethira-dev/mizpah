export type FilterOp = "eq" | "neq" | "contains" | "gt" | "lt" | "exists" | "in"

export type FilterChip = {
  path: string
  op: FilterOp
  value?: string | null
  values?: string[]
}

export type LogEntry = {
  id: number
  receivedAt: string
  service: string
  data: Record<string, unknown>
}

export type PropertyInfo = {
  path: string
  types: string[]
  sampleValues: string[]
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

export type WsEvent =
  | { type: "log"; entry: LogEntry }
  | { type: "evicted"; ids: number[] }
  | { type: "services"; names: string[] }
  | { type: "properties"; paths: PropertyInfo[] }

export const FILTER_OPS: { value: FilterOp; label: string }[] = [
  { value: "eq", label: "=" },
  { value: "neq", label: "!=" },
  { value: "contains", label: "contains" },
  { value: "gt", label: ">" },
  { value: "lt", label: "<" },
  { value: "exists", label: "exists" },
  { value: "in", label: "in" },
]
