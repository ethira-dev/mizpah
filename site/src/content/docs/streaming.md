---
title: Streaming logs
description: How the local hub binds, tags services, and normalizes JSON (and non-JSON) lines.
order: 3
---

First process binds `:1738` and serves the UI. Everything else attaches. Tag streams with `--service` (defaults to absolute cwd), switch services in the UI, and keep a 1 GiB in-memory ring buffer hot without shipping logs to the cloud.

Every row gets a reserved `_mzp` object identifying the mizpah receiver: `cwd` (terminal folder), `user`, `pid`, and `exe`.

Pretty-printed Nest / `util.inspect` dumps? mizpah reassembles them into structured JSON when it can. Non-JSON lines land as `{ "_raw": "…" }`. Prefer NDJSON when you control the logger.

```bash
api-server | mzp --service api --project /path/to/repo
```

## Tips

- Multiple processes can share one hub; only the first binds the port.
- Use distinct `--service` names when you want clean switching in the UI.
- Set `--project` / `MIZPAH_PROJECT` so **Check with Claude / Cursor** opens in the right repo.

See also: [Attach sources](../attach/) · [CEL filters](../cel/)
