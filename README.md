# xet-ai

`xet-ai` is an offline-first alpha for CAS-backed large-file workflows in Git using `xet-core` locally.

## Alpha install

### Option 1: local install script

```bash
bash scripts/install-local.sh
```

### Option 2: manual

```bash
cargo build --release
cp target/release/xet-ai ~/.local/bin/xet-ai
```

## Quickstart (Alpha)

Machine A:

```bash
git init myrepo
cd myrepo
xet-ai init --init-config --track "*.bin" "*.parquet"
xet-ai remote add origin /tmp/xet-remote
xet-ai remote set-default origin

# create or edit files
git add .
git commit -m "initial"
xet-ai push origin --ref main
```

Machine B:

```bash
git clone <machine-a-repo-path> myrepo
cd myrepo
xet-ai init
xet-ai doctor
xet-ai pull origin --ref main
git checkout -f -- .
```

## Useful commands

```bash
xet-ai status
xet-ai doctor
xet-ai track "*.bin" "*.parquet"
xet-ai push origin --ref main --plan-only
# optional: validate plan by hydrating representative data
xet-ai push origin --ref main --plan-only --validate
xet-ai remote refs origin
xet-ai remote tx list origin
xet-ai remote tx gc origin --older-than 120
xet-ai manifest list
xet-ai manifest verify <sha>
```

## Documentation

- [Design overview](docs/design.md)
- [Tutorial](docs/tutorial.md)

## Shared vs local config

- Shared/tracked: `.xet_ai.toml`
- Local overlay: `.xet_ai/config.local.toml`

Local values override shared values. `auto_pull_on_smudge` is local-only and opt-in.

## Safety behavior

- Push is transactional (staged then published markers).
- Pull/push perform verified transfer checks (size always, hash for small files).
- Default minimal push validates a representative hydration and falls back to all-CAS if validation fails.
- `--plan-only` does not validate by default; add `--validate` to run validation during planning.
- `--minimal-no-validate` exists for developers and prints a warning.

## Known limitations (Alpha)

- Reachability planning is heuristic and may fall back to all-CAS.
- Filesystem remote backend only.
- No full remote GC of published objects yet (transaction cleanup only via `remote tx gc`).
- No HuggingFace Hub / network backend integration yet.

## Development checks

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test --workspace
cargo build --release
shellcheck scripts/e2e.sh
bash scripts/e2e.sh
```
