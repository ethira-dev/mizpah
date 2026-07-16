export type QueryHistoryEntry = {
  expression: string
  usedAt: number
}

export type SavedQuery = {
  id: string
  expression: string
  name: string
  savedAt: number
}

const HISTORY_KEY = "mizpah.query.history"
const SAVED_KEY = "mizpah.query.saved"
const HISTORY_CAP = 40
const SAVED_CAP = 50

type Listener = () => void

export type QueryLibrarySnapshot = {
  history: QueryHistoryEntry[]
  saved: SavedQuery[]
}

const listeners = new Set<Listener>()
let cachedSnapshot: QueryLibrarySnapshot | null = null

function emit(): void {
  cachedSnapshot = null
  for (const listener of listeners) listener()
}

export function subscribeQueryLibrary(listener: Listener): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

function readJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key)
    if (!raw) return fallback
    return JSON.parse(raw) as T
  } catch {
    return fallback
  }
}

function writeJson(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value))
    emit()
  } catch {
    // Ignore quota / private-mode failures
  }
}

function normalizeExpression(expression: string): string {
  return expression.trim()
}

export function defaultQueryName(expression: string, maxLen = 48): string {
  const trimmed = normalizeExpression(expression)
  if (trimmed.length <= maxLen) return trimmed
  return `${trimmed.slice(0, maxLen - 1)}…`
}

export function readHistory(): QueryHistoryEntry[] {
  const entries = readJson<QueryHistoryEntry[]>(HISTORY_KEY, [])
  if (!Array.isArray(entries)) return []
  return entries.filter(
    (e) =>
      e &&
      typeof e.expression === "string" &&
      e.expression.trim() !== "" &&
      typeof e.usedAt === "number"
  )
}

export function readSaved(): SavedQuery[] {
  const entries = readJson<SavedQuery[]>(SAVED_KEY, [])
  if (!Array.isArray(entries)) return []
  return entries.filter(
    (e) =>
      e &&
      typeof e.id === "string" &&
      typeof e.expression === "string" &&
      e.expression.trim() !== "" &&
      typeof e.name === "string" &&
      typeof e.savedAt === "number"
  )
}

export function getQueryLibrarySnapshot(): QueryLibrarySnapshot {
  if (!cachedSnapshot) {
    cachedSnapshot = { history: readHistory(), saved: readSaved() }
  }
  return cachedSnapshot
}

export function pushHistory(expression: string): void {
  const normalized = normalizeExpression(expression)
  if (!normalized) return

  const now = Date.now()
  const next = [
    { expression: normalized, usedAt: now },
    ...readHistory().filter((e) => e.expression !== normalized),
  ].slice(0, HISTORY_CAP)

  writeJson(HISTORY_KEY, next)
}

export function removeHistory(expression: string): void {
  const normalized = normalizeExpression(expression)
  writeJson(
    HISTORY_KEY,
    readHistory().filter((e) => e.expression !== normalized)
  )
}

export function findSavedByExpression(expression: string): SavedQuery | undefined {
  const normalized = normalizeExpression(expression)
  return readSaved().find((e) => e.expression === normalized)
}

export function saveQuery(input: {
  expression: string
  name?: string
}): SavedQuery | null {
  const expression = normalizeExpression(input.expression)
  if (!expression) return null

  const name = (input.name?.trim() || defaultQueryName(expression)).trim()
  const existing = findSavedByExpression(expression)
  const now = Date.now()

  let next: SavedQuery[]
  if (existing) {
    next = readSaved().map((e) =>
      e.id === existing.id ? { ...e, name, savedAt: now } : e
    )
  } else {
    const entry: SavedQuery = {
      id: crypto.randomUUID(),
      expression,
      name,
      savedAt: now,
    }
    next = [entry, ...readSaved()].slice(0, SAVED_CAP)
  }

  writeJson(SAVED_KEY, next)
  return next.find((e) => e.expression === expression) ?? null
}

export function removeSaved(id: string): void {
  writeJson(
    SAVED_KEY,
    readSaved().filter((e) => e.id !== id)
  )
}

export function removeSavedByExpression(expression: string): void {
  const normalized = normalizeExpression(expression)
  writeJson(
    SAVED_KEY,
    readSaved().filter((e) => e.expression !== normalized)
  )
}
