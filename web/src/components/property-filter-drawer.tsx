import { ChevronLeft, ChevronRight, PanelLeft, Search, X } from "lucide-react"
import { useMemo, useState } from "react"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"
import { buildCelEqualityFilter } from "@/lib/filter-from-property"
import type { LogEntry, PropertyInfo } from "@/lib/types"
import { cn } from "@/lib/utils"

const EDGE_W = 8
const PEEK_W = 20
const OPEN_W = 260

type PropertyFilterDrawerProps = {
  properties: PropertyInfo[]
  services: string[]
  entries: LogEntry[]
  onApplyFilter: (cel: string) => void
}

function formatCount(n: number): string {
  if (n > 999) return "999+"
  return String(n)
}

function compareAlpha(a: string, b: string): number {
  return a.localeCompare(b, undefined, { sensitivity: "base" })
}

/** Resolve a dotted / bracket path in log JSON (`user.id`, `items[0].name`). */
function getAtPath(data: Record<string, unknown>, path: string): unknown {
  const parts = path.match(/[^.[\]]+|\[(?:\d+)\]/g)
  if (!parts) return undefined
  let cur: unknown = data
  for (const raw of parts) {
    if (cur == null || typeof cur !== "object") return undefined
    const key = raw.startsWith("[") ? raw.slice(1, -1) : raw
    cur = (cur as Record<string, unknown>)[key]
  }
  return cur
}

function valueExists(entry: LogEntry, path: string): boolean {
  if (path === "service") return true
  return getAtPath(entry.data, path) !== undefined
}

/** Match sample string form used by the store (bool/number/null/string/truncated). */
function sampleMatches(actual: unknown, sample: string): boolean {
  if (sample.endsWith("…")) {
    const prefix = sample.slice(0, -1)
    return typeof actual === "string" && actual.startsWith(prefix)
  }
  if (actual === null) return sample === "null"
  if (typeof actual === "boolean") return sample === String(actual)
  if (typeof actual === "number") return sample === String(actual)
  if (typeof actual === "string") return sample === actual
  return false
}

function valueMatches(entry: LogEntry, path: string, sample: string): boolean {
  if (path === "service") return entry.service === sample
  const actual = getAtPath(entry.data, path)
  if (actual === undefined) return false
  return sampleMatches(actual, sample)
}

function CountPill({ count }: { count: number }) {
  return (
    <span
      className={cn(
        "inline-flex h-4 w-9 shrink-0 items-center justify-center rounded-sm",
        "border border-border bg-muted font-mono text-[10px] tabular-nums text-muted-foreground"
      )}
    >
      {formatCount(count)}
    </span>
  )
}

