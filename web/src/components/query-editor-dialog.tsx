import CodeMirror, { type ReactCodeMirrorRef } from "@uiw/react-codemirror"
import { CircleAlert, Loader2, Star } from "lucide-react"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import {
  QueryLibraryPanel,
  SaveQueryForm,
} from "@/components/query-library-panel"
import { SqlResultTable } from "@/components/sql-result-table"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { useQueryLibrary } from "@/hooks/use-query-library"
import { runSql, type SqlResult } from "@/lib/api"
import { celSyntaxHint, createCelExtensions } from "@/lib/cel-lang"
import {
  CEL_CHEAT_SHEET,
  CEL_RECIPES,
  fieldChips,
} from "@/lib/cel-recipes"
import type { QueryMode } from "@/lib/filter-storage"
import { defaultQueryName } from "@/lib/query-library-storage"
import type { PropertyInfo } from "@/lib/types"
import { cn } from "@/lib/utils"

type QueryEditorDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  mode: QueryMode
  value: string
  onApply: (value: string) => void
  sqlValue: string
  onSqlChange: (value: string) => void
  properties: PropertyInfo[]
  /** Server error from the last applied query (shown when draft matches applied). */
  appliedError?: string | null
}

function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false
  return /Mac|iPhone|iPad|iPod/.test(navigator.platform)
}

