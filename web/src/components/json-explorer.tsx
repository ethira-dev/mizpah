import {
  ChevronDown,
  ChevronRight,
  ClipboardCopy,
  Copy,
  Filter,
  Search,
  X,
} from "lucide-react"
import {
  useCallback,
  useMemo,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react"

import { JsonHighlight } from "@/components/json-highlight"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { buildCelEqualityFilter } from "@/lib/filter-from-property"
import {
  copyText,
  joinJsonPath,
  primitiveToFilterValue,
} from "@/lib/log-format"
import { cn } from "@/lib/utils"

export type JsonViewMode = "tree" | "raw"

type JsonExplorerProps = {
  value: Record<string, unknown>
  mode: JsonViewMode
  onModeChange: (mode: JsonViewMode) => void
  onApplyFilter?: (cel: string) => void
  searchRef?: React.RefObject<HTMLInputElement | null>
  /** Extra controls rendered after the mode toggle (e.g. Copy JSON). */
  toolbarStart?: ReactNode
  className?: string
}

type NodeKind = "object" | "array" | "primitive"

function kindOf(value: unknown): NodeKind {
  if (value === null || typeof value !== "object") return "primitive"
  return Array.isArray(value) ? "array" : "object"
}

function collectExpandablePaths(value: unknown, path: string, out: Set<string>) {
  const kind = kindOf(value)
  if (kind === "primitive") return
  if (path) out.add(path)
  if (kind === "object") {
    const obj = value as Record<string, unknown>
    for (const [k, child] of Object.entries(obj)) {
      collectExpandablePaths(child, joinJsonPath(path, k), out)
    }
  } else if (kind === "array") {
    const arr = value as unknown[]
    for (let i = 0; i < arr.length; i++) {
      collectExpandablePaths(arr[i], joinJsonPath(path, i), out)
    }
  }
}

function initialExpandedPaths(value: Record<string, unknown>): Set<string> {
  const next = new Set<string>()
  for (const [k, child] of Object.entries(value)) {
    if (kindOf(child) !== "primitive") {
      next.add(joinJsonPath("", k))
    }
  }
  return next
}

function pathMatchesSearch(
  path: string,
  value: unknown,
  query: string
): boolean {
  if (!query) return true
  const q = query.toLowerCase()
  if (path.toLowerCase().includes(q)) return true
  if (kindOf(value) === "primitive") {
    const s = value === null ? "null" : String(value)
    return s.toLowerCase().includes(q)
  }
  return false
}

/** True if this node or any descendant matches the search. */
function subtreeMatches(value: unknown, path: string, query: string): boolean {
  if (!query) return true
  if (pathMatchesSearch(path, value, query)) return true
  const kind = kindOf(value)
  if (kind === "object") {
    return Object.entries(value as Record<string, unknown>).some(([k, child]) =>
      subtreeMatches(child, joinJsonPath(path, k), query)
    )
  }
  if (kind === "array") {
    return (value as unknown[]).some((child, i) =>
      subtreeMatches(child, joinJsonPath(path, i), query)
    )
  }
  return false
}

function collectMatchingAncestorPaths(
  value: unknown,
  path: string,
  query: string,
  out: Set<string>
): boolean {
  if (!query) return true
  const kind = kindOf(value)
  let selfOrChild = pathMatchesSearch(path, value, query)
  if (kind === "object") {
    for (const [k, child] of Object.entries(value as Record<string, unknown>)) {
      const childPath = joinJsonPath(path, k)
      if (collectMatchingAncestorPaths(child, childPath, query, out)) {
        selfOrChild = true
      }
    }
  } else if (kind === "array") {
    ;(value as unknown[]).forEach((child, i) => {
      const childPath = joinJsonPath(path, i)
      if (collectMatchingAncestorPaths(child, childPath, query, out)) {
        selfOrChild = true
      }
    })
  }
  if (selfOrChild && path) out.add(path)
  return selfOrChild
}

function formatPreview(value: unknown): string {
  if (value === null) return "null"
  if (typeof value === "string") {
    return value.length > 48 ? `${JSON.stringify(value.slice(0, 48))}…` : JSON.stringify(value)
  }
  if (typeof value === "number" || typeof value === "boolean") return String(value)
  if (Array.isArray(value)) return `Array(${value.length})`
  if (typeof value === "object") {
    const n = Object.keys(value as object).length
    return `{${n}}`
  }
  return String(value)
}

function PrimitiveValue({ value }: { value: unknown }) {
  if (value === null) {
    return <span className="text-amber-600 dark:text-amber-400">null</span>
  }
  if (typeof value === "string") {
    return (
      <span className="text-emerald-600 break-all dark:text-emerald-400">
        {JSON.stringify(value)}
      </span>
    )
  }
  if (typeof value === "number") {
    return <span className="text-sky-600 dark:text-sky-400">{String(value)}</span>
  }
  if (typeof value === "boolean") {
    return (
      <span className="text-amber-600 dark:text-amber-400">{String(value)}</span>
    )
  }
  return <span className="text-muted-foreground">{String(value)}</span>
}

type TreeNodeProps = {
  name: string | number
  path: string
  value: unknown
  depth: number
  expanded: Set<string>
  onToggle: (path: string) => void
  search: string
  onApplyFilter?: (cel: string) => void
}

function TreeNode({
  name,
  path,
  value,
  depth,
  expanded,
  onToggle,
  search,
  onApplyFilter,
}: TreeNodeProps) {
  const kind = kindOf(value)
  const isExpanded = expanded.has(path)
  const label = typeof name === "number" ? String(name) : name
  const [copied, setCopied] = useState<"path" | "value" | null>(null)

  if (search && !subtreeMatches(value, path, search)) {
    return null
  }

  async function handleCopy(what: "path" | "value") {
    const text =
      what === "path"
        ? path
        : value === null
          ? "null"
          : typeof value === "string"
            ? value
            : JSON.stringify(value)
    const ok = await copyText(text)
    if (ok) {
      setCopied(what)
      window.setTimeout(() => setCopied(null), 1200)
    }
  }

  function handleFilter() {
    if (!onApplyFilter) return
    const prim = primitiveToFilterValue(value)
    if (!prim) return
    onApplyFilter(buildCelEqualityFilter(path, prim.text, prim.types))
  }

  const children: ReactNode =
    kind !== "primitive" && isExpanded
      ? kind === "object"
        ? Object.entries(value as Record<string, unknown>).map(([k, child]) => (
            <TreeNode
              key={k}
              name={k}
              path={joinJsonPath(path, k)}
              value={child}
              depth={depth + 1}
              expanded={expanded}
              onToggle={onToggle}
              search={search}
              onApplyFilter={onApplyFilter}
            />
          ))
        : (value as unknown[]).map((child, i) => (
            <TreeNode
              key={i}
              name={i}
              path={joinJsonPath(path, i)}
              value={child}
              depth={depth + 1}
              expanded={expanded}
              onToggle={onToggle}
              search={search}
              onApplyFilter={onApplyFilter}
            />
          ))
      : null

  return (
    <div>
      <div
        className={cn(
          "group flex items-start gap-1 rounded-md py-0.5 pr-1 transition-colors",
          "hover:bg-muted/40",
          search && pathMatchesSearch(path, value, search) && "bg-muted/30"
        )}
        style={{ paddingLeft: `${depth * 14 + 4}px` }}
      >
        {kind !== "primitive" ? (
          <button
            type="button"
            className="mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            onClick={() => onToggle(path)}
            aria-expanded={isExpanded}
            aria-label={isExpanded ? `Collapse ${label}` : `Expand ${label}`}
          >
            {isExpanded ? (
              <ChevronDown className="size-3.5" />
            ) : (
              <ChevronRight className="size-3.5" />
            )}
          </button>
        ) : (
          <span className="mt-0.5 size-4 shrink-0" />
        )}

        <div className="min-w-0 flex-1 font-mono text-[13px] leading-relaxed">
          <span className="text-foreground">{label}</span>
          <span className="text-muted-foreground">: </span>
          {kind === "primitive" ? (
            onApplyFilter ? (
              <button
                type="button"
                className="rounded-sm text-left hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                onClick={handleFilter}
                title={`Filter by ${path}`}
              >
                <PrimitiveValue value={value} />
              </button>
            ) : (
              <PrimitiveValue value={value} />
            )
          ) : (
            <button
              type="button"
              className="rounded-sm text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
              onClick={() => onToggle(path)}
            >
              {isExpanded
                ? kind === "array"
                  ? "["
                  : "{"
                : formatPreview(value)}
            </button>
          )}
        </div>

        <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                onClick={() => void handleCopy("path")}
                aria-label="Copy path"
              >
                <Copy className="size-3" />
              </Button>
            </TooltipTrigger>
            <TooltipContent side="top">
              {copied === "path" ? "Copied path" : "Copy path"}
            </TooltipContent>
          </Tooltip>
          {kind === "primitive" ? (
            <>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    onClick={() => void handleCopy("value")}
                    aria-label="Copy value"
                  >
                    <ClipboardCopy className="size-3" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="top">
                  {copied === "value" ? "Copied value" : "Copy value"}
                </TooltipContent>
              </Tooltip>
              {onApplyFilter ? (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      onClick={handleFilter}
                      aria-label="Filter by value"
                    >
                      <Filter className="size-3" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent side="top">Filter by this</TooltipContent>
                </Tooltip>
              ) : null}
            </>
          ) : null}
        </div>
      </div>
      {children}
      {kind !== "primitive" && isExpanded ? (
        <div
          className="font-mono text-[13px] leading-relaxed text-muted-foreground"
          style={{ paddingLeft: `${depth * 14 + 4 + 16}px` }}
        >
          {kind === "array" ? "]" : "}"}
        </div>
      ) : null}
    </div>
  )
}

export function JsonExplorer({
  value,
  mode,
  onModeChange,
  onApplyFilter,
  searchRef,
  toolbarStart,
  className,
}: JsonExplorerProps) {
  const rootEntries = useMemo(() => Object.entries(value), [value])
  const allExpandable = useMemo(() => {
    const set = new Set<string>()
    collectExpandablePaths(value, "", set)
    return set
  }, [value])

  const [expanded, setExpanded] = useState(() => initialExpandedPaths(value))
  const [search, setSearch] = useState("")

  const searchAncestors = useMemo(() => {
    const q = search.trim()
    if (!q) return null
    const ancestors = new Set<string>()
    for (const [k, child] of Object.entries(value)) {
      collectMatchingAncestorPaths(child, joinJsonPath("", k), q, ancestors)
    }
    return ancestors
  }, [search, value])

  const effectiveExpanded = useMemo(() => {
    if (!searchAncestors) return expanded
    const next = new Set(expanded)
    for (const p of searchAncestors) next.add(p)
    return next
  }, [expanded, searchAncestors])

  const onToggle = useCallback((path: string) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })
  }, [])

  function expandAll() {
    setExpanded(new Set(allExpandable))
  }

  function collapseAll() {
    setExpanded(new Set())
  }

  const visibleCount = useMemo(() => {
    const q = search.trim()
    if (!q) return rootEntries.length
    return rootEntries.filter(([k, child]) =>
      subtreeMatches(child, joinJsonPath("", k), q)
    ).length
  }, [rootEntries, search])

  return (
    <div className={cn("flex min-h-0 flex-1 flex-col", className)}>
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b px-4 py-2">
        {toolbarStart}
        <div className="inline-flex rounded-lg border border-border p-0.5">
          <Button
            type="button"
            variant={mode === "tree" ? "secondary" : "ghost"}
            size="xs"
            onClick={() => onModeChange("tree")}
            aria-pressed={mode === "tree"}
          >
            Tree
          </Button>
          <Button
            type="button"
            variant={mode === "raw" ? "secondary" : "ghost"}
            size="xs"
            onClick={() => onModeChange("raw")}
            aria-pressed={mode === "raw"}
          >
            Raw
          </Button>
        </div>

        {mode === "tree" ? (
          <>
            <div className="relative min-w-[10rem] flex-1">
              <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                ref={searchRef}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="Search fields…"
                className="h-7 pr-7 pl-7 font-mono text-xs md:text-xs"
                aria-label="Search JSON fields"
                onKeyDown={(e: KeyboardEvent<HTMLInputElement>) => {
                  if (e.key === "Escape") {
                    e.stopPropagation()
                    setSearch("")
                  }
                }}
              />
              {search ? (
                <button
                  type="button"
                  className="absolute top-1/2 right-1.5 -translate-y-1/2 rounded-sm p-0.5 text-muted-foreground opacity-70 hover:opacity-100 focus-visible:ring-2 focus-visible:ring-ring"
                  onClick={() => setSearch("")}
                  aria-label="Clear field search"
                >
                  <X className="size-3.5" />
                </button>
              ) : null}
            </div>
            <Button type="button" variant="ghost" size="xs" onClick={expandAll}>
              Expand
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={collapseAll}
            >
              Collapse
            </Button>
          </>
        ) : (
          <span className="flex-1 text-xs text-muted-foreground">
            Pretty-printed JSON
          </span>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {mode === "raw" ? (
          <JsonHighlight value={value} className="px-4 py-3" />
        ) : visibleCount === 0 ? (
          <p className="px-4 py-8 text-center text-xs text-muted-foreground">
            No fields match “{search.trim()}”
          </p>
        ) : (
          <div className="px-2 py-2">
            {rootEntries.map(([k, child]) => (
              <TreeNode
                key={k}
                name={k}
                path={joinJsonPath("", k)}
                value={child}
                depth={0}
                expanded={effectiveExpanded}
                onToggle={onToggle}
                search={search.trim()}
                onApplyFilter={onApplyFilter}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
