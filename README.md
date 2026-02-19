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

## Run end-to-end demo

```bash
bash scripts/e2e.sh
```

The script simulates two machines with different clone directory names, verifies pointer pass-through before pull, then validates hydration after pull and checksum equality.

## Minimal usage

```bash
git init
xet-ai init
# commit stable repo identity so clones share one remote namespace
git add .xet_ai_repo_id
git commit -m "track xet-ai repo id"

# add a large .bin file tracked by git filter
python3 - <<'PY'
with open('big.bin','wb') as f:
    f.write(b'a' * (4 * 1024 * 1024))
PY

git add big.bin
git commit -m "add large blob"

xet-ai remote add origin /tmp/xet-ai-remote
xet-ai push origin

# inspect what CAS directories are synced
xet-ai debug cas-tree

# in another clone/repo:
xet-ai pull origin
git checkout -f -- big.bin
```