export function QueryEditorDialog({
  open,
  onOpenChange,
  mode,
  value,
  onApply,
  sqlValue,
  onSqlChange,
  properties,
  appliedError,
}: QueryEditorDialogProps) {
  const [draft, setDraft] = useState(value)
  const [sqlDraft, setSqlDraft] = useState(sqlValue)
  const [wasOpen, setWasOpen] = useState(open)
  const [naming, setNaming] = useState(false)
  const [sqlResult, setSqlResult] = useState<SqlResult | null>(null)
  const [sqlLoading, setSqlLoading] = useState(false)
  const [sqlError, setSqlError] = useState<string | null>(null)
  const editorRef = useRef<ReactCodeMirrorRef>(null)
  const sqlTextareaRef = useRef<HTMLTextAreaElement>(null)
  const modKey = isMacPlatform() ? "⌘" : "Ctrl"
  const library = useQueryLibrary()
  const isSql = mode === "sql"

  if (open !== wasOpen) {
    setWasOpen(open)
    if (open) {
      setDraft(value)
      setSqlDraft(sqlValue)
      setNaming(false)
      setSqlError(null)
    }
  }

  useEffect(() => {
    if (!open) return
    const id = window.setTimeout(() => {
      if (isSql) {
        sqlTextareaRef.current?.focus()
      } else {
        editorRef.current?.view?.focus()
      }
    }, 50)
    return () => window.clearTimeout(id)
  }, [open, isSql])

  const paths = useMemo(
    () => properties.map((p) => p.path),
    [properties]
  )

  const extensions = useMemo(
    () =>
      createCelExtensions({
        paths,
        multiline: true,
        placeholder: 'level == "error" && msg.contains("timeout")',
      }),
    [paths]
  )

  const syntaxHint = useMemo(() => celSyntaxHint(draft), [draft])
  const showAppliedError =
    Boolean(appliedError) && draft.trim() === value.trim()
  const displayError = syntaxHint ?? (showAppliedError ? appliedError : null)

  const chips = useMemo(() => fieldChips(paths), [paths])
  const draftTrimmed = draft.trim()
  const existingSaved = draftTrimmed
    ? library.findSavedByExpression(draftTrimmed)
    : undefined
  const canSave = Boolean(draftTrimmed) && !syntaxHint

  function apply(expression?: string) {
    const next = (expression ?? draft).trim()
    if (expression === undefined && syntaxHint) return
    if (expression !== undefined) setDraft(expression)
    onApply(next)
    onOpenChange(false)
  }

  const runSqlQuery = useCallback(async () => {
    const next = sqlDraft.trim()
    if (!next || sqlLoading) return
    onSqlChange(sqlDraft)
    setSqlLoading(true)
    setSqlError(null)
    try {
      setSqlResult(await runSql(next, 100))
    } catch (e) {
      setSqlResult(null)
      setSqlError(e instanceof Error ? e.message : "SQL failed")
    } finally {
      setSqlLoading(false)
    }
  }, [onSqlChange, sqlDraft, sqlLoading])

  function insertAtCursor(text: string) {
    const view = editorRef.current?.view
    if (!view) {
      setDraft((prev) => (prev.trim() ? `${prev}${text}` : text))
      return
    }
    const { from, to } = view.state.selection.main
    view.dispatch({
      changes: { from, to, insert: text },
      selection: { anchor: from + text.length },
    })
    view.focus()
  }

  function setRecipe(expression: string) {
    setDraft(expression)
    window.requestAnimationFrame(() => {
      editorRef.current?.view?.focus()
    })
  }

  function loadExpression(expression: string) {
    setDraft(expression)
    setNaming(false)
    window.requestAnimationFrame(() => {
      editorRef.current?.view?.focus()
    })
  }

  function handleSaveClick() {
    if (!canSave) return
    if (existingSaved) {
      library.removeSaved(existingSaved.id)
      setNaming(false)
      return
    }
    setNaming(true)
  }

  function confirmSave(name: string) {
    if (!draftTrimmed) return
    library.saveQuery({ expression: draftTrimmed, name })
    setNaming(false)
  }

  function saveExpressionQuick(expression: string) {
    library.saveQuery({ expression })
  }

  function renderLibraryPanel() {
    return (
      <QueryLibraryPanel
        saved={library.saved}
        history={library.history}
        draft={draft}
        onLoad={loadExpression}
        onApply={(expression) => apply(expression)}
        onSaveExpression={saveExpressionQuick}
        onUnsave={library.removeSaved}
        onRemoveHistory={library.removeHistory}
        isSaved={(expression) =>
          library.saved.some((s) => s.expression === expression)
        }
      />
    )
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        showCloseButton
        className="flex max-h-[88vh] w-[min(96vw,72rem)] max-w-none flex-col gap-0 overflow-hidden p-0 sm:max-w-none"
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault()
            if (isSql) {
              void runSqlQuery()
            } else {
              apply()
            }
          }
        }}
      >
        <DialogHeader className="shrink-0 space-y-1 border-b px-5 py-4 pr-12">
          <DialogTitle>{isSql ? "Query with SQL" : "Filter logs"}</DialogTitle>
          <DialogDescription>
            {isSql
              ? "Read-only SELECT over all_logs · results stay in this dialog"
              : "CEL expression · empty shows everything"}
          </DialogDescription>
        </DialogHeader>

        {isSql ? (
          <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-5 py-4">
            <div className="space-y-2">
              <textarea
                ref={sqlTextareaRef}
                className={cn(
                  "min-h-[10.5rem] w-full resize-y rounded-lg border border-input bg-background/50 px-3 py-2",
                  "font-mono text-xs outline-none",
                  "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50",
                  sqlError &&
                    "border-destructive focus-visible:border-destructive focus-visible:ring-destructive/30"
                )}
                value={sqlDraft}
                onChange={(e) => {
                  const next = e.target.value
                  setSqlDraft(next)
                  onSqlChange(next)
                  setSqlError(null)
                }}
                spellCheck={false}
                placeholder="SELECT service, count(*) AS n FROM all_logs GROUP BY 1"
              />
              {sqlError ? (
                <p className="flex items-start gap-1.5 text-xs text-destructive">
                  <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
                  <span className="font-mono">{sqlError}</span>
                </p>
              ) : (
                <p className="text-xs text-muted-foreground">
                  Columns: id, received_at, event_time, service, format_id,
                  level, msg, data
                </p>
              )}
            </div>
            {sqlResult ? <SqlResultTable result={sqlResult} /> : null}
          </div>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden md:flex-row">
            <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-5 overflow-y-auto px-5 py-4">
              <div className="space-y-2">
                <div
                  className={cn(
                    "min-h-[10.5rem] overflow-hidden rounded-lg border border-input bg-background/50",
                    "focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50",
                    displayError &&
                      "border-destructive focus-within:border-destructive focus-within:ring-destructive/30"
                  )}
                >
                  <CodeMirror
                    ref={editorRef}
                    value={draft}
                    height="168px"
                    basicSetup={false}
                    theme="none"
                    extensions={extensions}
                    onChange={setDraft}
                    className="cm-cel-query-modal [&_.cm-editor]:bg-transparent [&_.cm-content]:bg-transparent"
                  />
                </div>
                {displayError ? (
                  <p className="flex items-start gap-1.5 text-xs text-destructive">
                    <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
                    <span className="font-mono">{displayError}</span>
                  </p>
                ) : (
                  <p className="text-xs text-muted-foreground">
                    Autocomplete suggests fields and helpers as you type.
                  </p>
                )}
              </div>

              <div className="space-y-2">
                <p className="text-xs font-medium text-muted-foreground">
                  Try a recipe
                </p>
                <div className="flex flex-wrap gap-1.5">
                  {CEL_RECIPES.map((recipe) => (
                    <button
                      key={recipe.expression}
                      type="button"
                      onClick={() => setRecipe(recipe.expression)}
                      className={cn(
                        "rounded-md border border-border bg-muted/40 px-2 py-1",
                        "font-mono text-[11px] text-foreground/90 transition-colors",
                        "hover:border-primary/50 hover:bg-primary/10",
                        "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50",
                        draft.trim() === recipe.expression &&
                          "border-primary/60 bg-primary/15"
                      )}
                      title={recipe.expression}
                    >
                      {recipe.label}
                    </button>
                  ))}
                </div>
              </div>

              <div className="space-y-2">
                <p className="text-xs font-medium text-muted-foreground">
                  Insert field
                </p>
                <div className="flex flex-wrap gap-1.5">
                  {chips.map((path) => (
                    <button
                      key={path}
                      type="button"
                      onClick={() => insertAtCursor(path)}
                      className={cn(
                        "rounded-md border border-border px-2 py-1",
                        "font-mono text-[11px] text-muted-foreground transition-colors",
                        "hover:border-border hover:bg-muted hover:text-foreground",
                        "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
                      )}
                    >
                      {path}
                    </button>
                  ))}
                </div>
              </div>

              <div className="grid gap-4 border-t pt-4 sm:grid-cols-3">
                {CEL_CHEAT_SHEET.map((group) => (
                  <div key={group.title} className="space-y-2">
                    <p className="text-xs font-medium text-muted-foreground">
                      {group.title}
                    </p>
                    <ul className="space-y-1.5">
                      {group.items.map((item) => (
                        <li
                          key={item.code}
                          className="flex flex-wrap items-baseline gap-x-1.5 gap-y-0.5 text-xs"
                        >
                          <code className="font-mono text-[11px] text-foreground">
                            {item.code}
                          </code>
                          <span className="text-muted-foreground">
                            {item.hint}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </div>
                ))}
              </div>

              <div className="border-t pt-4 md:hidden">
                <div className="max-h-64 overflow-hidden rounded-lg border border-border">
                  {renderLibraryPanel()}
                </div>
              </div>
            </div>

            <div className="hidden min-h-0 w-[16.5rem] shrink-0 border-l md:flex md:flex-col">
              {renderLibraryPanel()}
            </div>
          </div>
        )}

        <DialogFooter
          className={cn(
            "mx-0 mb-0 shrink-0 sm:items-center",
            naming ? "sm:justify-stretch" : "sm:justify-between"
          )}
        >
          {isSql ? (
            <>
              <p className="hidden text-xs text-muted-foreground sm:block">
                {modKey}↵ run · Esc close
              </p>
              <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => {
                    setSqlDraft("")
                    onSqlChange("")
                    setSqlResult(null)
                    setSqlError(null)
                  }}
                >
                  Clear
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => onOpenChange(false)}
                >
                  Close
                </Button>
                <Button
                  type="button"
                  size="sm"
                  disabled={sqlLoading || !sqlDraft.trim()}
                  onClick={() => void runSqlQuery()}
                >
                  {sqlLoading ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : null}
                  Run SELECT
                </Button>
              </div>
            </>
          ) : naming ? (
            <SaveQueryForm
              defaultName={defaultQueryName(draftTrimmed)}
              onConfirm={confirmSave}
              onCancel={() => setNaming(false)}
            />
          ) : (
            <>
              <p className="hidden text-xs text-muted-foreground sm:block">
                {modKey}↵ apply · Esc close
              </p>
              <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => setDraft("")}
                >
                  Clear
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!canSave}
                  onClick={handleSaveClick}
                >
                  <Star
                    className={cn("size-3.5", existingSaved && "fill-current")}
                  />
                  {existingSaved ? "Saved" : "Save"}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => onOpenChange(false)}
                >
                  Cancel
                </Button>
                <Button
                  type="button"
                  size="sm"
                  disabled={Boolean(syntaxHint)}
                  onClick={() => apply()}
                >
                  Apply filter
                </Button>
              </div>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
