import {
  ChevronLeft,
  ChevronRight,
  ListFilter,
  Search,
  X,
} from "lucide-react"
import { useEffect, useMemo, useState } from "react"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { fetchProperties } from "@/lib/api"
import { buildCelEqualityFilter } from "@/lib/filter-from-property"
import { levelBadgeClass } from "@/lib/log-format"
import type { PropertyInfo } from "@/lib/types"
import { cn } from "@/lib/utils"

const EDGE_W = 8
const PEEK_W = 20
const OPEN_W = 288
const SEARCH_DEBOUNCE_MS = 200
const OPEN_STORAGE_KEY = "mizpah.propertyDrawer.open.v2"
const LEVEL_PATHS = ["level", "severity", "severity.name", "severity.level"]

type PropertyFilterDrawerProps = {
  /** Bumps when the hub rediscovers properties so the open drawer can refresh. */
  propertiesRevision: number
  /** Known property count from the live hub (for collapsed-rail indicator). */
  propertyCount?: number
  services: string[]
  onApplyFilter: (cel: string) => void
}

function readOpenPreference(): boolean {
  try {
    const raw = sessionStorage.getItem(OPEN_STORAGE_KEY)
    if (raw === null) return false
    return raw === "1"
  } catch {
    return false
  }
}

function writeOpenPreference(open: boolean) {
  try {
    sessionStorage.setItem(OPEN_STORAGE_KEY, open ? "1" : "0")
  } catch {
    /* ignore quota / private mode */
  }
}

function formatCount(n: number): string {
  if (n > 999) return "999+"
  return String(n)
}

function CountLabel({ count }: { count: number }) {
  if (count <= 0) return null
  return (
    <span className="shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground">
      {formatCount(count)}
    </span>
  )
}

function shortcutLabel(): string {
  if (typeof navigator !== "undefined" && /Mac|iPhone|iPad/i.test(navigator.platform)) {
    return "⌘\\"
  }
  return "Ctrl+\\"
}

function valuesFor(item: PropertyInfo): { value: string; count: number }[] {
  if (item.values && item.values.length > 0) {
    return item.values
  }
  return item.sampleValues.map((value) => ({ value, count: 0 }))
}

function findLevelProperty(items: PropertyInfo[]): PropertyInfo | undefined {
  for (const path of LEVEL_PATHS) {
    const match = items.find((item) => item.path === path)
    if (match) return match
  }
  return items.find(
    (item) =>
      item.path === "level" ||
      item.path.endsWith(".level") ||
      item.path.endsWith(".severity")
  )
}

