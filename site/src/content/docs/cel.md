---
title: CEL filters
description: Query the live buffer with CEL. Syntax highlighting, autocomplete, nested paths.
order: 5
---

The filter bar is a real query language: syntax highlighting, property autocomplete, nested paths like `user.id`.

| Binding | Meaning |
|---------|---------|
| `service` | Stream service tag |
| `level` | First of `level` / `severity` / `lvl` in the JSON |
| `source` | Attach source when present (`browser`, `cursor`, `claude`, …) |
| `kind` | Event kind (browser: `console` / `network`; agent hooks: lifecycle name) |
| `cmd` | Full shell command (attach mode); also a normal JSON field when present |
| `_mzp.*` | Receiver metadata (`cwd`, `user`, `pid`, `exe`) |
| *fields* | Every top-level key from the log JSON (nested via `.`) |

```
service == "api" && level == "error"
cmd.contains("npm test")
msg.contains("timeout") || error.contains("timeout")
has(user.id) && user.id == "42"
level in ["error", "warn"]
msg.matches("(?i)time.?out")
```

Empty query = everything. REST: `GET /api/logs?q=<cel>` · `GET /api/properties?q=<search>`.

Learn more about the language at [cel.dev](https://cel.dev/).
