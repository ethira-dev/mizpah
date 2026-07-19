import { useEffect, useMemo, useRef, useState } from "react"
import { cn } from "@/lib/utils"

export type FormatIndexEntry = {
  id: string
  title: string
  description: string
  file: string
}

type Props = {
  formats: FormatIndexEntry[]
  base: string
  docsHref: string
}

function JsonNode({
  value,
  name,
  depth = 0,
}: {
  value: unknown
  name?: string
  depth?: number
}) {
  const pad = { paddingLeft: depth * 14 }
  const keyHtml =
    name !== undefined ? (
      <>
        <span className="mzp-fmt__jk">{name}</span>
        <span className="mzp-fmt__jp">: </span>
      </>
    ) : null

  if (value === null) {
    return (
      <div className="mzp-fmt__jrow" style={pad}>
        {keyHtml}
        <span className="mzp-fmt__jnull">null</span>
      </div>
    )
  }
  if (typeof value === "string") {
    return (
      <div className="mzp-fmt__jrow" style={pad}>
        {keyHtml}
        <span className="mzp-fmt__jstr">"{value}"</span>
      </div>
    )
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return (
      <div className="mzp-fmt__jrow" style={pad}>
        {keyHtml}
        <span className="mzp-fmt__jprim">{String(value)}</span>
      </div>
    )
  }
  if (Array.isArray(value)) {
    if (value.length === 0) {
      return (
        <div className="mzp-fmt__jrow" style={pad}>
          {keyHtml}
          <span className="mzp-fmt__jp">[]</span>
        </div>
      )
    }
    return (
      <>
        <div className="mzp-fmt__jrow" style={pad}>
          {keyHtml}
          <span className="mzp-fmt__jp">[</span>
        </div>
        {value.map((v, i) => (
          <JsonNode key={i} value={v} name={String(i)} depth={depth + 1} />
        ))}
        <div className="mzp-fmt__jrow" style={pad}>
          <span className="mzp-fmt__jp">]</span>
        </div>
      </>
    )
  }
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
    if (entries.length === 0) {
      return (
        <div className="mzp-fmt__jrow" style={pad}>
          {keyHtml}
          <span className="mzp-fmt__jp">{"{}"}</span>
        </div>
      )
    }
    return (
      <>
        <div className="mzp-fmt__jrow" style={pad}>
          {keyHtml}
          <span className="mzp-fmt__jp">{"{"}</span>
        </div>
        {entries.map(([k, v]) => (
          <JsonNode key={k} value={v} name={k} depth={depth + 1} />
        ))}
        <div className="mzp-fmt__jrow" style={pad}>
          <span className="mzp-fmt__jp">{"}"}</span>
        </div>
      </>
    )
  }
  return (
    <div className="mzp-fmt__jrow" style={pad}>
      {keyHtml}
      <span className="mzp-fmt__jprim">{String(value)}</span>
    </div>
  )
}

function packUrl(base: string, file: string) {
  return `${base}formats/packs/${file}`.replace(/([^:]\/)\/+/g, "$1")
}

