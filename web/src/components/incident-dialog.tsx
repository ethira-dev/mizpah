import { Loader2 } from "lucide-react"
import { useCallback, useEffect, useState } from "react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { fetchIncident, type IncidentSummary } from "@/lib/api"
import { levelBadgeClass } from "@/lib/log-format"
import { cn } from "@/lib/utils"

type IncidentDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  onJumpToId: (id: number) => void
  minutes?: number
}

export function IncidentDialog({
  open,
  onOpenChange,
  onJumpToId,
  minutes = 15,
}: IncidentDialogProps) {
  const [summary, setSummary] = useState<IncidentSummary | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      setSummary(await fetchIncident(minutes))
    } catch (e) {
      setSummary(null)
      setError(e instanceof Error ? e.message : "Failed to load incident")
    } finally {
      setLoading(false)
    }
  }, [minutes])

  useEffect(() => {
    if (open) void load()
  }, [open, load])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        showCloseButton
        className="flex max-h-[88vh] w-[min(96vw,36rem)] max-w-none flex-col gap-0 overflow-hidden p-0 sm:max-w-none"
      >
        <DialogHeader className="shrink-0 space-y-1 border-b px-5 py-4 pr-12">
          <DialogTitle>What broke?</DialogTitle>
          <DialogDescription>
            Incident summary for the last {minutes} minutes
          </DialogDescription>
        </DialogHeader>

        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-5 py-4">
          {loading ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" />
              Loading…
            </div>
          ) : null}

          {error ? (
            <p className="text-xs text-destructive whitespace-pre-wrap">{error}</p>
          ) : null}

          {summary && !loading ? (
            <>
              <div className="flex flex-wrap items-center gap-2 text-sm">
                <span className="tabular-nums text-muted-foreground">
                  {summary.total} events
                </span>
                {summary.byLevel.map((row) => (
                  <Badge
                    key={row.level}
                    variant="outline"
                    className={cn("rounded-md tabular-nums", levelBadgeClass(row.level))}
                  >
                    {row.level} {row.count}
                  </Badge>
                ))}
              </div>

              {summary.topServices.length > 0 ? (
                <section className="space-y-2">
                  <h3 className="text-xs font-medium text-muted-foreground">
                    Top services
                  </h3>
                  <ul className="space-y-1">
                    {summary.topServices.map((s) => (
                      <li
                        key={s.service}
                        className="flex justify-between gap-2 font-mono text-xs"
                      >
                        <span className="truncate">{s.service}</span>
                        <span className="tabular-nums text-muted-foreground">
                          {s.count}
                        </span>
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}

              {summary.topMessages.length > 0 ? (
                <section className="space-y-2">
                  <h3 className="text-xs font-medium text-muted-foreground">
                    Top messages
                  </h3>
                  <ul className="space-y-2">
                    {summary.topMessages.map((m) => (
                      <li
                        key={`${m.msg}-${m.sampleId ?? ""}`}
                        className="rounded-md border px-3 py-2 text-xs"
                      >
                        <div className="flex items-start justify-between gap-2">
                          <p className="min-w-0 flex-1 font-mono leading-snug">
                            {m.msg || "(empty)"}
                          </p>
                          <span className="shrink-0 tabular-nums text-muted-foreground">
                            ×{m.count}
                          </span>
                        </div>
                        {m.sampleId != null ? (
                          <Button
                            type="button"
                            variant="link"
                            size="sm"
                            className="mt-1 h-auto px-0 font-mono text-xs"
                            onClick={() => {
                              onJumpToId(m.sampleId!)
                              onOpenChange(false)
                            }}
                          >
                            Jump to #{m.sampleId}
                          </Button>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}

              {summary.topTraces.length > 0 ? (
                <section className="space-y-2">
                  <h3 className="text-xs font-medium text-muted-foreground">
                    Top traces
                  </h3>
                  <ul className="space-y-1">
                    {summary.topTraces.map((t) => (
                      <li
                        key={t.opid}
                        className="flex justify-between gap-2 font-mono text-xs"
                      >
                        <span className="truncate">{t.opid}</span>
                        <span className="tabular-nums text-muted-foreground">
                          {t.errorCount} errors
                        </span>
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}

              {summary.notes.length > 0 ? (
                <ul className="space-y-1 text-xs text-muted-foreground">
                  {summary.notes.map((note) => (
                    <li key={note}>{note}</li>
                  ))}
                </ul>
              ) : null}

              {summary.total === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No events in this window.
                </p>
              ) : null}
            </>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  )
}