export function PropertyFilterDrawer({
  propertiesRevision,
  propertyCount = 0,
  services,
  onApplyFilter,
}: PropertyFilterDrawerProps) {
  const [open, setOpen] = useState(readOpenPreference)
  const [peeking, setPeeking] = useState(false)
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set())
  const [search, setSearch] = useState("")
  const [debouncedSearch, setDebouncedSearch] = useState("")
  const [items, setItems] = useState<PropertyInfo[]>([])
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    writeOpenPreference(open)
  }, [open])

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key !== "\\" || !(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey) {
        return
      }
      const target = e.target as HTMLElement | null
      if (
        target &&
        (target.isContentEditable ||
          target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT")
      ) {
        return
      }
      e.preventDefault()
      setOpen((v) => !v)
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [])

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

  const searching = debouncedSearch.length > 0
  const displayCount = items.length > 0 ? items.length : propertyCount
  const showActivityDot = displayCount > 0 || services.length > 0

  const levelProperty = useMemo(() => findLevelProperty(items), [items])
  const levelValues = useMemo(() => {
    if (!levelProperty) return []
    return valuesFor(levelProperty).slice(0, 8)
  }, [levelProperty])

  const serviceChips = useMemo(() => services.slice(0, 8), [services])
  const showQuickFilters =
    !searching && (serviceChips.length > 0 || levelValues.length > 0)

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

  const width = open ? OPEN_W : peeking ? PEEK_W : 0
  const showChrome = open || peeking

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
          "flex h-full flex-col overflow-hidden",
          "bg-sidebar text-sidebar-foreground",
          "transition-[width] duration-200 ease-out",
          showChrome && "border-r border-sidebar-border"
        )}
        style={{ width }}
        onMouseEnter={() => {
          if (!open) setPeeking(true)
        }}
        aria-label="Property filters"
      >
        {showChrome && !open ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                className={cn(
                  "relative flex h-full w-full flex-col items-center justify-center gap-2",
                  "bg-sidebar-accent/60 text-muted-foreground",
                  "hover:bg-sidebar-accent hover:text-sidebar-foreground",
                  "focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none"
                )}
                onClick={() => {
                  setOpen(true)
                  setPeeking(false)
                }}
                aria-label="Expand property filters"
                aria-expanded={false}
              >
                <ListFilter className="size-3.5" />
                {showActivityDot ? (
                  <span
                    className="absolute top-3 size-1.5 rounded-full bg-sidebar-primary"
                    aria-hidden
                  />
                ) : null}
              </button>
            </TooltipTrigger>
            <TooltipContent side="right" sideOffset={8}>
              <span>Property filters</span>
              <kbd data-slot="kbd" className="font-mono text-[10px] text-muted-foreground">
                {shortcutLabel()}
              </kbd>
            </TooltipContent>
          </Tooltip>
        ) : null}

        {open ? (
        <>
          <div className="flex shrink-0 items-center justify-between gap-2 px-3 pt-3 pb-2">
            <div className="flex min-w-0 items-baseline gap-2">
              <span className="text-sm font-medium tracking-tight">Properties</span>
              {displayCount > 0 ? (
                <span className="font-mono text-[10px] tabular-nums text-muted-foreground">
                  {displayCount}
                </span>
              ) : null}
            </div>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  className="text-muted-foreground hover:text-sidebar-foreground"
                  onClick={() => {
                    setOpen(false)
                    setPeeking(false)
                  }}
                  aria-label="Collapse property filters"
                  aria-expanded={true}
                >
                  <ChevronLeft className="size-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent side="bottom" sideOffset={6}>
                <span>Collapse</span>
                <kbd data-slot="kbd" className="font-mono text-[10px] text-muted-foreground">
                  {shortcutLabel()}
                </kbd>
              </TooltipContent>
            </Tooltip>
          </div>

          <div className="shrink-0 px-3 pb-2">
            <div className="relative">
              <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="Filter properties…"
                className={cn(
                  "h-8 border-sidebar-border bg-sidebar-accent/40 pr-8 pl-8 text-xs md:text-xs",
                  "placeholder:text-muted-foreground/80 focus-visible:ring-sidebar-ring"
                )}
                aria-label="Search properties and values"
              />
              {search ? (
                <button
                  type="button"
                  className={cn(
                    "absolute top-1/2 right-2 -translate-y-1/2 rounded-sm p-0.5",
                    "text-muted-foreground opacity-70 hover:opacity-100",
                    "focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none"
                  )}
                  onClick={() => setSearch("")}
                  aria-label="Clear search"
                >
                  <X className="size-3.5" />
                </button>
              ) : null}
            </div>
          </div>

          <ScrollArea className="min-h-0 flex-1">
            <div className="flex flex-col gap-3 px-2 pb-3">
              {showQuickFilters ? (
                <div className="space-y-2.5 px-1">
                  {serviceChips.length > 0 ? (
                    <div className="space-y-1.5">
                      <p className="px-1 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">
                        Service
                      </p>
                      <div className="flex flex-wrap gap-1">
                        {serviceChips.map((name) => (
                          <button
                            key={name}
                            type="button"
                            className={cn(
                              "max-w-full truncate rounded-md px-2 py-1",
                              "border border-sidebar-border bg-sidebar-accent/50",
                              "font-mono text-[11px] text-sidebar-foreground/80",
                              "hover:border-sidebar-primary/40 hover:bg-sidebar-primary/10 hover:text-sidebar-foreground",
                              "focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none"
                            )}
                            title={name}
                            onClick={() =>
                              applyValue("service", name, ["string"])
                            }
                          >
                            {name}
                          </button>
                        ))}
                      </div>
                    </div>
                  ) : null}

                  {levelValues.length > 0 && levelProperty ? (
                    <div className="space-y-1.5">
                      <p className="px-1 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">
                        Level
                      </p>
                      <div className="flex flex-wrap gap-1">
                        {levelValues.map(({ value }) => (
                          <button
                            key={value}
                            type="button"
                            className={cn(
                              "max-w-full truncate rounded-md px-2 py-1 text-[11px]",
                              "hover:opacity-90",
                              "focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none",
                              levelBadgeClass(value)
                            )}
                            title={value}
                            onClick={() =>
                              applyValue(
                                levelProperty.path,
                                value,
                                levelProperty.types
                              )
                            }
                          >
                            {value}
                          </button>
                        ))}
                      </div>
                    </div>
                  ) : null}
                </div>
              ) : null}

              <div>
                {showQuickFilters ? (
                  <p className="mb-1.5 px-2 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">
                    Fields
                  </p>
                ) : null}

                {loading && items.length === 0 ? (
                  <p className="px-2 py-8 text-center text-xs text-muted-foreground">
                    Loading properties…
                  </p>
                ) : items.length === 0 ? (
                  <p className="px-2 py-8 text-center text-xs leading-relaxed text-muted-foreground">
                    {searching
                      ? "No properties match that search."
                      : "Waiting for logs to discover fields…"}
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
                              "hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none",
                              isOpen && "bg-sidebar-accent/70"
                            )}
                            onClick={() => toggleExpanded(item.path)}
                            aria-expanded={isOpen}
                          >
                            <ChevronRight
                              className={cn(
                                "size-3 shrink-0 text-muted-foreground transition-transform duration-150",
                                isOpen && "rotate-90"
                              )}
                            />
                            <span className="min-w-0 flex-1 truncate font-mono text-xs text-sidebar-foreground">
                              {item.path}
                            </span>
                            <CountLabel count={propCount} />
                          </button>

                          {isOpen ? (
                            <ul className="mt-0.5 mb-1.5 ml-[18px] space-y-0.5 border-l border-sidebar-border pl-2">
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
                                        "text-muted-foreground",
                                        "hover:bg-sidebar-primary/10 hover:text-sidebar-foreground",
                                        "focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none"
                                      )}
                                      title={value}
                                      onClick={() =>
                                        applyValue(item.path, value, item.types)
                                      }
                                    >
                                      <span className="min-w-0 flex-1 truncate font-mono text-[11px]">
                                        {value}
                                      </span>
                                      <CountLabel count={count} />
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
            </div>
          </ScrollArea>
        </>
        ) : null}
      </aside>
    </div>
  )
}
