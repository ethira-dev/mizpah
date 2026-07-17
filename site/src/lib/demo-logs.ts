export type DemoSource = "shell" | "browser" | "cursor" | "claude"

export type DemoLevel = "debug" | "info" | "warn" | "error"

export type DemoLog = {
  id: number
  source: DemoSource
  service: string
  /** Omit to match rows that have no level/severity field */
  level?: DemoLevel
  /** Message line; matches summarizeLog() output in the real UI */
  msg: string
  /** Payload shown in the detail dialog JSON explorer */
  data: Record<string, unknown>
}

export const SOURCE_META: Record<
  DemoSource,
  { label: string; command: string; blurb: string; filter: string }
> = {
  shell: {
    label: "shell",
    command: "mzp attach shell",
    blurb: "Tee stdout/stderr from new interactive shells into the hub.",
    filter: 'source == "shell" || has(cmd)',
  },
  browser: {
    label: "browser",
    command: "mzp attach browser --launch",
    blurb: "Forward Chromium console and network via CDP.",
    filter: 'source == "browser"',
  },
  cursor: {
    label: "cursor",
    command: "mzp attach cursor",
    blurb: "Observe-only Cursor agent hooks → same buffer.",
    filter: 'source == "cursor"',
  },
  claude: {
    label: "claude",
    command: "mzp attach claude",
    blurb: "Observe-only Claude Code lifecycle hooks → same buffer.",
    filter: 'source == "claude"',
  },
}

function entry(
  id: number,
  source: DemoSource,
  service: string,
  msg: string,
  data: Record<string, unknown>,
  level?: DemoLevel,
): DemoLog {
  const payload: Record<string, unknown> = { ...data }
  if (level && payload.level == null) payload.level = level
  if (payload.msg == null) payload.msg = msg
  return { id, source, service, level, msg, data: payload }
}

export const DEMO_LOGS: DemoLog[] = [
  entry(1801, "shell", "ttys008", "mizpah hub started at http://127.0.0.1:3149", {
    event: "hub_start",
    url: "http://127.0.0.1:3149",
  }),
  entry(
    1802,
    "shell",
    "/Users/lucas/dev/api",
    "Nest application successfully started",
    { context: "NestFactory", pid: 48210 },
    "info",
  ),
  entry(
    1803,
    "shell",
    "/Users/lucas/dev/api",
    "Mapped {/api/health, GET} route",
    { route: "/api/health", method: "GET" },
    "debug",
  ),
  entry(
    1804,
    "shell",
    "ttys008",
    "deprecated dependency: lodash@4.17.15",
    { package: "lodash", version: "4.17.15" },
    "warn",
  ),
  entry(
    1805,
    "shell",
    "worker",
    "thread panicked at src/jobs.rs:88",
    {
      error: "thread panicked at src/jobs.rs:88",
      thread: "job-worker-3",
      backtrace: ["jobs.rs:88", "runtime.rs:214"],
    },
    "error",
  ),
  entry(
    1806,
    "shell",
    "api",
    "GET /health 200 (12ms)",
    { method: "GET", path: "/health", status: 200, duration_ms: 12 },
    "debug",
  ),
  entry(
    1901,
    "browser",
    "localhost:5173",
    "Uncaught TypeError: Cannot read properties of null (reading 'id')",
    {
      console: { method: "error" },
      stack:
        "TypeError: Cannot read properties of null (reading 'id')\n    at SessionView (App.tsx:42:18)",
      url: "http://localhost:5173/",
    },
    "error",
  ),
  entry(
    1902,
    "browser",
    "localhost:5173",
    "GET /api/session → 200",
    { method: "GET", url: "/api/session", status: 200 },
    "info",
  ),
  entry(
    1903,
    "browser",
    "localhost:5173",
    "POST /api/login → 429 Too Many Requests",
    { method: "POST", url: "/api/login", status: 429 },
    "warn",
  ),
  entry(
    1904,
    "browser",
    "localhost:5173",
    "[vite] hot updated: /src/App.tsx",
    { event: "hmr", path: "/src/App.tsx" },
    "debug",
  ),
  entry(
    1905,
    "browser",
    "localhost:5173",
    "Failed to load resource: net::ERR_CONNECTION_REFUSED",
    {
      error: "net::ERR_CONNECTION_REFUSED",
      url: "http://127.0.0.1:3000/api/ingest",
    },
    "error",
  ),
  entry(
    2001,
    "cursor",
    "cursor",
    'afterShellExecution: rg "timeout" crates/',
    {
      hook: "afterShellExecution",
      command: 'rg "timeout" crates/',
      exitCode: 0,
    },
    "info",
  ),
  entry(
    2002,
    "cursor",
    "cursor",
    "afterFileEdit: crates/mizpah/src/hub.rs",
    { hook: "afterFileEdit", path: "crates/mizpah/src/hub.rs" },
    "info",
  ),
  entry(
    2003,
    "cursor",
    "cursor",
    "afterShellExecution: cargo clippy (exit 1)",
    { hook: "afterShellExecution", command: "cargo clippy", exitCode: 1 },
    "warn",
  ),
  entry(
    2004,
    "cursor",
    "cursor",
    "beforeSubmitPrompt: why is /api/ingest returning 503?",
    {
      hook: "beforeSubmitPrompt",
      prompt: "why is /api/ingest returning 503?",
    },
    "debug",
  ),
  entry(
    2101,
    "claude",
    "agents",
    "PreToolUse · Bash: just test",
    { hook: "PreToolUse", tool: "Bash", input: "just test" },
    "info",
  ),
  entry(
    2102,
    "claude",
    "agents",
    "PostToolUse · Read: web/src/App.tsx",
    { hook: "PostToolUse", tool: "Read", path: "web/src/App.tsx" },
    "info",
  ),
  entry(
    2103,
    "claude",
    "agents",
    "PreToolUse · Bash failed: npm run lint",
    {
      hook: "PreToolUse",
      tool: "Bash",
      input: "npm run lint",
      error: "command failed",
      exitCode: 1,
    },
    "error",
  ),
  entry(
    2104,
    "claude",
    "agents",
    "Stop · session ended (14 tool uses)",
    { hook: "Stop", toolUses: 14 },
    "debug",
  ),
]

export const SOURCES: DemoSource[] = ["shell", "browser", "cursor", "claude"]
