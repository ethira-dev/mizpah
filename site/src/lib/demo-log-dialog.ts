import type { DemoLevel, DemoLog } from "./demo-logs"

const CONTEXT_RADIUS = 8
const STYLE_ID = "mzp-demo-dialog-styles"
const ROOT_ID = "mzp-demo-dialog"

function escapeHtml(s: string) {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
}

function formatRelativeTime(iso: number, now = Date.now()): string {
  const diffSec = Math.round((now - iso) / 1000)
  const abs = Math.abs(diffSec)
  if (abs < 5) return "just now"
  if (abs < 60) return `${abs}s ago`
  if (abs < 3600) return `${Math.floor(abs / 60)}m ago`
  if (abs < 86400) return `${Math.floor(abs / 3600)}h ago`
  return `${Math.floor(abs / 86400)}d ago`
}

function treeHtml(value: unknown, key?: string, depth = 0): string {
  const indent = depth * 14
  const keyHtml =
    key !== undefined
      ? `<span class="mzp-dlg__jk">${escapeHtml(key)}</span><span class="mzp-dlg__jp">: </span>`
      : ""

  if (value === null) {
    return `<div class="mzp-dlg__jrow" style="padding-left:${indent}px">${keyHtml}<span class="mzp-dlg__jnull">null</span></div>`
  }
  if (typeof value === "string") {
    return `<div class="mzp-dlg__jrow" style="padding-left:${indent}px">${keyHtml}<span class="mzp-dlg__jstr">"${escapeHtml(value)}"</span></div>`
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return `<div class="mzp-dlg__jrow" style="padding-left:${indent}px">${keyHtml}<span class="mzp-dlg__jprim">${escapeHtml(String(value))}</span></div>`
  }
  if (Array.isArray(value)) {
    if (value.length === 0) {
      return `<div class="mzp-dlg__jrow" style="padding-left:${indent}px">${keyHtml}<span class="mzp-dlg__jp">[]</span></div>`
    }
    const kids = value.map((v, i) => treeHtml(v, String(i), depth + 1)).join("")
    return `<div class="mzp-dlg__jrow" style="padding-left:${indent}px">${keyHtml}<span class="mzp-dlg__jp">[</span></div>${kids}<div class="mzp-dlg__jrow" style="padding-left:${indent}px"><span class="mzp-dlg__jp">]</span></div>`
  }
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
    if (entries.length === 0) {
      return `<div class="mzp-dlg__jrow" style="padding-left:${indent}px">${keyHtml}<span class="mzp-dlg__jp">{}</span></div>`
    }
    const kids = entries.map(([k, v]) => treeHtml(v, k, depth + 1)).join("")
    return `<div class="mzp-dlg__jrow" style="padding-left:${indent}px">${keyHtml}<span class="mzp-dlg__jp">{</span></div>${kids}<div class="mzp-dlg__jrow" style="padding-left:${indent}px"><span class="mzp-dlg__jp">}</span></div>`
  }
  return `<div class="mzp-dlg__jrow" style="padding-left:${indent}px">${keyHtml}<span class="mzp-dlg__jprim">${escapeHtml(String(value))}</span></div>`
}

