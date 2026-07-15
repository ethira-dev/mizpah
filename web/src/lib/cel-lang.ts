import {
  autocompletion,
  completionKeymap,
  type Completion,
  type CompletionContext,
  type CompletionResult,
} from "@codemirror/autocomplete"
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands"
import {
  HighlightStyle,
  StreamLanguage,
  syntaxHighlighting,
  type StreamParser,
} from "@codemirror/language"
import { EditorState, type Extension } from "@codemirror/state"
import { EditorView, keymap, placeholder as cmPlaceholder } from "@codemirror/view"
import { tags as t } from "@lezer/highlight"

const CEL_KEYWORDS = new Set([
  "true",
  "false",
  "null",
  "in",
])

const CEL_FUNCTIONS: Completion[] = [
  { label: "has", type: "function", detail: "has(path) → bool", info: "True if the field path exists" },
  { label: "size", type: "function", detail: "size(x) → int", info: "Length of string, list, or map" },
  { label: "int", type: "function", detail: "int(x) → int" },
  { label: "uint", type: "function", detail: "uint(x) → uint" },
  { label: "double", type: "function", detail: "double(x) → double" },
  { label: "string", type: "function", detail: "string(x) → string" },
  { label: "matches", type: "method", detail: "s.matches(re) → bool", info: "Regex match (like)" },
  { label: "contains", type: "method", detail: "s.contains(sub) → bool" },
  { label: "startsWith", type: "method", detail: "s.startsWith(prefix) → bool" },
  { label: "endsWith", type: "method", detail: "s.endsWith(suffix) → bool" },
]

const CEL_KEYWORD_COMPLETIONS: Completion[] = [
  { label: "true", type: "keyword" },
  { label: "false", type: "keyword" },
  { label: "null", type: "keyword" },
  { label: "in", type: "keyword" },
  { label: "&&", type: "keyword", detail: "AND" },
  { label: "||", type: "keyword", detail: "OR" },
]

