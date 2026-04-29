# Design

The source design is maintained in `docs/v0.0.1.md`.

This implementation starts with the `v0.0.1` target from that document:

- single coordinator
- outbound worker connection
- WebSocket + JSON
- allow-all policy
- none sandbox
- local process executor
- SQLite coordinator store

The important invariant is preserved from day one:

```text
PolicyEngine -> SandboxBackend -> Executor
```
