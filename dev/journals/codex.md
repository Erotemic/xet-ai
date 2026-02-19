## 2026-02-19 04:11:55 +0000

### What changed
- `src/main.rs`
  - Added manifest-based push/pull flow.
  - Added `pull --ref <sha>`.
  - Made clean/smudge `--path` optional.
  - Changed clean stdin fallback to spool stdin to `.xet_ai/tmp/` temp file and stream from disk.
  - Updated push/pull output lines to include manifest and verification info.
- `src/sync.rs`
  - Added manifest models `ManifestEntry` and `Manifest`.
  - Added manifest generation over allowlisted CAS files with full-file SHA256 for each entry.
  - Added atomic file write for manifest/HEAD.
  - Added atomic verified copy routine (temp + rename, size check always, hash check for files <= 8 MiB).
  - Added manifest-driven push and pull sync helpers.
  - Added allowlist unit test.
- `src/repo.rs`
  - Added reusable git stdout capture helper and `git_head_sha`.
- `scripts/e2e.sh`
  - Added checks for remote manifests/HEAD and `manifests/<sha>.json` after push.
  - Added check for local manifest cache after pull.
  - Kept clone-before-pull pointer pass-through check and post-pull hydration checks.
- `README.md`
  - Documented `.xet_ai_repo_id` requirement and manifest-based remote behavior.
- `.github/workflows/ci.yml`
  - Added CI workflow for fmt, clippy, tests, release build, and e2e.
- `Cargo.toml`
  - Added `sha2` dependency.

### Why / design decisions
- Manifest refs keyed by git SHA make remote data retrieval deterministic and avoid expensive remote scans on pull.
- Atomic writes and rename-based publishing reduce risk of partial object files and partially-written manifests.
- Keeping CAS immutable (no overwrite) plus verification catches corruption early.
- Spooling stdin for clean avoids unbounded RAM usage for large blobs.

### How I tested
- `cargo fmt`
- `cargo build --release`
- `cargo test`
- `cargo clippy -- -D warnings`
- `bash scripts/e2e.sh`

### What remains / next steps
- Add explicit user-facing manifest introspection commands (`manifest list/show`) if desired.
- Add more failure-path tests for manifest corruption and missing remote CAS entries.
- Consider stronger durability semantics around directory fsync if crash-consistency guarantees are needed.
