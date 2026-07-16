import { CircleAlert, Search, X } from "lucide-react"
import { useEffect, useState } from "react"

import { QueryEditorDialog } from "@/components/query-editor-dialog"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import type { PropertyInfo } from "@/lib/types"
import { cn } from "@/lib/utils"

type CelQueryEditorProps = {
  value: string
  onChange: (value: string) => void
  properties: PropertyInfo[]
  error?: string | null
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
}: CelQueryEditorProps) {
  const [open, setOpen] = useState(false)
  const modKey = isMacPlatform() ? "⌘" : "Ctrl"

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

  const display = value.trim()
  const hasError = Boolean(error)

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
          aria-label={display ? `Edit filter: ${display}` : "Open filter editor"}
        >
          <Search className="size-3.5 shrink-0 text-muted-foreground" />
          <span
            className={cn(
              "min-w-0 flex-1 truncate font-mono text-xs",
              display ? "text-foreground" : "text-muted-foreground"
            )}
          >
            {display || "Filter with CEL…"}
          </span>
          <kbd className="pointer-events-none hidden shrink-0 rounded border border-border bg-muted px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground sm:inline">
            {modKey}K
          </kbd>
        </button>

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

        {display ? (
          <button
            type="button"
            className="rounded-sm p-0.5 text-muted-foreground opacity-70 hover:opacity-100 focus-visible:ring-2 focus-visible:ring-ring"
            onClick={(e) => {
              e.stopPropagation()
              onChange("")
            }}
            aria-label="Clear query"
          >
            <X className="size-3.5" />
          </button>
        ) : null}
      </div>

      <QueryEditorDialog
        open={open}
        onOpenChange={setOpen}
        value={value}
        onApply={onChange}
        properties={properties}
        appliedError={error}
      />
    </div>
  )
}
