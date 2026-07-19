import {
  Bookmark,
  Copy,
  Filter,
  GitBranch,
  ListChecks,
  PanelRightOpen,
  Sparkles,
} from "lucide-react"
import type { ReactNode } from "react"
import { useCallback, useMemo, useState } from "react"

import {
  ContextMenu,
  ContextMenuCheckboxItem,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuRadioGroup,
  ContextMenuRadioItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from "@/components/ui/context-menu"
import { fetchTrace, setBookmark, startInvestigate } from "@/lib/api"
import { buildCelEqualityFilter } from "@/lib/filter-from-property"
import {
  copyText,
  joinJsonPath,
  levelOf,
  primitiveToFilterValue,
  summarizeLog,
} from "@/lib/log-format"
import type { LogEntry } from "@/lib/types"

const TRACE_FIELDS = [
  "trace_id",
  "traceId",
  "request_id",
  "requestId",
  "correlation_id",
  "correlationId",
  "span_id",
  "spanId",
  "opid",
] as const

const MAX_MATCH_FIELDS = 60
const VALUE_PREVIEW_LEN = 28

type MatchField = {
  path: string
  text: string
  types: string[]
}

function resolveOpid(
  data: Record<string, unknown>
): { field: string; value: string } | null {
  for (const field of TRACE_FIELDS) {
    const v = data[field]
    if (typeof v === "string" && v.trim()) {
      return { field, value: v.trim() }
    }
    if (typeof v === "number") {
      return { field, value: String(v) }
    }
  }
  return null
}

function celQuote(s: string): string {
  return `"${s.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`
}

function truncatePreview(text: string, max = VALUE_PREVIEW_LEN): string {
  if (text.length <= max) return text
  return `${text.slice(0, max - 1)}…`
}

/** Collect primitive fields (incl. nested) that can become CEL equality filters. */
function collectMatchFields(entry: LogEntry): MatchField[] {
  const out: MatchField[] = []

  out.push({
    path: "service",
    text: entry.service,
    types: ["string"],
  })

  function walk(value: unknown, path: string) {
    if (out.length >= MAX_MATCH_FIELDS) return

    const prim = primitiveToFilterValue(value)
    if (prim) {
      if (path) out.push({ path, text: prim.text, types: prim.types })
      return
    }

    if (value && typeof value === "object") {
      if (Array.isArray(value)) {
        for (let i = 0; i < value.length; i++) {
          if (out.length >= MAX_MATCH_FIELDS) break
          walk(value[i], joinJsonPath(path, i))
        }
        return
      }
      for (const [k, child] of Object.entries(value as Record<string, unknown>)) {
        if (out.length >= MAX_MATCH_FIELDS) break
        walk(child, joinJsonPath(path, k))
      }
    }
  }

  walk(entry.data, "")
  return out
}

function buildMatchFilter(
  fields: MatchField[],
  selected: Set<string>,
  join: "and" | "or"
): string | null {
  const clauses = fields
    .filter((f) => selected.has(f.path))
    .map((f) => buildCelEqualityFilter(f.path, f.text, f.types))
  if (clauses.length === 0) return null
  if (clauses.length === 1) return clauses[0]
  const sep = join === "and" ? " && " : " || "
  return clauses.join(sep)
}

type LogRowContextMenuProps = {
  entry: LogEntry
  children: ReactNode
  /** Select/focus the row when the menu opens (does not open detail). */
  onActivate: () => void
  onOpenDetails: () => void
  onApplyFilter: (cel: string) => void
}

export function LogRowContextMenu({
  entry,
  children,
  onActivate,
  onOpenDetails,
  onApplyFilter,
}: LogRowContextMenuProps) {
  const [busy, setBusy] = useState<string | null>(null)
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set())
  const [joinMode, setJoinMode] = useState<"and" | "or">("or")

  const level = useMemo(() => levelOf(entry.data), [entry.data])
  const opid = useMemo(() => resolveOpid(entry.data), [entry.data])
  const message = useMemo(() => summarizeLog(entry.data), [entry.data])
  const matchFields = useMemo(() => collectMatchFields(entry), [entry])

  const allSelected =
    matchFields.length > 0 && selectedPaths.size === matchFields.length

  function resetMatchFields() {
    setSelectedPaths(new Set())
    setJoinMode("or")
  }

  const run = useCallback(async (key: string, fn: () => Promise<void>) => {
    setBusy(key)
    try {
      await fn()
    } finally {
      setBusy(null)
    }
  }, [])

  function togglePath(path: string, checked: boolean) {
    setSelectedPaths((prev) => {
      const next = new Set(prev)
      if (checked) next.add(path)
      else next.delete(path)
      return next
    })
  }

  function toggleAll(checked: boolean) {
    setSelectedPaths(
      checked ? new Set(matchFields.map((f) => f.path)) : new Set()
    )
  }

  return (
    <ContextMenu
      onOpenChange={(open) => {
        if (open) onActivate()
      }}
    >
      <ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
      <ContextMenuContent className="w-56">
        <ContextMenuLabel className="font-mono truncate">
          #{entry.id} · {entry.service}
        </ContextMenuLabel>
        <ContextMenuSeparator />

        <ContextMenuItem
          onSelect={() => {
            onActivate()
            onOpenDetails()
          }}
        >
          <PanelRightOpen />
          Open details
        </ContextMenuItem>

        <ContextMenuSeparator />

        <ContextMenuItem
          disabled={busy != null}
          onSelect={() => {
            void run("copy-msg", async () => {
              await copyText(message)
            })
          }}
        >
          <Copy />
          Copy message
        </ContextMenuItem>
        <ContextMenuItem
          disabled={busy != null}
          onSelect={() => {
            void run("copy-json", async () => {
              await copyText(JSON.stringify(entry, null, 2))
            })
          }}
        >
          <Copy />
          Copy JSON
        </ContextMenuItem>
        <ContextMenuItem
          disabled={busy != null}
          onSelect={() => {
            void run("copy-id", async () => {
              await copyText(String(entry.id))
            })
          }}
        >
          <Copy />
          Copy ID
        </ContextMenuItem>

        <ContextMenuSeparator />

        <ContextMenuItem
          onSelect={() => {
            onApplyFilter(`service == ${celQuote(entry.service)}`)
          }}
        >
          <Filter />
          Filter by service
        </ContextMenuItem>
        <ContextMenuItem
          disabled={!level}
          onSelect={() => {
            if (!level) return
            onApplyFilter(`level == ${celQuote(level)}`)
          }}
        >
          <Filter />
          Filter by level
          {level ? (
            <span className="ml-auto font-mono text-xs text-muted-foreground">
              {level}
            </span>
          ) : null}
        </ContextMenuItem>
        <ContextMenuItem
          disabled={!opid || busy != null}
          onSelect={() => {
            if (!opid) return
            void run("trace", async () => {
              await fetchTrace(opid.value)
              const fields = TRACE_FIELDS.map(
                (f) => `${f} == ${celQuote(opid.value)}`
              )
              onApplyFilter(fields.join(" || "))
            })
          }}
        >
          <GitBranch />
          Show related
        </ContextMenuItem>

        <ContextMenuSub
          onOpenChange={(open) => {
            if (open) resetMatchFields()
          }}
        >
          <ContextMenuSubTrigger disabled={matchFields.length === 0}>
            <ListChecks />
            Match fields
          </ContextMenuSubTrigger>
          <ContextMenuSubContent className="w-72 max-h-80 overflow-y-auto">
            <ContextMenuCheckboxItem
              checked={allSelected}
              onCheckedChange={(checked) => toggleAll(checked === true)}
              onSelect={(e) => e.preventDefault()}
            >
              Select all
            </ContextMenuCheckboxItem>
            <ContextMenuSeparator />
            {matchFields.map((field) => (
              <ContextMenuCheckboxItem
                key={field.path}
                checked={selectedPaths.has(field.path)}
                onCheckedChange={(checked) =>
                  togglePath(field.path, checked === true)
                }
                onSelect={(e) => e.preventDefault()}
              >
                <span className="min-w-0 flex-1 truncate font-mono text-xs">
                  <span>{field.path}</span>
                  <span className="text-muted-foreground">
                    {" "}
                    = {truncatePreview(field.text)}
                  </span>
                </span>
              </ContextMenuCheckboxItem>
            ))}
            <ContextMenuSeparator />
            <ContextMenuLabel>Combine with</ContextMenuLabel>
            <ContextMenuRadioGroup
              value={joinMode}
              onValueChange={(v) => setJoinMode(v as "and" | "or")}
            >
              <ContextMenuRadioItem
                value="or"
                onSelect={(e) => e.preventDefault()}
              >
                Any field (OR)
              </ContextMenuRadioItem>
              <ContextMenuRadioItem
                value="and"
                onSelect={(e) => e.preventDefault()}
              >
                All fields (AND)
              </ContextMenuRadioItem>
            </ContextMenuRadioGroup>
            <ContextMenuSeparator />
            <ContextMenuItem
              disabled={selectedPaths.size === 0}
              onSelect={() => {
                const cel = buildMatchFilter(
                  matchFields,
                  selectedPaths,
                  joinMode
                )
                if (cel) onApplyFilter(cel)
              }}
            >
              <Filter />
              Apply
              <span className="ml-auto font-mono text-xs text-muted-foreground">
                {selectedPaths.size}
              </span>
            </ContextMenuItem>
          </ContextMenuSubContent>
        </ContextMenuSub>

        <ContextMenuSeparator />

        <ContextMenuItem
          disabled={busy != null}
          onSelect={() => {
            void run("bookmark", async () => {
              await setBookmark({ id: entry.id, marked: true })
            })
          }}
        >
          <Bookmark />
          Bookmark
        </ContextMenuItem>

        <ContextMenuSub>
          <ContextMenuSubTrigger disabled={busy != null}>
            <Sparkles />
            Investigate
          </ContextMenuSubTrigger>
          <ContextMenuSubContent className="w-44">
            <ContextMenuItem
              disabled={busy != null}
              onSelect={() => {
                void run("inv-cursor", async () => {
                  await startInvestigate("cursor", entry.id)
                })
              }}
            >
              With Cursor
            </ContextMenuItem>
            <ContextMenuItem
              disabled={busy != null}
              onSelect={() => {
                void run("inv-claude", async () => {
                  await startInvestigate("claude", entry.id)
                })
              }}
            >
              With Claude
            </ContextMenuItem>
          </ContextMenuSubContent>
        </ContextMenuSub>
      </ContextMenuContent>
    </ContextMenu>
  )
}
