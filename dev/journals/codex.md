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

## 2026-02-19 (commit: pending) — Reachability planning + minimal push/pull integration

### What changed
- Fixed formatting and aligned the tree for CI style gates.
- Consolidated binary modules into thin wrappers that delegate shared logic to `xet_ai_core` (`src/config.rs`, `src/repo.rs`, `src/sync.rs`).
- Added `xet_ai_core::repo` with repo identity/path helpers and git2-based HEAD/branch helpers.
- Added `xet_ai_core::reachability` with:
  - `ReachabilityPlan`
  - CAS access/hydration traits
  - tracing access recorder
  - pointer-cache-aware planner
  - default hash-token hydrator strategy
  - focused unit test using temp repo + fake hydrator over IO abstraction.
- Extended sync manifest builder to support a relpath subset (`build_manifest_for_relpaths`) for minimal manifests.
- Updated CLI push/pull:
  - default push computes reachability and writes minimal manifest, plus remote pointer indexes (`pointers/<sha>.json`, `pointers/HEAD`).
  - fallback `--all-cas` keeps prior allowlisted full CAS sync behavior.
  - pull supports `--all-cas` fallback to fetch allowlisted remote CAS, otherwise uses remote manifest.
- Extended e2e to validate remote pointer index creation and compare default minimal push vs `--all-cas` on a fresh remote.

### Why / design notes
- The thin-wrapper approach removes duplicate “truth” while minimizing churn in CLI command flow.
- Reachability now has an explicit extension point (`PointerHydrator` + `CasFileAccess`) so we can later replace heuristic hydration with true xet-core IO-bound tracing without rewriting planner call sites.
- Minimal manifests are now first-class and can be consumed uniformly by existing push/pull copy+verify logic.

### State of mind / reflections
This iteration felt like a careful compromise between architectural direction and practical correctness risk. I’m satisfied that we now have an explicit reachability planning interface and that command behavior can switch between minimal and `--all-cas` fallback predictably. The major risk I still feel is fidelity of the default pointer hash-token hydrator: it is deterministic and testable, but it remains a heuristic until we complete deeper tracing through xet-core’s actual read paths. I chose to make that risk obvious in structure rather than hiding it in ad-hoc code.

I’m more confident in the maintainability outcome than in the immediate optimality of transferred bytes for all real-world pointer shapes. The upside is we now have clean seams to improve correctness (swap hydrator implementation), testability (pure unit tests with fake hydrators), and performance (cache-aware planning) incrementally.

The tradeoff I kept revisiting was whether to attempt invasive xet-core integration now or stabilize planner interfaces first. Given sprint scope and CI reliability requirements, interface-first felt safer and easier to verify end-to-end. The e2e enhancement gives a concrete signal that default mode is more minimal than full sync, which helps validate product direction while acknowledging we still have room to improve exactness.

## 2026-02-19 (commit: pending) — Core command orchestration + RemoteStore + staged transactions

### What changed
- Added `xet_ai_core::commands` and moved init/push/pull/remote/manifest orchestration into core.
- Simplified CLI so it is mostly clap parsing + delegating into core commands; kept clean/smudge in binary.
- Introduced `RemoteStore` trait and `FilesystemRemoteStore` with atomic write/copy primitives and prefix listing.
- Refactored push/pull to use `RemoteStore` operations instead of direct remote path manipulation.
- Made minimal manifest subset building O(N relpaths) by iterating requested relpaths directly rather than walking full CAS.
- Added default push mode semantics:
  - minimal + validation (`PushMode::MinimalValidate`),
  - full fallback (`--all-cas`),
  - unsafe developer mode (`--minimal-no-validate`).
- Implemented local validation workflow for minimal push using isolated `.xet_ai/validate/<sha>/xet` CAS and representative pointer hydration to sink.
- Added staged transactional publish layout under `tx/<txid>/...` with commit marker `tx/COMMITTED/<txid>`, then live finalize ordering.
- Added `xet-ai remote tx list` command and e2e assertion that tx directories exist on remote.
- Pull now fetches pointer index best-effort into `.xet_ai/pointers/<sha>.json`.

### State of mind / reflections
This pass felt like finally crossing from "feature accumulation" to "system boundaries." The biggest cognitive load was deciding where responsibilities should live: CLI ergonomics versus core orchestration versus backend IO boundaries. The new `RemoteStore` trait is intentionally small and filesystem-biased in this iteration, and I’m aware there is still a gap to a truly streaming/object-store-native backend. But creating an explicit seam now felt more valuable than perfecting every operation shape in one go.

The validation-vs-performance tradeoff is still front of mind. I chose to prioritize correctness by validating minimal plans before publish, even though it adds local work. The fallback-to-all-cas behavior may look conservative, but it is the right safety rail while reachability remains heuristic. I’d rather absorb a temporary performance hit than risk publishing refs that cannot hydrate.

Transactional publish sequencing also improved my confidence: writing staged artifacts and only moving live pointers at the end makes crash behavior significantly less scary. I still think we should eventually add explicit transaction GC and richer telemetry so operators can inspect long-lived staged directories, but the current shape already reduces partial-state risk in a tangible way.

