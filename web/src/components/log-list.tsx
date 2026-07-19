import { useVirtualizer } from "@tanstack/react-virtual"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import { LogDetailDialog } from "@/components/log-detail-dialog"
import { LogRowContextMenu } from "@/components/log-row-context-menu"
import { Badge } from "@/components/ui/badge"
import { fetchKeymap, fetchNavLevel, type Keymap } from "@/lib/api"
import { entryDisplayTime, isErrorLevel, isWarnLevel } from "@/lib/entry-time"
import { levelOf, summarizeLog } from "@/lib/log-format"
import { levelBadgeClass } from "@/lib/log-format"
import type { LogEntry, TimeRange } from "@/lib/types"
import { cn } from "@/lib/utils"

type LogListProps = {
  entries: LogEntry[]
  autoScroll: boolean
  onAutoScrollChange: (v: boolean) => void
  onApplyFilter: (cel: string) => void
  hasMore?: boolean
  loadingMore?: boolean
  onLoadMore?: () => void
  query?: string
  timeRange?: TimeRange | null
  onSelectedIdChange?: (id: number | null) => void
  jumpToId?: number | null
  onJumpToIdConsumed?: () => void
}

const LOAD_MORE_THRESHOLD_PX = 240

function findLevelIndex(
  entries: LogEntry[],
  fromIndex: number,
  direction: 1 | -1,
  want: "error" | "warn" | "either"
): number | null {
  let i = fromIndex + direction
  while (i >= 0 && i < entries.length) {
    const level = levelOf(entries[i]!.data)
    const ok =
      want === "error"
        ? isErrorLevel(level)
        : want === "warn"
          ? isWarnLevel(level)
          : isErrorLevel(level) || isWarnLevel(level)
    if (ok) return i
    i += direction
  }
  return null
}

