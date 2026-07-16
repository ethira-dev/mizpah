import { useEffect, useRef, useState } from "react"

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
  fetchUpdateStatus,
  streamUpdate,
  type UpdateEvent,
} from "@/lib/api"
import { cn } from "@/lib/utils"

const WAIT_TIMEOUT_MS = 60_000
const POLL_INTERVAL_MS = 500

type UpdateDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  expectedLatest: string
}

type Phase = "running" | "waiting" | "error" | "done"

export function UpdateDialog({
  open,
  onOpenChange,
  expectedLatest,
}: UpdateDialogProps) {
  const [step, setStep] = useState("Starting update…")
  const [progress, setProgress] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [phase, setPhase] = useState<Phase>("running")
  const expectedRef = useRef(expectedLatest)

  useEffect(() => {
    expectedRef.current = expectedLatest
  }, [expectedLatest])

  useEffect(() => {
    if (!open) return

    const ac = new AbortController()
    let waiting = false
    let failed = false

    const beginWait = () => {
      if (waiting || failed) return
      waiting = true
      setPhase("waiting")
      setStep("Waiting for Mizpah to come back…")
      setProgress(0.98)

      const started = Date.now()
      const poll = async () => {
        while (Date.now() - started < WAIT_TIMEOUT_MS) {
          if (ac.signal.aborted) return
          try {
            const status = await fetchUpdateStatus()
            if (status.currentVersion === expectedRef.current) {
              setPhase("done")
              setProgress(1)
              setStep("Update complete")
              window.location.reload()
              return
            }
          } catch {
            // hub still down
          }
          await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS))
        }
        failed = true
        setPhase("error")
        setError(
          "Mizpah did not come back after the update. Run `mizpah hub start` and reload."
        )
      }
      void poll()
    }

    const onEvent = (ev: UpdateEvent) => {
      if (ev.step) setStep(ev.step)
      if (typeof ev.progress === "number") {
        setProgress(Math.min(1, Math.max(0, ev.progress)))
      }
      if (ev.error) {
        failed = true
        setPhase("error")
        setError(ev.error)
        return
      }
      if (ev.restarting) {
        beginWait()
      }
    }

    void streamUpdate(onEvent, ac.signal)
      .then(() => {
        if (!waiting && !failed && !ac.signal.aborted) {
          beginWait()
        }
      })
      .catch((err: unknown) => {
        if (ac.signal.aborted || waiting || failed) return
        failed = true
        setPhase("error")
        setError(err instanceof Error ? err.message : String(err))
      })

    return () => {
      ac.abort()
    }
  }, [open])

  const inProgress = phase === "running" || phase === "waiting"
  const progressPct = Math.round(progress * 100)

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (inProgress) return
        onOpenChange(next)
      }}
    >
      <DialogContent
        showCloseButton={!inProgress}
        className="sm:max-w-md"
        onPointerDownOutside={(e) => {
          if (inProgress) e.preventDefault()
        }}
        onEscapeKeyDown={(e) => {
          if (inProgress) e.preventDefault()
        }}
        onInteractOutside={(e) => {
          if (inProgress) e.preventDefault()
        }}
      >
        <DialogHeader>
          <DialogTitle>Updating Mizpah</DialogTitle>
          <DialogDescription>
            Installing v{expectedLatest}. The log buffer will clear when the hub
            restarts.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3 px-1 py-2">
          <p className="text-sm text-foreground">{step}</p>
          <div
            className="h-2 w-full overflow-hidden rounded-full bg-muted"
            role="progressbar"
            aria-label="Update progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={progressPct}
          >
            <div
              className={cn(
                "h-full rounded-full bg-primary transition-[width] duration-300",
                phase === "error" && "bg-destructive"
              )}
              style={{ width: `${progressPct}%` }}
            />
          </div>
          {error ? (
            <p className="text-xs text-destructive whitespace-pre-wrap">{error}</p>
          ) : null}
        </div>

        {phase === "error" ? (
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Close
            </Button>
          </DialogFooter>
        ) : null}
      </DialogContent>
    </Dialog>
  )
}
