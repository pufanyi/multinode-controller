# Releasing

This project publishes GitHub Releases with prebuilt Linux binaries. crates.io
publishing is intentionally out of scope for now.

## Release shape

Each release tag, for example `v0.0.1`, should provide:

- `multinode-controller-v0.0.1-x86_64-unknown-linux-gnu.tar.gz`
- `checksums.txt`

The archive contains:

- `bin/agent-runtime`
- `bin/agentctl`
- `bin/agent-coordinator`
- `bin/agent-worker`
- `examples/*.yaml`
- `skills/agent-runtime`
- `README.md`, `LICENSE`, and `install.sh`

Users install with:

```bash
curl -fsSL https://raw.githubusercontent.com/pufanyi/multinode-controller/main/install.sh | sh
```

Pinned install:

```bash
curl -fsSL https://raw.githubusercontent.com/pufanyi/multinode-controller/main/install.sh | sh -s -- --version v0.0.1
```

## Maintainer checklist

1. Set crate versions to the release version.
2. Run:

   ```bash
   cargo fmt --all -- --check
   cargo check --workspace --locked
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   ```

3. Build a local release package smoke test:

   ```bash
   cargo build --workspace --release --locked --target x86_64-unknown-linux-gnu
   ```

4. Commit the release prep.
5. Create and push an annotated tag:

   ```bash
   git tag -a v0.0.1 -m "Release v0.0.1"
   git push origin main
   git push origin v0.0.1
   ```

6. Wait for the `Release` workflow to publish the GitHub Release.
7. Verify install into a temporary directory:

   ```bash
   tmp="$(mktemp -d)"
   curl -fsSL https://raw.githubusercontent.com/pufanyi/multinode-controller/main/install.sh | sh -s -- --version v0.0.1 --bin-dir "$tmp/bin"
   "$tmp/bin/agent-runtime" --version
   "$tmp/bin/agentctl" --version
   ```
