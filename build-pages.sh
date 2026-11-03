#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
(cd ui && ./build-local.sh profiling)
(cd ui-sdr && ./build-local.sh profiling)
mkdir -p pages-dist
rm -rf pages-dist/ui pages-dist/ui-sdr
cp -R ui/web-dist pages-dist/ui
cp -R ui-sdr/web-dist pages-dist/ui-sdr
cp pages-index.html pages-dist/index.html
touch pages-dist/.nojekyll
