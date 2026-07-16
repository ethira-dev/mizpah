/** Relative time like "12s ago" / "3m ago". */
export function formatRelativeTime(iso: string, now = Date.now()): string {
  const t = new Date(iso).getTime()
  if (Number.isNaN(t)) return "—"
  const diffSec = Math.round((now - t) / 1000)
  const abs = Math.abs(diffSec)
  if (abs < 5) return "just now"
  if (abs < 60) return `${diffSec < 0 ? "in " : ""}${abs}s${diffSec < 0 ? "" : " ago"}`
  if (abs < 3600) {
    const m = Math.floor(abs / 60)
    return `${diffSec < 0 ? "in " : ""}${m}m${diffSec < 0 ? "" : " ago"}`
  }
  if (abs < 86400) {
    const h = Math.floor(abs / 3600)
    return `${diffSec < 0 ? "in " : ""}${h}h${diffSec < 0 ? "" : " ago"}`
  }
  const d = Math.floor(abs / 86400)
  return `${diffSec < 0 ? "in " : ""}${d}d${diffSec < 0 ? "" : " ago"}`
}

/** Join a CEL-style property path (matches hub discovery: `a.b`, `arr[0]`). */
export function joinJsonPath(parent: string, key: string | number): string {
  if (typeof key === "number") {
    return parent ? `${parent}[${key}]` : `[${key}]`
  }
  if (/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(key)) {
    return parent ? `${parent}.${key}` : key
  }
  const escaped = key.replace(/\\/g, "\\\\").replace(/"/g, '\\"')
  return parent ? `${parent}["${escaped}"]` : `["${escaped}"]`
}

export function levelVariant(
  level: string | null
): "default" | "secondary" | "destructive" | "outline" {
  if (!level) return "outline"
  if (level.includes("error") || level.includes("fatal") || level === "50") {
    return "destructive"
  }
  if (level.includes("warn") || level === "40") return "secondary"
  return "outline"
}

/** Left border accent class for the detail dialog header. */
export function levelAccentClass(level: string | null): string {
  if (!level) return "border-l-border"
  if (level.includes("error") || level.includes("fatal") || level === "50") {
    return "border-l-destructive"
  }
  if (level.includes("warn") || level === "40") {
    return "border-l-muted-foreground/50"
  }
  return "border-l-border"
}

export function primitiveToFilterValue(value: unknown): {
  text: string
  types: string[]
} | null {
  if (value === null) return { text: "null", types: ["null"] }
  if (typeof value === "string") return { text: value, types: ["string"] }
  if (typeof value === "number" && Number.isFinite(value)) {
    return { text: String(value), types: ["number"] }
  }
  if (typeof value === "boolean") {
    return { text: String(value), types: ["boolean"] }
  }
  return null
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    return false
  }
}
