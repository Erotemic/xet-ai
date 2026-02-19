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

## 2026-02-19 04:47:47 +0000

### What changed
- `src/sync.rs`
  - Added persistent hash cache support at `.xet_ai/hash_cache.json` with atomic writes and schema `relpath -> { size, sha256 }`.
  - Updated manifest generation to reuse cached hashes when file size matches, and only rehash on cache miss/size change.
  - Added `HashProvider` abstraction and a cache reuse unit test using a counting hasher.
  - Hardened atomic temp naming with pid + UUID suffix and improved temp-file cleanup on failure paths.
  - Added local manifest verification helper for `manifest verify`.
- `src/main.rs`
  - Added `manifest` command group: `list`, `show <sha>`, `verify <sha>`.
  - Updated `push` to cache manifest locally in `.xet_ai/manifests/<sha>.json` in addition to remote writes.
  - Updated push manifest build call to use the new hash cache path.
- `scripts/e2e.sh`
  - Added assertions that push creates local cached manifests in repoA.
  - Added `xet-ai manifest verify <sha>` check in repoB after pull.
- `.github/workflows/ci.yml`
  - Added ShellCheck installation and lint step for `scripts/e2e.sh`.

### Why / design decisions
- Rehashing all CAS files every push does not scale; a local hash cache keyed by relpath+size keeps repeated pushes fast while still remaining simple and deterministic.
- I used size as the cache validity key because CAS objects are immutable in expected operation; this keeps lookup cheap.
- Hashing remains exact for cache misses and all manifest entries still carry full SHA256 so downstream verification remains strong.
- Manifest commands were added to improve developer observability/debugging with minimal runtime complexity.

### How you tested
- `export CARGO_TARGET_DIR=/tmp/xet-target && cargo fmt` (pass)
- `export CARGO_TARGET_DIR=/tmp/xet-target && cargo build --release` (pass)
- `export CARGO_TARGET_DIR=/tmp/xet-target && cargo test` (pass; includes new cache reuse unit test)
- `export CARGO_TARGET_DIR=/tmp/xet-target && cargo clippy -- -D warnings` (pass)
- `export CARGO_TARGET_DIR=/tmp/xet-target && bash scripts/e2e.sh` (pass; includes manifest checks and verify command)

### What remains / next steps
- Add an explicit benchmark script to quantify push-time improvements from hash cache reuse.
- Consider pruning stale hash cache entries no longer present in local CAS.
- Add tests for `manifest show` formatting and `manifest verify` failure cases (missing files / hash mismatch).

### State of mind / reflections
I felt more confident in this iteration than the previous one because the direction was clear: address a concrete scaling concern without tearing apart the protocol changes that were already working. The hash cache was a good fit for this kind of incremental improvement: it gives a large practical win on repeat pushes while preserving the manifest format and remote layout unchanged. That alignment made the work feel low-risk from a compatibility standpoint. I also felt good about extending the testing story in a way that isn’t purely end-to-end: adding a counting-hasher unit test gave me a focused guardrail that the cache is actually being used and not just written.

I was less certain about cache invalidation edge cases. Using relpath+size as validity is pragmatic and probably right for immutable CAS, but it is still an assumption. If any unexpected mutability sneaks in due to bugs or external file tampering, the cache could serve stale hashes. I mitigated this indirectly by keeping manifest verification capabilities and by preserving size/hash checks during copy paths, but I still think this is one of the more brittle areas over the long term. Another risk area was lock-like files under the xorb lookup DB; I had to be careful that manifest scope and copy semantics don’t accidentally include volatile files.

What I think I handled especially well was keeping the changes cohesive: caching, inspection commands, e2e assertions, and CI linting all support the same operational goal (faster + more observable pushes). If I were doing this again, I would likely start by defining and documenting a formal “cache trust model” first (what invariants we assume and how we detect violations), then implement code around that explicitly. That would make future hardening—like optional full rehash modes or cache scrubbing—more straightforward.

### Open questions / hypotheses
- Hypothesis: in repos with stable CAS contents across commits, hash-cache hit rates should be >95% and push CPU time should drop materially.
- Open question: should we opportunistically remove cache entries for relpaths not seen in the latest manifest to keep cache size bounded?

## 2026-02-19 15:13:39 +0000

### What changed
- `src/config.rs`
  - Replaced single local config model with layered config loading:
    - shared tracked config `.xet_ai.toml`
    - local overlay `.xet_ai/config.local.toml`
  - Added effective merge semantics where local values override shared values.
  - Added config fields `default_remote` and `auto_pull_on_smudge` support in the merged model.
  - Added unit test for merge behavior.
