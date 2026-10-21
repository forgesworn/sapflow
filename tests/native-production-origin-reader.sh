#!/bin/bash
# Exercise the actual production decoder using generated origin media only.
set -euo pipefail
if [[ $# -ne 2 ]]; then
  echo "Usage: bash tests/native-production-origin-reader.sh <new-target-output> <origin-binary>" >&2
  exit 2
fi
sapflow_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$sapflow_root"
sapflow_output="$1"
sapflow_origin="$2"
python3 - "$sapflow_output" <<'PY'
import os,sys
from pathlib import Path
output=Path(sys.argv[1]).resolve()
assert output.is_relative_to(Path('target').resolve()) and not output.exists()
os.umask(0o077)
output.mkdir(parents=True)
PY
sapflow_scratch="$(mktemp -d "$sapflow_root/target/native-production-reader-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
cp tests/native/OriginProductionDecode.swift "$sapflow_scratch/main.swift"
swiftc -swift-version 5 -warnings-as-errors -O \
  mobile/ios/Sources/SrtDestination.swift mobile/ios/Sources/V4vContracts.swift \
  mobile/ios/Sources/OriginObservationPolicy.swift mobile/ios/Sources/OriginMediaHTTP.swift \
  mobile/ios/Sources/OriginMediaDecoder.swift "$sapflow_scratch/main.swift" \
  -framework AVFoundation -framework VideoToolbox -framework AudioToolbox \
  -framework CoreMedia -framework CoreVideo -o "$sapflow_scratch/production-decode" \
  > "$sapflow_output/compile.local.log" 2>&1
python3 - "$sapflow_output" "$sapflow_scratch/production-decode" <<'PY'
import hashlib,json,sys
from pathlib import Path
files=['mobile/ios/Sources/SrtDestination.swift','mobile/ios/Sources/V4vContracts.swift',
       'mobile/ios/Sources/OriginObservationPolicy.swift','mobile/ios/Sources/OriginMediaHTTP.swift',
       'mobile/ios/Sources/OriginMediaDecoder.swift','tests/native/OriginProductionDecode.swift',
       'tests/native-production-origin-reader.sh','tests/native-origin-reader.py']
manifest={'files':[{'path':p,'sha256':hashlib.sha256(Path(p).read_bytes()).hexdigest()} for p in files],
          'decoderBinarySha256':hashlib.sha256(Path(sys.argv[2]).read_bytes()).hexdigest(),
          'inputProvenance':'newly generated test pattern and sine only','deviceAccess':False}
(Path(sys.argv[1])/'source-hashes.local.json').write_text(json.dumps(manifest,indent=2)+'\n')
PY
python3 -B tests/native-origin-reader.py --origin-binary "$sapflow_origin" \
  --reader-executable "$sapflow_scratch/production-decode" --output "$sapflow_output/generated-check" \
  --require-cleanup-complete
