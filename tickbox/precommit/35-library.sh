#!/usr/bin/env bash
set -ueo pipefail
cd "$TICKBOX_TEMPDIR/work"
export CARGO_TARGET_DIR="$TICKBOX_CWD/target/${TICKBOX_BRANCH}.library"
cargo test --locked --no-default-features --lib
cargo check --locked --no-default-features --lib --target wasm32-unknown-unknown