- `src/main.rs`
  - Updated `init` to support `--init-config` and avoid creating `.xet_ai.toml` by default.
  - Updated remote commands:
    - `remote add ... [--local]`
    - `remote set-default ... [--local]`
    - `remote refs <remote>`
    - `remote head <remote>`
  - Added optional remote name resolution via `default_remote` for push/pull/remote-ref operations.
  - Extended push/pull to Git-like ref model:
    - push writes `refs/<refname>` (default branch name or `HEAD` detached)
    - pull resolves `--ref` as SHA or named ref; defaults to manifest HEAD when unspecified.
  - Added smudge auto-pull behavior gated by config (`auto_pull_on_smudge`) with one-shot recursion guard env var.
- `src/sync.rs`
  - Added ref utilities:
    - `is_sha1_hex`
    - `resolve_ref_or_sha`
    - `list_remote_refs`
  - Added push locking implementation with filesystem lock file:
    - acquire via create_new
    - stale lock detection
    - `--force-lock` handling
    - lock metadata write and RAII release.
  - Added unit tests for ref resolution and lock acquire/release.
- `src/repo.rs`
  - Added `git_current_branch_short()` helper for default push ref naming.
- `scripts/e2e.sh`
  - Updated workflow to commit shared `.xet_ai.toml` and `.xet_ai_repo_id` in repoA.
  - Added deterministic branch setup (`main`) and push via `--ref main`.
  - Added checks for `refs/main`, manifests HEAD, and lock cleanup.
  - Removed repoB remote reconfiguration; repoB now uses cloned shared config.
  - Added pull by ref (`--ref main`) and post-pull manifest verify.
- `README.md`
  - Documented shared/local config layering, ref model (`refs/<refname>` + `manifests/HEAD`), and smudge auto-pull behavior.

### Why / design decisions
- The shared tracked config moves UX toward DVC-like team workflows where clone consumers inherit remote settings and defaults.
- Keeping local overlay allows machine-specific preferences (especially auto-pull behavior) without polluting committed repo config.
- Remote refs add Git-like semantics and make manifest selection explicit and scriptable beyond a single HEAD pointer.
- Push locking was implemented as a coarse filesystem guard for practical safety in concurrent local-remote usage while remaining simple enough for MVP constraints.

### How you tested
- `export CARGO_TARGET_DIR=/tmp/xet-target && cargo fmt` (pass)
- `export CARGO_TARGET_DIR=/tmp/xet-target && cargo build --release` (pass)
- `export CARGO_TARGET_DIR=/tmp/xet-target && cargo test` (pass; includes config merge/ref resolution/lock tests)
- `export CARGO_TARGET_DIR=/tmp/xet-target && cargo clippy -- -D warnings` (pass)
- `export CARGO_TARGET_DIR=/tmp/xet-target && bash scripts/e2e.sh` (pass; validates shared config cloning, refs/main, pull by ref, lock cleanup)

### What remains / next steps
- Add explicit commands to toggle local `auto_pull_on_smudge` from CLI instead of editing TOML manually.
- Expand tests for stale-lock timing edge cases and lock break behavior under simulated races.
- Consider richer remote ref namespaces (`refs/heads/*`, `refs/tags/*`) and validation rules.

### State of mind / reflections
I felt this sprint was more architectural than the previous one, and I noticed myself balancing two competing instincts: keep the code compact versus make the UX genuinely closer to what users expect from DVC/Git-like data tooling. The config layering change felt especially important because it shifts the product from single-user setup toward collaborative defaults. I was fairly confident in the merge strategy itself (local overrides shared), but less confident about all of the ergonomic edge cases around command defaults and where users expect state to live. The decision to avoid creating `.xet_ai.toml` unless explicitly requested felt right from a cleanliness perspective, but I know some users may prefer magical setup and could find this conservative behavior surprising.

The push lock design also forced a tradeoff: I chose a coarse lock that is intentionally simple and local-filesystem friendly. I’m confident it reduces obvious race conditions, but I’m aware it is not bulletproof against all failure modes (e.g., abrupt process deaths around rename boundaries, clock/mtime oddities, or NFS quirks). I still think this is a good compromise for the MVP’s filesystem-only scope. The smudge auto-pull path made me think hardest about safety; recursion/re-entrancy bugs are subtle here, so I deliberately used a one-shot env guard and kept fallback behavior unchanged when anything goes wrong.

What I think went especially well is that the e2e script now exercises the same “shared config + ref-based pull” path we want real users to rely on. If I had extra time in this sprint, I would invest in targeted tests for auto-pull failure branches and lock staleness handling under mocked times. Those are the parts that still feel most brittle and most likely to surface surprises later.

### Open questions / hypotheses
- Hypothesis: teams will mostly want shared `default_remote`, but local override frequency will still be high for `auto_pull_on_smudge`.
- Open question: should lock stale duration be configurable per remote in future iterations?