function ensureStyles() {
  if (document.getElementById(STYLE_ID)) return
  const style = document.createElement("style")
  style.id = STYLE_ID
  style.textContent = `
.mzp-dlg[hidden] { display: none !important; }
.mzp-dlg {
  position: fixed;
  inset: 0;
  z-index: 80;
  display: grid;
  place-items: center;
  padding: 1rem;
}
.mzp-dlg__overlay {
  position: absolute;
  inset: 0;
  background: oklch(0 0 0 / 55%);
}
.mzp-dlg__panel {
  position: relative;
  z-index: 1;
  display: flex;
  flex-direction: column;
  width: min(96vw, 72rem);
  max-height: 88vh;
  overflow: hidden;
  border-radius: 0.75rem;
  background: oklch(0.18 0.01 160);
  color: oklch(0.985 0 0);
  box-shadow:
    0 0 0 1px oklch(1 0 0 / 10%),
    0 24px 64px -16px oklch(0 0 0 / 70%);
  border-left: 4px solid oklch(1 0 0 / 18%);
}
.mzp-dlg__panel--debug { border-left-color: oklch(0.72 0.04 250); }
.mzp-dlg__panel--info { border-left-color: oklch(0.74 0.1 220); }
.mzp-dlg__panel--warn { border-left-color: oklch(0.78 0.14 75); }
.mzp-dlg__panel--error { border-left-color: oklch(0.76 0.16 8); }
.mzp-dlg__close {
  position: absolute;
  top: 0.5rem;
  right: 0.5rem;
  z-index: 2;
  display: grid;
  place-items: center;
  width: 2rem;
  height: 2rem;
  border: 0;
  border-radius: 0.45rem;
  background: transparent;
  color: oklch(0.72 0 0);
  cursor: pointer;
}
.mzp-dlg__close:hover { background: oklch(1 0 0 / 8%); color: oklch(0.985 0 0); }
.mzp-dlg__header {
  flex-shrink: 0;
  display: flex;
  align-items: flex-start;
  gap: 0.75rem;
  padding: 1rem 3rem 1rem 1.25rem;
  border-bottom: 1px solid oklch(1 0 0 / 10%);
}
.mzp-dlg__header-main { min-width: 0; flex: 1; }
.mzp-dlg__title {
  margin: 0;
  font-family: var(--font-mono);
  font-size: 0.875rem;
  font-weight: 500;
  line-height: 1.35;
  color: oklch(0.985 0 0);
}
.mzp-dlg__meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.35rem 0.5rem;
  margin-top: 0.5rem;
  font-size: 0.75rem;
  color: oklch(0.708 0 0);
}
.mzp-dlg__meta-sep { opacity: 0.4; }
.mzp-dlg__id {
  border: 0;
  background: transparent;
  padding: 0;
  font: inherit;
  font-family: var(--font-mono);
  font-variant-numeric: tabular-nums;
  color: oklch(0.708 0 0);
  cursor: pointer;
}
.mzp-dlg__id:hover { color: oklch(0.985 0 0); }
.mzp-dlg__nav {
  display: flex;
  flex-shrink: 0;
  gap: 0.25rem;
  padding-top: 0.15rem;
}
.mzp-dlg__navbtn {
  display: grid;
  place-items: center;
  width: 2rem;
  height: 2rem;
  border-radius: 0.45rem;
  border: 1px solid oklch(1 0 0 / 12%);
  background: transparent;
  color: oklch(0.92 0 0);
  cursor: pointer;
}
.mzp-dlg__navbtn:hover:not(:disabled) { background: oklch(1 0 0 / 6%); }
.mzp-dlg__navbtn:disabled { opacity: 0.35; cursor: default; }
.mzp-dlg__body {
  display: flex;
  min-height: 0;
  flex: 1;
  flex-direction: column;
}
@media (min-width: 1024px) {
  .mzp-dlg__body { flex-direction: row; }
}
.mzp-dlg__json {
  display: flex;
  min-width: 0;
  min-height: 12rem;
  flex: 1;
  flex-direction: column;
  border-bottom: 1px solid oklch(1 0 0 / 10%);
}
@media (min-width: 1024px) {
  .mzp-dlg__json {
    border-bottom: 0;
    border-right: 1px solid oklch(1 0 0 / 10%);
  }
}
.mzp-dlg__toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.4rem;
  padding: 0.55rem 0.85rem;
  border-bottom: 1px solid oklch(1 0 0 / 8%);
}
.mzp-dlg__mode {
  display: inline-flex;
  border: 1px solid oklch(1 0 0 / 12%);
  border-radius: 0.4rem;
  overflow: hidden;
}
.mzp-dlg__modebtn {
  border: 0;
  background: transparent;
  color: oklch(0.7 0 0);
  font-family: var(--font-mono);
  font-size: 0.7rem;
  padding: 0.25rem 0.55rem;
  cursor: pointer;
}
.mzp-dlg__modebtn[aria-pressed="true"] {
  background: oklch(1 0 0 / 8%);
  color: oklch(0.985 0 0);
}
.mzp-dlg__toolbtn {
  display: inline-flex;
  align-items: center;
  gap: 0.3rem;
  height: 1.7rem;
  padding: 0 0.55rem;
  border-radius: 0.4rem;
  border: 1px solid oklch(1 0 0 / 12%);
  background: transparent;
  color: oklch(0.92 0 0);
  font-size: 0.7rem;
  cursor: pointer;
}
.mzp-dlg__toolbtn:hover { background: oklch(1 0 0 / 6%); }
.mzp-dlg__json-view {
  min-height: 0;
  flex: 1;
  overflow: auto;
  padding: 0.75rem 1rem;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  line-height: 1.45;
}
.mzp-dlg__jrow { white-space: pre-wrap; word-break: break-word; }
.mzp-dlg__jk { color: oklch(0.78 0.08 220); }
.mzp-dlg__jp { color: oklch(0.65 0 0); }
.mzp-dlg__jstr { color: oklch(0.78 0.12 145); }
.mzp-dlg__jprim { color: oklch(0.78 0.12 75); }
.mzp-dlg__jnull { color: oklch(0.65 0 0); font-style: italic; }
.mzp-dlg__raw {
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
  color: oklch(0.9 0 0);
}
.mzp-dlg__aside {
  display: flex;
  width: 100%;
  max-height: 40vh;
  flex-shrink: 0;
  flex-direction: column;
}
@media (min-width: 1024px) {
  .mzp-dlg__aside {
    width: 280px;
    max-height: none;
  }
}
.mzp-dlg__aside-head {
  flex-shrink: 0;
  padding: 0.5rem 0.75rem;
  border-bottom: 1px solid oklch(1 0 0 / 10%);
}
.mzp-dlg__aside-title {
  margin: 0;
  font-size: 0.75rem;
  font-weight: 500;
}
.mzp-dlg__aside-sub {
  margin: 0.15rem 0 0;
  font-size: 0.6875rem;
  color: oklch(0.708 0 0);
}
.mzp-dlg__context {
  min-height: 0;
  flex: 1;
  overflow: auto;
  list-style: none;
  margin: 0;
  padding: 0;
}
.mzp-dlg__context li { border-bottom: 1px solid oklch(1 0 0 / 6%); }
.mzp-dlg__ctxbtn {
  display: flex;
  width: 100%;
  flex-direction: column;
  gap: 0.15rem;
  padding: 0.5rem 0.75rem;
  border: 0;
  background: transparent;
  color: inherit;
  text-align: left;
  cursor: pointer;
}
.mzp-dlg__ctxbtn:hover { background: oklch(0.269 0 0 / 50%); }
.mzp-dlg__ctxbtn[aria-current="true"] { background: oklch(0.269 0 0 / 70%); }
.mzp-dlg__ctx-top {
  display: flex;
  align-items: center;
  gap: 0.35rem;
}
.mzp-dlg__ctx-time {
  font-family: var(--font-mono);
  font-size: 0.625rem;
  font-variant-numeric: tabular-nums;
  color: oklch(0.708 0 0);
}
.mzp-dlg__ctx-id {
  margin-left: auto;
  font-family: var(--font-mono);
  font-size: 0.625rem;
  color: oklch(0.708 0 0 / 70%);
}
.mzp-dlg__ctx-msg {
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  line-height: 1.35;
  color: oklch(0.708 0 0);
}
.mzp-dlg__ctxbtn[aria-current="true"] .mzp-dlg__ctx-msg { color: oklch(0.985 0 0); }
.mzp-dlg__badge {
  display: inline-flex;
  height: 1.25rem;
  align-items: center;
  padding: 0 0.5rem;
  border-radius: 0.375rem;
  font-family: var(--font-mono);
  font-size: 0.75rem;
  font-weight: 500;
  line-height: 1;
  border: 1px solid transparent;
}
.mzp-dlg__badge--svc {
  background: oklch(0.269 0 0);
  color: oklch(0.985 0 0);
}
.mzp-dlg__badge--level {
  text-transform: uppercase;
}
.mzp-dlg__badge--sm {
  height: 1rem;
  padding: 0 0.25rem;
  border-radius: 0.2rem;
  font-size: 0.5625rem;
}
.mzp-dlg__badge--debug { background: oklch(0.72 0.04 250 / 12%); color: oklch(0.72 0.04 250); }
.mzp-dlg__badge--info { background: oklch(0.74 0.1 220 / 12%); color: oklch(0.74 0.1 220); }
.mzp-dlg__badge--warn { background: oklch(0.78 0.14 75 / 12%); color: oklch(0.78 0.14 75); }
.mzp-dlg__badge--error { background: oklch(0.76 0.16 8 / 12%); color: oklch(0.76 0.16 8); }
.mzp-dlg__footer {
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding: 1rem;
  border-top: 1px solid oklch(1 0 0 / 10%);
  background: oklch(0.22 0.01 160 / 50%);
}
.mzp-dlg__footer-row {
  display: flex;
  flex-direction: column-reverse;
  gap: 0.5rem;
}
@media (min-width: 640px) {
  .mzp-dlg__footer-row {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
  }
}
.mzp-dlg__hint {
  margin: 0;
  font-size: 0.6875rem;
  color: oklch(0.708 0 0);
}
.mzp-dlg__actions {
  display: flex;
  flex-direction: column-reverse;
  gap: 0.5rem;
}
@media (min-width: 640px) {
  .mzp-dlg__actions {
    flex-direction: row;
    justify-content: flex-end;
  }
}
.mzp-dlg__action {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.35rem;
  height: 2rem;
  padding: 0 0.75rem;
  border-radius: 0.5rem;
  border: 1px solid oklch(1 0 0 / 12%);
  background: transparent;
  color: oklch(0.985 0 0);
  font-size: 0.8rem;
  cursor: pointer;
}
.mzp-dlg__action:hover { background: oklch(1 0 0 / 6%); }
.mzp-dlg__action--primary {
  border-color: #a9fbc0;
  background: #a9fbc0;
  color: oklch(0.205 0 0);
}
.mzp-dlg__action--primary:hover { filter: brightness(1.05); }
body.mzp-dlg-open { overflow: hidden; }
`
  document.head.appendChild(style)
}

