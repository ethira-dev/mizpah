import { Bookmark, ChartColumn, Loader2, SquareTerminal, Waves } from "lucide-react"
import { useCallback, useMemo, useState } from "react"

import { SqlResultTable } from "@/components/sql-result-table"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet"
import {
  fetchAggregate,
  fetchBookmarks,
  fetchSpectrogram,
  runSql,
  setBookmark,
  type AggregateRow,
  type AnnotatedEntry,
  type SpectrogramResult,
  type SqlResult,
} from "@/lib/api"
import { DEFAULT_SQL } from "@/lib/filter-storage"
import type { TimeRange } from "@/lib/types"
import { cn } from "@/lib/utils"

type Tab = "bookmarks" | "sql" | "aggregate" | "spectrogram"

type PowerToolsSheetProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  query: string
  timeRange: TimeRange | null
  onJumpToId: (id: number) => void
  selectedEntryId?: number | null
}

export function PowerToolsSheet({
  open,
  onOpenChange,
  query,
  timeRange,
  onJumpToId,
  selectedEntryId = null,
}: PowerToolsSheetProps) {
  const [tab, setTab] = useState<Tab>("bookmarks")
  const [error, setError] = useState<string | null>(null)

  const [bookmarks, setBookmarks] = useState<AnnotatedEntry[]>([])
  const [bookmarksLoading, setBookmarksLoading] = useState(false)

  const [sql, setSql] = useState(DEFAULT_SQL)
  const [sqlResult, setSqlResult] = useState<SqlResult | null>(null)
  const [sqlLoading, setSqlLoading] = useState(false)

  const [groupBy, setGroupBy] = useState("level")
  const [aggRows, setAggRows] = useState<AggregateRow[]>([])
  const [aggLoading, setAggLoading] = useState(false)

  const [specField, setSpecField] = useState("level")
  const [spec, setSpec] = useState<SpectrogramResult | null>(null)
  const [specLoading, setSpecLoading] = useState(false)

  const loadBookmarks = useCallback(async () => {
    setBookmarksLoading(true)
    setError(null)
    try {
      setBookmarks(await fetchBookmarks())
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load bookmarks")
    } finally {
      setBookmarksLoading(false)
    }
  }, [])

  const selectTab = useCallback(
    (id: Tab) => {
      setTab(id)
      setError(null)
      if (id === "bookmarks") void loadBookmarks()
    },
    [loadBookmarks]
  )

  const handleOpenChange = useCallback(
    (next: boolean) => {
      onOpenChange(next)
      if (next && tab === "bookmarks") void loadBookmarks()
    },
    [loadBookmarks, onOpenChange, tab]
  )

  const markSelected = useCallback(async () => {
    if (selectedEntryId == null) {
      setError("Select a log row first")
      return
    }
    setError(null)
    try {
      await setBookmark({ id: selectedEntryId, marked: true })
      await loadBookmarks()
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to bookmark")
    }
  }, [loadBookmarks, selectedEntryId])

  const runSqlQuery = useCallback(async () => {
    setSqlLoading(true)
    setError(null)
    try {
      setSqlResult(await runSql(sql, 100))
    } catch (e) {
      setSqlResult(null)
      setError(e instanceof Error ? e.message : "SQL failed")
    } finally {
      setSqlLoading(false)
    }
  }, [sql])

  const runAggregate = useCallback(async () => {
    setAggLoading(true)
    setError(null)
    try {
      const rows = await fetchAggregate({
        q: query,
        groupBy,
        limit: 20,
      })
      setAggRows(rows)
    } catch (e) {
      setAggRows([])
      setError(e instanceof Error ? e.message : "Aggregate failed")
    } finally {
      setAggLoading(false)
    }
  }, [groupBy, query])

  const runSpectrogram = useCallback(async () => {
    setSpecLoading(true)
    setError(null)
    try {
      setSpec(
        await fetchSpectrogram({
          field: specField,
          from: timeRange?.from,
          to: timeRange?.to,
          timeBuckets: 24,
          valueBuckets: 8,
        })
      )
    } catch (e) {
      setSpec(null)
      setError(e instanceof Error ? e.message : "Spectrogram failed")
    } finally {
      setSpecLoading(false)
    }
  }, [specField, timeRange])

  const maxAgg = useMemo(
    () => aggRows.reduce((m, r) => Math.max(m, r.count), 0) || 1,
    [aggRows]
  )

  const maxSpec = useMemo(() => {
    if (!spec) return 1
    let m = 1
    for (const row of spec.counts) {
      for (const c of row) m = Math.max(m, c)
    }
    return m
  }, [spec])

  const tabs: { id: Tab; label: string; icon: typeof Bookmark }[] = [
    { id: "bookmarks", label: "Bookmarks", icon: Bookmark },
    { id: "sql", label: "SQL", icon: SquareTerminal },
    { id: "aggregate", label: "Aggregate", icon: ChartColumn },
    { id: "spectrogram", label: "Spectrogram", icon: Waves },
  ]

  return (
    <Sheet open={open} onOpenChange={handleOpenChange}>
      <SheetContent
        side="right"
        className="w-full gap-0 sm:max-w-xl"
        showCloseButton
      >
        <SheetHeader className="border-b">
          <SheetTitle>Power tools</SheetTitle>
          <SheetDescription>
            Bookmarks, SQL, aggregates, and spectrogram against the live hub.
          </SheetDescription>
        </SheetHeader>

        <div className="flex gap-1 border-b px-4 py-2">
          {tabs.map(({ id, label, icon: Icon }) => (
            <Button
              key={id}
              type="button"
              size="sm"
              variant={tab === id ? "secondary" : "ghost"}
              className="gap-1.5"
              onClick={() => selectTab(id)}
            >
              <Icon className="size-3.5" />
              {label}
            </Button>
          ))}
        </div>

        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-4">
          {error ? (
            <p className="text-xs text-destructive whitespace-pre-wrap">{error}</p>
          ) : null}

          {tab === "bookmarks" ? (
            <>
              <div className="flex items-center gap-2">
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => void markSelected()}
                  disabled={selectedEntryId == null}
                >
                  Bookmark selected
                  {selectedEntryId != null ? ` #${selectedEntryId}` : ""}
                </Button>
                <Button
                  type="button"
                  size="sm"
                  variant="ghost"
                  onClick={() => void loadBookmarks()}
                  disabled={bookmarksLoading}
                >
                  {bookmarksLoading ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : (
                    "Refresh"
                  )}
                </Button>
              </div>
              {bookmarks.length === 0 ? (
                <p className="text-sm text-muted-foreground">No bookmarks yet.</p>
              ) : (
                <ul className="space-y-2">
                  {bookmarks.map((b) => (
                    <li
                      key={b.id}
                      className="flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-xs"
                    >
                      <button
                        type="button"
                        className="font-mono underline-offset-2 hover:underline"
                        onClick={() => {
                          onJumpToId(b.id)
                          onOpenChange(false)
                        }}
                      >
                        #{b.id}
                      </button>
                      {b.annotation.marked ? (
                        <Badge variant="secondary" className="rounded-md">
                          marked
                        </Badge>
                      ) : null}
                      {b.annotation.tags.map((t) => (
                        <Badge key={t} variant="outline" className="rounded-md">
                          {t}
                        </Badge>
                      ))}
                      {b.annotation.comment ? (
                        <span className="text-muted-foreground">
                          {b.annotation.comment}
                        </span>
                      ) : null}
                    </li>
                  ))}
                </ul>
              )}
            </>
          ) : null}

          {tab === "sql" ? (
            <>
              <textarea
                className="min-h-28 w-full resize-y rounded-md border bg-background px-3 py-2 font-mono text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
                value={sql}
                onChange={(e) => setSql(e.target.value)}
                spellCheck={false}
              />
              <Button
                type="button"
                size="sm"
                className="self-start"
                disabled={sqlLoading || !sql.trim()}
                onClick={() => void runSqlQuery()}
              >
                {sqlLoading ? (
                  <Loader2 className="size-3.5 animate-spin" />
                ) : null}
                Run SELECT
              </Button>
              {sqlResult ? <SqlResultTable result={sqlResult} /> : null}
            </>
          ) : null}

          {tab === "aggregate" ? (
            <>
              <div className="flex flex-wrap items-end gap-2">
                <label className="flex flex-col gap-1 text-xs">
                  <span className="text-muted-foreground">groupBy</span>
                  <Input
                    value={groupBy}
                    onChange={(e) => setGroupBy(e.target.value)}
                    className="h-8 w-40 font-mono text-xs"
                  />
                </label>
                <Button
                  type="button"
                  size="sm"
                  disabled={aggLoading || !groupBy.trim()}
                  onClick={() => void runAggregate()}
                >
                  {aggLoading ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : null}
                  Run
                </Button>
              </div>
              {query.trim() ? (
                <p className="text-[11px] text-muted-foreground">
                  Filtered by current CEL query.
                </p>
              ) : null}
              <ul className="space-y-1.5">
                {aggRows.map((row) => (
                  <li key={row.keys.join("|")} className="text-xs">
                    <div className="mb-0.5 flex justify-between gap-2 font-mono">
                      <span className="truncate">{row.keys.join(" / ") || "(empty)"}</span>
                      <span className="tabular-nums text-muted-foreground">
                        {row.count}
                      </span>
                    </div>
                    <div className="h-1.5 overflow-hidden rounded-full bg-muted">
                      <div
                        className="h-full bg-primary/70"
                        style={{ width: `${(row.count / maxAgg) * 100}%` }}
                      />
                    </div>
                  </li>
                ))}
              </ul>
            </>
          ) : null}

          {tab === "spectrogram" ? (
            <>
              <div className="flex flex-wrap items-end gap-2">
                <label className="flex flex-col gap-1 text-xs">
                  <span className="text-muted-foreground">field</span>
                  <Input
                    value={specField}
                    onChange={(e) => setSpecField(e.target.value)}
                    className="h-8 w-40 font-mono text-xs"
                  />
                </label>
                <Button
                  type="button"
                  size="sm"
                  disabled={specLoading || !specField.trim()}
                  onClick={() => void runSpectrogram()}
                >
                  {specLoading ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : null}
                  Render
                </Button>
              </div>
              {spec ? (
                <div className="overflow-auto">
                  <div
                    className="inline-grid gap-px bg-border"
                    style={{
                      gridTemplateColumns: `auto repeat(${spec.timeStarts.length}, minmax(10px, 1fr))`,
                    }}
                  >
                    <div className="bg-background px-1 py-0.5 text-[10px] text-muted-foreground" />
                    {spec.timeStarts.map((t) => (
                      <div
                        key={t}
                        className="bg-background px-0.5 py-0.5 text-center text-[9px] text-muted-foreground"
                        title={t}
                      >
                        {new Date(t).toLocaleTimeString([], {
                          hour: "2-digit",
                          minute: "2-digit",
                        })}
                      </div>
                    ))}
                    {spec.valueLabels.map((label, vi) => (
                      <div key={`row-${label}`} className="contents">
                        <div className="bg-background px-1 py-0.5 font-mono text-[10px]">
                          {label}
                        </div>
                        {spec.counts.map((row, ti) => {
                          const c = row[vi] ?? 0
                          const alpha = c === 0 ? 0 : 0.15 + (c / maxSpec) * 0.85
                          return (
                            <div
                              key={`${label}-${ti}`}
                              className={cn(
                                "min-h-4 min-w-2.5 bg-background",
                                c > 0 && "bg-primary"
                              )}
                              style={c > 0 ? { opacity: alpha } : undefined}
                              title={`${label} @ ${spec.timeStarts[ti]}: ${c}`}
                            />
                          )
                        })}
                      </div>
                    ))}
                  </div>
                  <p className="mt-2 text-[11px] text-muted-foreground">
                    {spec.fieldPath} · {spec.valueLabels.length} values ·{" "}
                    {spec.timeStarts.length} buckets
                  </p>
                </div>
              ) : (
                <p className="text-sm text-muted-foreground">
                  Run to render a time × field heat-map.
                </p>
              )}
            </>
          ) : null}
        </div>
      </SheetContent>
    </Sheet>
  )
}
