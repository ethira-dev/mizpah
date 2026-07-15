import { Check, ChevronLeft, Search, X } from "lucide-react"
import { useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandItem,
  CommandList,
} from "@/components/ui/command"
import { Input } from "@/components/ui/input"
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
} from "@/components/ui/popover"
import { cn } from "@/lib/utils"
import type { FilterChip, PropertyInfo } from "@/lib/types"

const STANDARD_LEVELS = ["error", "warn", "info", "debug", "trace"]
const LEVEL_SOURCE_PATHS = ["level", "severity", "lvl"]

type FilterSearchProps = {
  properties: PropertyInfo[]
  services: string[]
  filters: FilterChip[]
  onChange: (filters: FilterChip[]) => void
}

type Stage =
  | { kind: "closed" }
  | { kind: "properties"; query: string }
  | { kind: "values"; path: string; selected: Set<string>; custom: string }

function chipLabel(f: FilterChip): string {
  if (f.op === "in" && f.values?.length) {
    return `${f.path} = ${f.values.join(", ")}`
  }
  if (f.value != null && f.value !== "") {
    return `${f.path} = ${f.value}`
  }
  return `${f.path} =`
}

function chipVariant(path: string): "default" | "secondary" | "outline" {
  if (path === "service") return "default"
  if (path === "level") return "outline"
  return "secondary"
}

function upsertFilter(filters: FilterChip[], chip: FilterChip): FilterChip[] {
  const without = filters.filter((f) => f.path !== chip.path)
  return [...without, chip]
}

function uniqueStrings(values: string[]): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const v of values) {
    if (seen.has(v)) continue
    seen.add(v)
    out.push(v)
  }
  return out
}

function highlightInput(text: string): ReactNode {
  if (!text) return null
  const at = text.indexOf("@")
  if (at === -1) {
    return <span className="text-muted-foreground">{text}</span>
  }
  const before = text.slice(0, at)
  const afterAt = text.slice(at + 1)
  const tokenEnd = afterAt.search(/[\s@]/)
  const prop =
    tokenEnd === -1 ? afterAt : afterAt.slice(0, tokenEnd)
  const rest = tokenEnd === -1 ? "" : afterAt.slice(tokenEnd)
  return (
    <>
      {before ? <span className="text-muted-foreground">{before}</span> : null}
      <span className="text-primary/70">@</span>
      <span className="text-primary">{prop}</span>
      {rest ? <span className="text-muted-foreground">{rest}</span> : null}
    </>
  )
}

