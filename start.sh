#!/usr/bin/env bash
# Builds and opens HT Plugin Manager. Logs stay in this terminal; Ctrl-C quits.
# It runs from the signed bundle, so the keychain keeps trusting it across
# rebuilds and the GitHub login survives.
#   ./start.sh
set -euo pipefail
cd "$(dirname "$0")"
scripts/bundle.sh >/dev/null
exec "target/HT Plugin Manager.app/Contents/MacOS/ht-plugin-manager" "$@"
