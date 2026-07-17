import { useVirtualizer } from "@tanstack/react-virtual"
import { useEffect, useMemo, useRef, useState } from "react"

import { LogDetailDialog } from "@/components/log-detail-dialog"
import { Badge } from "@/components/ui/badge"
import { levelOf, summarizeLog } from "@/lib/log-format"
import { levelBadgeClass } from "@/lib/log-format"
import type { LogEntry } from "@/lib/types"
import { cn } from "@/lib/utils"

type LogListProps = {
  entries: LogEntry[]
  autoScroll: boolean
  onAutoScrollChange: (v: boolean) => void
  onApplyFilter: (cel: string) => void
}

export function LogList({
  entries,
  autoScroll,
  onAutoScrollChange,
  onApplyFilter,
}: LogListProps) {
  const parentRef = useRef<HTMLDivElement>(null)
  const [selectedId, setSelectedId] = useState<number | null>(null)

  const selected = useMemo(() => {
    if (selectedId == null) return null
    return entries.find((e) => e.id === selectedId) ?? null
  }, [entries, selectedId])

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

  function onScroll() {
    const el = parentRef.current
    if (!el) return
    const atTop = el.scrollTop < 48
    if (atTop && !autoScroll) onAutoScrollChange(true)
    if (!atTop && autoScroll) onAutoScrollChange(false)
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
        className="min-h-0 flex-1 overflow-auto font-mono text-xs"
      >
        <div
          style={{ height: virtualizer.getTotalSize(), position: "relative" }}
          className="w-full"
        >
          {virtualizer.getVirtualItems().map((item) => {
            const entry = entries[item.index]!
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
                <button
                  type="button"
                  className={cn(
                    "flex w-full items-start gap-2 px-3 py-2 text-left transition-colors hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none",
                    isSelected && "bg-muted/50"
                  )}
                  onClick={() => setSelectedId(entry.id)}
                >
                  <span className="w-20 shrink-0 tabular-nums text-muted-foreground">
                    {new Date(entry.receivedAt).toLocaleTimeString()}
                  </span>
                  <Badge variant="secondary" className="shrink-0 rounded-md font-mono">
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
              </div>
            )
          })}
        </div>
      </div>

      <LogDetailDialog
        entry={selected}
        entries={entries}
        onSelect={(entry) => setSelectedId(entry?.id ?? null)}
        onApplyFilter={onApplyFilter}
      />
    </>
  )
}
