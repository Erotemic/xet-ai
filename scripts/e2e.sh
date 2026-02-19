#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build --release
export PATH="$ROOT/target/release:$PATH"

extract_bytes() {
  awk '/copied [0-9]+ files \([0-9]+ bytes\)/ {gsub(/[^0-9]/,"",$4); print $4}' | tail -n1
}

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

remote_dir="$tmpdir/remote"
machineA="$tmpdir/machineA"
machineB="$tmpdir/machineB"
repoA="$machineA/project_A"
repoB="$machineB/project_B"
mkdir -p "$remote_dir" "$machineA" "$machineB"

git init "$repoA" >/dev/null
cd "$repoA"
git config user.email "xet-ai@example.com"
git config user.name "xet-ai"

xet-ai init
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

git add .
git commit -m "add big file" >/dev/null

xet-ai remote add origin "$remote_dir"
push1_output="$(xet-ai push origin)"
echo "$push1_output"
bytes_push1="$(echo "$push1_output" | extract_bytes)"

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
push2_output="$(xet-ai push origin)"
echo "$push2_output"
bytes_push2="$(echo "$push2_output" | extract_bytes)"

if (( bytes_push2 >= 20 * 1024 * 1024 )); then
  echo "Push #2 copied too many bytes: $bytes_push2" >&2
  exit 1
fi

sha_a="$(sha256sum big.bin | awk '{print $1}')"

cd "$machineB"
git clone "$repoA" "$repoB" >/dev/null

cd "$repoB"
# clone should not hydrate without local CAS; pointer should still be present
if ! python3 - <<'PY'
import json
from pathlib import Path
p = Path('big.bin').read_text()
obj = json.loads(p)
assert 'hash' in obj and 'file_size' in obj
PY
then
  echo "Expected pointer pass-through in clone before pull" >&2
  exit 1
fi

xet-ai init
xet-ai remote add origin "$remote_dir"
xet-ai pull origin >/dev/null
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