export function LogList({
  entries,
  autoScroll,
  onAutoScrollChange,
  onApplyFilter,
  hasMore = false,
  loadingMore = false,
  onLoadMore,
  query = "",
  timeRange = null,
  onSelectedIdChange,
  jumpToId = null,
  onJumpToIdConsumed,
}: LogListProps) {
  const parentRef = useRef<HTMLDivElement>(null)
  const [selectedId, setSelectedId] = useState<number | null>(null)
  const [focusedIndex, setFocusedIndex] = useState(0)
  const [detailOpen, setDetailOpen] = useState(false)
  const [pinnedEntry, setPinnedEntry] = useState<LogEntry | null>(null)
  const [keymap, setKeymap] = useState<Keymap>({
    nextError: "e",
    prevError: "E",
    down: "j",
    up: "k",
    quit: "q",
    showTrace: "t",
  })
  const navPending = useRef(false)

  useEffect(() => {
    void fetchKeymap()
      .then(setKeymap)
      .catch(() => {
        /* keep defaults */
      })
  }, [])

  useEffect(() => {
    onSelectedIdChange?.(selectedId)
  }, [selectedId, onSelectedIdChange])

  const selected = useMemo(() => {
    if (selectedId == null) return null
    return (
      entries.find((e) => e.id === selectedId) ??
      (pinnedEntry?.id === selectedId ? pinnedEntry : null)
    )
  }, [entries, selectedId, pinnedEntry])

  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 44,
    overscan: 20,
  })

  useEffect(() => {
    if (!autoScroll || entries.length === 0) return
    parentRef.current?.scrollTo({ top: 0 })
  }, [entries, autoScroll])

  useEffect(() => {
    if (entries.length === 0) {
      setFocusedIndex(0)
      return
    }
    if (focusedIndex >= entries.length) {
      setFocusedIndex(entries.length - 1)
    }
  }, [entries.length, focusedIndex])

  const focusRow = useCallback(
    (index: number, opts?: { openDetail?: boolean }) => {
      if (entries.length === 0) return
      const i = Math.max(0, Math.min(entries.length - 1, index))
      setFocusedIndex(i)
      const entry = entries[i]!
      setPinnedEntry(null)
      setSelectedId(entry.id)
      virtualizer.scrollToIndex(i, { align: "auto" })
      onAutoScrollChange(false)
      if (opts?.openDetail) setDetailOpen(true)
    },
    [entries, onAutoScrollChange, virtualizer]
  )

  useEffect(() => {
    if (jumpToId == null) return
    const idx = entries.findIndex((e) => e.id === jumpToId)
    if (idx >= 0) {
      focusRow(idx, { openDetail: true })
    } else {
      setSelectedId(jumpToId)
      setDetailOpen(true)
    }
    onJumpToIdConsumed?.()
  }, [jumpToId, entries, focusRow, onJumpToIdConsumed])

  const focusEntry = useCallback(
    (entry: LogEntry, opts?: { openDetail?: boolean }) => {
      const idx = entries.findIndex((e) => e.id === entry.id)
      if (idx >= 0) {
        focusRow(idx, opts)
        return
      }
      setPinnedEntry(entry)
      setSelectedId(entry.id)
      onAutoScrollChange(false)
      if (opts?.openDetail) setDetailOpen(true)
    },
    [entries, focusRow, onAutoScrollChange]
  )

  const navByLevel = useCallback(
    async (direction: "next" | "prev") => {
      if (entries.length === 0 || navPending.current) return
      const localDir = direction === "next" ? 1 : -1
      const local = findLevelIndex(entries, focusedIndex, localDir, "either")
      if (local != null) {
        focusRow(local)
        return
      }
      const from = entries[focusedIndex] ?? entries[0]!
      navPending.current = true
      try {
        const hit = await fetchNavLevel({
          fromId: from.id,
          direction,
          q: query,
          from: timeRange?.from,
          to: timeRange?.to,
        })
        if (hit) focusEntry(hit, { openDetail: detailOpen })
      } catch {
        /* keep focus */
      } finally {
        navPending.current = false
      }
    },
    [detailOpen, entries, focusEntry, focusRow, focusedIndex, query, timeRange]
  )

  useEffect(() => {
    function onKeyDown(ev: KeyboardEvent) {
      const t = ev.target as HTMLElement | null
      if (
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.isContentEditable ||
          t.closest("[role='textbox']"))
      ) {
        return
      }
      if (entries.length === 0) return

      if (ev.key === keymap.down || ev.key === "ArrowDown") {
        ev.preventDefault()
        focusRow(focusedIndex + 1)
        return
      }
      if (ev.key === keymap.up || ev.key === "ArrowUp") {
        ev.preventDefault()
        focusRow(focusedIndex - 1)
        return
      }
      if (ev.key === "Enter") {
        ev.preventDefault()
        focusRow(focusedIndex, { openDetail: true })
        return
      }
      if (ev.key === keymap.nextError) {
        ev.preventDefault()
        void navByLevel("next")
        return
      }
      if (ev.key === keymap.prevError) {
        ev.preventDefault()
        void navByLevel("prev")
        return
      }
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [entries, focusedIndex, focusRow, keymap, navByLevel])

  function onScroll() {
    const el = parentRef.current
    if (!el) return
    const atTop = el.scrollTop < 48
    if (atTop && !autoScroll) onAutoScrollChange(true)
    if (!atTop && autoScroll) onAutoScrollChange(false)

    const distanceFromBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight
    if (
      hasMore &&
      !loadingMore &&
      onLoadMore &&
      distanceFromBottom < LOAD_MORE_THRESHOLD_PX
    ) {
      onLoadMore()
    }
  }

  if (entries.length === 0) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 p-8 text-center">
        <p className="text-lg font-medium">Waiting for logs</p>
        <p className="max-w-sm text-sm text-muted-foreground">
          Pipe NDJSON into Mizpah to see entries here.
        </p>
      </div>
    )
  }

  return (
    <>
      <div
        ref={parentRef}
        onScroll={onScroll}
        tabIndex={0}
        className="min-h-0 flex-1 overflow-auto font-mono text-xs outline-none"
        aria-label="Log list. j/k move, Enter open, e/E next/previous error or warn. Right-click for actions."
      >
        <div
          style={{ height: virtualizer.getTotalSize(), position: "relative" }}
          className="w-full"
        >
          {virtualizer.getVirtualItems().map((item) => {
            const entry = entries[item.index]!
            const isFocused = focusedIndex === item.index
            const isSelected = selectedId === entry.id
            const level = levelOf(entry.data)
            return (
              <div
                key={entry.id}
                data-index={item.index}
                ref={virtualizer.measureElement}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${item.start}px)`,
                }}
                className="border-b border-border/60"
              >
                <LogRowContextMenu
                  entry={entry}
                  onActivate={() => {
                    setPinnedEntry(null)
                    setFocusedIndex(item.index)
                    setSelectedId(entry.id)
                    onAutoScrollChange(false)
                  }}
                  onOpenDetails={() => {
                    setPinnedEntry(null)
                    setFocusedIndex(item.index)
                    setSelectedId(entry.id)
                    setDetailOpen(true)
                    onAutoScrollChange(false)
                  }}
                  onApplyFilter={onApplyFilter}
                >
                  <button
                    type="button"
                    className={cn(
                      "flex w-full items-start gap-2 px-3 py-2 text-left transition-colors hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none",
                      (isSelected || isFocused) && "bg-muted/50",
                      isFocused && "ring-1 ring-inset ring-primary/40"
                    )}
                    onClick={() => {
                      setFocusedIndex(item.index)
                      setSelectedId(entry.id)
                      setDetailOpen(true)
                    }}
                  >
                    <span className="w-20 shrink-0 tabular-nums text-muted-foreground">
                      {new Date(entryDisplayTime(entry)).toLocaleTimeString()}
                    </span>
                    <Badge
                      variant="secondary"
                      className="shrink-0 rounded-md font-mono"
                    >
                      {entry.service}
                    </Badge>
                    {level ? (
                      <Badge
                        variant="outline"
                        className={cn(
                          "shrink-0 rounded-md",
                          levelBadgeClass(level)
                        )}
                      >
                        {level}
                      </Badge>
                    ) : null}
                    <span className="min-w-0 flex-1 truncate text-foreground">
                      {summarizeLog(entry.data)}
                    </span>
                  </button>
                </LogRowContextMenu>
              </div>
            )
          })}
        </div>
        {loadingMore ? (
          <div className="px-3 py-2 text-center text-muted-foreground">
            Loading older logs…
          </div>
        ) : null}
      </div>

      <LogDetailDialog
        entry={detailOpen ? selected : null}
        entries={entries}
        onSelect={(entry) => {
          if (entry == null) {
            setDetailOpen(false)
            return
          }
          focusEntry(entry)
          setDetailOpen(true)
        }}
        onApplyFilter={onApplyFilter}
      />
    </>
  )
}
