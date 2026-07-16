import { ClaudeHeader } from "@/components/brainless/claude/claude-header"
import { ClaudeMessage } from "@/components/brainless/claude/claude-message"
import { ClaudePrompt } from "@/components/brainless/claude/claude-prompt"
import { ClaudeToolCall } from "@/components/brainless/claude/claude-tool-call"

export function ClaudeMcpSession() {
  return (
    <div className="space-y-3 font-mono text-[13px] leading-[1.6] text-[#c0caf5]">
      <ClaudeHeader
        version="v2.1.206"
        user="lucas"
        model="Opus 4 with high effort · Claude Max"
        org="mizpah demo"
        cwd="~/dev/api"
        tips={[
          "Ask Claude to search the live mizpah buffer over MCP",
          "Keep limits small: search_logs default 20, max 50",
        ]}
        whatsNew={[
          "mizpah MCP: search_logs, get_logs_around, list_properties",
          'CEL filters: level == "error", msg.contains("timeout")',
        ]}
      />

      <div style={{ color: "#4ea96f" }}>
        <span aria-hidden>✓ </span>
        MCP server mizpah connected · search_logs ready
      </div>

      <div className="space-y-3 pt-1">
        <ClaudeMessage role="user">
          what errors did api emit in the last few minutes?
        </ClaudeMessage>

        <ClaudeToolCall
          tool="search_logs"
          arg='service: "api", q: level == "error", limit: 5'
          result="3 matching logs"
          defaultOpen
        >
          {`[
  { "id": 1842, "level": "error", "msg": "POST /api/ingest failed: connection refused" },
  { "id": 1837, "level": "error", "msg": "timeout waiting for redis at 127.0.0.1:6379" },
  { "id": 1829, "level": "error", "msg": "Unhandled exception in JobsController" }
]`}
        </ClaudeToolCall>

        <ClaudeToolCall
          tool="get_logs_around"
          arg="id: 1837, before: 2, after: 2"
          result="5 logs around #1837"
        >
          {`1826  info   redis pool warm
1835  warn   redis latency p99=210ms
1837  error  timeout waiting for redis at 127.0.0.1:6379
1838  info   retry scheduled in 250ms
1840  info   request completed 503`}
        </ClaudeToolCall>

        <ClaudeMessage>
          Three api errors recently. The redis timeout at #1837 is the spike:
          latency warning just before, then a 503 and a retry. The ingest
          connection refused and JobsController exception look separate.
        </ClaudeMessage>
      </div>

      <div className="pt-2">
        <ClaudePrompt
          defaultValue=""
          placeholder="Ask about the buffer…"
          mode="auto"
          effort="high"
        />
      </div>
    </div>
  )
}
