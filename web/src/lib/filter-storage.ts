const STORAGE_KEY = "mizpah.query"
const LEGACY_KEY = "mizpah.filters"

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
