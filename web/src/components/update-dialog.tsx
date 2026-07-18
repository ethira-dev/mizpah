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
import { WaveText } from "@/components/ui/wave-text"
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

type Phase = "confirm" | "running" | "waiting" | "error" | "done"

export function UpdateDialog({
  open,
  onOpenChange,
  expectedLatest,
}: UpdateDialogProps) {
  const [step, setStep] = useState("Starting update…")
  const [progress, setProgress] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [phase, setPhase] = useState<Phase>("confirm")
  /** Incremented on confirm so the stream effect runs once without re-aborting on waiting. */
  const [runToken, setRunToken] = useState(0)
  const expectedRef = useRef(expectedLatest)

  useEffect(() => {
    expectedRef.current = expectedLatest
  }, [expectedLatest])

  useEffect(() => {
    if (!open || runToken === 0) return

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
            if (status.installedVersion === expectedRef.current) {
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
  }, [open, runToken])

  const inProgress = phase === "running" || phase === "waiting"
  const canDismiss = phase === "confirm" || phase === "error"
  const progressPct = Math.round(progress * 100)

  function confirmUpdate() {
    setError(null)
    setProgress(0)
    setStep("Starting update…")
    setPhase("running")
    setRunToken((n) => n + 1)
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!canDismiss) return
        onOpenChange(next)
      }}
    >
      <DialogContent
        showCloseButton={canDismiss}
        className="sm:max-w-md"
        onPointerDownOutside={(e) => {
          if (!canDismiss) e.preventDefault()
        }}
        onEscapeKeyDown={(e) => {
          if (!canDismiss) e.preventDefault()
        }}
        onInteractOutside={(e) => {
          if (!canDismiss) e.preventDefault()
        }}
      >
        <DialogHeader>
          <DialogTitle>
            {phase === "confirm" ? "Update Mizpah?" : "Updating Mizpah"}
          </DialogTitle>
          <DialogDescription>
            {phase === "confirm"
              ? `Install v${expectedLatest}? Your log buffer is saved across the restart.`
              : `Installing v${expectedLatest}. Your log buffer is saved across the restart.`}
          </DialogDescription>
        </DialogHeader>

        {phase !== "confirm" ? (
          <div className="space-y-3 px-1 py-2">
            {inProgress ? (
              <p className="text-sm text-foreground">
                <WaveText className="text-sm text-foreground">{step}</WaveText>
              </p>
            ) : (
              <p className="text-sm text-foreground">{step}</p>
            )}
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
                  "relative h-full overflow-hidden rounded-full transition-[width] duration-300",
                  phase === "error" ? "bg-destructive" : "bg-primary"
                )}
                style={{ width: `${progressPct}%` }}
              >
                {inProgress ? (
                  <div
                    aria-hidden="true"
                    className="absolute inset-0 motion-safe:animate-[progress-shimmer_1.4s_linear_infinite]"
                    style={{
                      backgroundImage:
                        "linear-gradient(90deg, transparent 0%, color-mix(in oklch, var(--primary-foreground) 35%, transparent) 50%, transparent 100%)",
                      backgroundSize: "200% 100%",
                    }}
                  />
                ) : null}
              </div>
            </div>
            {error ? (
              <p className="text-xs text-destructive whitespace-pre-wrap">
                {error}
              </p>
            ) : null}
          </div>
        ) : null}

        {phase === "confirm" ? (
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="button" onClick={confirmUpdate}>
              Update
            </Button>
          </DialogFooter>
        ) : null}

        {phase === "error" ? (
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Close
            </Button>
          </DialogFooter>
        ) : null}
      </DialogContent>
    </Dialog>
  )
}
