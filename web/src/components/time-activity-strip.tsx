import { Minus, Plus } from "lucide-react"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import { Button } from "@/components/ui/button"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import {
  clampTimeZoomIndex,
  isZoomScrollModifier,
  TIME_ZOOM_LEVELS,
  type TimeZoomLevel,
} from "@/lib/time-zoom"
import type { ActivityBucket, TimeRange } from "@/lib/types"
import { cn } from "@/lib/utils"

type TimeActivityStripProps = {
  buckets: ActivityBucket[]
  selected: TimeRange | null
  onSelect: (range: TimeRange | null) => void
  zoom: TimeZoomLevel
  zoomIndex: number
  onZoomIndexChange: (index: number) => void
}

/** −1 = zoom in (finer); +1 = zoom out (coarser). */
type ZoomDirection = -1 | 1

const ZOOM_DEBOUNCE_MS = 140
const HUD_VISIBLE_MS = 700
const EDGE_SHAKE_MS = 180

function formatBucketLabel(
  startIso: string,
  endIso: string,
  bucketMinutes: number,
  openEnded = false
): string {
  const start = new Date(startIso)
  const end = new Date(endIso)
  if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) {
    return "Invalid range"
  }
  const opts: Intl.DateTimeFormatOptions =
    bucketMinutes >= 1440
      ? { month: "short", day: "numeric" }
      : bucketMinutes >= 60
        ? { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }
        : { hour: "2-digit", minute: "2-digit" }
  const startLabel = start.toLocaleString(undefined, opts)
  if (openEnded) {
    return `${startLabel} – now`
  }
  return `${startLabel} – ${end.toLocaleString(undefined, opts)}`
}

function fillClass(count: number, isCurrent: boolean): string {
  if (isCurrent) return "bg-primary"
  if (count > 0) return "bg-muted-foreground/25"
  return "bg-transparent"
}

function isCurrentBucket(bucket: ActivityBucket, nowMs: number): boolean {
  const start = Date.parse(bucket.start)
  const end = Date.parse(bucket.end)
  if (Number.isNaN(start) || Number.isNaN(end)) return false
  return nowMs >= start && nowMs < end
}

function zoomShortcutHint(): string {
  if (
    typeof navigator !== "undefined" &&
    /Mac|iPhone|iPad/i.test(navigator.platform)
  ) {
    return "⌘ + scroll"
  }
  return "Ctrl + scroll"
}

function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(false)
  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)")
    setReduced(mq.matches)
    function onChange(e: MediaQueryListEvent) {
      setReduced(e.matches)
    }
    mq.addEventListener("change", onChange)
    return () => mq.removeEventListener("change", onChange)
  }, [])
  return reduced
}

