/** Escape a string for use inside a CEL double-quoted literal. */
export function escapeCelString(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"')
}

function quoteCelString(value: string): string {
  return `"${escapeCelString(value)}"`
}

const NUMBER_RE = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$/

/**
 * Build a CEL filter from a discovered property path and sample value.
 * Truncated samples (ending with `…`) use `.contains(...)` instead of equality.
 */
export function buildCelEqualityFilter(
  path: string,
  value: string,
  types: string[] = []
): string {
  if (value.endsWith("…")) {
    const prefix = value.slice(0, -1)
    return `${path}.contains(${quoteCelString(prefix)})`
  }

  if (value === "null") {
    return `${path} == null`
  }

  const hasString = types.includes("string")
  const hasBool = types.includes("boolean") || types.includes("bool")
  const hasNumber = types.includes("number")
  const unknown = types.length === 0

  if (
    (value === "true" || value === "false") &&
    (hasBool || unknown) &&
    !hasString
  ) {
    return `${path} == ${value}`
  }

  if (NUMBER_RE.test(value) && (hasNumber || unknown) && !hasString) {
    return `${path} == ${value}`
  }

  return `${path} == ${quoteCelString(value)}`
}
