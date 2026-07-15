import { useVirtualizer } from "@tanstack/react-virtual"
import { useEffect, useRef, useState } from "react"

import { Badge } from "@/components/ui/badge"
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet"
import { levelOf, summarizeLog } from "@/lib/api"
import type { LogEntry } from "@/lib/types"
import { cn } from "@/lib/utils"

type LogListProps = {
  entries: LogEntry[]
  autoScroll: boolean
  onAutoScrollChange: (v: boolean) => void
}

function levelVariant(level: string | null): "default" | "secondary" | "destructive" | "outline" {
  if (!level) return "outline"
  if (level.includes("error") || level.includes("fatal") || level === "50") {
    return "destructive"
  }
  if (level.includes("warn") || level === "40") return "secondary"
  return "outline"
}

export function LogList({ entries, autoScroll, onAutoScrollChange }: LogListProps) {
  const parentRef = useRef<HTMLDivElement>(null)
  const [selected, setSelected] = useState<LogEntry | null>(null)

  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 44,
    overscan: 20,
  })

  useEffect(() => {
    if (!autoScroll || entries.length === 0) return
    // Newest entries are at the top — keep pinned to top when live tailing
    parentRef.current?.scrollTo({ top: 0 })
  }, [entries, autoScroll])

  function onScroll() {
    const el = parentRef.current
    if (!el) return
    const atTop = el.scrollTop < 48
    if (atTop && !autoScroll) onAutoScrollChange(true)
    if (!atTop && autoScroll) onAutoScrollChange(false)
  }

  const selectedLevel = selected ? levelOf(selected.data) : null

  if (entries.length === 0) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 p-8 text-center">
        <p className="text-lg font-medium">Waiting for logs</p>
        <p className="max-w-sm text-sm text-muted-foreground">
          Pipe NDJSON into Mizpah with{" "}
          <code className="font-mono text-xs">--service</code> to see entries here.
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
            const entry = entries[item.index]
            const isSelected = selected?.id === entry.id
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
                  onClick={() => setSelected(entry)}
                >
                  <span className="w-20 shrink-0 tabular-nums text-muted-foreground">
                    {new Date(entry.receivedAt).toLocaleTimeString()}
                  </span>
                  <Badge variant="secondary" className="shrink-0 rounded-md font-mono">
                    {entry.service}
                  </Badge>
                  {level && (
                    <Badge
                      variant={levelVariant(level)}
                      className={cn("shrink-0 rounded-md uppercase")}
                    >
                      {level}
                    </Badge>
                  )}
                  <span className="min-w-0 flex-1 truncate text-foreground">
                    {summarizeLog(entry.data)}
                  </span>
                </button>
              </div>
            )
          })}
        </div>
      </div>

      <Sheet
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open) setSelected(null)
        }}
      >
        <SheetContent side="right" className="w-full gap-0 sm:max-w-xl">
          {selected && (
            <>
              <SheetHeader className="border-b">
                <SheetTitle className="font-mono text-sm">
                  {summarizeLog(selected.data)}
                </SheetTitle>
                <SheetDescription asChild>
                  <div className="flex flex-wrap items-center gap-2 pt-1">
                    <span className="tabular-nums">
                      {new Date(selected.receivedAt).toLocaleString()}
                    </span>
                    <Badge variant="secondary" className="rounded-md font-mono">
                      {selected.service}
                    </Badge>
                    {selectedLevel && (
                      <Badge
                        variant={levelVariant(selectedLevel)}
                        className="rounded-md uppercase"
                      >
                        {selectedLevel}
                      </Badge>
                    )}
                  </div>
                </SheetDescription>
              </SheetHeader>
              <pre className="min-h-0 flex-1 overflow-auto px-4 py-3 font-mono text-[11px] leading-relaxed text-muted-foreground">
                {JSON.stringify(selected.data, null, 2)}
              </pre>
            </>
          )}
        </SheetContent>
      </Sheet>
    </>
  )
}
