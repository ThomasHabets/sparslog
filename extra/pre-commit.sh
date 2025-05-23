#!/usr/bin/env bash
set -ueo pipefail
ROOT_DIR="$(pwd)"
exec tickbox --dir "$ROOT_DIR/tickbox/precommit/" --cwd "$ROOT_DIR"
