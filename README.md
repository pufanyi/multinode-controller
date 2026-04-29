# multinode-controller

Linux-only distributed agent runtime prototype based on a coordinator-worker
architecture.

The current design reference is [`docs/v0.1.md`](docs/v0.1.md).
The current two-machine lab runbook is [`docs/RUNBOOK.md`](docs/RUNBOOK.md).

## Current scope

This initial version implements:

- WebSocket + JSON coordinator/worker transport
- worker registration and heartbeat
- optional WebSocket handshake bearer-token authentication
- `agentctl nodes`
- `agentctl run --nodes node-a,node-b -- <argv...>`
- `agentctl job list|tail|kill`
- `allow_all` policy
- YAML policy loading with simple command allow/deny lists
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

## Trusted cluster authentication

For a trusted multi-machine cluster, prefer token files over command-line
passwords. Command-line secrets can leak through process listings, shell history,
and terminal logs.

Generate separate worker and client tokens on the coordinator host:

```bash
mkdir -p ~/.agent-runtime
python3 -c 'import secrets; print(secrets.token_urlsafe(32))' > ~/.agent-runtime/worker.token
python3 -c 'import secrets; print(secrets.token_urlsafe(32))' > ~/.agent-runtime/client.token
chmod 600 ~/.agent-runtime/*.token
```

Start the coordinator:

```bash
cargo run --bin agent-coordinator -- \
  --listen 0.0.0.0:8765 \
  --worker-token-file ~/.agent-runtime/worker.token \
  --client-token-file ~/.agent-runtime/client.token
```

Start each worker with the worker token:

```bash
cargo run --bin agent-worker -- \
  --coordinator ws://coordinator-host:8765 \
  --node-name node-a \
  --token-file ~/.agent-runtime/worker.token
```

Use `agentctl` with the client token:

```bash
cargo run --bin agentctl -- \
  --coordinator ws://coordinator-host:8765 \
  --token-file ~/.agent-runtime/client.token \
  nodes
```

For quick local demos, `--token-file` on the coordinator can be used as one
shared token for both workers and clients. Production-like setups should keep
worker and client tokens separate.

## Tmux startup

For lab machines, `scripts/agent-runtime-tmux.sh` starts long-running processes
inside tmux without needing systemd.

For the current two-port setup (`23456` and `23457`), follow
[`docs/RUNBOOK.md`](docs/RUNBOOK.md).

On the coordinator node:

```bash
PORT="${MASTER_PORT:-8765}" scripts/agent-runtime-tmux.sh start-master
```

On worker-only nodes, copy the worker token into the same path or set
`WORKER_TOKEN_FILE`, then run:

```bash
COORDINATOR_ADDR="$MASTER_ADDR" PORT="$MASTER_PORT" \
  scripts/agent-runtime-tmux.sh start-worker
```

Check or stop a session:

```bash
scripts/agent-runtime-tmux.sh status
scripts/agent-runtime-tmux.sh stop
```

Use the generated client token for CLI access:

```bash
./target/debug/agentctl \
  --coordinator "ws://${MASTER_ADDR}:${MASTER_PORT}" \
  --token-file "$HOME/.agent-runtime/client.token" \
  nodes
```

## Policy files

Workers read `--policy <path>` on startup. The initial policy engine supports
`mode: allow_all`, `mode: deny_all`, `allow_commands`, and `deny_commands`.

```yaml
version: 1
mode: allow_all
deny_commands:
  - rm
  - shutdown
```

Every launch still flows through `PolicyEngine -> SandboxBackend -> Executor`;
the current sandbox backend remains `none`.