## 2026-02-19 (commit: pending) — Robustness recovery: verified copies, tx semantics, autopull guard

### What changed
- Restored verified/immutable transfer semantics in core store sync paths:
  - added `VerifyPolicy` and verified copy helpers for local<->remote transfers.
  - push/pull store-based sync now verifies size always and hash for small files.
  - existing-destination mismatch is treated as corruption error.
- Fixed transaction marker semantics:
  - `tx/STAGED/<txid>` written after staging artifacts,
  - `tx/PUBLISHED/<txid>` written only after live finalize updates complete.
- Refactored transaction code into explicit `stage_transaction` + `finalize_transaction` helpers and added tests for staged-vs-published behavior and ref immutability before finalize.
- Fixed validation workspace hygiene by using unique run-scoped validation directories under `.xet_ai/validate/<sha>/<uuid>/`.
- Reintroduced smudge auto-pull behavior (attempt-once guarded via env var) by routing through core pull API helper.
- Added explicit remote capability surface (`RemoteCapabilities`) and capability gating for tx listing / list-prefix flows.
- Added unit tests for transfer corruption detection (size and hash mismatch), transaction marker semantics, validation dir uniqueness, and smudge autopull recursion guard behavior.

### State of mind / reflections
This sprint felt like paying down “accidental risk debt” introduced by a fast architecture move. The most uncomfortable part was acknowledging that the system got cleaner structurally while becoming less robust in data integrity paths. Reintroducing verification forced me to rebalance abstractions: the `RemoteStore` trait now stays backend-oriented while correctness checks live in sync orchestration, where manifest metadata is available and policy is explicit.

The transaction marker fix was also a good reminder that naming carries operational meaning. Calling something “committed” too early is worse than no marker because it can mislead debugging and automation. Splitting staged/published states made semantics clearer and made tests much easier to reason about.

I still see brittle edges: validation currently hydrates a representative pointer, which is safer than no validation but still probabilistic relative to full commit coverage. The current fallback strategy contains blast radius, but future work should improve planner fidelity and validation breadth without tanking UX. Even with those tradeoffs, this revision feels notably more trustworthy than the previous one.

## 2026-02-19 (commit: pending) — Alpha UX follow-through: plan-only dry run + logging polish

### What changed
- Added `xet-ai push --plan-only` as a dry-run mode that computes reachability and manifest summary without writing remote CAS/manifests/refs.
- Standardized warning/error log prefixes to `xet-ai:` in CLI smudge and push lock/minimal-mode warning paths.
- Improved doctor guidance by checking whether `.xet_ai_repo_id` is tracked in git and emitting actionable warning when missing.
- Added a lightweight smudge pass-through parser sanity check in doctor.
- Updated README useful commands and limitations wording to include `--plan-only` and clarify tx-gc scope.

### State of mind / reflections
This pass was less about adding brand-new capability and more about making existing behavior safer and easier to reason about in everyday use. The largest bug-risk I wanted to eliminate was accidental remote mutation during planning/debugging, so `--plan-only` now exits before transfer/publish. That gives developers a practical way to inspect minimal-vs-all-cas behavior without touching shared state.

I also tightened logging consistency because alpha usability is often won or lost in debugging sessions. Consistent `xet-ai:` prefixes make warnings easier to grep and less ambiguous in filter-heavy git command output.

Doctor remains intentionally pragmatic: it catches high-impact misconfigurations without trying to be exhaustive. The new repo-id tracked check addresses a real operational footgun for cloned repos while keeping the command fast.

## 2026-02-20 (commit: pending) — Plan-only semantics hardened + doctor signal quality

### What changed
- Added `push --validate` so `push --plan-only` can stay fast and side-effect-free by default while still supporting explicit plan-time validation when requested.
- Refactored push flow so plan/manifest computation occurs before any remote store construction/locking, and plan-only exits before transfer/stage/finalize paths.
- Reworked doctor into a structured report with meaningful checks: git filter keys (`clean/smudge/required`), tracked-pattern detection in `.gitattributes`, repo-id presence + tracked status, shared config/default-remote visibility, filesystem remote reachability/health hints, and local CAS accessibility + size summary.
- Removed the previous fake JSON parse check that did not exercise real behavior.
- Added regression tests for plan-only no-remote-mutation and validation-call skipping, plus a focused doctor-report test for missing-basics signaling.

### State of mind / reflections
This iteration was mostly about trust boundaries and user expectation management. `--plan-only` sounds like a dry-run, so any hidden hydration/validation work or remote touching feels like a contract violation even if technically "safe." I wanted the code shape to make that guarantee obvious, not accidental.

The doctor cleanup had a similar theme: diagnostics should reflect operator reality, not implementation internals. The previous parser check was logically true but practically useless. Swapping it for concrete repo/remote/filter checks makes the command much more actionable during onboarding and outage debugging.

I also felt the tension between “fast tests” and “high-confidence behavior.” The added tests intentionally stay narrow and local (temp dirs + git2), but they now pin down the two core promises that matter for this sprint: plan-only should not mutate remotes, and plan-only should not trigger validation unless explicitly asked.

