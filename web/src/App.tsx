import { ArrowUpCircle, Pause, Play, Radio } from "lucide-react"
import { useCallback, useEffect, useMemo, useState } from "react"

import { CelQueryEditor } from "@/components/cel-query-editor"
import { LogList } from "@/components/log-list"
import { PropertyFilterDrawer } from "@/components/property-filter-drawer"
import { ServicesDialog } from "@/components/services-dialog"
import { TimeActivityStrip } from "@/components/time-activity-strip"
import { UpdateDialog } from "@/components/update-dialog"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { useMizpah } from "@/hooks/use-mizpah"
import { fetchUpdateStatus } from "@/lib/api"
import { formatBytes } from "@/lib/log-format"
import type { UpdateStatus } from "@/lib/types"
import { readQueryFromSession, writeQueryToSession } from "@/lib/filter-storage"
import { pushHistory } from "@/lib/query-library-storage"
import {
  DEFAULT_TIME_ZOOM_INDEX,
  timeZoomAt,
} from "@/lib/time-zoom"
import type { TimeRange } from "@/lib/types"
import { cn } from "@/lib/utils"

export function App() {
  const [query, setQuery] = useState(() => readQueryFromSession())
  const [timeRange, setTimeRange] = useState<TimeRange | null>(null)
  const [timeZoomIndex, setTimeZoomIndex] = useState(DEFAULT_TIME_ZOOM_INDEX)
  const [autoScroll, setAutoScroll] = useState(true)
  const [servicesOpen, setServicesOpen] = useState(false)
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus | null>(null)
  const [updateOpen, setUpdateOpen] = useState(false)

  const timeZoom = useMemo(() => timeZoomAt(timeZoomIndex), [timeZoomIndex])

  useEffect(() => {
    writeQueryToSession(query)
  }, [query])

  useEffect(() => {
    let cancelled = false
    const poll = async () => {
      try {
        const status = await fetchUpdateStatus()
        if (!cancelled) setUpdateStatus(status)
      } catch {
        // hub may be restarting
      }
    }
    void poll()
    const id = window.setInterval(poll, 30_000)
    return () => {
      cancelled = true
      window.clearInterval(id)
    }
  }, [])

  const onQueryChange = useCallback((q: string) => {
    setQuery(q)
    if (q.trim()) pushHistory(q)
  }, [])

  const filterActive = Boolean(query.trim()) || timeRange != null

  const onClearFilter = useCallback(() => {
    setQuery("")
    setTimeRange(null)
  }, [])

  const {
    entries,
    hasMore,
    loadingMore,
    loadMore,
    services,
    blocked,
    properties,
    propertiesRevision,
    stats,
    activity,
    connected,
    loading,
    queryError,
    onDisconnectService,
    onReconnectService,
  } = useMizpah(query, timeRange, timeZoom)

  const memRatio = useMemo(() => {
    if (!stats || stats.maxBytes <= 0) return 0
    return Math.min(1, stats.approxBytes / stats.maxBytes)
  }, [stats])

  const memLabel = useMemo(() => {
    if (!stats) return "Memory usage unavailable"
    return `${formatBytes(stats.approxBytes)} / ${formatBytes(stats.maxBytes)}`
  }, [stats])

  const serviceCounts = useMemo(() => {
    const map: Record<string, number> = {}
    for (const s of stats?.services ?? []) {
      map[s.name] = s.count
    }
    return map
  }, [stats])

  const connectedCount = stats?.services.length ?? services.length
  const serviceLabel = `${connectedCount} service${connectedCount === 1 ? "" : "s"}`
  const blockedSuffix =
    blocked.length > 0 ? ` · ${blocked.length} disconnected` : ""

  return (
    <TooltipProvider>
      <div className="flex h-svh flex-col bg-background text-foreground">
        <header className="flex flex-wrap items-center gap-3 border-b px-4 py-3">
          <span className="text-lg font-semibold tracking-tight text-primary">mizpah</span>

          <CelQueryEditor
            value={query}
            onChange={onQueryChange}
            properties={properties}
            error={queryError}
            showingCount={entries.length}
            storedCount={stats?.count ?? null}
            filterActive={filterActive}
            onClearFilter={onClearFilter}
          />

          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setAutoScroll((v) => !v)}
          >
            {autoScroll ? (
              <>
                <Pause className="size-3.5" />
                Pause
              </>
            ) : (
              <>
                <Play className="size-3.5" />
                Resume
              </>
            )}
          </Button>
        </header>

        <TimeActivityStrip
          buckets={activity}
          selected={timeRange}
          onSelect={setTimeRange}
          zoom={timeZoom}
          zoomIndex={timeZoomIndex}
          onZoomIndexChange={setTimeZoomIndex}
        />

        <div className="flex min-h-0 flex-1">
          <PropertyFilterDrawer
            properties={properties}
            propertiesRevision={propertiesRevision}
            propertyCount={properties.length}
            services={services}
            onApplyFilter={onQueryChange}
          />
          <main className="flex min-h-0 min-w-0 flex-1 flex-col">
            {loading && entries.length === 0 ? (
              <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
                Loading…
              </div>
            ) : (
              <LogList
                entries={entries}
                autoScroll={autoScroll}
                onAutoScrollChange={setAutoScroll}
                onApplyFilter={onQueryChange}
                hasMore={hasMore}
                loadingMore={loadingMore}
                onLoadMore={loadMore}
              />
            )}
          </main>
        </div>

        <footer className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t px-4 py-2 text-xs text-muted-foreground">
          <div className="flex items-center gap-2">
            <Badge
              variant={connected ? "outline" : "destructive"}
              className={
                connected
                  ? "gap-1 rounded-md border-primary/40 bg-primary/10 text-primary"
                  : "gap-1 rounded-md"
              }
            >
              <Radio className={`size-3 ${connected ? "animate-pulse" : ""}`} />
              {connected ? "live" : "offline"}
            </Badge>
            <button
              type="button"
              className={cn(
                "rounded-sm tabular-nums underline-offset-2",
                "hover:text-foreground hover:underline",
                "focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
              )}
              onClick={() => setServicesOpen(true)}
              aria-haspopup="dialog"
              aria-expanded={servicesOpen}
            >
              <span className="text-foreground">{serviceLabel}</span>
              {blockedSuffix}
            </button>
          </div>
          <Tooltip>
            <TooltipTrigger asChild>
              <div
                className="flex h-2 w-24 items-center"
                role="progressbar"
                aria-label="Buffer memory usage"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.round(memRatio * 100)}
                aria-valuetext={memLabel}
              >
                <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                  <div
                    className={cn(
                      "h-full rounded-full transition-[width] duration-300",
                      memRatio >= 0.9
                        ? "bg-destructive"
                        : memRatio >= 0.7
                          ? "bg-primary/80"
                          : "bg-primary"
                    )}
                    style={{ width: `${memRatio * 100}%` }}
                  />
                </div>
              </div>
            </TooltipTrigger>
            <TooltipContent side="top" sideOffset={6}>
              <span className="tabular-nums">{memLabel}</span>
            </TooltipContent>
          </Tooltip>
          {updateStatus?.updateAvailable && updateStatus.latestVersion ? (
            <button
              type="button"
              className={cn(
                "ml-auto inline-flex items-center gap-1.5 rounded-sm text-primary",
                "underline-offset-2 hover:underline",
                "focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
              )}
              onClick={() => setUpdateOpen(true)}
            >
              <ArrowUpCircle className="size-3.5" />
              Update to v{updateStatus.latestVersion}
            </button>
          ) : null}
        </footer>

        <ServicesDialog
          open={servicesOpen}
          onOpenChange={setServicesOpen}
          services={services}
          blocked={blocked}
          serviceCounts={serviceCounts}
          onDisconnectService={onDisconnectService}
          onReconnectService={onReconnectService}
        />

        {updateStatus?.latestVersion && updateOpen ? (
          <UpdateDialog
            key={updateStatus.latestVersion}
            open={updateOpen}
            onOpenChange={setUpdateOpen}
            expectedLatest={updateStatus.latestVersion}
          />
        ) : null}
      </div>
    </TooltipProvider>
  )
}

export default App
