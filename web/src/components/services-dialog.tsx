import { Unplug } from "lucide-react"
import { useState } from "react"

import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { ScrollArea } from "@/components/ui/scroll-area"
import { cn } from "@/lib/utils"

type ServicesDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  services: string[]
  blocked: string[]
  serviceCounts: Record<string, number>
  onDisconnectService: (service: string) => void | Promise<void>
  onReconnectService: (service: string) => void | Promise<void>
}

function formatCount(n: number): string {
  if (n > 999) return "999+"
  return String(n)
}

export function ServicesDialog({
  open,
  onOpenChange,
  services,
  blocked,
  serviceCounts,
  onDisconnectService,
  onReconnectService,
}: ServicesDialogProps) {
  const [pending, setPending] = useState<string | null>(null)

  async function run(name: string, action: (s: string) => void | Promise<void>) {
    if (pending) return
    setPending(name)
    try {
      await action(name)
    } finally {
      setPending(null)
    }
  }

  const empty = services.length === 0 && blocked.length === 0

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md" showCloseButton>
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Unplug className="size-4 text-muted-foreground" />
            Services
          </DialogTitle>
          <DialogDescription>
            Disconnect stops ingest and clears buffered logs for that service.
            Reconnect allows ingest again.
          </DialogDescription>
        </DialogHeader>

        <ScrollArea className="max-h-[min(60vh,24rem)]">
          <div className="flex flex-col gap-4 pr-3">
            {empty ? (
              <p className="text-sm text-muted-foreground">
                No services have connected yet.
              </p>
            ) : null}

            {services.length > 0 ? (
              <section className="space-y-2">
                <h3 className="text-[10px] font-medium tracking-wide text-muted-foreground uppercase">
                  Connected
                </h3>
                <ul className="flex flex-col gap-1.5">
                  {services.map((name) => {
                    const count = serviceCounts[name] ?? 0
                    const busy = pending === name
                    return (
                      <li
                        key={name}
                        className={cn(
                          "flex items-center gap-2 rounded-lg border border-border",
                          "bg-muted/40 px-2.5 py-2"
                        )}
                      >
                        <div className="min-w-0 flex-1">
                          <p
                            className="truncate font-mono text-xs text-foreground"
                            title={name}
                          >
                            {name}
                          </p>
                          {count > 0 ? (
                            <p className="font-mono text-[10px] tabular-nums text-muted-foreground">
                              {formatCount(count)} entries
                            </p>
                          ) : null}
                        </div>
                        <Button
                          type="button"
                          variant="outline"
                          size="xs"
                          disabled={busy}
                          onClick={() => void run(name, onDisconnectService)}
                        >
                          Disconnect
                        </Button>
                      </li>
                    )
                  })}
                </ul>
              </section>
            ) : null}

            {blocked.length > 0 ? (
              <section className="space-y-2">
                <h3 className="text-[10px] font-medium tracking-wide text-muted-foreground uppercase">
                  Disconnected
                </h3>
                <ul className="flex flex-col gap-1.5">
                  {blocked.map((name) => {
                    const busy = pending === name
                    return (
                      <li
                        key={name}
                        className={cn(
                          "flex items-center gap-2 rounded-lg border border-dashed border-border",
                          "bg-muted/20 px-2.5 py-2"
                        )}
                      >
                        <p
                          className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground"
                          title={name}
                        >
                          {name}
                        </p>
                        <Button
                          type="button"
                          variant="outline"
                          size="xs"
                          disabled={busy}
                          onClick={() => void run(name, onReconnectService)}
                        >
                          Reconnect
                        </Button>
                      </li>
                    )
                  })}
                </ul>
              </section>
            ) : null}
          </div>
        </ScrollArea>
      </DialogContent>
    </Dialog>
  )
}
