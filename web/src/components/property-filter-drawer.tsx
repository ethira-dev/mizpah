import { ChevronLeft, ChevronRight, PanelLeft, Search, X } from "lucide-react"
import { useEffect, useState } from "react"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"
import { fetchProperties } from "@/lib/api"
import { buildCelEqualityFilter } from "@/lib/filter-from-property"
import type { PropertyInfo } from "@/lib/types"
import { cn } from "@/lib/utils"

const EDGE_W = 8
const PEEK_W = 20
const OPEN_W = 260
const SEARCH_DEBOUNCE_MS = 200

type PropertyFilterDrawerProps = {
  /** Bumps when the hub rediscovers properties so the open drawer can refresh. */
  propertiesRevision: number
  onApplyFilter: (cel: string) => void
}

function formatCount(n: number): string {
  if (n > 999) return "999+"
  return String(n)
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
  propertiesRevision,
  onApplyFilter,
}: PropertyFilterDrawerProps) {
  const [open, setOpen] = useState(false)
  const [peeking, setPeeking] = useState(false)
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set())
  const [search, setSearch] = useState("")
  const [debouncedSearch, setDebouncedSearch] = useState("")
  const [items, setItems] = useState<PropertyInfo[]>([])
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    const id = window.setTimeout(() => {
      setDebouncedSearch(search.trim())
    }, SEARCH_DEBOUNCE_MS)
    return () => window.clearTimeout(id)
  }, [search])

  useEffect(() => {
    if (!open) return

    let cancelled = false
    const id = window.setTimeout(() => {
      if (cancelled) return
      setLoading(true)
      void fetchProperties({ q: debouncedSearch || undefined })
        .then((props) => {
          if (!cancelled) setItems(props)
        })
        .catch(() => {
          if (!cancelled) setItems([])
        })
        .finally(() => {
          if (!cancelled) setLoading(false)
        })
    }, 0)

    return () => {
      cancelled = true
      window.clearTimeout(id)
    }
  }, [open, debouncedSearch, propertiesRevision])

  const width = open ? OPEN_W : peeking ? PEEK_W : 0
  const showChrome = open || peeking
  const searching = debouncedSearch.length > 0

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

  function valuesFor(item: PropertyInfo): { value: string; count: number }[] {
    if (item.values && item.values.length > 0) {
      return item.values
    }
    return item.sampleValues.map((value) => ({ value, count: 0 }))
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
                {loading && items.length === 0 ? (
                  <p className="px-2 py-6 text-center text-xs text-muted-foreground">
                    Loading…
                  </p>
                ) : items.length === 0 ? (
                  <p className="px-2 py-6 text-center text-xs text-muted-foreground">
                    {searching ? "No matches" : "Waiting for logs…"}
                  </p>
                ) : (
                  <ul className="space-y-0.5">
                    {items.map((item) => {
                      const values = valuesFor(item)
                      const isOpen =
                        expanded.has(item.path) ||
                        (searching && values.length > 0)
                      const propCount = item.count ?? 0
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
                              {values.length === 0 ? (
                                <li className="px-2 py-1 text-[11px] text-muted-foreground">
                                  No sample values
                                </li>
                              ) : (
                                values.map(({ value, count }) => (
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
                                        applyValue(item.path, value, item.types)
                                      }
                                    >
                                      <CountPill count={count} />
                                      <span className="min-w-0 flex-1 truncate font-mono text-[11px]">
                                        {value}
                                      </span>
                                    </button>
                                  </li>
                                ))
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
