# xet-ai

`xet-ai` is a local-only MVP for CAS-first blob storage in Git using HuggingFace `xet-core`.

## Build

```bash
cargo build --release
```

## Run end-to-end demo

```bash
bash scripts/e2e.sh
```

The script simulates two repositories (two machines), pushes/pulls CAS data via a filesystem remote, and verifies hydration and content identity.

## Minimal usage

```bash
git init
xet-ai init

# add a large .bin file tracked by git filter
python3 - <<'PY'
with open('big.bin','wb') as f:
    f.write(b'a' * (4 * 1024 * 1024))
PY

git add big.bin
git commit -m "add large blob"

xet-ai remote add origin /tmp/xet-ai-remote
xet-ai push origin

# in another clone/repo:
xet-ai pull origin
git checkout -f -- big.bin
```
