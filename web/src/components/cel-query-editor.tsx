import { CircleAlert, Search, X } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import CodeMirror from "@uiw/react-codemirror"

import { celSyntaxHint, createCelExtensions } from "@/lib/cel-lang"
import type { PropertyInfo } from "@/lib/types"
import { cn } from "@/lib/utils"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"

type CelQueryEditorProps = {
  value: string
  onChange: (value: string) => void
  properties: PropertyInfo[]
  error?: string | null
}

export function CelQueryEditor({
  value,
  onChange,
  properties,
  error,
}: CelQueryEditorProps) {
  const [draft, setDraft] = useState(value)
  const hint = useMemo(() => celSyntaxHint(draft), [draft])
  const displayError = error ?? hint

  useEffect(() => {
    setDraft(value)
  }, [value])

  useEffect(() => {
    if (draft === value) return
    const id = window.setTimeout(() => onChange(draft), 250)
    return () => window.clearTimeout(id)
  }, [draft, value, onChange])

  const paths = useMemo(
    () => properties.map((p) => p.path),
    [properties]
  )

  const extensions = useMemo(
    () =>
      createCelExtensions({
        paths,
        placeholder: 'CEL filter — level == "error" && msg.contains("timeout")',
      }),
    [paths]
  )

  return (
    <div className="min-w-0 flex-1">
      <div
        className={cn(
          "flex min-h-8 min-w-0 items-center gap-2 rounded-lg border border-input bg-transparent px-2 py-1",
          "focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50",
          displayError &&
            "border-destructive focus-within:border-destructive focus-within:ring-destructive/30"
        )}
      >
        <Search className="size-3.5 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1 overflow-hidden">
          <CodeMirror
            value={draft}
            height="24px"
            basicSetup={false}
            theme="none"
            extensions={extensions}
            onChange={setDraft}
            className="cm-cel-query [&_.cm-editor]:bg-transparent [&_.cm-content]:bg-transparent"
          />
        </div>
        {displayError ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                className="shrink-0 rounded-sm p-0.5 text-destructive focus-visible:ring-2 focus-visible:ring-ring"
                aria-label={displayError}
              >
                <CircleAlert className="size-3.5" />
              </button>
            </TooltipTrigger>
            <TooltipContent side="bottom" className="max-w-sm font-mono">
              {displayError}
            </TooltipContent>
          </Tooltip>
        ) : null}
        {draft ? (
          <button
            type="button"
            className="rounded-sm p-0.5 text-muted-foreground opacity-70 hover:opacity-100 focus-visible:ring-2 focus-visible:ring-ring"
            onClick={() => {
              setDraft("")
              onChange("")
            }}
            aria-label="Clear query"
          >
            <X className="size-3.5" />
          </button>
        ) : null}
      </div>
    </div>
  )
}
