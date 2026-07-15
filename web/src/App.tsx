import { Pause, Play, Radio } from "lucide-react"
import { useEffect, useMemo, useState } from "react"

import { FilterSearch } from "@/components/filter-search"
import { LogList } from "@/components/log-list"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { TooltipProvider } from "@/components/ui/tooltip"
import { useMizpah } from "@/hooks/use-mizpah"
import { formatBytes } from "@/lib/api"
import { readFiltersFromSession, writeFiltersToSession } from "@/lib/filter-storage"
import type { FilterChip } from "@/lib/types"

export function App() {
  const [filters, setFilters] = useState<FilterChip[]>(() => readFiltersFromSession())
  const [autoScroll, setAutoScroll] = useState(true)

  useEffect(() => {
    writeFiltersToSession(filters)
  }, [filters])

  const { entries, services, properties, stats, connected, loading } = useMizpah(filters)

  const memLabel = useMemo(() => {
    if (!stats) return "—"
    return `${formatBytes(stats.approxBytes)} / ${formatBytes(stats.maxBytes)}`
  }, [stats])

  return (
    <TooltipProvider>
      <div className="flex h-svh flex-col bg-background text-foreground">
        <header className="flex flex-wrap items-center gap-3 border-b px-4 py-3">
          <div className="flex items-center gap-2">
            <span className="text-lg font-semibold tracking-tight">Mizpah</span>
            <Badge
              variant={connected ? "secondary" : "destructive"}
              className="gap-1 rounded-md"
            >
              <Radio className="size-3" />
              {connected ? "live" : "offline"}
            </Badge>
          </div>

          <div className="min-w-0 flex-1">
            <FilterSearch
              properties={properties}
              services={services}
              filters={filters}
              onChange={setFilters}
            />
          </div>

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

        <main className="flex min-h-0 flex-1 flex-col">
          {loading && entries.length === 0 ? (
            <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
              Loading…
            </div>
          ) : (
            <LogList
              entries={entries}
              autoScroll={autoScroll}
              onAutoScrollChange={setAutoScroll}
            />
          )}
        </main>

        <footer className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t px-4 py-2 text-xs text-muted-foreground">
          <span>
            Showing <span className="tabular-nums text-foreground">{entries.length}</span>
            {stats ? (
              <>
                {" "}
                / <span className="tabular-nums text-foreground">{stats.count}</span> stored
              </>
            ) : null}
          </span>
          <span className="tabular-nums">{memLabel}</span>
          <span>
            {stats?.services.length ?? services.length} service
            {(stats?.services.length ?? services.length) === 1 ? "" : "s"}
          </span>
        </footer>
      </div>
    </TooltipProvider>
  )
}

export default App
