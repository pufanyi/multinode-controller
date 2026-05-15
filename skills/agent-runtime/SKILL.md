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
4. When attaching to an already-running cluster, discover the active address and
   runtime directory instead of assuming port `8765`. Check process arguments
   and the runtime directory:

   ```bash
   pgrep -af 'agent-(runtime|coordinator|worker)'
   ls -la "${RUNTIME_DIR:-$HOME/.agent-runtime}"
   ```

   If `pgrep` is unavailable, inspect `ps -ef`. The coordinator process shows
   the active `--listen` address, SQLite path, worker token file, and client
   token file. Do not print token contents.
5. Before operations with side effects, identify the coordinator address,
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
cargo install --git https://github.com/pufanyi/multinode-controller.git agent-coordinator --locked
cargo install --git https://github.com/pufanyi/multinode-controller.git agent-worker --locked
cargo install --git https://github.com/pufanyi/multinode-controller.git agent-cli --locked
cargo install --git https://github.com/pufanyi/multinode-controller.git agent-runtime --locked
```

For a private repository, use the SSH URL. If `git clone` succeeds but Cargo
cannot authenticate, set `CARGO_NET_GIT_FETCH_WITH_CLI=true` so Cargo uses the
system Git client for fetching.

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

In a managed Codex sandbox, local WebSocket access can fail with
`Operation not permitted`. If the coordinator process is running and an
`agentctl` command fails with that exact OS error, rerun the same `agentctl`
command with escalated permissions before concluding that the cluster is down.

List nodes:

```bash
"${AGENTCTL[@]}" nodes
```

## Verify access

When the user asks whether the cluster is usable, prove the full control path
with safe commands before running heavier work.

1. List nodes and use the exact node names from the output:

   ```bash
   "${AGENTCTL[@]}" nodes
   ```

2. Run a small command on all connected nodes:

   ```bash
   "${AGENTCTL[@]}" run -- python3 -c 'import socket, platform; print("node=" + socket.gethostname()); print("kernel=" + platform.release()); print("check=" + str(sum(i*i for i in range(10000))))'
   ```

3. For per-node validation, run the same command with `--nodes <node-name>`.

4. If the user asks about GPU availability, query each node with `nvidia-smi`:

   ```bash
   "${AGENTCTL[@]}" run -- nvidia-smi --query-gpu=index,name,memory.total,driver_version --format=csv,noheader
   ```

5. Prefer the ML shortcuts when the installed `agentctl` supports them:

   ```bash
   "${AGENTCTL[@]}" inventory gpu
   "${AGENTCTL[@]}" inventory cuda
   "${AGENTCTL[@]}" inventory torch
   "${AGENTCTL[@]}" health gpu
   "${AGENTCTL[@]}" health torch
   ```

Report node names, whether each task exited successfully, and the relevant
stdout. Keep the output concise; do not include tokens.

## ML workflows

Use these commands for machine-learning oriented operations:

```bash
"${AGENTCTL[@]}" inventory nodes
"${AGENTCTL[@]}" inventory gpu
"${AGENTCTL[@]}" inventory cuda
"${AGENTCTL[@]}" inventory torch
"${AGENTCTL[@]}" health gpu
"${AGENTCTL[@]}" health torch
"${AGENTCTL[@]}" health nccl --nodes node-a,node-b --gpus-per-node 1
"${AGENTCTL[@]}" lease create --nodes node-a,node-b --gpus-per-node 1
"${AGENTCTL[@]}" lease list
"${AGENTCTL[@]}" lease release lease_xxx
"${AGENTCTL[@]}" ml torchrun --lease lease_xxx --gpus-per-node 1 --timeout-seconds 3600 -- python train.py
"${AGENTCTL[@]}" run --wait false --timeout-seconds 3600 -- python train.py
"${AGENTCTL[@]}" job status job_xxx
"${AGENTCTL[@]}" job watch job_xxx --interval 5 --lines 20
"${AGENTCTL[@]}" job diagnose job_xxx --lines 200
```

`ml torchrun` sets per-node rank environment through the coordinator and runs
`torchrun` on each selected node. If `--master-addr` is omitted, `agentctl` uses
the hostname of the first selected node. Use `--wait false` when the task should
keep running while the agent does other work; this is background execution, not
process suspension. Use `--timeout-seconds` to have the worker kill overlong
tasks. Leases are in-memory coordinator reservations; recreate them after a
coordinator restart.

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