function levelBadge(level: DemoLevel | undefined, sm = false) {
  if (!level) return ""
  return `<span class="mzp-dlg__badge mzp-dlg__badge--level mzp-dlg__badge--${level}${sm ? " mzp-dlg__badge--sm" : ""}">${level}</span>`
}

export type DemoLogDialog = {
  open: (log: DemoLog, entries: DemoLog[], receivedAt: number) => void
  close: () => void
  isOpen: () => boolean
}

export function createDemoLogDialog(opts: {
  onOpenChange?: (open: boolean) => void
}): DemoLogDialog {
  ensureStyles()

  let root = document.getElementById(ROOT_ID)
  if (!root) {
    root = document.createElement("div")
    root.id = ROOT_ID
    root.className = "mzp-dlg"
    root.hidden = true
    root.innerHTML = `
      <div class="mzp-dlg__overlay" data-dlg-overlay></div>
      <div class="mzp-dlg__panel" data-dlg-panel role="dialog" aria-modal="true" aria-labelledby="mzp-dlg-title">
        <button type="button" class="mzp-dlg__close" data-dlg-close aria-label="Close">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
        </button>
        <div class="mzp-dlg__header">
          <div class="mzp-dlg__header-main">
            <h2 class="mzp-dlg__title" id="mzp-dlg-title" data-dlg-title></h2>
            <div class="mzp-dlg__meta" data-dlg-meta></div>
          </div>
          <div class="mzp-dlg__nav">
            <button type="button" class="mzp-dlg__navbtn" data-dlg-prev aria-label="Previous log">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m18 15-6-6-6 6"/></svg>
            </button>
            <button type="button" class="mzp-dlg__navbtn" data-dlg-next aria-label="Next log">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m6 9 6 6 6-6"/></svg>
            </button>
          </div>
        </div>
        <div class="mzp-dlg__body">
          <div class="mzp-dlg__json">
            <div class="mzp-dlg__toolbar">
              <button type="button" class="mzp-dlg__toolbtn" data-dlg-copy>Copy JSON</button>
              <div class="mzp-dlg__mode">
                <button type="button" class="mzp-dlg__modebtn" data-dlg-mode="tree" aria-pressed="true">Tree</button>
                <button type="button" class="mzp-dlg__modebtn" data-dlg-mode="raw" aria-pressed="false">Raw</button>
              </div>
            </div>
            <div class="mzp-dlg__json-view" data-dlg-json></div>
          </div>
          <aside class="mzp-dlg__aside">
            <div class="mzp-dlg__aside-head">
              <p class="mzp-dlg__aside-title">Around this log</p>
              <p class="mzp-dlg__aside-sub" data-dlg-around-sub></p>
            </div>
            <ul class="mzp-dlg__context" data-dlg-context></ul>
          </aside>
        </div>
        <div class="mzp-dlg__footer">
          <div class="mzp-dlg__footer-row">
            <p class="mzp-dlg__hint">↑↓ navigate · t/r view · demo only</p>
            <div class="mzp-dlg__actions">
              <button type="button" class="mzp-dlg__action" data-dlg-claude>
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 3l1.5 5.5L19 10l-5.5 1.5L12 17l-1.5-5.5L5 10l5.5-1.5L12 3z"/></svg>
                Check with Claude
              </button>
              <button type="button" class="mzp-dlg__action mzp-dlg__action--primary" data-dlg-cursor>
                Check with Cursor
              </button>
            </div>
          </div>
        </div>
      </div>
    `
    document.body.appendChild(root)
  }

  const panel = root.querySelector<HTMLElement>("[data-dlg-panel]")!
  const titleEl = root.querySelector<HTMLElement>("[data-dlg-title]")!
  const metaEl = root.querySelector<HTMLElement>("[data-dlg-meta]")!
  const jsonEl = root.querySelector<HTMLElement>("[data-dlg-json]")!
  const contextEl = root.querySelector<HTMLElement>("[data-dlg-context]")!
  const aroundSub = root.querySelector<HTMLElement>("[data-dlg-around-sub]")!
  const prevBtn = root.querySelector<HTMLButtonElement>("[data-dlg-prev]")!
  const nextBtn = root.querySelector<HTMLButtonElement>("[data-dlg-next]")!
  const copyBtn = root.querySelector<HTMLButtonElement>("[data-dlg-copy]")!
  const modeBtns = [
    ...root.querySelectorAll<HTMLButtonElement>("[data-dlg-mode]"),
  ]

  let entries: DemoLog[] = []
  let index = -1
  let receivedAtById = new Map<number, number>()
  let mode: "tree" | "raw" = "tree"
  let openState = false

  function receivedAt(log: DemoLog) {
    return receivedAtById.get(log.id) ?? Date.now()
  }

  function renderJson(log: DemoLog) {
    if (mode === "raw") {
      jsonEl.innerHTML = `<pre class="mzp-dlg__raw">${escapeHtml(JSON.stringify(log.data, null, 2))}</pre>`
      return
    }
    jsonEl.innerHTML = Object.entries(log.data)
      .map(([k, v]) => treeHtml(v, k, 0))
      .join("")
  }

  function renderContext(log: DemoLog) {
    const start = Math.max(0, index - CONTEXT_RADIUS)
    const end = Math.min(entries.length, index + CONTEXT_RADIUS + 1)
    const slice = entries.slice(start, end)
    aroundSub.textContent = `${index + 1} of ${entries.length} in current view`
    contextEl.innerHTML = slice
      .map((neighbor) => {
        const isCurrent = neighbor.id === log.id
        const t = new Date(receivedAt(neighbor)).toLocaleTimeString()
        return `<li>
          <button type="button" class="mzp-dlg__ctxbtn" data-dlg-pick="${neighbor.id}" ${isCurrent ? 'aria-current="true"' : ""}>
            <div class="mzp-dlg__ctx-top">
              <span class="mzp-dlg__ctx-time">${escapeHtml(t)}</span>
              ${levelBadge(neighbor.level, true)}
              <span class="mzp-dlg__ctx-id">#${neighbor.id}</span>
            </div>
            <span class="mzp-dlg__ctx-msg">${escapeHtml(neighbor.msg)}</span>
          </button>
        </li>`
      })
      .join("")
    contextEl
      .querySelector<HTMLElement>('[aria-current="true"]')
      ?.scrollIntoView({ block: "nearest" })
  }

  function render() {
    const log = entries[index]
    if (!log) return
    const at = receivedAt(log)
    titleEl.textContent = log.msg
    metaEl.innerHTML = `
      <span class="tabular-nums">${escapeHtml(new Date(at).toLocaleString())}</span>
      <span class="mzp-dlg__meta-sep">·</span>
      <span class="tabular-nums">${escapeHtml(formatRelativeTime(at))}</span>
      <span class="mzp-dlg__badge mzp-dlg__badge--svc">${escapeHtml(log.service)}</span>
      ${levelBadge(log.level)}
      <button type="button" class="mzp-dlg__id" data-dlg-copy-id>#${log.id}</button>
    `
    panel.className = `mzp-dlg__panel${log.level ? ` mzp-dlg__panel--${log.level}` : ""}`
    prevBtn.disabled = index <= 0
    nextBtn.disabled = index < 0 || index >= entries.length - 1
    modeBtns.forEach((btn) => {
      btn.setAttribute(
        "aria-pressed",
        btn.dataset.dlgMode === mode ? "true" : "false",
      )
    })
    renderJson(log)
    renderContext(log)
  }

  function selectIndex(next: number) {
    if (next < 0 || next >= entries.length) return
    index = next
    render()
  }

  function close() {
    if (!openState) return
    openState = false
    root!.hidden = true
    document.body.classList.remove("mzp-dlg-open")
    opts.onOpenChange?.(false)
  }

  function open(log: DemoLog, list: DemoLog[], at: number) {
    entries = list
    receivedAtById.set(log.id, at)
    // seed neighbors with staggered times if missing
    list.forEach((item, i) => {
      if (!receivedAtById.has(item.id)) {
        receivedAtById.set(item.id, at - (list.indexOf(log) - i) * 4000)
      }
    })
    index = list.findIndex((e) => e.id === log.id)
    if (index < 0) {
      entries = [log]
      index = 0
    }
    mode = "tree"
    openState = true
    root!.hidden = false
    document.body.classList.add("mzp-dlg-open")
    render()
    opts.onOpenChange?.(true)
  }

  root.querySelector("[data-dlg-overlay]")?.addEventListener("click", close)
  root.querySelector("[data-dlg-close]")?.addEventListener("click", close)
  prevBtn.addEventListener("click", () => selectIndex(index - 1))
  nextBtn.addEventListener("click", () => selectIndex(index + 1))
  copyBtn.addEventListener("click", async () => {
    const log = entries[index]
    if (!log) return
    try {
      await navigator.clipboard.writeText(JSON.stringify(log.data, null, 2))
      copyBtn.textContent = "Copied"
      window.setTimeout(() => {
        copyBtn.textContent = "Copy JSON"
      }, 1200)
    } catch {
      /* ignore */
    }
  })
  modeBtns.forEach((btn) => {
    btn.addEventListener("click", () => {
      mode = (btn.dataset.dlgMode as "tree" | "raw") ?? "tree"
      render()
    })
  })
  contextEl.addEventListener("click", (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>("[data-dlg-pick]")
    if (!btn) return
    const id = Number(btn.dataset.dlgPick)
    const next = entries.findIndex((e) => e.id === id)
    if (next >= 0) selectIndex(next)
  })
  metaEl.addEventListener("click", async (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>("[data-dlg-copy-id]")
    if (!btn || !entries[index]) return
    try {
      await navigator.clipboard.writeText(String(entries[index]!.id))
      btn.textContent = `#${entries[index]!.id} copied`
      window.setTimeout(() => {
        if (entries[index]) btn.textContent = `#${entries[index]!.id}`
      }, 1200)
    } catch {
      /* ignore */
    }
  })

  window.addEventListener("keydown", (e) => {
    if (!openState) return
    if (e.key === "Escape") {
      e.preventDefault()
      close()
      return
    }
    if (e.key === "ArrowUp" || e.key === "k") {
      e.preventDefault()
      selectIndex(index - 1)
    } else if (e.key === "ArrowDown" || e.key === "j") {
      e.preventDefault()
      selectIndex(index + 1)
    } else if (e.key === "t") {
      mode = "tree"
      render()
    } else if (e.key === "r") {
      mode = "raw"
      render()
    }
  })

  return {
    open,
    close,
    isOpen: () => openState,
  }
}
