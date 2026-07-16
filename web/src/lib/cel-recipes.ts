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
  { label: "Browser console", expression: 'kind == "console"' },
  {
    label: "Browser console errors",
    expression: 'kind == "console" && level == "error"',
  },
  {
    label: "Browser network errors",
    expression: 'kind == "network" && status >= 400',
  },
  {
    label: "Cursor hooks",
    expression: 'source == "cursor"',
  },
  {
    label: "Claude hooks",
    expression: 'source == "claude"',
  },
  {
    label: "Agent tool failures",
    expression:
      'kind == "postToolUseFailure" || kind == "PostToolUseFailure"',
  },
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
      { code: "source", hint: '"browser" | "cursor" | "claude"' },
      { code: "kind", hint: "console / network / hook event" },
      { code: "cmd", hint: "shell command when present" },
      { code: "_mzp.cwd", hint: "receiver terminal folder" },
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
