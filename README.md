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

## Installed runtime launcher

For lab and training machines, install the runtime binaries and run the same
environment-driven command on every node. `RANK=0` starts the coordinator plus a
local worker; other ranks start worker-only processes.

Install from Git:

```bash
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-coordinator --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-worker --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-cli --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-runtime --locked
```

Install from a checkout during development:

```bash
cargo install --path crates/coordinator --locked
cargo install --path crates/worker --locked
cargo install --path crates/cli --locked
cargo install --path crates/runtime --locked
```

Start the runtime in the foreground:

```bash
PORT="${MASTER_PORT:-8765}" \
COORDINATOR_ADDR="${MASTER_ADDR:-127.0.0.1}" \
agent-runtime start
```

The foreground command is intended to be wrapped by your scheduler, launch
script, or manually managed tmux session. `agent-runtime` writes
`coordinator.log` and `worker-<node>.log` under `RUNTIME_DIR`, which defaults to
`~/.agent-runtime`.

If you want `agent-runtime` to create tmux sessions itself, set `MODE=tmux`:

```bash
MODE=tmux SESSION=agent-runtime-8765 PORT="${MASTER_PORT:-8765}" agent-runtime start
MODE=tmux SESSION=agent-runtime-8765 agent-runtime status
MODE=tmux SESSION=agent-runtime-8765 agent-runtime stop
```

Worker-only nodes need the worker token at `WORKER_TOKEN_FILE` or
`$RUNTIME_DIR/worker.token`. The client token should stay on the coordinator
host.

Use the generated client token for CLI access:

```bash
agentctl \
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
