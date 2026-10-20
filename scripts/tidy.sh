#!/usr/bin/env bash
# Show how much space the build is using; with --clean, empty target/.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ ! -d target ]]; then
  echo "target/: none"
  exit 0
fi

du -sh target | awk '{print "target/: " $1}'

if [[ "${1:-}" == "--clean" ]]; then
  cargo clean
  echo "target/: cleaned"
fi
