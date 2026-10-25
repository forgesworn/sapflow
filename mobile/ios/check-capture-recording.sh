#!/bin/bash
# SPDX-License-Identifier: MIT
# Fresh current-host SDK + generated native recording matrix. No devices.
set -euo pipefail
if [ "$#" -ne 0 ]; then echo 'Usage: check-capture-recording.sh (generated input only)' >&2; exit 2; fi
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
exec python3 -B "$sapflow_root/mobile/ios/check-capture-recording.py"
