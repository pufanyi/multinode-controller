# Runtime runbook

This runbook covers the current two-machine lab setup.

## Port layout

Use separate tmux sessions and runtime directories when running more than one
coordinator on the same master.

```text
23456  primary runtime       SESSION=agent-runtime
23457  auth/debug runtime    SESSION=agent-runtime-auth-debug
```

Each runtime has its own coordinator SQLite file and token files under
`RUNTIME_DIR`. Workers never write the coordinator SQLite database directly.

## Prerequisites

Run commands from the repository root:

```bash
cd /mnt/umm/users/pufanyi/workspace/multinode-controller
cargo build --workspace --bins
```

The coordinator node must have `MASTER_ADDR` and `MASTER_PORT` set by the
environment, or pass `COORDINATOR_ADDR` and `PORT` explicitly.

Workers need the worker token file for the runtime they join. If `$HOME` is
shared across machines, the generated token file may already be visible on the
worker. Otherwise copy only the worker token to the worker host. Do not copy the
client token to worker-only machines.

## Restart 23456

On the master:

```bash
SESSION=agent-runtime \
PORT=23456 \
RUNTIME_DIR="$HOME/.agent-runtime" \
NODE_NAME="${HOSTNAME}-rank-${RANK:-0}" \
POLICY="$PWD/examples/deny-dangerous.yaml" \
scripts/agent-runtime-tmux.sh stop 2>/dev/null || true

SESSION=agent-runtime \
PORT=23456 \
RUNTIME_DIR="$HOME/.agent-runtime" \
NODE_NAME="${HOSTNAME}-rank-${RANK:-0}" \
POLICY="$PWD/examples/deny-dangerous.yaml" \
scripts/agent-runtime-tmux.sh start-master
```

On each worker-only machine:

```bash
cd /mnt/umm/users/pufanyi/workspace/multinode-controller

SESSION=agent-runtime \
PORT=23456 \
COORDINATOR_ADDR="$MASTER_ADDR" \
RUNTIME_DIR="$HOME/.agent-runtime" \
NODE_NAME="${HOSTNAME}-rank-${RANK}" \
POLICY="$PWD/examples/deny-dangerous.yaml" \
scripts/agent-runtime-tmux.sh stop 2>/dev/null || true

SESSION=agent-runtime \
PORT=23456 \
COORDINATOR_ADDR="$MASTER_ADDR" \
RUNTIME_DIR="$HOME/.agent-runtime" \
NODE_NAME="${HOSTNAME}-rank-${RANK}" \
POLICY="$PWD/examples/deny-dangerous.yaml" \
scripts/agent-runtime-tmux.sh start-worker
```

Verify from the master:

```bash
./target/debug/agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  nodes
```

## Restart 23457

On the master:

```bash
SESSION=agent-runtime-auth-debug \
PORT=23457 \
RUNTIME_DIR="$HOME/.agent-runtime/auth-debug" \
NODE_NAME="${HOSTNAME}-auth-rank-${RANK:-0}" \
POLICY="$PWD/examples/deny-dangerous.yaml" \
scripts/agent-runtime-tmux.sh stop 2>/dev/null || true

SESSION=agent-runtime-auth-debug \
PORT=23457 \
RUNTIME_DIR="$HOME/.agent-runtime/auth-debug" \
NODE_NAME="${HOSTNAME}-auth-rank-${RANK:-0}" \
POLICY="$PWD/examples/deny-dangerous.yaml" \
scripts/agent-runtime-tmux.sh start-master
```

On each worker-only machine:

```bash
cd /mnt/umm/users/pufanyi/workspace/multinode-controller

SESSION=agent-runtime-auth-debug \
PORT=23457 \
COORDINATOR_ADDR="$MASTER_ADDR" \
RUNTIME_DIR="$HOME/.agent-runtime/auth-debug" \
NODE_NAME="${HOSTNAME}-auth-rank-${RANK}" \
POLICY="$PWD/examples/deny-dangerous.yaml" \
scripts/agent-runtime-tmux.sh stop 2>/dev/null || true

SESSION=agent-runtime-auth-debug \
PORT=23457 \
COORDINATOR_ADDR="$MASTER_ADDR" \
RUNTIME_DIR="$HOME/.agent-runtime/auth-debug" \
NODE_NAME="${HOSTNAME}-auth-rank-${RANK}" \
POLICY="$PWD/examples/deny-dangerous.yaml" \
scripts/agent-runtime-tmux.sh start-worker
```

Verify from the master:

```bash
./target/debug/agentctl \
  --coordinator "ws://${MASTER_ADDR}:23457" \
  --token-file "$HOME/.agent-runtime/auth-debug/client.token" \
  nodes
```

## Smoke tests

Run on all connected nodes:

```bash
./target/debug/agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  run -- sh -lc 'hostname; echo rank=${RANK:-unset}'
```

Inspect jobs and logs:

```bash
./target/debug/agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  job list

./target/debug/agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  job tail job_xxx
```

Check policy denial:

```bash
./target/debug/agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  run --nodes "${HOSTNAME}-rank-${RANK}" -- rm
```

The command should fail because `examples/deny-dangerous.yaml` denies `rm`.

## Stop runtimes

On the master:

```bash
SESSION=agent-runtime scripts/agent-runtime-tmux.sh stop
SESSION=agent-runtime-auth-debug scripts/agent-runtime-tmux.sh stop
```

On a worker-only machine:

```bash
SESSION=agent-runtime scripts/agent-runtime-tmux.sh stop
SESSION=agent-runtime-auth-debug scripts/agent-runtime-tmux.sh stop
```

## Logs

Default log locations:

```text
~/.agent-runtime/coordinator.log
~/.agent-runtime/worker-<node>.log
~/.agent-runtime/auth-debug/coordinator.log
~/.agent-runtime/auth-debug/worker-<node>.log
```
