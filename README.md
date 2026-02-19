# xet-ai

`xet-ai` is a local-only MVP for CAS-first blob storage in Git using HuggingFace `xet-core`.

## Build

```bash
cargo build --release
```

## Dependency pinning

`xet-core` dependencies are pinned to a specific git revision in `Cargo.toml`.
To update intentionally, change the `rev = "..."` values for `data`, `file_reconstruction`, and `xet_runtime`, then run:

```bash
cargo update
cargo build --release
```

## Repo identity

`xet-ai init` creates a tracked `.xet_ai_repo_id` file if missing. Commit this file so all clones share the same remote namespace.

## Manifest-based push/pull

Push writes CAS objects and a manifest ref structure under:

- `<remote>/<repo_id>/xet/...`
- `<remote>/<repo_id>/manifests/<git_sha>.json`
- `<remote>/<repo_id>/manifests/HEAD`

Manifest JSON stores `repo_id`, `git_sha`, `total_bytes`, and per-file CAS metadata (`relpath`, `size`, `sha256`).

Pull reads `manifests/HEAD` by default (or `--ref <sha>`) and syncs only files listed in that manifest.

## Diagnostics

```bash
xet-ai debug cas-tree
```

Prints top-level `.xet_ai/xet/*` directories and INCLUDED/excluded status according to sync allowlist policy.

## Run end-to-end demo

```bash
bash scripts/e2e.sh
```

The script simulates two machines with different clone directory names, validates manifest creation/HEAD updates, verifies pointer pass-through before pull, and validates hydration + checksum parity after pull.
