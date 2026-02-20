# xet-ai Tutorial (Alpha)

This tutorial walks through a complete local workflow with two clones and a filesystem remote.

## Prerequisites
- `git`
- Rust toolchain (if building from source)
- `xet-ai` available on your `PATH`

## 1) Create repository and initialize xet-ai

```bash
git init myrepo
cd myrepo
xet-ai init --init-config --track "*.bin" "*.parquet"
```

This creates `.xet_ai` local state and updates tracking patterns.

## 2) Add a local filesystem remote

```bash
xet-ai remote add origin /tmp/xet-remote
xet-ai remote set-default origin
```

## 3) Commit data and push

```bash
# add or modify tracked files
python - <<'PY'
from pathlib import Path
Path('sample.bin').write_bytes(b'hello xet-ai\n')
PY

git add .
git commit -m "initial dataset"
xet-ai push origin --ref main
```

## 4) Inspect state

```bash
xet-ai status
xet-ai remote refs origin
xet-ai manifest list
```

## 5) Run a non-mutating dry run

```bash
xet-ai push origin --ref main --plan-only
# optional validation during planning
xet-ai push origin --ref main --plan-only --validate
```

## 6) Pull from another clone

```bash
cd ..
git clone myrepo myrepo-b
cd myrepo-b
xet-ai init
xet-ai pull origin --ref main
```

## 7) Health checks

```bash
xet-ai doctor
```

## Troubleshooting tips
- If minimal mode warns about missing required CAS inputs, use default validated mode or `--all-cas`.
- If remote paths are wrong, verify `.xet_ai.toml` and `.xet_ai/config.local.toml`.
- Use `xet-ai remote tx list origin` to inspect transaction state.
