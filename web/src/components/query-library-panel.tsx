import { Check, Play, Star, Trash2, X } from "lucide-react"

import { Button } from "@/components/ui/button"
import { ScrollArea } from "@/components/ui/scroll-area"
import type { QueryHistoryEntry, SavedQuery } from "@/lib/query-library-storage"
import { cn } from "@/lib/utils"

type QueryLibraryPanelProps = {
  saved: SavedQuery[]
  history: QueryHistoryEntry[]
  draft: string
  onLoad: (expression: string) => void
  onApply: (expression: string) => void
  onSaveExpression: (expression: string) => void
  onUnsave: (id: string) => void
  onRemoveHistory: (expression: string) => void
  isSaved: (expression: string) => boolean
  className?: string
}

function RowActions({
  expression,
  savedId,
  onApply,
  onSaveExpression,
  onUnsave,
  onRemove,
}: {
  expression: string
  savedId?: string
  onApply: (expression: string) => void
  onSaveExpression: (expression: string) => void
  onUnsave: (id: string) => void
  onRemove: () => void
}) {
  return (
    <div className="flex shrink-0 items-center gap-0.5 opacity-100 transition-opacity sm:opacity-0 sm:group-hover:opacity-100 sm:group-focus-within:opacity-100">
      <button
        type="button"
        className="rounded-sm p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
        aria-label="Apply query"
        title="Apply"
        onClick={(e) => {
          e.stopPropagation()
          onApply(expression)
        }}
      >
        <Play className="size-3" />
      </button>
      <button
        type="button"
        className={cn(
          "rounded-sm p-1 hover:bg-muted",
          savedId
            ? "text-primary hover:text-primary"
            : "text-muted-foreground hover:text-foreground"
        )}
        aria-label={savedId ? "Unsave query" : "Save query"}
        title={savedId ? "Unsave" : "Save"}
        onClick={(e) => {
          e.stopPropagation()
          if (savedId) onUnsave(savedId)
          else onSaveExpression(expression)
        }}
      >
        <Star className={cn("size-3", savedId && "fill-current")} />
      </button>
      <button
        type="button"
        className="rounded-sm p-1 text-muted-foreground hover:bg-muted hover:text-destructive"
        aria-label="Remove"
        title="Remove"
        onClick={(e) => {
          e.stopPropagation()
          onRemove()
        }}
      >
        <Trash2 className="size-3" />
      </button>
    </div>
  )
}

export function QueryLibraryPanel({
  saved,
  history,
  draft,
  onLoad,
  onApply,
  onSaveExpression,
  onUnsave,
  onRemoveHistory,
  isSaved,
  className,
}: QueryLibraryPanelProps) {
  const draftTrimmed = draft.trim()

  return (
    <aside
      className={cn(
        "flex h-full min-h-0 flex-col border-border bg-muted/20",
        className
      )}
    >
      <ScrollArea className="h-full min-h-0 flex-1">
        <div className="flex flex-col gap-5 p-3">
          <section className="space-y-2">
            <p className="text-xs font-medium text-muted-foreground">Saved</p>
            {saved.length === 0 ? (
              <p className="px-1 text-[11px] leading-relaxed text-muted-foreground/80">
                Star a query to keep it after restarts.
              </p>
            ) : (
              <ul className="space-y-0.5">
                {saved.map((item) => {
                  const active = draftTrimmed === item.expression
                  return (
                    <li key={item.id}>
                      <div
                        role="button"
                        tabIndex={0}
                        className={cn(
                          "group flex w-full items-start gap-1 rounded-md px-1.5 py-1.5 text-left transition-colors",
                          "hover:bg-muted/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                          active && "bg-primary/10"
                        )}
                        onClick={() => onLoad(item.expression)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault()
                            onLoad(item.expression)
                          }
                        }}
                      >
                        <div className="min-w-0 flex-1">
                          <p className="truncate text-xs font-medium text-foreground">
                            {item.name}
                          </p>
                          {item.name !== item.expression ? (
                            <p className="truncate font-mono text-[10px] text-muted-foreground">
                              {item.expression}
                            </p>
                          ) : null}
                        </div>
                        <RowActions
                          expression={item.expression}
                          savedId={item.id}
                          onApply={onApply}
                          onSaveExpression={onSaveExpression}
                          onUnsave={onUnsave}
                          onRemove={() => onUnsave(item.id)}
                        />
                      </div>
                    </li>
                  )
                })}
              </ul>
            )}
          </section>

          <section className="space-y-2">
            <p className="text-xs font-medium text-muted-foreground">Recent</p>
            {history.length === 0 ? (
              <p className="px-1 text-[11px] leading-relaxed text-muted-foreground/80">
                Applied filters show up here.
              </p>
            ) : (
              <ul className="space-y-0.5">
                {history.map((item) => {
                  const active = draftTrimmed === item.expression
                  const savedMatch = isSaved(item.expression)
                  return (
                    <li key={item.expression}>
                      <div
                        role="button"
                        tabIndex={0}
                        className={cn(
                          "group flex w-full items-start gap-1 rounded-md px-1.5 py-1.5 text-left transition-colors",
                          "hover:bg-muted/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                          active && "bg-primary/10"
                        )}
                        onClick={() => onLoad(item.expression)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault()
                            onLoad(item.expression)
                          }
                        }}
                      >
                        <p className="min-w-0 flex-1 truncate font-mono text-[11px] text-foreground/90">
                          {item.expression}
                        </p>
                        <RowActions
                          expression={item.expression}
                          savedId={
                            savedMatch
                              ? saved.find((s) => s.expression === item.expression)
                                  ?.id
                              : undefined
                          }
                          onApply={onApply}
                          onSaveExpression={onSaveExpression}
                          onUnsave={onUnsave}
                          onRemove={() => onRemoveHistory(item.expression)}
                        />
                      </div>
                    </li>
                  )
                })}
              </ul>
            )}
          </section>
        </div>
      </ScrollArea>
    </aside>
  )
}

type SaveQueryFormProps = {
  defaultName: string
  onConfirm: (name: string) => void
  onCancel: () => void
}

export function SaveQueryForm({
  defaultName,
  onConfirm,
  onCancel,
}: SaveQueryFormProps) {
  return (
    <form
      className="flex min-w-0 flex-1 flex-col gap-2 sm:flex-row sm:items-center"
      onSubmit={(e) => {
        e.preventDefault()
        const data = new FormData(e.currentTarget)
        const name = String(data.get("name") ?? "").trim()
        onConfirm(name || defaultName)
      }}
    >
      <input
        name="name"
        defaultValue={defaultName}
        autoFocus
        aria-label="Query name"
        placeholder="Name this query"
        className={cn(
          "h-8 min-w-0 flex-1 rounded-lg border border-input bg-transparent px-2.5 text-sm outline-none",
          "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
        )}
      />
      <div className="flex gap-1.5">
        <Button type="submit" size="sm" variant="secondary">
          <Check className="size-3.5" />
          Save
        </Button>
        <Button type="button" size="sm" variant="ghost" onClick={onCancel}>
          <X className="size-3.5" />
          Cancel
        </Button>
      </div>
    </form>
  )
}
