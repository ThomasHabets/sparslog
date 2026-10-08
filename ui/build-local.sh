#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
mode="${1:-dev}"
case "$mode" in dev|release) ;; *) echo 'Usage: ./build-local.sh [dev|release]' >&2; exit 1 ;; esac
wasm-pack build . --target web --out-dir web-dist "--$mode" -- --locked
cargo metadata --format-version 1 --locked > web-dist/metadata.json
python3 package-assets.py
rm web-dist/metadata.json
