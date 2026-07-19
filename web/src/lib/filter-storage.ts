const STORAGE_KEY = "mizpah.query"
const LEGACY_KEY = "mizpah.filters"
const MODE_KEY = "mizpah.queryMode"
const SQL_KEY = "mizpah.sql"

export type QueryMode = "cel" | "sql"

export const DEFAULT_SQL =
  "SELECT service, level, count(*) AS n FROM all_logs GROUP BY 1, 2 ORDER BY n DESC"

export function readQueryFromSession(): string {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY)
    if (typeof raw === "string") return raw
    // Drop legacy chip JSON if present
    sessionStorage.removeItem(LEGACY_KEY)
    return ""
  } catch {
    return ""
  }
}

export function writeQueryToSession(query: string): void {
  try {
    sessionStorage.setItem(STORAGE_KEY, query)
    sessionStorage.removeItem(LEGACY_KEY)
  } catch {
    // Ignore quota / private-mode failures
  }
}

export function readQueryModeFromSession(): QueryMode {
  try {
    const raw = sessionStorage.getItem(MODE_KEY)
    if (raw === "sql" || raw === "cel") return raw
    return "cel"
  } catch {
    return "cel"
  }
}

export function writeQueryModeToSession(mode: QueryMode): void {
  try {
    sessionStorage.setItem(MODE_KEY, mode)
  } catch {
    // Ignore quota / private-mode failures
  }
}

export function readSqlFromSession(): string {
  try {
    const raw = sessionStorage.getItem(SQL_KEY)
    if (typeof raw === "string" && raw.length > 0) return raw
    return DEFAULT_SQL
  } catch {
    return DEFAULT_SQL
  }
}

export function writeSqlToSession(sql: string): void {
  try {
    sessionStorage.setItem(SQL_KEY, sql)
  } catch {
    // Ignore quota / private-mode failures
  }
}