const celParser: StreamParser<unknown> = {
  name: "cel",
  startState() {
    return {}
  },
  token(stream) {
    if (stream.eatSpace()) return null

    // Line comment
    if (stream.match("//")) {
      stream.skipToEnd()
      return "comment"
    }

    // Strings
    if (stream.match('"') || stream.match("'")) {
      const quote = stream.current()
      let escaped = false
      while (!stream.eol()) {
        const ch = stream.next()
        if (!escaped && ch === quote) break
        escaped = !escaped && ch === "\\"
      }
      return "string"
    }

    // Bytes literals b"..." / b'...'
    if (stream.match(/^b["']/)) {
      const quote = stream.current().slice(-1)
      let escaped = false
      while (!stream.eol()) {
        const ch = stream.next()
        if (!escaped && ch === quote) break
        escaped = !escaped && ch === "\\"
      }
      return "string"
    }

    // Numbers
    if (stream.match(/^[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?/)) {
      return "number"
    }

    // Operators / punctuation
    if (stream.match(/^(&&|\|\||==|!=|<=|>=|[<>!?:.+\-*/%[\]{}(),])/)) {
      return "operator"
    }

    // Identifiers / keywords
    if (stream.match(/^[A-Za-z_][A-Za-z0-9_]*/)) {
      const word = stream.current()
      if (CEL_KEYWORDS.has(word)) return "keyword"
      return "variableName"
    }

    stream.next()
    return null
  },
  languageData: {
    commentTokens: { line: "//" },
  },
}

export const celLanguage = StreamLanguage.define(celParser)

const celHighlight = HighlightStyle.define([
  { tag: t.keyword, color: "var(--color-chart-2)", fontWeight: "500" },
  { tag: t.string, color: "var(--color-chart-3)" },
  { tag: t.number, color: "var(--color-chart-4)" },
  { tag: t.comment, color: "var(--color-muted-foreground)", fontStyle: "italic" },
  { tag: t.operator, color: "var(--color-foreground)" },
  { tag: t.variableName, color: "var(--color-primary)" },
])

/** Lightweight client-side sanity check (brackets / quotes), not a full CEL parse. */
export function celSyntaxHint(source: string): string | null {
  const trimmed = source.trim()
  if (!trimmed) return null

  let paren = 0
  let bracket = 0
  let brace = 0
  let inString: '"' | "'" | null = null
  let escaped = false

  for (let i = 0; i < trimmed.length; i++) {
    const ch = trimmed[i]!
    if (inString) {
      if (escaped) {
        escaped = false
        continue
      }
      if (ch === "\\") {
        escaped = true
        continue
      }
      if (ch === inString) inString = null
      continue
    }
    if (ch === '"' || ch === "'") {
      inString = ch
      continue
    }
    if (ch === "/" && trimmed[i + 1] === "/") break
    if (ch === "(") paren++
    else if (ch === ")") paren--
    else if (ch === "[") bracket++
    else if (ch === "]") bracket--
    else if (ch === "{") brace++
    else if (ch === "}") brace--
    if (paren < 0 || bracket < 0 || brace < 0) {
      return "Unbalanced brackets"
    }
  }

  if (inString) return "Unterminated string"
  if (paren !== 0 || bracket !== 0 || brace !== 0) return "Unbalanced brackets"
  return null
}

function pathCompletions(paths: string[]): Completion[] {
  const seen = new Set<string>()
  const out: Completion[] = []
  for (const path of ["service", "level", ...paths]) {
    if (!path || seen.has(path)) continue
    seen.add(path)
    out.push({
      label: path,
      type: "variable",
      detail: "property",
      boost: path === "service" || path === "level" ? 2 : 1,
    })
  }
  return out
}

function celCompletionSource(paths: string[]) {
  const variables = pathCompletions(paths)
  return (context: CompletionContext): CompletionResult | null => {
    const word = context.matchBefore(/[A-Za-z_][\w.]*/)
    if (!word && !context.explicit) return null
    const from = word ? word.from : context.pos
    const options = [...CEL_FUNCTIONS, ...CEL_KEYWORD_COMPLETIONS, ...variables]
    return {
      from,
      options,
      validFor: /^[\w.]*$/,
    }
  }
}

export type CelEditorOptions = {
  placeholder?: string
  paths?: string[]
}

export function createCelExtensions(opts: CelEditorOptions = {}): Extension[] {
  const paths = opts.paths ?? []
  return [
    celLanguage,
    syntaxHighlighting(celHighlight),
    history(),
    keymap.of([...defaultKeymap, ...historyKeymap, ...completionKeymap]),
    autocompletion({
      override: [celCompletionSource(paths)],
      activateOnTyping: true,
    }),
    EditorState.allowMultipleSelections.of(false),
    EditorView.lineWrapping,
    cmPlaceholder(opts.placeholder ?? 'CEL filter, e.g. level == "error" && msg.contains("timeout")'),
    EditorView.theme({
      "&": {
        fontSize: "12px",
        fontFamily: "var(--font-mono)",
        backgroundColor: "transparent",
        color: "var(--color-foreground)",
      },
      ".cm-content": {
        fontFamily: "var(--font-mono)",
        padding: "2px 0",
        caretColor: "var(--color-foreground)",
        backgroundColor: "transparent",
      },
      ".cm-scroller": {
        fontFamily: "var(--font-mono)",
        lineHeight: "1.4",
        overflowX: "auto",
      },
      "&.cm-focused": {
        outline: "none",
      },
      ".cm-gutters": {
        display: "none",
      },
      ".cm-activeLine": {
        backgroundColor: "transparent",
      },
      ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
        backgroundColor: "color-mix(in oklch, var(--color-foreground) 22%, transparent) !important",
      },
      ".cm-cursor, .cm-dropCursor": {
        borderLeftColor: "var(--color-foreground)",
      },
      ".cm-tooltip": {
        backgroundColor: "var(--color-popover)",
        color: "var(--color-popover-foreground)",
        border: "1px solid var(--color-border)",
        borderRadius: "8px",
        fontFamily: "var(--font-mono)",
        fontSize: "12px",
      },
      ".cm-tooltip.cm-tooltip-autocomplete > ul > li[aria-selected]": {
        backgroundColor: "var(--color-accent)",
        color: "var(--color-accent-foreground)",
      },
      ".cm-completionIcon": {
        opacity: 0.5,
      },
      ".cm-placeholder": {
        color: "var(--color-muted-foreground)",
        fontStyle: "normal",
      },
    }),
  ]
}
