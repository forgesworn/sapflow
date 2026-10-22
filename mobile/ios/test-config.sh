#!/bin/bash
# Pure creator configuration tests. No app build, Keychain, network or devices.
set -euo pipefail
if [ "${1:-}" = "--verify-info" ]; then
  shift
  python3 - "$@" <<'PYTHON'
import json, os, plistlib, stat, sys
from pathlib import Path
def require(condition):
    if not condition:
        raise ValueError()
try:
    mode = sys.argv[1]
    require(mode in ("disabled", "enabled"))
    require(len(sys.argv) == (3 if mode == "disabled" else 6))
    path = Path(sys.argv[2])
    require(path.name == "Info.plist")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as stream:
        require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode))
        data = stream.read(65_537)
    require(0 < len(data) <= 65_536)
    info = plistlib.loads(data)
    require(type(info) is dict)
    keys = ["SapflowV4vEnabled", "SapflowV4vApiOrigin", "SapflowV4vPlaybackOrigin", "SapflowV4vPlaybackPrefix"]
    expected = ["NO", "", "", ""] if mode == "disabled" else ["YES", *sys.argv[3:]]
    require(all(type(info.get(key)) is str and info[key] == value for key, value in zip(keys, expected)))
    print(json.dumps({"builtInfoVerified": True, "expectedMode": mode, "routingValuesPrinted": False}))
except (IndexError, OSError, ValueError, RecursionError, plistlib.InvalidFileException):
    print("Built creator configuration verification failed", file=sys.stderr)
    raise SystemExit(1)
PYTHON
  exit
fi
if [ "$#" -ne 0 ]; then
  echo 'Usage: test-config.sh [--verify-info disabled Info.plist | --verify-info enabled Info.plist apiOrigin playbackOrigin playbackPrefix]' >&2
  exit 2
fi
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-config-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/V4vCreatorConfigurationTests.swift" > "$sapflow_scratch/main.swift"
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = V4vCreatorConfigurationTests.defaultTestSuite
suite.run()
guard let result = suite.testRun, result.executionCount == 14, result.hasSucceeded else { exit(1) }
SWIFT
swiftc -swift-version 5 -warnings-as-errors -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
  -Xlinker -rpath -Xlinker "$sapflow_frameworks" -Xlinker -rpath -Xlinker "$sapflow_testlibs" \
  "$sapflow_root/mobile/ios/Sources/V4vContracts.swift" "$sapflow_root/mobile/ios/Sources/SrtDestination.swift" \
  "$sapflow_root/mobile/ios/Sources/DestinationProfile.swift" "$sapflow_root/mobile/ios/Sources/V4vCreatorConfiguration.swift" \
  "$sapflow_scratch/main.swift" -o "$sapflow_scratch/config-tests"
SAPFLOW_CONFIG_TEST_ROOT="$sapflow_root" "$sapflow_scratch/config-tests"
