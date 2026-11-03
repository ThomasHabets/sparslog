#!/usr/bin/env bash
set -ueo pipefail
unset RUSTFLAGS
cd "$TICKBOX_TEMPDIR/work/ui-sdr"
export CARGO_TARGET_DIR="$TICKBOX_CWD/target/${TICKBOX_BRANCH}.ui-sdr"
exec ./presubmit.sh
