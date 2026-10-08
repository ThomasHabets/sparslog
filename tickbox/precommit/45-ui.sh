#!/usr/bin/env bash
set -ueo pipefail
# Receiver RUSTFLAGS would override the UI's shared-memory WASM configuration.
unset RUSTFLAGS
cd "$TICKBOX_TEMPDIR/work/ui"
export CARGO_TARGET_DIR="$TICKBOX_CWD/target/${TICKBOX_BRANCH}.ui"
exec ./presubmit.sh
