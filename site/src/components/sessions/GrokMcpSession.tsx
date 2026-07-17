import { GrokEvent } from "@/components/brainless/grok/grok-event"
import { GrokHeader } from "@/components/brainless/grok/grok-header"
import { GrokMessage } from "@/components/brainless/grok/grok-message"
import { GrokPrompt } from "@/components/brainless/grok/grok-prompt"
import { GrokStatus } from "@/components/brainless/grok/grok-status"
import { GrokThought } from "@/components/brainless/grok/grok-thought"
import { GrokTool } from "@/components/brainless/grok/grok-tool"
import { GrokTurnEnd } from "@/components/brainless/grok/grok-turn-end"

export function GrokMcpSession() {
  return (
    <div className="space-y-3 font-mono text-[13px] leading-[1.6] text-[#e8e8e8]">
      <GrokStatus
        branch="main"
        directory="~/dev/web"
        contextUsed="12K"
        contextLimit="500K"
        turn={3}
        turnTotal={3}
      />

      <GrokHeader
        headline="mizpah MCP is wired"
        subhead="Query the local hub with list_properties and CEL filters."
      />

      <div className="space-y-2 pt-1">
        <GrokMessage role="user" time="1:12 AM">
          chrome console is noisy. what fields can I filter on, then show me
          recent TypeErrors?
        </GrokMessage>

        <GrokEvent label="user_prompt_submit" hooks={2} hooksOk={2} />
        <GrokThought elapsed="0.3s">
          Discover schema first via list_properties, then CEL on level/msg for
          the chrome service.
        </GrokThought>

        <GrokTool
          variant="card"
          title="Run mcp__mizpah__list_properties"
          hooks={1}
        >
          {`properties[7]{path,types,count}:
  service,[string],18420
  level,[string],18420
  msg,[string],18420
  ts,[string],1200
  url,[string],3188
  console.method,[string],900
  stack,[string],210`}
        </GrokTool>

        <GrokTool
          variant="card"
          title='Run mcp__mizpah__search_logs'
          hooks={1}
        >
          {`service: "chrome"
q: level == "error" && msg.contains("TypeError")
limit: 3

→ TOON
entries[3]:
  - id: 2201
    service: chrome
    data:
      level: error
      msg: "TypeError: Cannot read properties of undefined (reading 'id')"
  - id: 2188
    service: chrome
    data:
      level: error
      msg: "Uncaught TypeError: x.map is not a function"
  - id: 2174
    service: chrome
    data:
      level: error
      msg: "TypeError: Failed to fetch"
hasMore: false`}
        </GrokTool>

        <GrokMessage time="1:12 AM">
          Filterable fields include service, level, msg, ts, url, plus chrome
          console.method and stack. Three recent TypeErrors: undefined .id,
          .map on a non-array, and a failed fetch. Open #2201 in the hub UI or
          call get_logs_around for stack context.
        </GrokMessage>

        <GrokEvent label="stop" hooks={2} hooksOk={2} />
        <GrokTurnEnd elapsed="4.1s" />
      </div>

      <div className="pt-3">
        <GrokPrompt
          mode="always-approve"
          model="Grok 4.5 (xhigh)"
          showShortcuts={false}
          placeholder="Ask Grok about the buffer…"
        />
      </div>
    </div>
  )
}
