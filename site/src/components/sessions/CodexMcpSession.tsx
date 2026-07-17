import { CodexExec } from "@/components/brainless/codex/codex-exec"
import { CodexHeader } from "@/components/brainless/codex/codex-header"
import { CodexMessage } from "@/components/brainless/codex/codex-message"
import { CodexPrompt } from "@/components/brainless/codex/codex-prompt"

export function CodexMcpSession() {
  return (
    <div className="space-y-3 font-mono text-[13px] leading-[1.6] text-[#ededed]">
      <CodexHeader
        version="v0.132.0"
        model="gpt-5.6-sol low"
        directory="~/dev/api"
      />

      <div style={{ color: "#4ea96f" }}>
        <span aria-hidden>✓ </span>
        MCP mizpah ready
      </div>

      <div className="space-y-3 pt-1">
        <CodexMessage role="user">
          which services are the noisiest right now, and any timeouts?
        </CodexMessage>

        <CodexMessage>
          I&apos;ll check hub stats, then search the buffer for timeouts.
        </CodexMessage>

        <div className="space-y-1">
          <CodexExec command="mcp__mizpah__get_stats" result="→ ok (TOON)" defaultOpen>
            {`count: 18420
approxBytes: 134217728
maxBytes: 1073741824
services[4]{name,count}:
  api,9120
  worker,4102
  chrome,3188
  cursor,2010`}
          </CodexExec>
          <CodexExec
            command='mcp__mizpah__search_logs q: msg.contains("timeout") limit: 4'
            result="→ 4 hits (TOON)"
            defaultOpen
          >
            {`entries[4]:
  - id: 1837
    service: api
    data:
      level: error
      msg: "timeout waiting for redis"
  - id: 1760
    service: worker
    data:
      level: warn
      msg: "job lease timeout after 30s"
  - id: 1694
    service: api
    data:
      level: error
      msg: "upstream timeout GET /billing"
  - id: 1512
    service: chrome
    data:
      level: error
      msg: "Fetch timeout https://cdn.example/app.js"
hasMore: false`}
          </CodexExec>
          <CodexExec
            command='mcp__mizpah__list_services'
            result="→ 4 services"
          />
        </div>

        <CodexMessage>
          api is loudest (~9k entries). Timeouts cluster on redis from api, plus
          a worker lease and a chrome CDN fetch. Start with redis around #1837.
        </CodexMessage>
      </div>

      <div className="pt-2">
        <CodexPrompt
          directory="~/dev/api"
          model="gpt-5.6-sol low"
          placeholder="Ask Codex about the hub…"
        />
      </div>
    </div>
  )
}
