# xet-ai

`xet-ai` is a local-only MVP for CAS-first blob storage in Git using HuggingFace `xet-core`.

## Build

```bash
cargo build --release
```

## Dependency pinning

`xet-core` dependencies are pinned to a specific git revision in `Cargo.toml`.

## Repo identity

`xet-ai init` creates a tracked `.xet_ai_repo_id` file if missing. Commit this file so all clones share the same remote namespace.

## Shared vs local config

- Shared (tracked): `.xet_ai.toml`
- Local overlay (ignored): `.xet_ai/config.local.toml`

Effective config is merged with local values overriding shared values.

```bash
xet-ai init --init-config
xet-ai remote add origin /tmp/xet-ai-remote         # writes shared config
xet-ai remote add scratch /tmp/private --local      # writes local overlay
xet-ai remote set-default origin                     # shared default_remote
```

Local-only setting `auto_pull_on_smudge` can be set in `.xet_ai/config.local.toml`.

## Manifest + refs remote model

Push writes CAS objects and manifest refs under:

- `<remote>/<repo_id>/xet/...`
- `<remote>/<repo_id>/manifests/<git_sha>.json`
- `<remote>/<repo_id>/manifests/HEAD`
- `<remote>/<repo_id>/refs/<refname>`

`push` updates both manifest HEAD and `refs/<refname>` (default refname is current git branch; falls back to `HEAD` in detached state).

`pull` resolves data in this order:
- `--ref <40-hex-sha>` => direct manifest SHA
- `--ref <name>` => resolve remote `refs/<name>`
- no `--ref` => remote `manifests/HEAD`

Useful commands:

```bash
xet-ai remote refs origin
xet-ai remote head origin
xet-ai manifest list
xet-ai manifest show <sha>
xet-ai manifest verify <sha>
```

## Smudge auto-pull (opt-in)

If local config enables `auto_pull_on_smudge=true` and a `default_remote` exists, smudge will attempt one automatic pull+retry on missing CAS data.

## Diagnostics

```bash
xet-ai debug cas-tree
```

## Run end-to-end demo

```bash
bash scripts/e2e.sh
```
