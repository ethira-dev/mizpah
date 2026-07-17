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

export type LevelTone = "debug" | "info" | "warn" | "error" | "unknown"

/** Strip separators so `DEBUG`, `debug`, `d-bug` normalize alike. */
function levelToken(level: string): string {
  return level.toLowerCase().trim().replace(/[^a-z0-9]+/g, "")
}

/**
 * Map a raw level/severity string onto the UI tone spectrum.
 * Accepts common spellings, abbreviations, syslog (0–7), and pino/bunyan (10–60).
 */
export function levelTone(level: string | null | undefined): LevelTone {
  if (level == null || level === "") return "unknown"
  const l = levelToken(level)
  if (!l) return "unknown"

  if (/^\d+$/.test(l)) {
    const n = Number(l)
    // syslog: 0 emerg … 3 err → error; 4 warn; 5 notice / 6 info; 7 debug
    if (n <= 3) return "error"
    if (n === 4) return "warn"
    if (n === 5 || n === 6) return "info"
    if (n === 7) return "debug"
    // pino / bunyan style
    if (n >= 50) return "error"
    if (n >= 40) return "warn"
    if (n >= 30) return "info"
    if (n >= 10) return "debug"
    return "unknown"
  }

  // Single-letter (logcat / some CLIs): V/D/I/W/E/F/A
  if (l.length === 1) {
    if (l === "e" || l === "f" || l === "a") return "error"
    if (l === "w") return "warn"
    if (l === "i" || l === "n") return "info"
    if (l === "d" || l === "t" || l === "v") return "debug"
  }

  if (
    /^(err|error|errors|fatal|crit|critical|emerg|emergency|alert|panic|severe|fail|failure)$/.test(
      l
    ) ||
    l.includes("error") ||
    l.includes("fatal") ||
    l.includes("crit") ||
    l.includes("panic") ||
    l.includes("emerg") ||
    l.includes("severe")
  ) {
    return "error"
  }

  if (
    /^(warn|warning|warnings|wrn|caution)$/.test(l) ||
    l.includes("warn")
  ) {
    return "warn"
  }

  if (
    /^(info|informational|information|inf|notice|note|log)$/.test(l) ||
    l.includes("info") ||
    l.includes("notice")
  ) {
    return "info"
  }

  if (
    /^(debug|dbug|dbg|deb|verbose|verb|vrb|fine|finest|trace|trc)$/.test(l) ||
    l.includes("debug") ||
    l.includes("dbug") ||
    l.includes("trace") ||
    l.includes("verbose")
  ) {
    return "debug"
  }

  return "unknown"
}

/** Shared type + fill for level chips (mono, uppercase, error → cherry). */
export function levelBadgeClass(level: string | null | undefined): string {
  const type = "font-mono uppercase"
  switch (levelTone(level)) {
    case "error":
      return `${type} border-transparent bg-level-error/12 text-level-error`
    case "warn":
      return `${type} border-transparent bg-level-warn/12 text-level-warn`
    case "info":
      return `${type} border-transparent bg-level-info/12 text-level-info`
    case "debug":
      return `${type} border-transparent bg-level-debug/12 text-level-debug`
    default:
      return `${type} border-border text-muted-foreground`
  }
}

/** Left border accent for the detail dialog. */
export function levelAccentClass(level: string | null | undefined): string {
  switch (levelTone(level)) {
    case "error":
      return "border-l-4 border-l-level-error"
    case "warn":
      return "border-l-4 border-l-level-warn"
    case "info":
      return "border-l-4 border-l-level-info"
    case "debug":
      return "border-l-4 border-l-level-debug"
    default:
      return "border-l-4 border-l-border"
  }
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
