#!/usr/bin/env sh
set -eu

REPO="${MULTINODE_REPO:-pufanyi/multinode-controller}"
VERSION="${MULTINODE_VERSION:-latest}"
PREFIX="${PREFIX:-}"
BIN_DIR="${BIN_DIR:-}"
INSTALL_SKILL=0

usage() {
  cat <<'EOF'
Install multinode-controller release binaries.

Usage:
  install.sh [options]

Options:
  --version <tag>    Release tag to install, for example v0.0.1. Defaults to latest.
  --repo <owner/repo>
                    GitHub repository. Defaults to pufanyi/multinode-controller.
  --prefix <dir>     Install binaries into <dir>/bin.
  --bin-dir <dir>    Install binaries into an explicit directory.
  --with-skill       Also install the agent-runtime skill for Codex and Claude Code.
  -h, --help         Show this help.

Environment:
  MULTINODE_VERSION  Same as --version.
  MULTINODE_REPO     Same as --repo.
  PREFIX             Same as --prefix.
  BIN_DIR            Same as --bin-dir.
EOF
}

die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || die "--version requires a value"
      VERSION="$2"
      shift 2
      ;;
    --version=*)
      VERSION="${1#*=}"
      shift
      ;;
    --repo)
      [ "$#" -ge 2 ] || die "--repo requires a value"
      REPO="$2"
      shift 2
      ;;
    --repo=*)
      REPO="${1#*=}"
      shift
      ;;
    --prefix)
      [ "$#" -ge 2 ] || die "--prefix requires a value"
      PREFIX="$2"
      shift 2
      ;;
    --prefix=*)
      PREFIX="${1#*=}"
      shift
      ;;
    --bin-dir)
      [ "$#" -ge 2 ] || die "--bin-dir requires a value"
      BIN_DIR="$2"
      shift 2
      ;;
    --bin-dir=*)
      BIN_DIR="${1#*=}"
      shift
      ;;
    --with-skill | --skill)
      INSTALL_SKILL=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

need curl
need sed
need tar
need uname
need mktemp

case "$(uname -s)" in
  Linux) ;;
  *) die "prebuilt releases are Linux-only" ;;
esac

case "$(uname -m)" in
  x86_64 | amd64)
    TARGET="x86_64-unknown-linux-gnu"
    ;;
  aarch64 | arm64)
    TARGET="aarch64-unknown-linux-gnu"
    ;;
  *)
    die "unsupported CPU architecture: $(uname -m)"
    ;;
esac

if [ -z "$BIN_DIR" ]; then
  if [ -n "$PREFIX" ]; then
    BIN_DIR="$PREFIX/bin"
  else
    BIN_DIR="$HOME/.local/bin"
  fi
fi

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

if [ "$VERSION" = "latest" ]; then
  RELEASE_API="https://api.github.com/repos/$REPO/releases/latest"
else
  case "$VERSION" in
    v*) RELEASE_TAG="$VERSION" ;;
    *) RELEASE_TAG="v$VERSION" ;;
  esac
  RELEASE_API="https://api.github.com/repos/$REPO/releases/tags/$RELEASE_TAG"
fi

RELEASE_JSON="$TMP_DIR/release.json"
curl -fsSL "$RELEASE_API" -o "$RELEASE_JSON" || die "failed to read release metadata from $RELEASE_API"

TAG="$(sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' "$RELEASE_JSON" | head -n 1)"
[ -n "$TAG" ] || die "release metadata did not include a tag_name"

ARCHIVE="multinode-controller-$TAG-$TARGET.tar.gz"
ASSET_URL="$(sed -n 's|.*"browser_download_url":[[:space:]]*"\([^"]*'"$ARCHIVE"'\)".*|\1|p' "$RELEASE_JSON" | head -n 1)"
[ -n "$ASSET_URL" ] || die "release $TAG does not provide $ARCHIVE"

printf 'Installing multinode-controller %s for %s\n' "$TAG" "$TARGET"
curl -fL "$ASSET_URL" -o "$TMP_DIR/$ARCHIVE"
tar -xzf "$TMP_DIR/$ARCHIVE" -C "$TMP_DIR"

DIST_DIR="$TMP_DIR/multinode-controller-$TAG-$TARGET"
[ -d "$DIST_DIR/bin" ] || die "release archive is missing bin/"

mkdir -p "$BIN_DIR"
for bin in agent-runtime agentctl agent-coordinator agent-worker; do
  [ -x "$DIST_DIR/bin/$bin" ] || die "release archive is missing executable: $bin"
  if command -v install >/dev/null 2>&1; then
    install -m 755 "$DIST_DIR/bin/$bin" "$BIN_DIR/$bin"
  else
    cp "$DIST_DIR/bin/$bin" "$BIN_DIR/$bin"
    chmod 755 "$BIN_DIR/$bin"
  fi
done

if [ "$INSTALL_SKILL" -eq 1 ]; then
  [ -d "$DIST_DIR/skills/agent-runtime" ] || die "release archive is missing skills/agent-runtime"
  for root in "${CODEX_HOME:-$HOME/.codex}" "${CLAUDE_HOME:-$HOME/.claude}"; do
    mkdir -p "$root/skills"
    rm -rf "$root/skills/agent-runtime"
    cp -R "$DIST_DIR/skills/agent-runtime" "$root/skills/agent-runtime"
  done
fi

printf 'Installed binaries to %s\n' "$BIN_DIR"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) printf 'Add %s to PATH before running agent-runtime or agentctl.\n' "$BIN_DIR" ;;
esac

"$BIN_DIR/agent-runtime" --version
"$BIN_DIR/agentctl" --version