## 2026-02-20 (commit: pending) — Acceptance blocker hardening: verified copy coverage + finalize invariants

### What changed
- Expanded fast verification/immutability coverage with additional unit tests for pull-side corruption detection (same-size hash mismatch and truncation size mismatch) and non-overwrite semantics when destination is already correct.
- Strengthened transaction-finalize invariants by splitting finalize into explicit phases and adding a precondition guard that refuses live ref/HEAD updates if final manifest/pointer payloads are not present.
- Added crash-safety tests for staged-but-not-finalized pushes and for attempted live-ref updates without finalized payloads.
- Added validation workspace setup helper and a dedicated test proving stale files in previous validate directories are not used by new validation runs.
- Hardened plan-only regression tests so they assert true no-mutation at remote root/repo-id scope and walk the remote tree for forbidden artifact path creation.
- Installed and executed `shellcheck` locally so script linting now participates in the full local acceptance run.

### State of mind / reflections
This pass felt like moving from “feature correctness” toward “operational certainty.” Most of the risky bugs here are not obvious in happy-path demos; they show up after crashes, retries, or silent data drift. I wanted the tests to encode those failure modes directly so future refactors don’t regress safety promises.

The transaction work was the most important conceptual cleanup: live refs should be downstream of published payload existence, never the other way around. Enforcing that in code structure (not just comments) made the crash model much easier to reason about.

I also noticed how easy it is for tests to pass while asserting the wrong directory shape (plan-only remote writes under repo-id prefixes). Tightening those assertions made me more confident the dry-run contract is genuinely enforced, not accidentally untested.

## 2026-02-20 (commit: pending) — Final alpha-flake sweep: cwd independence + stronger non-mutation proofs

### What changed
- Refactored push orchestration to introduce an internal `push_in_repo(&Path, ...)` entrypoint so tests can execute push logic without mutating global process CWD.
- Updated plan-only tests to call `push_in_repo` directly and removed `std::env::set_current_dir(...)` usage from command tests, eliminating CWD race/flakiness in parallel test execution.
- Tightened plan-only non-mutation assertions to include remote-root emptiness checks and explicit repo-id absence checks so tests cannot pass when writes occur under nested remote prefixes.
- Normalized newline string literals to explicit escaped forms (`"\n"`, `b"...\n"`) in finalize/staging and related tests to remove formatting artifacts and keep semantics explicit.
- Kept single test module structure per file and retained trust-invariant coverage for verified transfer behavior, crash-safe transaction publishing, and validation workspace hygiene.

### State of mind / reflections
This patch felt like removing the last class of “false confidence” bugs. The hardest issues were not algorithmic—they were test assumptions that could accidentally mask regressions (especially around remote root layout and global cwd side effects). By making tests less magical and more explicit, the safety claims now feel materially stronger.

I’m particularly happy with moving tests off global CWD mutation. Even when protected by a mutex, global state in tests tends to become a latent source of flakes over time. `push_in_repo` gives us a cleaner seam that is both more testable and more maintainable.

At this stage, the codebase feels much closer to alpha-pilot quality: not because every edge case is solved, but because the highest-risk invariants are now encoded in fast, focused tests that should fail loudly if anything drifts.

## 2026-02-20 (commit: pending) — Streaming remote verification + strict minimal-manifest missing-object handling

### What changed
- Extended `RemoteStore` with lightweight metadata (`stat`) and true streaming reads (`open_reader` returning `Read + Send`) so verification logic no longer depends on full-buffer remote reads.
- Updated `FilesystemRemoteStore` to implement streaming reads via `File::open` and metadata via `fs::metadata().len()`.
- Reworked remote verification to use `stat` for size checks always and stream-hash only when object size is below `HASH_VERIFY_LIMIT`.
- Added `missing_required_relpaths(...)` and made subset manifest building fail on missing requested relpaths instead of silently skipping them.
- Updated push minimal-mode behavior to handle missing required CAS objects safely:
  - minimal+validate: explicit warning + fallback to all-cas,
  - minimal-no-validate: hard error to prevent inconsistent publish,
  - plan-only: reports missing-required count in summary without touching remote.
- Added regression tests for streaming verification behavior (large files avoid reader consumption, small files stream-hash bytes), and for strict missing-relpath handling in minimal-manifest inputs.

### State of mind / reflections
This was the most important scalability fix left for alpha. Reading remote blobs into memory for verification is fine for toy data and disastrous for real CAS workloads. Moving verification to metadata + streaming is not just a perf tweak; it is the abstraction we need before HTTP/S3 backends are credible.

I also wanted to close the “silent drop” loophole in minimal manifest generation. Silent skipping of required relpaths is dangerous because it creates success-shaped failures. Converting that into explicit fallback/error paths makes operator behavior predictable and safer.

Remaining risk for alpha is mostly around backend diversity: filesystem behavior is now much healthier, but network remotes will still need careful tuning for latency and retries. The key upside is that the trait surface now supports those implementations without reworking correctness logic again.
