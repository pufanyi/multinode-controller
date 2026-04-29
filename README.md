# multinode-controller

Linux-only distributed agent runtime prototype based on a coordinator-worker
architecture.

The current design reference is [`docs/v0.1.md`](docs/v0.1.md).

## Current scope

This initial version implements:

- WebSocket + JSON coordinator/worker transport
- worker registration and heartbeat
- `agentctl nodes`
- `agentctl run --nodes node-a,node-b -- <argv...>`
- `allow_all` policy
- `none` sandbox backend
- local process execution through `tokio::process::Command`
- SQLite-backed coordinator event storage

MCP tools, torchrun orchestration, real policy rules, and bubblewrap sandboxing
are planned next steps.

## Demo

Terminal 1:

```bash
cargo run --bin agent-coordinator -- --listen 127.0.0.1:8765
```

Terminal 2:

```bash
cargo run --bin agent-worker -- --coordinator ws://127.0.0.1:8765 --node-name node-a
```

Terminal 3:

```bash
cargo run --bin agentctl -- nodes
cargo run --bin agentctl -- run --nodes node-a -- hostname
```
