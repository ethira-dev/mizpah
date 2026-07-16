import { Pause, Play, Radio } from "lucide-react"
import { useCallback, useEffect, useMemo, useState } from "react"

import { CelQueryEditor } from "@/components/cel-query-editor"
import { LogList } from "@/components/log-list"
import { PropertyFilterDrawer } from "@/components/property-filter-drawer"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { TooltipProvider } from "@/components/ui/tooltip"
import { useMizpah } from "@/hooks/use-mizpah"
import { formatBytes } from "@/lib/api"
import { readQueryFromSession, writeQueryToSession } from "@/lib/filter-storage"
import { pushHistory } from "@/lib/query-library-storage"

export function App() {
  const [query, setQuery] = useState(() => readQueryFromSession())
  const [autoScroll, setAutoScroll] = useState(true)

  useEffect(() => {
    writeQueryToSession(query)
  }, [query])

  const onQueryChange = useCallback((q: string) => {
    setQuery(q)
    if (q.trim()) pushHistory(q)
  }, [])

  const { entries, services, properties, propertiesRevision, stats, connected, loading, queryError } =
    useMizpah(query)

  const memLabel = useMemo(() => {
    if (!stats) return "—"
    return `${formatBytes(stats.approxBytes)} / ${formatBytes(stats.maxBytes)}`
  }, [stats])

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

        <div className="flex min-h-0 flex-1">
          <PropertyFilterDrawer
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
              />
            )}
          </main>
        </div>

        <footer className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t px-4 py-2 text-xs text-muted-foreground">
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
