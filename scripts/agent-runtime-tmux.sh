#!/usr/bin/env bash
set -euo pipefail

SESSION="${SESSION:-agent-runtime}"
PORT="${PORT:-${MASTER_PORT:-8765}}"
COORDINATOR_ADDR="${COORDINATOR_ADDR:-${MASTER_ADDR:-127.0.0.1}}"
LISTEN="${LISTEN:-0.0.0.0:${PORT}}"
COORDINATOR_URL="${COORDINATOR_URL:-ws://${COORDINATOR_ADDR}:${PORT}}"
RUNTIME_DIR="${RUNTIME_DIR:-$HOME/.agent-runtime}"
WORKER_TOKEN_FILE="${WORKER_TOKEN_FILE:-$RUNTIME_DIR/worker.token}"
CLIENT_TOKEN_FILE="${CLIENT_TOKEN_FILE:-$RUNTIME_DIR/client.token}"
DB="${DB:-$RUNTIME_DIR/coordinator.sqlite}"
REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
NODE_NAME="${NODE_NAME:-${HOSTNAME:-node}-rank-${RANK:-0}}"
POLICY="${POLICY:-$REPO_ROOT/examples/allow-all.yaml}"

usage() {
  cat <<'USAGE'
Usage:
  scripts/agent-runtime-tmux.sh start-master
  scripts/agent-runtime-tmux.sh start-worker
  scripts/agent-runtime-tmux.sh status
  scripts/agent-runtime-tmux.sh stop

For training-cluster style launch, prefer scripts/agent-runtime-launch.sh.

Environment overrides:
  SESSION              tmux session name, default agent-runtime
  PORT                 coordinator port, default MASTER_PORT or 8765
  COORDINATOR_ADDR     coordinator host, default MASTER_ADDR or 127.0.0.1
  COORDINATOR_URL      worker/client URL, default ws://${COORDINATOR_ADDR}:${PORT}
  RUNTIME_DIR          log/token/db directory, default ~/.agent-runtime
  WORKER_TOKEN_FILE    worker bearer token file
  CLIENT_TOKEN_FILE    client bearer token file
  NODE_NAME            worker node name
  POLICY               worker policy yaml
USAGE
}

require_tmux() {
  command -v tmux >/dev/null || {
    echo "tmux is required" >&2
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

build_bins() {
  cargo build --workspace --bins
}

start_master() {
  require_tmux
  mkdir -p "$RUNTIME_DIR"
  ensure_token "$WORKER_TOKEN_FILE"
  ensure_token "$CLIENT_TOKEN_FILE"
  build_bins

  if tmux has-session -t "$(tmux_target)" 2>/dev/null; then
    echo "tmux session $SESSION already exists" >&2
    exit 1
  fi

  : > "$RUNTIME_DIR/coordinator.log"
  : > "$RUNTIME_DIR/worker-${NODE_NAME}.log"

  tmux new-session -d -s "$SESSION" -n coordinator \
    "cd '$REPO_ROOT' && ./target/debug/agent-coordinator --listen '$LISTEN' --db '$DB' --worker-token-file '$WORKER_TOKEN_FILE' --client-token-file '$CLIENT_TOKEN_FILE' >> '$RUNTIME_DIR/coordinator.log' 2>&1"
  sleep 1
  tmux new-window -t "$(tmux_target)" -n worker \
    "cd '$REPO_ROOT' && ./target/debug/agent-worker --coordinator '$COORDINATOR_URL' --node-name '$NODE_NAME' --workspace-root '$REPO_ROOT' --policy '$POLICY' --token-file '$WORKER_TOKEN_FILE' >> '$RUNTIME_DIR/worker-${NODE_NAME}.log' 2>&1"

  echo "started tmux session $SESSION"
  echo "coordinator: $COORDINATOR_URL"
  echo "client token: $CLIENT_TOKEN_FILE"
}

start_worker() {
  require_tmux
  mkdir -p "$RUNTIME_DIR"
  if [ ! -s "$WORKER_TOKEN_FILE" ]; then
    echo "missing worker token file: $WORKER_TOKEN_FILE" >&2
    exit 1
  fi
  build_bins

  if tmux has-session -t "$(tmux_target)" 2>/dev/null; then
    tmux kill-session -t "$(tmux_target)"
  fi

  : > "$RUNTIME_DIR/worker-${NODE_NAME}.log"
  tmux new-session -d -s "$SESSION" -n worker \
    "cd '$REPO_ROOT' && ./target/debug/agent-worker --coordinator '$COORDINATOR_URL' --node-name '$NODE_NAME' --workspace-root '$REPO_ROOT' --policy '$POLICY' --token-file '$WORKER_TOKEN_FILE' >> '$RUNTIME_DIR/worker-${NODE_NAME}.log' 2>&1"

  echo "started worker in tmux session $SESSION"
  echo "coordinator: $COORDINATOR_URL"
}

status() {
  require_tmux
  tmux list-windows -t "$(tmux_target)"
}

stop() {
  require_tmux
  tmux kill-session -t "$(tmux_target)"
}

case "${1:-}" in
  start-master) start_master ;;
  start-worker) start_worker ;;
  status) status ;;
  stop) stop ;;
  -h|--help|help|"") usage ;;
  *)
    usage >&2
    exit 1
    ;;
esac