export function FormatsBrowser({ formats, base, docsHref }: Props) {
  const [query, setQuery] = useState("")
  const [selectedId, setSelectedId] = useState<string | null>(() => {
    if (typeof window === "undefined") return formats[0]?.id ?? null
    const hash = window.location.hash.replace(/^#/, "")
    if (hash && formats.some((f) => f.id === hash)) return hash
    return formats[0]?.id ?? null
  })
  const [packJson, setPackJson] = useState<unknown>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const searchRef = useRef<HTMLInputElement>(null)
  const cacheRef = useRef(new Map<string, unknown>())

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return formats
    return formats.filter(
      (f) =>
        f.id.toLowerCase().includes(q) ||
        f.title.toLowerCase().includes(q) ||
        f.description.toLowerCase().includes(q),
    )
  }, [formats, query])

  const selected = useMemo(
    () => formats.find((f) => f.id === selectedId) ?? null,
    [formats, selectedId],
  )

  useEffect(() => {
    queueMicrotask(() => searchRef.current?.focus())
  }, [])

  useEffect(() => {
    const onHash = () => {
      const hash = window.location.hash.replace(/^#/, "")
      if (hash && formats.some((f) => f.id === hash)) {
        setSelectedId(hash)
      }
    }
    window.addEventListener("hashchange", onHash)
    return () => window.removeEventListener("hashchange", onHash)
  }, [formats])

  useEffect(() => {
    if (!selected) return
    const next = `#${selected.id}`
    if (window.location.hash !== next) {
      history.replaceState(null, "", next)
    }
  }, [selected])

  useEffect(() => {
    if (!selected) {
      setPackJson(null)
      setLoadError(null)
      setLoading(false)
      return
    }

    const cached = cacheRef.current.get(selected.file)
    if (cached !== undefined) {
      setPackJson(cached)
      setLoadError(null)
      setLoading(false)
      return
    }

    let cancelled = false
    setLoading(true)
    setLoadError(null)
    setPackJson(null)

    fetch(packUrl(base, selected.file))
      .then(async (res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`)
        return res.json()
      })
      .then((data) => {
        if (cancelled) return
        cacheRef.current.set(selected.file, data)
        setPackJson(data)
        setLoading(false)
      })
      .catch((err: Error) => {
        if (cancelled) return
        setLoadError(err.message || "Failed to load pack")
        setLoading(false)
      })

    return () => {
      cancelled = true
    }
  }, [selected, base])

  useEffect(() => {
    if (selectedId && filtered.some((f) => f.id === selectedId)) return
    setSelectedId(filtered[0]?.id ?? null)
  }, [filtered, selectedId])

  return (
    <div className="mzp-fmt">
      <div className="mzp-fmt__body">
        <aside className="mzp-fmt__list-pane">
          <div className="mzp-fmt__search-wrap">
            <input
              ref={searchRef}
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search formats…"
              className="mzp-fmt__search"
              aria-label="Search formats"
            />
            <p className="mzp-fmt__count">
              {filtered.length === formats.length
                ? `${formats.length} packs`
                : `${filtered.length} / ${formats.length}`}
            </p>
          </div>
          <ul className="mzp-fmt__list" role="listbox">
            {filtered.length === 0 ? (
              <li className="px-3 py-6 text-center text-sm text-muted-foreground">
                No formats match
              </li>
            ) : (
              filtered.map((f) => (
                <li key={f.id}>
                  <button
                    type="button"
                    role="option"
                    aria-selected={f.id === selectedId}
                    className={cn(
                      "mzp-fmt__row",
                      f.id === selectedId && "mzp-fmt__row--active",
                    )}
                    onClick={() => setSelectedId(f.id)}
                  >
                    <span className="mzp-fmt__row-title">{f.title}</span>
                    <span className="mzp-fmt__row-id">{f.id}</span>
                  </button>
                </li>
              ))
            )}
          </ul>
        </aside>

        <section className="mzp-fmt__detail">
          {selected ? (
            <>
              <div className="mzp-fmt__detail-head">
                <h2 className="text-base font-medium text-foreground sm:text-lg">
                  {selected.title}
                </h2>
                <code className="mt-1 block font-mono text-xs text-primary/90 sm:text-sm">
                  {selected.id}
                </code>
                {selected.description ? (
                  <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
                    {selected.description}
                  </p>
                ) : null}
              </div>
              <div className="mzp-fmt__json-view">
                {loading ? (
                  <p className="px-1 py-4 text-sm text-muted-foreground">
                    Loading…
                  </p>
                ) : loadError ? (
                  <p className="px-1 py-4 text-sm text-red-400">{loadError}</p>
                ) : packJson != null ? (
                  <JsonNode value={packJson} />
                ) : null}
              </div>
            </>
          ) : (
            <p className="px-4 py-8 text-sm text-muted-foreground">
              Select a format to view its pack definition.
            </p>
          )}
        </section>
      </div>

      <footer className="mzp-fmt__footer">
        <a href={docsHref} className="text-sm text-primary hover:underline">
          Log formats docs →
        </a>
        <span className="font-mono text-[11px] text-muted-foreground">
          crates/mizpah/formats/packs
        </span>
      </footer>
    </div>
  )
}
