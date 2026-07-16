import {
  ChevronDown,
  ChevronUp,
  Copy,
  Loader2,
  Sparkles,
} from "lucide-react"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import {
  JsonExplorer,
  type JsonViewMode,
} from "@/components/json-explorer"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import {
  levelOf,
  startInvestigate,
  summarizeLog,
  type InvestigateTarget,
} from "@/lib/api"
import {
  copyText,
  formatRelativeTime,
  levelAccentClass,
  levelBadgeClass,
} from "@/lib/log-format"
import type { LogEntry } from "@/lib/types"
import { cn } from "@/lib/utils"

const CONTEXT_RADIUS = 8

type LogDetailDialogProps = {
  entry: LogEntry | null
  entries: LogEntry[]
  onSelect: (entry: LogEntry | null) => void
  onApplyFilter: (cel: string) => void
}

function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  const tag = target.tagName
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true
  return target.isContentEditable
}

export function LogDetailDialog({
  entry,
  entries,
  onSelect,
  onApplyFilter,
}: LogDetailDialogProps) {
  const [mode, setMode] = useState<JsonViewMode>("tree")
  const [investigatePending, setInvestigatePending] = useState(false)
  const [investigateError, setInvestigateError] = useState<string | null>(null)
  const [copiedWhat, setCopiedWhat] = useState<"json" | "id" | null>(null)
  const [now, setNow] = useState(() => Date.now())
  const searchRef = useRef<HTMLInputElement>(null)
  const contextCurrentRef = useRef<HTMLButtonElement>(null)

  const selectedIndex = useMemo(() => {
    if (!entry) return -1
    return entries.findIndex((e) => e.id === entry.id)
  }, [entry, entries])

  const contextEntries = useMemo(() => {
    if (selectedIndex < 0) return []
    const start = Math.max(0, selectedIndex - CONTEXT_RADIUS)
    const end = Math.min(entries.length, selectedIndex + CONTEXT_RADIUS + 1)
    return entries.slice(start, end)
  }, [entries, selectedIndex])

  const level = entry ? levelOf(entry.data) : null
  const canPrev = selectedIndex > 0
  const canNext = selectedIndex >= 0 && selectedIndex < entries.length - 1

  const resetTransient = useCallback(() => {
    setInvestigateError(null)
    setInvestigatePending(false)
    setCopiedWhat(null)
    setMode("tree")
  }, [])

  const close = useCallback(() => {
    resetTransient()
    onSelect(null)
  }, [onSelect, resetTransient])

  const goPrev = useCallback(() => {
    if (selectedIndex <= 0) return
    onSelect(entries[selectedIndex - 1]!)
  }, [entries, onSelect, selectedIndex])

  const goNext = useCallback(() => {
    if (selectedIndex < 0 || selectedIndex >= entries.length - 1) return
    onSelect(entries[selectedIndex + 1]!)
  }, [entries, onSelect, selectedIndex])

  const copyJson = useCallback(async () => {
    if (!entry) return
    const ok = await copyText(JSON.stringify(entry.data, null, 2))
    if (ok) {
      setCopiedWhat("json")
      window.setTimeout(() => setCopiedWhat(null), 1200)
    }
  }, [entry])

  useEffect(() => {
    if (!entry) return
    const id = window.setInterval(() => setNow(Date.now()), 15_000)
    return () => window.clearInterval(id)
  }, [entry])

  const entryId = entry?.id
  useEffect(() => {
    if (entryId == null) return
    const id = window.requestAnimationFrame(() => {
      contextCurrentRef.current?.scrollIntoView({ block: "nearest" })
    })
    return () => window.cancelAnimationFrame(id)
  }, [entryId])

  async function onInvestigate(target: InvestigateTarget) {
    if (!entry || investigatePending) return
    setInvestigatePending(true)
    setInvestigateError(null)
    try {
      await startInvestigate(target, entry.id)
    } catch (err) {
      setInvestigateError(err instanceof Error ? err.message : String(err))
    } finally {
      setInvestigatePending(false)
    }
  }

  async function copyId() {
    if (!entry) return
    const ok = await copyText(String(entry.id))
    if (ok) {
      setCopiedWhat("id")
      window.setTimeout(() => setCopiedWhat(null), 1200)
    }
  }

  function handleApplyFilter(cel: string) {
    onApplyFilter(cel)
    close()
  }

  useEffect(() => {
    if (!entry) return

    function onKeyDown(e: KeyboardEvent) {
      if (isTypingTarget(e.target)) return

      if (e.key === "ArrowUp" || e.key === "k") {
        e.preventDefault()
        goPrev()
        return
      }
      if (e.key === "ArrowDown" || e.key === "j") {
        e.preventDefault()
        goNext()
        return
      }
      if (e.key === "c" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault()
        void copyJson()
        return
      }
      if (e.key === "/") {
        e.preventDefault()
        setMode("tree")
        window.requestAnimationFrame(() => searchRef.current?.focus())
        return
      }
      if (e.key === "t" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault()
        setMode("tree")
        return
      }
      if (e.key === "r" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault()
        setMode("raw")
      }
    }

    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [entry, goPrev, goNext, copyJson])

  return (
    <Dialog
      open={entry !== null}
      onOpenChange={(open) => {
        if (!open) close()
      }}
    >
      <DialogContent
        showCloseButton
        className={cn(
          "flex max-h-[88vh] min-h-0 w-[min(96vw,72rem)] max-w-none flex-col gap-0 overflow-hidden p-0 sm:max-w-none",
          levelAccentClass(level)
        )}
      >
        {entry ? (
          <>
            <DialogHeader
              key={entry.id}
              className="shrink-0 space-y-0 border-b px-5 py-4 pr-12 duration-200 animate-in fade-in"
            >
              <div className="flex items-start gap-3">
                <div className="min-w-0 flex-1">
                  <DialogTitle className="font-mono text-sm leading-snug font-medium">
                    {summarizeLog(entry.data)}
                  </DialogTitle>
                  <DialogDescription asChild>
                    <div className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1.5 text-xs">
                      <span className="tabular-nums text-muted-foreground">
                        {new Date(entry.receivedAt).toLocaleString()}
                      </span>
                      <span className="text-muted-foreground/40">·</span>
                      <span className="tabular-nums text-muted-foreground">
                        {formatRelativeTime(entry.receivedAt, now)}
                      </span>
                      <Badge
                        variant="secondary"
                        className="rounded-md font-mono"
                      >
                        {entry.service}
                      </Badge>
                      {level ? (
                        <Badge
                          variant="outline"
                          className={cn(
                            "rounded-md",
                            levelBadgeClass(level)
                          )}
                        >
                          {level}
                        </Badge>
                      ) : null}
                      <button
                        type="button"
                        className="rounded-md font-mono text-muted-foreground tabular-nums hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                        onClick={() => void copyId()}
                        title="Copy id"
                      >
                        #{entry.id}
                        {copiedWhat === "id" ? (
                          <span className="ml-1 text-[10px]">copied</span>
                        ) : null}
                      </button>
                    </div>
                  </DialogDescription>
                </div>

                <div className="flex shrink-0 items-center gap-1 pt-0.5">
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <Button
                        type="button"
                        variant="outline"
                        size="icon-sm"
                        disabled={!canPrev}
                        onClick={goPrev}
                        aria-label="Previous log"
                      >
                        <ChevronUp className="size-3.5" />
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent side="bottom">
                      Previous (↑ / k)
                    </TooltipContent>
                  </Tooltip>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <Button
                        type="button"
                        variant="outline"
                        size="icon-sm"
                        disabled={!canNext}
                        onClick={goNext}
                        aria-label="Next log"
                      >
                        <ChevronDown className="size-3.5" />
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent side="bottom">Next (↓ / j)</TooltipContent>
                  </Tooltip>
                </div>
              </div>
            </DialogHeader>

            <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
              <div className="flex min-h-0 min-w-0 flex-1 flex-col border-b lg:border-r lg:border-b-0">
                <JsonExplorer
                  key={entry.id}
                  value={entry.data}
                  mode={mode}
                  onModeChange={setMode}
                  onApplyFilter={handleApplyFilter}
                  searchRef={searchRef}
                  className="min-h-[12rem]"
                  toolbarStart={
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          type="button"
                          variant="outline"
                          size="xs"
                          onClick={() => void copyJson()}
                        >
                          <Copy className="size-3" />
                          {copiedWhat === "json" ? "Copied" : "Copy JSON"}
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent side="bottom">
                        Copy JSON (c)
                      </TooltipContent>
                    </Tooltip>
                  }
                />
              </div>

              <aside className="flex max-h-[40vh] w-full shrink-0 flex-col lg:max-h-none lg:w-[280px]">
                <div className="shrink-0 border-b px-3 py-2">
                  <p className="text-xs font-medium">Around this log</p>
                  <p className="text-[11px] text-muted-foreground">
                    {selectedIndex + 1} of {entries.length} in current view
                  </p>
                </div>
                <div className="min-h-0 flex-1 overflow-auto">
                  <ul className="divide-y divide-border/60">
                    {contextEntries.map((neighbor) => {
                      const isCurrent = neighbor.id === entry.id
                      const nLevel = levelOf(neighbor.data)
                      return (
                        <li key={neighbor.id}>
                          <button
                            type="button"
                            ref={isCurrent ? contextCurrentRef : undefined}
                            className={cn(
                              "flex w-full flex-col gap-0.5 px-3 py-2 text-left transition-colors",
                              "hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                              isCurrent && "bg-muted/70"
                            )}
                            onClick={() => onSelect(neighbor)}
                            aria-current={isCurrent ? "true" : undefined}
                          >
                            <div className="flex items-center gap-1.5">
                              <span className="font-mono text-[10px] tabular-nums text-muted-foreground">
                                {new Date(
                                  neighbor.receivedAt
                                ).toLocaleTimeString()}
                              </span>
                              {nLevel ? (
                                <Badge
                                  variant="outline"
                                  className={cn(
                                    "h-4 rounded-sm px-1 text-[9px]",
                                    levelBadgeClass(nLevel)
                                  )}
                                >
                                  {nLevel}
                                </Badge>
                              ) : null}
                              <span className="ml-auto font-mono text-[10px] text-muted-foreground/70">
                                #{neighbor.id}
                              </span>
                            </div>
                            <span
                              className={cn(
                                "line-clamp-2 font-mono text-[11px] leading-snug",
                                isCurrent
                                  ? "text-foreground"
                                  : "text-muted-foreground"
                              )}
                            >
                              {summarizeLog(neighbor.data)}
                            </span>
                          </button>
                        </li>
                      )
                    })}
                  </ul>
                </div>
              </aside>
            </div>

            <DialogFooter className="mx-0 mb-0 shrink-0 flex-col gap-2 sm:flex-col sm:items-stretch">
              {investigateError ? (
                <p className="text-xs text-destructive">{investigateError}</p>
              ) : null}
              <div className="flex flex-col-reverse gap-2 sm:flex-row sm:items-center sm:justify-between">
                <p className="text-[11px] text-muted-foreground">
                  {investigatePending
                    ? "Opening investigation…"
                    : "↑↓ navigate · / search · t/r view · click value to filter"}
                </p>
                <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={investigatePending}
                    onClick={() => void onInvestigate("claude")}
                  >
                    {investigatePending ? (
                      <Loader2 className="size-3.5 animate-spin" />
                    ) : (
                      <Sparkles className="size-3.5" />
                    )}
                    Check with Claude
                  </Button>
                  <Button
                    type="button"
                    variant="default"
                    size="sm"
                    disabled={investigatePending}
                    onClick={() => void onInvestigate("cursor")}
                  >
                    {investigatePending ? (
                      <Loader2 className="size-3.5 animate-spin" />
                    ) : null}
                    Check with Cursor
                  </Button>
                </div>
              </div>
            </DialogFooter>
          </>
        ) : null}
      </DialogContent>
    </Dialog>
  )
}
