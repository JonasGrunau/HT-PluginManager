#!/usr/bin/env bash
# Builds and opens HurenTohn. Logs stay in this terminal; Ctrl-C quits.
# It runs from the signed bundle, so the keychain keeps trusting it across
# rebuilds and the GitHub login survives.
#   ./start.sh
set -euo pipefail
cd "$(dirname "$0")"
scripts/bundle.sh >/dev/null
exec "target/HurenTohn Plugin Manager.app/Contents/MacOS/hurentohn" "$@"
