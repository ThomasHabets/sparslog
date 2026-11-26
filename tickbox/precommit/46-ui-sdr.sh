#!/usr/bin/env bash
set -ueo pipefail
unset RUSTFLAGS
cd "$TICKBOX_TEMPDIR/work/ui-sdr"
export CARGO_TARGET_DIR="$TICKBOX_CWD/target/${TICKBOX_BRANCH}.ui-sdr"

failed=0
check() {
    if ! "$@"; then failed=1; fi
}
check cargo +nightly fmt -- --check
check env RUSTDOCFLAGS='-D warnings' cargo +nightly doc --locked --target wasm32-unknown-unknown --no-deps
check cargo +nightly test --locked
check cargo +nightly clippy --locked --target wasm32-unknown-unknown --lib -- -W clippy::pedantic -D warnings
check ./build-local.sh
exit "$failed"
