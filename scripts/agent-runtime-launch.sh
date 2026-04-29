#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
RANK="${RANK:-0}"
PORT="${PORT:-${MASTER_PORT:-8765}}"
COORDINATOR_ADDR="${COORDINATOR_ADDR:-${MASTER_ADDR:-127.0.0.1}}"
COORDINATOR_URL="${COORDINATOR_URL:-ws://${COORDINATOR_ADDR}:${PORT}}"
LISTEN="${LISTEN:-0.0.0.0:${PORT}}"
RUNTIME_DIR="${RUNTIME_DIR:-$HOME/.agent-runtime}"
WORKER_TOKEN_FILE="${WORKER_TOKEN_FILE:-$RUNTIME_DIR/worker.token}"
CLIENT_TOKEN_FILE="${CLIENT_TOKEN_FILE:-$RUNTIME_DIR/client.token}"
DB="${DB:-$RUNTIME_DIR/coordinator.sqlite}"
NODE_NAME="${NODE_NAME:-${HOSTNAME:-node}-rank-${RANK}}"
POLICY="${POLICY:-$REPO_ROOT/examples/allow-all.yaml}"
SESSION="${SESSION:-agent-runtime-${PORT}}"
MODE="${MODE:-tmux}"
ROLE="${ROLE:-auto}"

usage() {
  cat <<'USAGE'
Usage:
  scripts/agent-runtime-launch.sh start
  scripts/agent-runtime-launch.sh stop
  scripts/agent-runtime-launch.sh status

Default behavior:
  ROLE=auto starts coordinator+worker when RANK=0, and worker only otherwise.

Common environment:
  RANK                 node rank, default 0
  MASTER_ADDR          coordinator host from training environment
  MASTER_PORT          coordinator port from training environment
  PORT                 overrides MASTER_PORT
  ROLE                 auto, master, worker
  MODE                 tmux or foreground, default tmux
  SESSION              tmux session name, default agent-runtime-${PORT}
  RUNTIME_DIR          log/token/db directory, default ~/.agent-runtime
  WORKER_TOKEN_FILE    worker bearer token file
  CLIENT_TOKEN_FILE    client bearer token file
  NODE_NAME            worker node name
  POLICY               worker policy yaml

Token files:
  RANK=0 creates worker/client tokens when missing.
  Worker-only ranks require WORKER_TOKEN_FILE to already exist.
USAGE
}

resolved_role() {
  case "$ROLE" in
    auto)
      if [ "$RANK" = "0" ]; then
        echo master
      else
        echo worker
      fi
      ;;
    master|worker) echo "$ROLE" ;;
    *)
      echo "invalid ROLE=$ROLE; expected auto, master, or worker" >&2
      exit 1
      ;;
  esac
}

require_tmux() {
  command -v tmux >/dev/null || {
    echo "tmux is required when MODE=tmux" >&2
    exit 1
  }
}

tmux_target() {
  printf '=%s' "$SESSION"
}

generate_token() {
  if command -v python3 >/dev/null; then
    python3 -c 'import secrets; print(secrets.token_urlsafe(32))'
  else
    openssl rand -base64 32
  fi
}

ensure_token() {
  local path="$1"
  mkdir -p "$(dirname "$path")"
  if [ ! -s "$path" ]; then
    generate_token > "$path"
    chmod 600 "$path"
  fi
}

require_worker_token() {
  if [ ! -s "$WORKER_TOKEN_FILE" ]; then
    echo "missing worker token file: $WORKER_TOKEN_FILE" >&2
    echo "copy it from the coordinator node or set WORKER_TOKEN_FILE" >&2
    exit 1
  fi
}

build_bins() {
  cargo build --workspace --bins
}

coordinator_cmd() {
  printf "cd '%s' && ./target/debug/agent-coordinator --listen '%s' --db '%s' --worker-token-file '%s' --client-token-file '%s' >> '%s/coordinator.log' 2>&1" \
    "$REPO_ROOT" "$LISTEN" "$DB" "$WORKER_TOKEN_FILE" "$CLIENT_TOKEN_FILE" "$RUNTIME_DIR"
}

worker_cmd() {
  printf "cd '%s' && ./target/debug/agent-worker --coordinator '%s' --node-name '%s' --workspace-root '%s' --policy '%s' --token-file '%s' >> '%s/worker-%s.log' 2>&1" \
    "$REPO_ROOT" "$COORDINATOR_URL" "$NODE_NAME" "$REPO_ROOT" "$POLICY" "$WORKER_TOKEN_FILE" "$RUNTIME_DIR" "$NODE_NAME"
}

start_tmux() {
  local role="$1"
  require_tmux
  if tmux has-session -t "$(tmux_target)" 2>/dev/null; then
    echo "tmux session $SESSION already exists" >&2
    exit 1
  fi

  case "$role" in
    master)
      : > "$RUNTIME_DIR/coordinator.log"
      : > "$RUNTIME_DIR/worker-${NODE_NAME}.log"
      tmux new-session -d -s "$SESSION" -n coordinator "$(coordinator_cmd)"
      sleep 1
      tmux new-window -t "$(tmux_target)" -n worker "$(worker_cmd)"
      ;;
    worker)
      : > "$RUNTIME_DIR/worker-${NODE_NAME}.log"
      tmux new-session -d -s "$SESSION" -n worker "$(worker_cmd)"
      ;;
  esac
}

start_foreground() {
  local role="$1"
  case "$role" in
    master)
      : > "$RUNTIME_DIR/coordinator.log"
      : > "$RUNTIME_DIR/worker-${NODE_NAME}.log"
      bash -lc "$(coordinator_cmd)" &
      local coordinator_pid=$!
      sleep 1
      bash -lc "$(worker_cmd)" &
      local worker_pid=$!
      trap 'kill "$worker_pid" "$coordinator_pid" 2>/dev/null || true; wait 2>/dev/null || true' INT TERM EXIT
      wait -n "$coordinator_pid" "$worker_pid"
      ;;
    worker)
      exec bash -lc "$(worker_cmd)"
      ;;
  esac
}

start() {
  local role
  role="$(resolved_role)"
  mkdir -p "$RUNTIME_DIR"
  build_bins

  if [ "$role" = "master" ]; then
    ensure_token "$WORKER_TOKEN_FILE"
    ensure_token "$CLIENT_TOKEN_FILE"
  else
    require_worker_token
  fi

  case "$MODE" in
    tmux) start_tmux "$role" ;;
    foreground) start_foreground "$role" ;;
    *)
      echo "invalid MODE=$MODE; expected tmux or foreground" >&2
      exit 1
      ;;
  esac

  echo "started role=$role mode=$MODE"
  echo "coordinator: $COORDINATOR_URL"
  echo "node: $NODE_NAME"
  if [ "$role" = "master" ]; then
    echo "client token: $CLIENT_TOKEN_FILE"
  fi
}

stop() {
  if [ "$MODE" = "tmux" ]; then
    require_tmux
    tmux kill-session -t "$(tmux_target)"
  else
    echo "stop is only supported for MODE=tmux" >&2
    exit 1
  fi
}

status() {
  if [ "$MODE" = "tmux" ]; then
    require_tmux
    tmux list-windows -t "$(tmux_target)"
  else
    echo "status is only supported for MODE=tmux" >&2
    exit 1
  fi
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  status) status ;;
  -h|--help|help|"") usage ;;
  *)
    usage >&2
    exit 1
    ;;
esac