export function FilterSearch({
  properties,
  services,
  filters,
  onChange,
}: FilterSearchProps) {
  const [input, setInput] = useState("")
  const [stage, setStage] = useState<Stage>({ kind: "closed" })
  const inputRef = useRef<HTMLInputElement>(null)
  const containerRef = useRef<HTMLDivElement>(null)

  const open = stage.kind !== "closed"

  const { universal, discovered } = useMemo(() => {
    const levelSamples = uniqueStrings([
      ...STANDARD_LEVELS,
      ...properties
        .filter((p) => LEVEL_SOURCE_PATHS.includes(p.path))
        .flatMap((p) => p.sampleValues),
    ])

    const universalProps: PropertyInfo[] = [
      {
        path: "service",
        types: ["string"],
        sampleValues: [...services],
      },
      {
        path: "level",
        types: ["string"],
        sampleValues: levelSamples,
      },
    ]

    const reserved = new Set(["service", "level"])
    const rest = properties.filter((p) => !reserved.has(p.path))
    return { universal: universalProps, discovered: rest }
  }, [properties, services])

  const allProperties = useMemo(
    () => [...universal, ...discovered],
    [universal, discovered]
  )

  const propertyQuery = stage.kind === "properties" ? stage.query : ""
  const filteredUniversal = useMemo(() => {
    const q = propertyQuery.toLowerCase()
    if (!q) return universal
    return universal.filter((p) => p.path.toLowerCase().includes(q))
  }, [universal, propertyQuery])

  const filteredDiscovered = useMemo(() => {
    const q = propertyQuery.toLowerCase()
    if (!q) return discovered
    return discovered.filter((p) => p.path.toLowerCase().includes(q))
  }, [discovered, propertyQuery])

  const activeProperty =
    stage.kind === "values"
      ? allProperties.find((p) => p.path === stage.path)
      : undefined

  function openPropertyMenu(value: string) {
    const at = value.lastIndexOf("@")
    if (at === -1) {
      setStage({ kind: "closed" })
      return
    }
    setStage({ kind: "properties", query: value.slice(at + 1) })
  }

  function handleInputChange(value: string) {
    setInput(value)
    if (stage.kind === "values") return
    if (value.includes("@")) {
      openPropertyMenu(value)
    } else {
      setStage({ kind: "closed" })
    }
  }

  function selectProperty(path: string) {
    const existing = filters.find((f) => f.path === path)
    const selected = new Set<string>()
    if (existing?.op === "in" && existing.values) {
      for (const v of existing.values) selected.add(v)
    } else if (existing?.value) {
      selected.add(existing.value)
    }
    setStage({ kind: "values", path, selected, custom: "" })
    setInput("")
  }

  function toggleValue(value: string) {
    if (stage.kind !== "values") return
    const next = new Set(stage.selected)
    if (next.has(value)) next.delete(value)
    else next.add(value)
    setStage({ ...stage, selected: next })
  }

  function applyValues() {
    if (stage.kind !== "values") return
    const values = Array.from(stage.selected)
    if (values.length === 0) {
      onChange(filters.filter((f) => f.path !== stage.path))
    } else if (values.length === 1) {
      onChange(
        upsertFilter(filters, {
          path: stage.path,
          op: "eq",
          value: values[0],
        })
      )
    } else {
      onChange(
        upsertFilter(filters, {
          path: stage.path,
          op: "in",
          values,
        })
      )
    }
    setStage({ kind: "closed" })
    setInput("")
    inputRef.current?.focus()
  }

  function addCustomValue() {
    if (stage.kind !== "values") return
    const v = stage.custom.trim()
    if (!v) return
    const next = new Set(stage.selected)
    next.add(v)
    setStage({ ...stage, selected: next, custom: "" })
  }

  function removeFilter(path: string) {
    onChange(filters.filter((f) => f.path !== path))
  }

  function handleKeyDown(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Escape") {
      e.preventDefault()
      if (stage.kind === "values") {
        setStage({ kind: "properties", query: "" })
        setInput("@")
      } else {
        setStage({ kind: "closed" })
        setInput("")
      }
      return
    }

    if (e.key === "Backspace" && input === "" && filters.length > 0 && stage.kind !== "values") {
      e.preventDefault()
      const last = filters[filters.length - 1]
      removeFilter(last.path)
      return
    }

    if (e.key === "Enter" && stage.kind === "values") {
      e.preventDefault()
      if (stage.custom.trim()) {
        addCustomValue()
      } else {
        applyValues()
      }
    }
  }

  const valueOptions = useMemo(() => {
    if (!activeProperty) return [] as string[]
    const samples = activeProperty.sampleValues
    if (stage.kind !== "values") return samples
    const extras = Array.from(stage.selected).filter((v) => !samples.includes(v))
    return [...samples, ...extras]
  }, [activeProperty, stage])

  const noMatches =
    filteredUniversal.length === 0 && filteredDiscovered.length === 0

  return (
    <div ref={containerRef} className="flex min-w-0 items-center gap-2">
      <Popover
        open={open}
        onOpenChange={(next) => {
          if (!next) {
            if (stage.kind === "values") {
              applyValues()
            } else {
              setStage({ kind: "closed" })
            }
          }
        }}
      >
        <PopoverAnchor asChild>
          <div
            className={cn(
              "flex min-h-8 min-w-0 flex-1 flex-wrap items-center gap-1.5 rounded-lg border border-input bg-transparent px-2 py-1",
              "focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50"
            )}
            onClick={() => inputRef.current?.focus()}
          >
            <Search className="size-3.5 shrink-0 text-muted-foreground" />

            {filters.map((f) => (
              <Badge
                key={f.path}
                variant={chipVariant(f.path)}
                className="h-5 gap-1 rounded-md font-mono text-[0.7rem]"
              >
                <span>{chipLabel(f)}</span>
                <button
                  type="button"
                  className="rounded-sm opacity-70 hover:opacity-100 focus-visible:ring-2 focus-visible:ring-ring"
                  onClick={(e) => {
                    e.stopPropagation()
                    removeFilter(f.path)
                  }}
                >
                  <X className="size-3" />
                  <span className="sr-only">Remove filter</span>
                </button>
              </Badge>
            ))}

            <div className="relative min-w-[8rem] flex-1">
              <div
                aria-hidden
                className="pointer-events-none absolute inset-0 flex items-center overflow-hidden font-mono text-xs whitespace-pre"
              >
                {highlightInput(input)}
              </div>
              <input
                ref={inputRef}
                value={input}
                onChange={(e) => handleInputChange(e.target.value)}
                onKeyDown={handleKeyDown}
                onFocus={() => {
                  if (input.includes("@") && stage.kind === "closed") {
                    openPropertyMenu(input)
                  }
                }}
                placeholder={filters.length === 0 ? "Filter with @property…" : ""}
                className={cn(
                  "relative h-5 w-full min-w-0 bg-transparent font-mono text-xs text-transparent caret-foreground outline-none",
                  "placeholder:text-muted-foreground"
                )}
                spellCheck={false}
                autoComplete="off"
              />
            </div>
          </div>
        </PopoverAnchor>

        <PopoverContent
          className="w-80 p-0"
          align="start"
          onOpenAutoFocus={(e) => e.preventDefault()}
          onInteractOutside={(e) => {
            if (containerRef.current?.contains(e.target as Node)) {
              e.preventDefault()
            }
          }}
        >
          {stage.kind === "properties" && (
            <Command shouldFilter={false}>
              <CommandList>
                <CommandEmpty>
                  {noMatches ? "No matching properties" : null}
                </CommandEmpty>
                {filteredUniversal.length > 0 && (
                  <CommandGroup heading="Universal">
                    {filteredUniversal.map((p) => (
                      <CommandItem
                        key={p.path}
                        value={p.path}
                        className="font-mono text-xs"
                        onSelect={() => selectProperty(p.path)}
                      >
                        {p.path}
                      </CommandItem>
                    ))}
                  </CommandGroup>
                )}
                {filteredDiscovered.length > 0 && (
                  <CommandGroup heading="Properties">
                    {filteredDiscovered.map((p) => (
                      <CommandItem
                        key={p.path}
                        value={p.path}
                        className="font-mono text-xs"
                        onSelect={() => selectProperty(p.path)}
                      >
                        {p.path}
                      </CommandItem>
                    ))}
                  </CommandGroup>
                )}
              </CommandList>
            </Command>
          )}

          {stage.kind === "values" && (
            <div className="flex flex-col">
              <div className="flex items-center gap-1 border-b px-2 py-1.5">
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  onClick={() => {
                    setStage({ kind: "properties", query: "" })
                    setInput("@")
                    inputRef.current?.focus()
                  }}
                >
                  <ChevronLeft className="size-3.5" />
                </Button>
                <span className="truncate font-mono text-xs font-medium">{stage.path}</span>
                <span className="ml-auto text-[0.7rem] text-muted-foreground">equals</span>
              </div>

              <div className="max-h-56 overflow-y-auto p-1">
                {valueOptions.length === 0 ? (
                  <p className="px-2 py-4 text-center text-xs text-muted-foreground">
                    No sample values — type a custom value below
                  </p>
                ) : (
                  valueOptions.map((v) => {
                    const checked = stage.selected.has(v)
                    return (
                      <button
                        key={v}
                        type="button"
                        className={cn(
                          "flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left font-mono text-xs outline-none hover:bg-muted",
                          checked && "bg-muted"
                        )}
                        onClick={() => toggleValue(v)}
                      >
                        <span
                          className={cn(
                            "flex size-3.5 shrink-0 items-center justify-center rounded-[3px] border border-input",
                            checked && "border-primary bg-primary text-primary-foreground"
                          )}
                        >
                          {checked ? <Check className="size-2.5" /> : null}
                        </span>
                        <span className="truncate">{v}</span>
                      </button>
                    )
                  })
                )}
              </div>

              <div className="flex gap-1.5 border-t p-2">
                <Input
                  value={stage.custom}
                  onChange={(e) => setStage({ ...stage, custom: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault()
                      if (stage.custom.trim()) addCustomValue()
                      else applyValues()
                    }
                  }}
                  placeholder="Custom value…"
                  className="h-7 flex-1 font-mono text-xs"
                />
                <Button type="button" size="sm" onClick={applyValues}>
                  Done
                </Button>
              </div>
            </div>
          )}
        </PopoverContent>
      </Popover>

      {filters.length > 0 && (
        <Button type="button" variant="ghost" size="sm" onClick={() => onChange([])}>
          Clear
        </Button>
      )}
    </div>
  )
}
