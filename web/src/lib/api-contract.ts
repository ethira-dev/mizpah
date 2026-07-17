/**
 * Compile-time guardrail: fixtures must satisfy the wire types mirrored from
 * crates/mizpah/src/models.rs (keep in sync with tests/fixtures/*.json).
 */
import type { LogEntry, WsEvent } from "./types"

/** Mirrors crates/mizpah/tests/fixtures/log_entry.json */
export const LOG_ENTRY_FIXTURE: LogEntry = {
  id: 42,
  receivedAt: "2026-07-17T00:00:00Z",
  service: "api",
  data: {
    level: "error",
    msg: "timeout",
  },
}

/** Mirrors crates/mizpah/tests/fixtures/ws_events.json */
export const WS_EVENT_FIXTURES: WsEvent[] = [
  {
    type: "log",
    entry: {
      id: 1,
      receivedAt: "2026-07-17T00:00:00Z",
      service: "api",
      data: { msg: "hi" },
    },
  },
  { type: "evicted", ids: [1, 2, 3] },
  { type: "services", names: ["api", "web"], blocked: ["old"] },
  {
    type: "properties",
    paths: [
      {
        path: "level",
        types: ["string"],
        sampleValues: ["error"],
        count: 3,
      },
    ],
  },
  { type: "pong" },
  { type: "lagged", skipped: 7 },
]

/** Touch fixtures so `tsc` always type-checks this module. */
export function assertApiContractFixtures(): number {
  return LOG_ENTRY_FIXTURE.id + WS_EVENT_FIXTURES.length
}
