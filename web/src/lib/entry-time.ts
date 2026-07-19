import type { LogEntry } from "@/lib/types"

/** Prefer eventTime (payload wall clock) over receivedAt for display. */
export function entryDisplayTime(entry: LogEntry): string {
  return entry.eventTime ?? entry.receivedAt
}

export function isErrorLevel(level: string | null | undefined): boolean {
  if (!level) return false
  const l = level.toLowerCase()
  return (
    l === "error" ||
    l === "err" ||
    l === "fatal" ||
    l === "critical" ||
    l === "emerg" ||
    l === "alert"
  )
}

export function isWarnLevel(level: string | null | undefined): boolean {
  if (!level) return false
  const l = level.toLowerCase()
  return l === "warn" || l === "warning"
}

export function isErrorOrWarn(level: string | null | undefined): boolean {
  return isErrorLevel(level) || isWarnLevel(level)
}
