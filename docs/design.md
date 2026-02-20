# xet-ai Design Overview (Alpha)

## Goals
- Keep large-file workflows git-friendly using pointer files and local CAS.
- Provide safe push/pull behavior with integrity checks and transactional publish.
- Keep backend integration abstract via `RemoteStore` while starting with filesystem remotes.

## Architecture
`xet-ai` is split into a thin CLI (`src/main.rs`) and a core library (`crates/xet_ai_core`).

### Core modules
- `config`: shared/local config merge and effective runtime options.
- `git`: minimal wrappers for repository discovery and commit/tree reads.
- `pointers`: pointer index construction from commit content.
- `reachability`: minimal-CAS planning from representative hydration.
- `remote`: remote backend abstraction + filesystem implementation.
- `sync`: verified transfer, manifest generation/validation, and transaction publish.
- `commands`: end-to-end command orchestration.

## Data model
- CAS objects are addressed by relpaths under allowlisted top-level dirs.
- Manifests contain `(relpath, size, sha256)` entries plus repo/ref metadata.
- Pointer indices map repository paths to serialized `XetFileInfo`.

## Safety invariants
- Transfer verification: always check size, and check hash for objects at/below `HASH_VERIFY_LIMIT`.
- Immutability semantics: existing destination content mismatch is treated as corruption.
- Transaction semantics: publish writes staged artifacts first, then advances live refs/head only when final payloads are present.
- Plan-only mode: must not mutate remote state.

## Current limitations
- Only filesystem remotes are implemented in alpha.
- Reachability for minimal mode is heuristic and may fall back to all-CAS.
- Remote GC and operational telemetry are intentionally minimal.

## Planned next steps
1. Add network/object-store backends (HTTP/S3-like) over `RemoteStore`.
2. Improve retries/timeouts and observability for remote operations.
3. Broaden minimal planning/validation fidelity while preserving safe fallbacks.
4. Extend transaction lifecycle tooling (inspection and cleanup policy controls).
