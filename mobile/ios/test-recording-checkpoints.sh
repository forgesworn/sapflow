#!/bin/bash
# Root-only real SDK/generated-byte filesystem gate. Consumes an explicit
# source-bound cached host witness; never runs Cargo/bindgen/full packaging.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
: "${SAPFLOW_CHECKPOINT_HOST_WITNESS:?Set the root-owned host-build receipt path}"
: "${SAPFLOW_CHECKPOINT_HOST_WITNESS_SHA256:?Set its independently reviewed SHA256}"
: "${SAPFLOW_CHECKPOINT_APPLE_RECEIPT:?Set the qualified Apple wrapper/header receipt path}"
: "${SAPFLOW_CHECKPOINT_APPLE_RECEIPT_SHA256:?Set its independently reviewed SHA256}"
: "${SAPFLOW_CHECKPOINT_EVIDENCE:?Set a NEW retained evidence directory}"
mkdir "$SAPFLOW_CHECKPOINT_EVIDENCE"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-checkpoint-owner-XXXXXX")"
sapflow_finalise() {
    sapflow_exit=$?
    trap - EXIT
    python3 - "$sapflow_root" "$sapflow_scratch" "$SAPFLOW_CHECKPOINT_EVIDENCE" "$sapflow_exit" <<'PY'
from pathlib import Path
import hashlib,json,shutil,subprocess,sys
root,scratch,evidence=map(Path,sys.argv[1:4]);exit_code=int(sys.argv[4])
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
before=evidence/'before.local.json';same=False;head_same=False;problem=None
try:
    saved=json.loads(before.read_text())
    same=all((root/name).is_file() and sha(root/name)==value for name,value in saved['allInputs'].items()) and all(Path(name).is_file() and sha(Path(name))==value for name,value in saved['proofs'].items())
    head_same=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()==saved['head']
except BaseException as error:problem=str(error)
assert scratch.parent==root/'target' and scratch.name.startswith('sapflow-checkpoint-owner-') and not scratch.is_symlink()
shutil.rmtree(scratch)
removed=not scratch.exists()
logs={p.name:sha(p) for p in evidence.glob('*.log')}
result={'passed':exit_code==0 and same and head_same and removed,'exit':exit_code,'sourceUnchanged':same,'headUnchangedDuringRun':head_same,
 'taskOwnedScratchRemoved':removed,'logs':logs,'failure':problem,'expectedRealSdkTests':16,
 'scope':'Real shared SDK + generated-byte checkpoint installation/ownership only; no decoder/native AAC/device/recovery/normal Stop/2s proof.'}
(evidence/'receipt.local.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
if not result['passed']:raise SystemExit(1)
PY
    sapflow_proof_exit=$?
    if [ "$sapflow_exit" -ne 0 ]; then exit "$sapflow_exit"; fi
    exit "$sapflow_proof_exit"
}
trap sapflow_finalise EXIT
mkdir -p "$sapflow_scratch/modules" "$sapflow_scratch/tmp"
python3 - "$sapflow_root" "$SAPFLOW_CHECKPOINT_EVIDENCE" <<'PY'
from pathlib import Path
import hashlib,json,os,platform,subprocess,sys
root,evidence=map(Path,sys.argv[1:3]);sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert sys.platform=='darwin' and platform.machine()=='arm64','This witnessed driver requires the ARM64 macOS host'
witness=Path(os.environ['SAPFLOW_CHECKPOINT_HOST_WITNESS']).resolve();apple=Path(os.environ['SAPFLOW_CHECKPOINT_APPLE_RECEIPT']).resolve()
assert sha(witness)==os.environ['SAPFLOW_CHECKPOINT_HOST_WITNESS_SHA256']
assert sha(apple)==os.environ['SAPFLOW_CHECKPOINT_APPLE_RECEIPT_SHA256']
host=json.loads(witness.read_text());package=json.loads(apple.read_text())
assert host['passed'] and host['sourceUnchanged'] and host['childrenStopped'] and host['exit']==0
assert len(host['inputs'])==118 and package['passed'] and package['sourceUnchanged'] and package['childrenStopped']
# HEAD may differ from host witness because of later docs-only commits. Every
# actual 118 Rust/build input and actual host dylib must still match exactly.
inputs=dict(host['inputs']);inputs.update(host['outputs'])
assert 'target/debug/libsapflow_ffi.dylib' in host['outputs']
selected=['target/native-packages/apple/Sources/Sapflow/Sapflow.swift',
 'target/native-packages/apple/SapflowFFI.xcframework/macos-arm64/Headers/SapflowFFI.h',
 'target/native-packages/apple/SapflowFFI.xcframework/macos-arm64/Headers/module.modulemap']
for name in selected:inputs[name]=package['outputHashes'][name]
for name,value in inputs.items():assert sha(root/name)==value,'Stale source/runtime: '+name
used=['mobile/ios/Sources/CaptureOrientation.swift','mobile/ios/Sources/CaptureProfile.swift','mobile/ios/Sources/RecordingStorage.swift',
 'mobile/ios/Sources/RecordingCatalogue.swift','mobile/ios/Sources/RecordingCheckpointOwner.swift',
 'mobile/ios/Tests/RecordingCatalogueTests.swift','mobile/ios/Tests/RecordingCheckpointOwnerTests.swift',
 'mobile/ios/test-recordings.sh','mobile/ios/test-recording-checkpoints.sh']
for name in used:inputs[name]=sha(root/name)
value={'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),
 'hostWitnessSHA256':sha(witness),'appleReceiptSHA256':sha(apple),'proofs':{str(witness):sha(witness),str(apple):sha(apple)},'allInputs':inputs,
 'hostRuntimeSHA256':inputs['target/debug/libsapflow_ffi.dylib'],'expectedRealSdkTests':16}
(evidence/'before.local.json').write_text(json.dumps(value,indent=2)+'\n')
PY
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
sapflow_headers="$sapflow_root/target/native-packages/apple/SapflowFFI.xcframework/macos-arm64/Headers"
swiftc -swift-version 5 -warnings-as-errors -parse-as-library -emit-module -emit-object -module-name Sapflow \
    -module-cache-path "$sapflow_scratch/modules" -I "$sapflow_headers" \
    -Xcc "-fmodule-map-file=$sapflow_headers/module.modulemap" \
    "$sapflow_root/target/native-packages/apple/Sources/Sapflow/Sapflow.swift" \
    -emit-module-path "$sapflow_scratch/Sapflow.swiftmodule" -o "$sapflow_scratch/Sapflow.o" \
    > "$SAPFLOW_CHECKPOINT_EVIDENCE/sdk-compile.log" 2>&1
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/RecordingCheckpointOwnerTests.swift" > "$sapflow_scratch/main.swift"
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = XCTestSuite(name: "Real shared SDK checkpoint file ownership")
suite.addTest(RecordingCheckpointOwnerTests.defaultTestSuite)
suite.run()
guard let result = suite.testRun, result.executionCount == 16, result.hasSucceeded else { exit(1) }
SWIFT
swiftc -swift-version 5 -warnings-as-errors -module-cache-path "$sapflow_scratch/modules" \
    -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
    -I "$sapflow_scratch" -I "$sapflow_headers" -Xcc "-fmodule-map-file=$sapflow_headers/module.modulemap" \
    -L "$sapflow_root/target/debug" -lsapflow_ffi \
    -Xlinker -rpath -Xlinker "$sapflow_frameworks" -Xlinker -rpath -Xlinker "$sapflow_testlibs" \
    -Xlinker -rpath -Xlinker "$sapflow_root/target/debug" \
    "$sapflow_root/mobile/ios/Sources/CaptureOrientation.swift" "$sapflow_root/mobile/ios/Sources/CaptureProfile.swift" \
    "$sapflow_root/mobile/ios/Sources/RecordingStorage.swift" "$sapflow_root/mobile/ios/Sources/RecordingCatalogue.swift" \
    "$sapflow_root/mobile/ios/Sources/RecordingCheckpointOwner.swift" "$sapflow_scratch/main.swift" "$sapflow_scratch/Sapflow.o" \
    -o "$sapflow_scratch/checkpoint-owner-tests" > "$SAPFLOW_CHECKPOINT_EVIDENCE/test-compile.log" 2>&1
TMPDIR="$sapflow_scratch/tmp" "$sapflow_scratch/checkpoint-owner-tests" > "$SAPFLOW_CHECKPOINT_EVIDENCE/test-run.log" 2>&1
