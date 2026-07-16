export type CelRecipe = {
  label: string
  expression: string
}

export const CEL_RECIPES: CelRecipe[] = [
  { label: "Errors", expression: 'level == "error"' },
  {
    label: "Service errors",
    expression: 'service == "api" && level == "error"',
  },
  { label: "Contains", expression: 'msg.contains("timeout")' },
  { label: "Has field", expression: "has(user.id)" },
  { label: "Levels", expression: 'level in ["error", "warn"]' },
  { label: "Regex", expression: 'msg.matches("(?i)time.?out")' },
]

export type CheatSheetGroup = {
  title: string
  items: { code: string; hint: string }[]
}

export const CEL_CHEAT_SHEET: CheatSheetGroup[] = [
  {
    title: "Bindings",
    items: [
      { code: "service", hint: "stream service tag" },
      { code: "level", hint: "level / severity / lvl" },
      { code: "cmd", hint: "shell command when present" },
      { code: "field.nested", hint: "JSON fields via ." },
    ],
  },
  {
    title: "Combine",
    items: [
      { code: "&&", hint: "and" },
      { code: "||", hint: "or" },
      { code: "==  !=", hint: "compare" },
      { code: "in [...]", hint: "one of" },
    ],
  },
  {
    title: "Helpers",
    items: [
      { code: "contains", hint: "substring" },
      { code: "matches", hint: "regex" },
      { code: "has(path)", hint: "field exists" },
      { code: "startsWith", hint: "prefix" },
    ],
  },
]

/** Prefer discovered fields; always include service / level as fallbacks. */
export function fieldChips(paths: string[], limit = 8): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const path of ["service", "level", ...paths]) {
    if (!path || seen.has(path)) continue
    seen.add(path)
    out.push(path)
    if (out.length >= limit) break
  }
  return out
}