export function TimeActivityStrip({
  buckets,
  selected,
  onSelect,
  zoom,
  zoomIndex,
  onZoomIndexChange,
}: TimeActivityStripProps) {
  const maxCount = useMemo(
    () => buckets.reduce((m, b) => Math.max(m, b.count), 0),
    [buckets]
  )
  const nowMs = Date.now()
  const reducedMotion = usePrefersReducedMotion()

  const lastZoomAt = useRef(0)
  const zoomIndexRef = useRef(zoomIndex)
  const onSelectRef = useRef(onSelect)
  const onZoomIndexChangeRef = useRef(onZoomIndexChange)
  const hudTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const edgeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const wheelCleanupRef = useRef<(() => void) | null>(null)

  const [zoomDirection, setZoomDirection] = useState<ZoomDirection | null>(null)
  const [hudVisible, setHudVisible] = useState(false)
  const [edgeShake, setEdgeShake] = useState(false)
  const [hudKey, setHudKey] = useState(0)

  useEffect(() => {
    zoomIndexRef.current = zoomIndex
  }, [zoomIndex])
  useEffect(() => {
    onSelectRef.current = onSelect
  }, [onSelect])
  useEffect(() => {
    onZoomIndexChangeRef.current = onZoomIndexChange
  }, [onZoomIndexChange])

  useEffect(() => {
    return () => {
      if (hudTimerRef.current) clearTimeout(hudTimerRef.current)
      if (edgeTimerRef.current) clearTimeout(edgeTimerRef.current)
      wheelCleanupRef.current?.()
    }
  }, [])

  const flashHud = useCallback(() => {
    setHudVisible(true)
    setHudKey((k) => k + 1)
    if (hudTimerRef.current) clearTimeout(hudTimerRef.current)
    hudTimerRef.current = setTimeout(() => {
      setHudVisible(false)
    }, HUD_VISIBLE_MS)
  }, [])

  const flashEdge = useCallback(() => {
    setEdgeShake(true)
    flashHud()
    if (edgeTimerRef.current) clearTimeout(edgeTimerRef.current)
    edgeTimerRef.current = setTimeout(() => {
      setEdgeShake(false)
    }, EDGE_SHAKE_MS)
  }, [flashHud])

  const stepZoom = useCallback(
    (direction: ZoomDirection) => {
      const current = zoomIndexRef.current
      const next = clampTimeZoomIndex(current + direction)
      if (next === current) {
        flashEdge()
        return
      }
      setZoomDirection(direction)
      onSelectRef.current(null)
      onZoomIndexChangeRef.current(next)
      flashHud()
    },
    [flashEdge, flashHud]
  )

  const stepZoomRef = useRef(stepZoom)
  useEffect(() => {
    stepZoomRef.current = stepZoom
  }, [stepZoom])

  const rootCallbackRef = useCallback((el: HTMLDivElement | null) => {
    wheelCleanupRef.current?.()
    wheelCleanupRef.current = null
    if (!el) return

    function onWheel(e: WheelEvent) {
      if (!isZoomScrollModifier(e)) return
      e.preventDefault()
      const now = performance.now()
      if (now - lastZoomAt.current < ZOOM_DEBOUNCE_MS) return
      if (Math.abs(e.deltaY) < 2) return
      lastZoomAt.current = now

      // Scroll up → zoom in (finer); scroll down → zoom out (coarser).
      const direction: ZoomDirection = e.deltaY < 0 ? -1 : 1
      stepZoomRef.current(direction)
    }

    el.addEventListener("wheel", onWheel, { passive: false })
    wheelCleanupRef.current = () => {
      el.removeEventListener("wheel", onWheel)
    }
  }, [])

  const canZoomIn = zoomIndex > 0
  const canZoomOut = zoomIndex < TIME_ZOOM_LEVELS.length - 1
  const zoomLabel = `${zoom.windowLabel} · ${zoom.bucketLabel}`

  return (
    <div ref={rootCallbackRef} className="relative border-b px-3 py-2">
      <div className="mb-1.5 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <div
            className={cn(
              "overflow-hidden",
              edgeShake &&
                !reducedMotion &&
                "animate-[zoom-edge-shake_180ms_ease-in-out]"
            )}
          >
            <p
              key={zoomIndex}
              className={cn(
                "text-[10px] font-medium tracking-wide text-muted-foreground uppercase",
                !reducedMotion &&
                  zoomDirection === -1 &&
                  "animate-in fade-in-0 slide-in-from-bottom-1 duration-200",
                !reducedMotion &&
                  zoomDirection === 1 &&
                  "animate-in fade-in-0 slide-in-from-top-1 duration-200"
              )}
            >
              <span aria-live="polite" className="sr-only">
                {zoomLabel}
              </span>
              {zoom.windowLabel}
              <span className="ml-1.5 font-normal normal-case tracking-normal">
                · {zoom.bucketLabel}
              </span>
            </p>
          </div>

          <div className="flex items-center gap-0.5">
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label="Zoom out (coarser tiles)"
              disabled={!canZoomOut}
              onClick={() => stepZoom(1)}
            >
              <Minus />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label="Zoom in (finer tiles)"
              disabled={!canZoomIn}
              onClick={() => stepZoom(-1)}
            >
              <Plus />
            </Button>
          </div>

          <span className="hidden text-[10px] text-muted-foreground/70 sm:inline">
            {zoomShortcutHint()}
          </span>
        </div>

        {selected ? (
          <button
            type="button"
            className={cn(
              "shrink-0 text-[10px] text-muted-foreground underline-offset-2",
              "hover:text-foreground hover:underline",
              "focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            )}
            onClick={() => onSelect(null)}
          >
            Clear time filter
          </button>
        ) : null}
      </div>

      <div className="relative">
        {buckets.length === 0 ? (
          <div className="flex h-8 items-center text-xs text-muted-foreground">
            Loading activity…
          </div>
        ) : (
          <div
            key={zoomIndex}
            className={cn(
              "flex h-8 w-full items-stretch gap-px",
              !reducedMotion &&
                zoomDirection === -1 &&
                "animate-in fade-in-0 zoom-in-95 duration-200",
              !reducedMotion &&
                zoomDirection === 1 &&
                "animate-in fade-in-0 zoom-in-[1.05] duration-200"
            )}
            role="listbox"
            aria-label={`${zoom.windowLabel}, ${zoom.bucketLabel} per tile`}
          >
            {buckets.map((bucket) => {
              const isCurrent = isCurrentBucket(bucket, nowMs)
              const isSelected = isCurrent
                ? selected != null &&
                  selected.from === bucket.start &&
                  selected.to == null
                : selected?.from === bucket.start &&
                  selected?.to === bucket.end
              return (
                <Tooltip key={bucket.start}>
                  <TooltipTrigger asChild>
                    <button
                      type="button"
                      role="option"
                      aria-selected={isSelected}
                      aria-current={isCurrent ? "true" : undefined}
                      className={cn(
                        "relative min-w-0 flex-1 overflow-hidden rounded-[2px]",
                        "bg-muted/60 transition-colors",
                        "hover:ring-1 hover:ring-primary/50",
                        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none",
                        isSelected && "ring-2 ring-primary",
                        isCurrent &&
                          "z-10 ring-1 ring-primary/70 shadow-[0_0_10px_2px] shadow-primary/55 animate-pulse"
                      )}
                      onClick={() => {
                        if (isSelected) {
                          onSelect(null)
                          return
                        }
                        // Current tile: period start → now (open upper bound so live logs keep arriving).
                        if (isCurrent) {
                          onSelect({ from: bucket.start })
                          return
                        }
                        onSelect({ from: bucket.start, to: bucket.end })
                      }}
                    >
                      <span
                        className={cn(
                          "absolute inset-x-0 bottom-0 transition-[height]",
                          fillClass(bucket.count, isCurrent)
                        )}
                        style={{
                          height:
                            bucket.count > 0 && maxCount > 0
                              ? `${Math.max(18, Math.round((bucket.count / maxCount) * 100))}%`
                              : isCurrent
                                ? "35%"
                                : "0%",
                        }}
                        aria-hidden
                      />
                    </button>
                  </TooltipTrigger>
                  <TooltipContent side="bottom" sideOffset={6}>
                    <div className="flex flex-col gap-0.5">
                      <span>
                        {isCurrent ? "Now · " : ""}
                        {formatBucketLabel(
                          bucket.start,
                          bucket.end,
                          zoom.bucketMinutes,
                          isCurrent
                        )}
                      </span>
                      <span className="tabular-nums text-muted-foreground">
                        {bucket.count}{" "}
                        {bucket.count === 1 ? "entry" : "entries"}
                      </span>
                    </div>
                  </TooltipContent>
                </Tooltip>
              )
            })}
          </div>
        )}

        {hudVisible ? (
          <div
            key={hudKey}
            className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center"
            aria-hidden
          >
            <div
              className={cn(
                !reducedMotion && "animate-in fade-in-0 duration-200",
                !reducedMotion && zoomDirection === -1 && "zoom-in-95",
                !reducedMotion && zoomDirection === 1 && "zoom-in-[1.03]"
              )}
            >
              <div
                className={cn(
                  "flex flex-col items-center gap-1.5 rounded-md border border-border/80",
                  "bg-popover/95 px-3 py-2 shadow-md backdrop-blur-sm",
                  edgeShake &&
                    !reducedMotion &&
                    "animate-[zoom-edge-shake_180ms_ease-in-out]"
                )}
              >
                <span className="text-xs font-medium tabular-nums text-popover-foreground">
                  {zoomLabel}
                </span>
                <div className="flex items-center gap-1">
                  {TIME_ZOOM_LEVELS.map((_, i) => (
                    <span
                      key={i}
                      className={cn(
                        "size-1.5 rounded-full transition-colors duration-150",
                        i === zoomIndex
                          ? "bg-foreground"
                          : "bg-muted-foreground/30"
                      )}
                    />
                  ))}
                </div>
              </div>
            </div>
          </div>
        ) : null}
      </div>

      <style>{`
        @keyframes zoom-edge-shake {
          0%, 100% { transform: translateX(0); }
          25% { transform: translateX(-3px); }
          75% { transform: translateX(3px); }
        }
      `}</style>
    </div>
  )
}
