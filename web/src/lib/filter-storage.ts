import type { FilterChip, FilterOp } from "@/lib/types"
import { FILTER_OPS } from "@/lib/types"

const STORAGE_KEY = "mizpah.filters"

const VALID_OPS = new Set<string>(FILTER_OPS.map((o) => o.value))

function isFilterChip(value: unknown): value is FilterChip {
  if (!value || typeof value !== "object") return false
  const chip = value as Record<string, unknown>
  if (typeof chip.path !== "string" || !chip.path) return false
  if (typeof chip.op !== "string" || !VALID_OPS.has(chip.op)) return false
  if (chip.value !== undefined && chip.value !== null && typeof chip.value !== "string") {
    return false
  }
  if (chip.values !== undefined) {
    if (!Array.isArray(chip.values) || !chip.values.every((v) => typeof v === "string")) {
      return false
    }
  }
  return true
}

export function readFiltersFromSession(): FilterChip[] {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY)
    if (!raw) return []
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    return parsed.filter(isFilterChip).map((chip) => ({
      path: chip.path,
      op: chip.op as FilterOp,
      ...(chip.value !== undefined ? { value: chip.value } : {}),
      ...(chip.values !== undefined ? { values: chip.values } : {}),
    }))
  } catch {
    return []
  }
}

export function writeFiltersToSession(filters: FilterChip[]): void {
  try {
    sessionStorage.setItem(STORAGE_KEY, JSON.stringify(filters))
  } catch {
    // Ignore quota / private-mode failures
  }
}
