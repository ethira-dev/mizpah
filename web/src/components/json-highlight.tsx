import type { ReactNode } from "react"

import { cn } from "@/lib/utils"

type JsonHighlightProps = {
  value: unknown
  className?: string
}

const TOKEN_RE =
  /("(?:\\.|[^"\\])*")\s*:|("(?:\\.|[^"\\])*")|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)|\b(true|false|null)\b|([{}[\],:])/g

function highlightJson(json: string): ReactNode[] {
  const nodes: React.ReactNode[] = []
  let lastIndex = 0
  let match: RegExpExecArray | null
  let key = 0

  TOKEN_RE.lastIndex = 0
  while ((match = TOKEN_RE.exec(json)) !== null) {
    if (match.index > lastIndex) {
      nodes.push(json.slice(lastIndex, match.index))
    }

    const [full, keyStr, str, num, literal, punct] = match

    if (keyStr !== undefined) {
      const colonIdx = full.lastIndexOf(":")
      nodes.push(
        <span key={key++} className="text-foreground">
          {full.slice(0, colonIdx)}
        </span>
      )
      nodes.push(
        <span key={key++} className="text-muted-foreground">
          {full.slice(colonIdx)}
        </span>
      )
    } else if (str !== undefined) {
      nodes.push(
        <span key={key++} className="text-emerald-600 dark:text-emerald-400">
          {str}
        </span>
      )
    } else if (num !== undefined) {
      nodes.push(
        <span key={key++} className="text-sky-600 dark:text-sky-400">
          {num}
        </span>
      )
    } else if (literal !== undefined) {
      nodes.push(
        <span key={key++} className="text-amber-600 dark:text-amber-400">
          {literal}
        </span>
      )
    } else if (punct !== undefined) {
      nodes.push(
        <span key={key++} className="text-muted-foreground">
          {punct}
        </span>
      )
    }

    lastIndex = match.index + full.length
  }

  if (lastIndex < json.length) {
    nodes.push(json.slice(lastIndex))
  }

  return nodes
}

export function JsonHighlight({ value, className }: JsonHighlightProps) {
  const json = JSON.stringify(value, null, 2) ?? "null"

  return (
    <pre
      className={cn(
        "overflow-auto font-mono text-[13px] leading-relaxed",
        className
      )}
    >
      {highlightJson(json)}
    </pre>
  )
}
