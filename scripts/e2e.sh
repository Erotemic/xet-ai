#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build --release
export PATH="$ROOT/target/release:$PATH"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

remote_dir="$tmpdir/remote"
machineA="$tmpdir/machineA"
machineB="$tmpdir/machineB"
repoA="$machineA/project"
repoB="$machineB/project"
mkdir -p "$remote_dir" "$machineA" "$machineB"

git init "$repoA" >/dev/null
cd "$repoA"
git config user.email "xet-ai@example.com"
git config user.name "xet-ai"

xet-ai init

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
xet-ai push origin
remote_size1="$(du -sb "$remote_dir" | awk '{print $1}')"

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
xet-ai push origin
remote_size2="$(du -sb "$remote_dir" | awk '{print $1}')"

if (( remote_size2 - remote_size1 >= 16 * 1024 * 1024 )); then
  echo "Delta too large: $((remote_size2 - remote_size1))" >&2
  exit 1
fi

sha_a="$(sha256sum big.bin | awk '{print $1}')"

cd "$machineB"
git clone "$repoA" "$repoB" >/dev/null

cd "$repoB"
xet-ai init
xet-ai remote add origin "$remote_dir"
xet-ai pull origin
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
