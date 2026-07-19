import { Check, ChevronDown, CircleAlert, Search, X } from "lucide-react"
import { useEffect, useState } from "react"

import { QueryEditorDialog } from "@/components/query-editor-dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import {
  readQueryModeFromSession,
  readSqlFromSession,
  writeQueryModeToSession,
  writeSqlToSession,
  type QueryMode,
} from "@/lib/filter-storage"
import type { PropertyInfo } from "@/lib/types"
import { cn } from "@/lib/utils"

type CelQueryEditorProps = {
  value: string
  onChange: (value: string) => void
  properties: PropertyInfo[]
  error?: string | null
  /** Entries currently shown in the list. */
  showingCount?: number
  /** Total entries stored in the hub buffer. */
  storedCount?: number | null
  /** True when a CEL or time filter is active. */
  filterActive?: boolean
  /** Clear all active filters (CEL + time). */
  onClearFilter?: () => void
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  if (target.isContentEditable) return true
  const tag = target.tagName
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT"
}

function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false
  return /Mac|iPhone|iPad|iPod/.test(navigator.platform)
}

export function CelQueryEditor({
  value,
  onChange,
  properties,
  error,
  showingCount,
  storedCount = null,
  filterActive = false,
  onClearFilter,
}: CelQueryEditorProps) {
  const [open, setOpen] = useState(false)
  const [mode, setMode] = useState<QueryMode>(() => readQueryModeFromSession())
  const [sql, setSql] = useState(() => readSqlFromSession())
  const modKey = isMacPlatform() ? "⌘" : "Ctrl"

  useEffect(() => {
    writeQueryModeToSession(mode)
  }, [mode])

  useEffect(() => {
    writeSqlToSession(sql)
  }, [sql])

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "k") return
      // Ignore when typing in other inputs (e.g. log detail field search).
      if (isEditableTarget(e.target)) return
      e.preventDefault()
      setOpen(true)
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [])

  const display = mode === "sql" ? sql.trim() : value.trim()
  const hasError = mode === "cel" && Boolean(error)
  const showFilterChip = filterActive && showingCount != null
  const placeholder =
    mode === "sql" ? "Query with SQL…" : "Filter with CEL…"

  function clearFilter() {
    if (onClearFilter) {
      onClearFilter()
      return
    }
    onChange("")
  }

  return (
    <div className="min-w-0 flex-1">
      <div
        className={cn(
          "flex min-h-8 min-w-0 items-center gap-2 rounded-lg border border-input bg-transparent px-2 py-1",
          "transition-colors hover:bg-muted/40",
          hasError && "border-destructive"
        )}
      >
        <button
          type="button"
          data-slot="query-bar-trigger"
          className={cn(
            "flex min-w-0 flex-1 items-center gap-2 rounded-sm py-0.5 text-left",
            "outline-none focus-visible:ring-2 focus-visible:ring-ring"
          )}
          onClick={() => setOpen(true)}
          aria-label={
            display
              ? `Edit ${mode === "sql" ? "SQL" : "filter"}: ${display}`
              : mode === "sql"
                ? "Open SQL editor"
                : "Open filter editor"
          }
        >
          <Search className="size-3.5 shrink-0 text-muted-foreground" />
          <span
            className={cn(
              "min-w-0 flex-1 truncate font-mono text-xs",
              display ? "text-foreground" : "text-muted-foreground"
            )}
          >
            {display || placeholder}
          </span>
        </button>

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              className={cn(
                "flex shrink-0 items-center gap-0.5 rounded-full border border-border",
                "bg-muted/70 px-2 py-0.5",
                "text-[10px] font-medium text-muted-foreground",
                "hover:bg-muted hover:text-foreground",
                "focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none",
                "data-[state=open]:bg-muted data-[state=open]:text-foreground"
              )}
              aria-label={`Query language: ${mode === "sql" ? "SQL" : "CEL"}`}
              onClick={(e) => e.stopPropagation()}
            >
              {mode === "sql" ? "SQL" : "CEL"}
              <ChevronDown className="size-3 opacity-70" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-28">
            <DropdownMenuItem
              onSelect={() => setMode("cel")}
              className="justify-between"
            >
              CEL
              {mode === "cel" ? <Check className="size-3.5" /> : null}
            </DropdownMenuItem>
            <DropdownMenuItem
              onSelect={() => setMode("sql")}
              className="justify-between"
            >
              SQL
              {mode === "sql" ? <Check className="size-3.5" /> : null}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>

        <kbd className="pointer-events-none hidden shrink-0 rounded border border-border bg-muted px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground sm:inline">
          {modKey}K
        </kbd>

        {hasError ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                className="shrink-0 rounded-sm p-0.5 text-destructive focus-visible:ring-2 focus-visible:ring-ring"
                aria-label={error ?? "Query error"}
                onClick={() => setOpen(true)}
              >
                <CircleAlert className="size-3.5" />
              </button>
            </TooltipTrigger>
            <TooltipContent side="bottom" className="max-w-sm font-mono">
              {error}
            </TooltipContent>
          </Tooltip>
        ) : null}

        {showFilterChip ? (
          <button
            type="button"
            className={cn(
              "flex shrink-0 items-center gap-1.5 rounded-md border border-border",
              "bg-muted/70 px-1.5 py-0.5",
              "text-[10px] text-muted-foreground",
              "hover:bg-muted hover:text-foreground",
              "focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            )}
            onClick={clearFilter}
            aria-label="Clear filter"
            title="Clear filter"
          >
            <span>
              Showing{" "}
              <span className="tabular-nums text-foreground">{showingCount}</span>
              {storedCount != null ? (
                <>
                  {" "}
                  /{" "}
                  <span className="tabular-nums text-foreground">{storedCount}</span>{" "}
                  stored
                </>
              ) : null}
            </span>
            <X className="size-3 opacity-70" />
          </button>
        ) : null}
      </div>

      <QueryEditorDialog
        open={open}
        onOpenChange={setOpen}
        mode={mode}
        value={value}
        onApply={onChange}
        sqlValue={sql}
        onSqlChange={setSql}
        properties={properties}
        appliedError={error}
      />
    </div>
  )
}
