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
