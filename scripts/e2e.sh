#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build --release
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  if [[ "$CARGO_TARGET_DIR" = /* ]]; then
    target_dir="$CARGO_TARGET_DIR"
  else
    target_dir="$ROOT/$CARGO_TARGET_DIR"
  fi
else
  target_dir="$ROOT/target"
fi
export PATH="$target_dir/release:$PATH"

extract_bytes() {
  awk '/copied [0-9]+ files \([0-9]+ bytes\)/ {gsub(/[^0-9]/,"",$4); print $4}' | tail -n1
}

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

remote_dir="$tmpdir/remote"
remote_full_dir="$tmpdir/remote-full"
machineA="$tmpdir/machineA"
machineB="$tmpdir/machineB"
repoA="$machineA/project_A"
repoB="$machineB/project_B"
mkdir -p "$remote_dir" "$remote_full_dir" "$machineA" "$machineB"

git init "$repoA" >/dev/null
cd "$repoA"
git config user.email "xet-ai@example.com"
git config user.name "xet-ai"
git branch -m main

xet-ai init --init-config
xet-ai track "*.bin"
if [[ ! -f .xet_ai_repo_id ]]; then
  echo "missing .xet_ai_repo_id" >&2
  exit 1
fi

python3 - <<'PY'
from pathlib import Path
size = 32 * 1024 * 1024
chunk = 1024 * 1024
with Path('big.bin').open('wb') as f:
    for i in range(size // chunk):
        b = bytes(((i + j) % 251 for j in range(chunk)))
        f.write(b)
PY

xet-ai remote add origin "$remote_dir"
xet-ai remote set-default origin
git add .
git commit -m "add big file and shared config" >/dev/null
sha1="$(git rev-parse HEAD)"
repo_id="$(tr -d '\n' < .xet_ai_repo_id)"

push1_output="$(xet-ai push origin --ref main)"
echo "$push1_output"
manifest_head="$remote_dir/$repo_id/manifests/HEAD"
manifest_sha1="$remote_dir/$repo_id/manifests/$sha1.json"
pointer_sha1="$remote_dir/$repo_id/pointers/$sha1.json"
ref_main="$remote_dir/$repo_id/refs/main"

[[ -f "$manifest_head" ]] || { echo "missing remote HEAD" >&2; exit 1; }
[[ -f "$manifest_sha1" ]] || { echo "missing remote manifest sha1" >&2; exit 1; }
[[ -f "$ref_main" ]] || { echo "missing remote refs/main" >&2; exit 1; }
[[ -f "$pointer_sha1" ]] || { echo "missing remote pointer index sha1" >&2; exit 1; }
[[ "$(tr -d '\n' < "$manifest_head")" == "$sha1" ]] || { echo "HEAD mismatch" >&2; exit 1; }
[[ "$(tr -d '\n' < "$ref_main")" == "$sha1" ]] || { echo "refs/main mismatch" >&2; exit 1; }
[[ -f ".xet_ai/manifests/$sha1.json" ]] || { echo "missing local cached manifest after push1" >&2; exit 1; }
[[ ! -f "$remote_dir/$repo_id/locks/push.lock" ]] || { echo "push lock left behind" >&2; exit 1; }

tx_dir="$remote_dir/$repo_id/tx"
[[ -d "$tx_dir" ]] || { echo "missing remote tx dir" >&2; exit 1; }
if [[ -z "$(find "$tx_dir" -mindepth 1 -maxdepth 1 -type d | head -n1)" ]]; then
  echo "expected at least one transaction directory" >&2
  exit 1
fi

python3 - <<'PY'
from pathlib import Path
chunk = 1024 * 1024
p = Path('big.bin')
with p.open('r+b') as f:
    f.seek(-chunk, 2)
    block = bytes(((255 - (j % 251)) % 256 for j in range(chunk)))
    f.write(block)
PY

git add big.bin
git commit -m "update tail" >/dev/null
sha2="$(git rev-parse HEAD)"
push2_output="$(xet-ai push origin --ref main)"
echo "$push2_output"
bytes_push2="$(echo "$push2_output" | extract_bytes)"

[[ -f "$remote_dir/$repo_id/manifests/$sha2.json" ]] || { echo "missing remote manifest sha2" >&2; exit 1; }
[[ -f "$remote_dir/$repo_id/pointers/$sha2.json" ]] || { echo "missing remote pointer index sha2" >&2; exit 1; }
[[ "$(tr -d '\n' < "$manifest_head")" == "$sha2" ]] || { echo "HEAD mismatch after push2" >&2; exit 1; }
[[ "$(tr -d '\n' < "$ref_main")" == "$sha2" ]] || { echo "refs/main mismatch after push2" >&2; exit 1; }
[[ -f ".xet_ai/manifests/$sha2.json" ]] || { echo "missing local cached manifest after push2" >&2; exit 1; }
[[ ! -f "$remote_dir/$repo_id/locks/push.lock" ]] || { echo "push lock left behind after push2" >&2; exit 1; }

if (( bytes_push2 >= 20 * 1024 * 1024 )); then
  echo "Push #2 copied too many bytes: $bytes_push2" >&2
  exit 1
fi

sha_a="$(sha256sum big.bin | awk '{print $1}')"

# Compare minimal default push against all-cas fallback on a fresh remote.
xet-ai remote add full "$remote_full_dir"
full_push_output="$(xet-ai push full --ref main --all-cas)"
echo "$full_push_output"
manifest_default_entries="$(python3 - <<PY
import json
from pathlib import Path
print(len(json.loads(Path('$remote_dir/$repo_id/manifests/$sha2.json').read_text())['entries']))
PY
)"
manifest_full_entries="$(python3 - <<PY
import json
from pathlib import Path
print(len(json.loads(Path('$remote_full_dir/$repo_id/manifests/$sha2.json').read_text())['entries']))
PY
)"
if (( manifest_default_entries > manifest_full_entries )); then
  echo "Expected default manifest entries ($manifest_default_entries) <= all-cas entries ($manifest_full_entries)" >&2
  exit 1
fi

cd "$machineB"
git clone "$repoA" "$repoB" >/dev/null

cd "$repoB"
xet-ai init
xet-ai doctor
if ! python3 - <<'PY'
import json
from pathlib import Path
p = Path('big.bin').read_text()
assert p.strip().startswith('{')
obj = json.loads(p)
assert 'hash' in obj and 'file_size' in obj
PY
then
  echo "Expected pointer pass-through in clone before pull" >&2
  exit 1
fi

xet-ai pull origin --ref main
xet-ai manifest verify "$sha2" >/dev/null
git checkout -f -- big.bin

size_b="$(stat -c%s big.bin)"
if [[ "$size_b" != "$((32 * 1024 * 1024))" ]]; then
  echo "Unexpected size in repoB: $size_b" >&2
  exit 1
fi

sha_b="$(sha256sum big.bin | awk '{print $1}')"
if [[ "$sha_a" != "$sha_b" ]]; then
  echo "SHA mismatch: $sha_a vs $sha_b" >&2
  exit 1
fi

echo "E2E OK"