export function PropertyFilterDrawer({
  properties,
  services,
  entries,
  onApplyFilter,
}: PropertyFilterDrawerProps) {
  const [open, setOpen] = useState(false)
  const [peeking, setPeeking] = useState(false)
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set())
  const [search, setSearch] = useState("")

  const items = useMemo((): PropertyInfo[] => {
    const list: PropertyInfo[] = properties.filter((p) => p.path !== "service")
    if (services.length > 0) {
      list.push({
        path: "service",
        types: ["string"],
        sampleValues: [...services],
      })
    }
    return list
      .map((item) => ({
        ...item,
        sampleValues: [...item.sampleValues].sort(compareAlpha),
      }))
      .sort((a, b) => compareAlpha(a.path, b.path))
  }, [properties, services])

  const filteredItems = useMemo(() => {
    const q = search.trim().toLowerCase()
    if (!q) return items

    const out: PropertyInfo[] = []
    for (const item of items) {
      const pathMatch = item.path.toLowerCase().includes(q)
      const matchedValues = pathMatch
        ? item.sampleValues
        : item.sampleValues.filter((v) => v.toLowerCase().includes(q))
      if (!pathMatch && matchedValues.length === 0) continue
      out.push({ ...item, sampleValues: matchedValues })
    }
    return out
  }, [items, search])

  const counts = useMemo(() => {
    const propertyCounts = new Map<string, number>()
    const valueCounts = new Map<string, number>()

    for (const item of items) {
      let propCount = 0
      const valueMap = new Map(item.sampleValues.map((v) => [v, 0]))

      for (const entry of entries) {
        if (!valueExists(entry, item.path)) continue
        propCount++
        for (const sample of item.sampleValues) {
          if (valueMatches(entry, item.path, sample)) {
            valueMap.set(sample, (valueMap.get(sample) ?? 0) + 1)
          }
        }
      }

      propertyCounts.set(item.path, propCount)
      for (const [sample, n] of valueMap) {
        valueCounts.set(`${item.path}\0${sample}`, n)
      }
    }

    return { propertyCounts, valueCounts }
  }, [entries, items])

  const width = open ? OPEN_W : peeking ? PEEK_W : 0
  const showChrome = open || peeking
  const searching = search.trim().length > 0

  function toggleExpanded(path: string) {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })
  }

  function applyValue(path: string, value: string, types: string[]) {
    onApplyFilter(buildCelEqualityFilter(path, value, types))
  }

  return (
    <div
      className="relative flex h-full shrink-0"
      onMouseLeave={() => {
        if (!open) setPeeking(false)
      }}
    >
      {!open && !peeking ? (
        <div
          className="absolute inset-y-0 left-0 z-20"
          style={{ width: EDGE_W }}
          onMouseEnter={() => setPeeking(true)}
          aria-hidden
        />
      ) : null}

      <aside
        className={cn(
          "flex h-full flex-col overflow-hidden bg-background",
          "transition-[width] duration-200 ease-out",
          showChrome && "border-r border-border"
        )}
        style={{ width }}
        onMouseEnter={() => {
          if (!open) setPeeking(true)
        }}
      >
        {showChrome && !open ? (
          <button
            type="button"
            className={cn(
              "flex h-full w-full items-center justify-center bg-muted text-muted-foreground",
              "hover:bg-muted/80 hover:text-foreground",
              "focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            )}
            onClick={() => {
              setOpen(true)
              setPeeking(false)
            }}
            aria-label="Expand property filters"
          >
            <ChevronRight className="size-3.5" />
          </button>
        ) : null}

        {open ? (
          <>
            <div className="flex shrink-0 items-center justify-between gap-2 border-b border-border px-3 py-2">
              <div className="flex min-w-0 items-center gap-1.5">
                <PanelLeft className="size-3.5 shrink-0 text-muted-foreground" />
                <span className="text-sm font-medium">Filters</span>
              </div>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                onClick={() => {
                  setOpen(false)
                  setPeeking(false)
                }}
                aria-label="Collapse property filters"
              >
                <ChevronLeft className="size-3.5" />
              </Button>
            </div>

            <div className="shrink-0 border-b border-border p-2">
              <div className="relative">
                <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
                <Input
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                  placeholder="Search properties…"
                  className="h-7 pr-7 pl-7 text-xs md:text-xs"
                  aria-label="Search properties and values"
                />
                {search ? (
                  <button
                    type="button"
                    className="absolute top-1/2 right-1.5 -translate-y-1/2 rounded-sm p-0.5 text-muted-foreground opacity-70 hover:opacity-100 focus-visible:ring-2 focus-visible:ring-ring"
                    onClick={() => setSearch("")}
                    aria-label="Clear search"
                  >
                    <X className="size-3.5" />
                  </button>
                ) : null}
              </div>
            </div>

            <ScrollArea className="min-h-0 flex-1">
              <div className="p-2">
                {items.length === 0 ? (
                  <p className="px-2 py-6 text-center text-xs text-muted-foreground">
                    Waiting for logs…
                  </p>
                ) : filteredItems.length === 0 ? (
                  <p className="px-2 py-6 text-center text-xs text-muted-foreground">
                    No matches
                  </p>
                ) : (
                  <ul className="space-y-0.5">
                    {filteredItems.map((item) => {
                      const isOpen =
                        expanded.has(item.path) ||
                        (searching && item.sampleValues.length > 0)
                      const propCount =
                        counts.propertyCounts.get(item.path) ?? 0
                      return (
                        <li key={item.path}>
                          <button
                            type="button"
                            className={cn(
                              "flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-left",
                              "hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none",
                              isOpen && "bg-muted/60"
                            )}
                            onClick={() => toggleExpanded(item.path)}
                            aria-expanded={isOpen}
                          >
                            <ChevronRight
                              className={cn(
                                "size-3 shrink-0 text-muted-foreground transition-transform",
                                isOpen && "rotate-90"
                              )}
                            />
                            <CountPill count={propCount} />
                            <span className="min-w-0 flex-1 truncate font-mono text-xs">
                              {item.path}
                            </span>
                          </button>

                          {isOpen ? (
                            <ul className="mt-0.5 mb-1 ml-4 space-y-0.5 border-l border-border pl-2">
                              {item.sampleValues.length === 0 ? (
                                <li className="px-2 py-1 text-[11px] text-muted-foreground">
                                  No sample values
                                </li>
                              ) : (
                                item.sampleValues.map((value) => {
                                  const valueCount =
                                    counts.valueCounts.get(
                                      `${item.path}\0${value}`
                                    ) ?? 0
                                  return (
                                    <li key={value}>
                                      <button
                                        type="button"
                                        className={cn(
                                          "flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left",
                                          "text-muted-foreground hover:bg-muted hover:text-foreground",
                                          "focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                                        )}
                                        title={value}
                                        onClick={() =>
                                          applyValue(
                                            item.path,
                                            value,
                                            item.types
                                          )
                                        }
                                      >
                                        <CountPill count={valueCount} />
                                        <span className="min-w-0 flex-1 truncate font-mono text-[11px]">
                                          {value}
                                        </span>
                                      </button>
                                    </li>
                                  )
                                })
                              )}
                            </ul>
                          ) : null}
                        </li>
                      )
                    })}
                  </ul>
                )}
              </div>
            </ScrollArea>
          </>
        ) : null}
      </aside>
    </div>
  )
}
