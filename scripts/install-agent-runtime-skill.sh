#!/usr/bin/env sh
set -eu

skill_name="agent-runtime"
repo_url="${MULTINODE_REPO_URL:-https://github.com/pufanyi/multinode-controller}"
ref="${MULTINODE_REF:-main}"
install_codex=1
install_claude=1

usage() {
  cat <<'USAGE'
Install the multinode-controller agent-runtime skill.

By default this installs to both:
  ${CODEX_HOME:-$HOME/.codex}/skills/agent-runtime
  ${CLAUDE_HOME:-$HOME/.claude}/skills/agent-runtime

Options:
  --codex       install only for Codex
  --claude      install only for Claude Code
  --all         install for both Codex and Claude Code
  --help        show this help

Environment:
  CODEX_HOME            override Codex home, default $HOME/.codex
  CLAUDE_HOME           override Claude home, default $HOME/.claude
  MULTINODE_REPO_URL    GitHub repository URL used for curl-pipe installs
  MULTINODE_REF         branch name used for curl-pipe installs, default main
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --codex)
      install_codex=1
      install_claude=0
      ;;
    --claude)
      install_codex=0
      install_claude=1
      ;;
    --all)
      install_codex=1
      install_claude=1
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [ -z "${HOME:-}" ]; then
  echo "HOME is required" >&2
  exit 1
fi

cleanup_dir=""
cleanup() {
  if [ -n "$cleanup_dir" ]; then
    rm -rf "$cleanup_dir"
  fi
}
trap cleanup EXIT HUP INT TERM

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd -P || pwd -P)
source_dir=""

if [ -f "$script_dir/../skills/$skill_name/SKILL.md" ]; then
  source_dir="$script_dir/../skills/$skill_name"
elif [ -f "skills/$skill_name/SKILL.md" ]; then
  source_dir="skills/$skill_name"
else
  cleanup_dir=$(mktemp -d "${TMPDIR:-/tmp}/agent-runtime-skill.XXXXXX")
  archive="$cleanup_dir/repo.tar.gz"
  archive_url="${repo_url%/}/archive/refs/heads/${ref}.tar.gz"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$archive_url" -o "$archive"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$archive" "$archive_url"
  else
    echo "curl or wget is required for remote installation" >&2
    exit 1
  fi

  tar -xzf "$archive" -C "$cleanup_dir"
  skill_md=$(find "$cleanup_dir" -type f -path "*/skills/$skill_name/SKILL.md" | head -n 1)
  if [ -z "$skill_md" ]; then
    echo "could not find skills/$skill_name/SKILL.md in $archive_url" >&2
    exit 1
  fi
  source_dir=$(dirname "$skill_md")
fi

copy_skill() {
  dest=$1
  dest_parent=$(dirname "$dest")
  mkdir -p "$dest_parent"

  src_real=$(CDPATH= cd -- "$source_dir" && pwd -P)
  parent_real=$(CDPATH= cd -- "$dest_parent" && pwd -P)
  dest_real="$parent_real/$(basename "$dest")"

  if [ "$src_real" = "$dest_real" ]; then
    echo "already installed: $dest"
    return
  fi

  tmp_dest="${dest}.tmp.$$"
  rm -rf "$tmp_dest"
  mkdir -p "$tmp_dest"
  (cd "$source_dir" && tar -cf - .) | (cd "$tmp_dest" && tar -xf -)
  rm -rf "$dest"
  mv "$tmp_dest" "$dest"
  echo "installed: $dest"
}

if [ "$install_codex" -eq 1 ]; then
  codex_home="${CODEX_HOME:-$HOME/.codex}"
  copy_skill "$codex_home/skills/$skill_name"
fi

if [ "$install_claude" -eq 1 ]; then
  claude_home="${CLAUDE_HOME:-$HOME/.claude}"
  copy_skill "$claude_home/skills/$skill_name"
fi

cat <<'NEXT'

Next:
  - Restart Codex so it picks up newly installed skills.
  - Claude Code watches existing skill directories, but restart Claude Code if this created ~/.claude/skills for the first time.
  - Invoke in Codex as: $agent-runtime
  - Invoke in Claude Code as: /agent-runtime
NEXT
