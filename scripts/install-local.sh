#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build --release

dest_dir="${HOME}/.local/bin"
mkdir -p "$dest_dir"
cp target/release/xet-ai "$dest_dir/xet-ai"
chmod +x "$dest_dir/xet-ai"

echo "Installed xet-ai to $dest_dir/xet-ai"
echo "Ensure $dest_dir is on PATH"
