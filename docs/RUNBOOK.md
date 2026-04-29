# Runtime runbook

This runbook covers the current two-machine lab setup. It assumes the binaries
are installed on each machine and available on `PATH`.

The runtime is launched through installable binaries. The repository does not
provide startup scripts as the deployment contract.

## Port layout

Use separate runtime directories when running more than one coordinator on the
same master.

```text
23456  primary runtime       RUNTIME_DIR=$HOME/.agent-runtime
23457  auth/debug runtime    RUNTIME_DIR=$HOME/.agent-runtime/auth-debug
```

Each runtime has its own coordinator SQLite file and token files under
`RUNTIME_DIR`. Workers never write the coordinator SQLite database directly.

## Install

Install the four binaries on every machine:

```bash
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-coordinator --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-worker --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-cli --locked
cargo install --git ssh://git@github.com/pufanyi/multinode-controller.git agent-runtime --locked
```

For local development from a checkout:

```bash
cargo install --path crates/coordinator --locked
cargo install --path crates/worker --locked
cargo install --path crates/cli --locked
cargo install --path crates/runtime --locked
```

## Environment contract

The same `agent-runtime start` command can be launched on every node. The
launcher reads training-style environment variables:

```text
RANK=0       coordinator + local worker
RANK!=0      worker only
MASTER_ADDR  coordinator host, unless COORDINATOR_ADDR is set
MASTER_PORT  coordinator port, unless PORT is set
```

Common optional variables:

```text
PORT                 coordinator port; falls back to MASTER_PORT, then 8765
COORDINATOR_ADDR     coordinator host; falls back to MASTER_ADDR, then 127.0.0.1
RUNTIME_DIR          token, database, and log directory; defaults to ~/.agent-runtime
NODE_NAME            worker node name; defaults to <hostname>-rank-<RANK>
POLICY               worker policy YAML path; unset uses the worker default
WORKER_TOKEN_FILE    worker token path; defaults to $RUNTIME_DIR/worker.token
CLIENT_TOKEN_FILE    client token path; defaults to $RUNTIME_DIR/client.token
MODE                 foreground or tmux; defaults to foreground
SESSION              tmux session name when MODE=tmux
```

Rank 0 creates missing worker and client tokens. Worker-only ranks require the
worker token to exist at `WORKER_TOKEN_FILE` or `$RUNTIME_DIR/worker.token`. If
`$HOME` is not shared across machines, copy only the worker token to worker-only
hosts. Do not copy the client token to worker-only machines.

## Foreground launch

Default mode is foreground, which is the preferred shape for schedulers,
training launch scripts, and tmux sessions that you manage yourself.

This command can be the full body of a per-machine training launcher once the
environment has set `RANK`, `MASTER_ADDR`, and `MASTER_PORT`.

```bash
PORT="${MASTER_PORT:-8765}" \
COORDINATOR_ADDR="${MASTER_ADDR:-127.0.0.1}" \
agent-runtime start
```

If you want to keep it in a manual tmux session:

```bash
tmux new -s agent-runtime-23456

PORT=23456 \
COORDINATOR_ADDR="$MASTER_ADDR" \
agent-runtime start
```

Stop foreground mode with `Ctrl-C` or by closing the process manager that owns
the command.

## Optional tmux mode

`MODE=tmux` lets `agent-runtime` create and stop tmux sessions itself. This is
convenient for ad hoc lab debugging, but it is not required for installed
deployments.

```bash
MODE=tmux SESSION=agent-runtime-23456 PORT=23456 agent-runtime start
MODE=tmux SESSION=agent-runtime-23456 agent-runtime status
MODE=tmux SESSION=agent-runtime-23456 agent-runtime stop
```

`agent-runtime stop` and `agent-runtime status` are only defined for
`MODE=tmux`.

## Restart 23456

Run this on every node:

```bash
PORT=23456 \
COORDINATOR_ADDR="$MASTER_ADDR" \
RUNTIME_DIR="$HOME/.agent-runtime" \
NODE_NAME="${HOSTNAME}-rank-${RANK:-0}" \
agent-runtime start
```

With a local policy file:

```bash
PORT=23456 \
COORDINATOR_ADDR="$MASTER_ADDR" \
RUNTIME_DIR="$HOME/.agent-runtime" \
NODE_NAME="${HOSTNAME}-rank-${RANK:-0}" \
POLICY="/path/to/deny-dangerous.yaml" \
agent-runtime start
```

Verify from the master:

```bash
agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  nodes
```

## Restart 23457

Run this on every node:

```bash
PORT=23457 \
COORDINATOR_ADDR="$MASTER_ADDR" \
RUNTIME_DIR="$HOME/.agent-runtime/auth-debug" \
NODE_NAME="${HOSTNAME}-auth-rank-${RANK:-0}" \
agent-runtime start
```

Verify from the master:

```bash
agentctl \
  --coordinator "ws://${MASTER_ADDR}:23457" \
  --token-file "$HOME/.agent-runtime/auth-debug/client.token" \
  nodes
```

## Smoke tests

Run on all connected nodes:

```bash
agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  run -- sh -lc 'hostname; echo rank=${RANK:-unset}'
```

Inspect jobs and logs:

```bash
agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  job list

agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  job tail job_xxx
```

Check policy denial when a policy file denies `rm`:

```bash
agentctl \
  --coordinator "ws://${MASTER_ADDR}:23456" \
  --token-file "$HOME/.agent-runtime/client.token" \
  run --nodes "${HOSTNAME}-rank-${RANK:-0}" -- rm
```

## Logs

Default log locations:

```text
~/.agent-runtime/coordinator.log
~/.agent-runtime/worker-<node>.log
~/.agent-runtime/auth-debug/coordinator.log
~/.agent-runtime/auth-debug/worker-<node>.log
```
