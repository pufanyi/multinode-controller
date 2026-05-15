---
name: agent-runtime
description: Control the multinode-controller runtime with agent-runtime and agentctl. Use when the user asks to install runtime binaries, start or stop a multinode coordinator/worker cluster, inspect connected nodes, run commands across nodes, list or tail jobs, kill jobs, debug runtime logs, or manage policy files for this repository.
---

# Agent runtime

Use this skill to operate the Linux-only `multinode-controller` runtime through
the installed binaries:

- `agent-runtime` starts and manages coordinator/worker processes.
- `agentctl` controls the cluster through the coordinator WebSocket API.
- `agent-coordinator` and `agent-worker` are lower-level binaries used by
  `agent-runtime`.

The deployment contract is installed binaries, not repository-local startup
scripts. If startup behavior needs to change, update `crates/runtime`,
`README.md`, and `docs/RUNBOOK.md`.

## First checks

1. Confirm the host is Linux.
2. Check whether the runtime binaries are on `PATH`:

   ```bash
   command -v agent-runtime agentctl agent-coordinator agent-worker
   ```

3. If behavior is unclear, read `README.md`, `docs/RUNBOOK.md`, and
   `docs/PROTOCOL.md` from the current repository before acting.
4. Before operations with side effects, identify the coordinator address,
   runtime directory, token files, and target nodes. Do not invent remote host
   names or token paths.

## Install binaries

From a checkout:

```bash
cargo install --path crates/coordinator --locked
cargo install --path crates/worker --locked
cargo install --path crates/cli --locked
cargo install --path crates/runtime --locked
```

From Git:

```bash
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-coordinator --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-worker --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-cli --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-runtime --locked
```

## Runtime environment

Common variables:

```text
RANK=0                 coordinator plus local worker
RANK!=0                worker only
MASTER_ADDR            coordinator host, unless COORDINATOR_ADDR is set
MASTER_PORT            coordinator port, unless PORT is set
PORT                   coordinator port, default 8765
COORDINATOR_ADDR       coordinator host, default 127.0.0.1
RUNTIME_DIR            token, database, and log directory
NODE_NAME              worker node name
POLICY                 worker policy YAML path
WORKER_TOKEN_FILE      worker auth token path
CLIENT_TOKEN_FILE      client auth token path
MODE                   foreground or tmux
SESSION                tmux session name when MODE=tmux
```

`MODE=foreground` is the deployment default and blocks until stopped. For
interactive agent-managed lab sessions, prefer `MODE=tmux` so the command
returns and `agent-runtime status|stop` can manage the session.

## Start and stop

Start a local master in tmux:

```bash
RANK=0 \
PORT=23456 \
RUNTIME_DIR="$HOME/.agent-runtime" \
MODE=tmux \
SESSION=agent-runtime-23456 \
agent-runtime start
```

Start a worker-only node:

```bash
RANK=1 \
MASTER_ADDR="<coordinator-host>" \
MASTER_PORT=23456 \
RUNTIME_DIR="$HOME/.agent-runtime" \
MODE=tmux \
SESSION=agent-runtime-23456 \
agent-runtime start
```

Worker-only nodes require the worker token at `WORKER_TOKEN_FILE` or
`$RUNTIME_DIR/worker.token`. Copy only the worker token to worker hosts; keep the
client token on the coordinator host.

Inspect or stop a tmux-managed runtime:

```bash
MODE=tmux SESSION=agent-runtime-23456 PORT=23456 agent-runtime status
MODE=tmux SESSION=agent-runtime-23456 PORT=23456 agent-runtime stop
```

## Control with agentctl

Set reusable variables on the coordinator host:

```bash
MASTER_ADDR="${MASTER_ADDR:-127.0.0.1}"
MASTER_PORT="${MASTER_PORT:-8765}"
RUNTIME_DIR="${RUNTIME_DIR:-$HOME/.agent-runtime}"
AGENTCTL=(agentctl --coordinator "ws://${MASTER_ADDR}:${MASTER_PORT}" --token-file "$RUNTIME_DIR/client.token")
```

List nodes:

```bash
"${AGENTCTL[@]}" nodes
```

Run a command on all connected nodes:

```bash
"${AGENTCTL[@]}" run -- hostname
```

Run on selected nodes:

```bash
"${AGENTCTL[@]}" run --nodes node-a,node-b -- hostname
```

Run from a specific working directory:

```bash
"${AGENTCTL[@]}" run --cwd /path/to/workspace -- sh -lc 'pwd; hostname'
```

List jobs, tail logs, or request kill:

```bash
"${AGENTCTL[@]}" job list --limit 20
"${AGENTCTL[@]}" job tail job_xxx --lines 100
"${AGENTCTL[@]}" job kill job_xxx
```

## Logs and diagnosis

Default logs:

```text
~/.agent-runtime/coordinator.log
~/.agent-runtime/worker-<node>.log
```

Use these checks before deeper debugging:

```bash
tail -n 200 "$RUNTIME_DIR/coordinator.log"
ls -1 "$RUNTIME_DIR"/worker-*.log
tail -n 200 "$RUNTIME_DIR"/worker-*.log
```

If `agentctl` fails authentication, verify that it uses the client token, not the
worker token. If a worker cannot connect, verify `MASTER_ADDR`, `MASTER_PORT`,
`WORKER_TOKEN_FILE`, and network reachability from the worker to the coordinator.

## Safety rules

- Preserve the runtime enforcement flow:
  `PolicyEngine -> SandboxBackend -> Executor`.
- Use `agentctl` and `agent-runtime` rather than editing the coordinator SQLite
  database directly.
- Do not commit tokens, `.env` files, runtime logs, or machine-specific policy
  files.
- Do not copy the client token to worker-only hosts.
- For destructive commands such as `rm`, `shutdown`, package removal, or broad
  process kills, confirm the exact target nodes and command before running.
