#!/usr/bin/env node
// Dedicated cold auth package. Never overwrite the existing media SDK package.
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync, cpSync, rmSync, existsSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const consumerSources = ['V4vContracts', 'V4vCreatorContracts', 'SrtDestination', 'V4vSessionEngine', 'V4vJournal', 'V4vControlClient', 'V4vLoginContracts', 'V4vSignetLoginOwner', 'V4vNativeSignet', 'OriginObservationPolicy', 'OriginSDKConfirmation'];
const mode = process.argv[2];
if (!['check', 'apple'].includes(mode) || process.argv.length !== 3) throw new Error('choose check or apple; native slices require a separate reviewed build gate');
const apple = join(root, 'target/native-packages/signet-apple');
if (process.env.CARGO_TARGET_DIR && resolve(process.env.CARGO_TARGET_DIR) !== join(root, 'target')) throw new Error('reuse existing target');
const output = join(root, mode === 'apple' ? 'target/signet-auth-apple-headers-v2' : 'target/signet-auth-host');
const resultPath = join(output, 'result.local.json');
const archiveReceipt = join(output, 'archive.local.json');
const appleArchive = join(root, 'target/native-packages/signet-auth-apple.zip');
// A previous invocation's receipt is historical, never this invocation's success.
rmSync(resultPath, {force: true}); rmSync(archiveReceipt, {force: true});
if (mode === 'apple' && (existsSync(apple) || existsSync(appleArchive))) throw new Error('refuse overwriting existing SignetAuth package/archive');
const staging = join(output, 'staging');
if (existsSync(staging)) throw new Error('staging already exists; inspect prior run before removal');
mkdirSync(staging, { recursive: true });
function run(cmd, args, options = {}) {
 const result = spawnSync(cmd, args, { cwd: root, stdio: 'inherit', env: { ...process.env, CARGO_BUILD_JOBS: '2' }, ...options });
 if (result.status !== 0) throw new Error(`${cmd} failed (${result.status ?? result.signal})`);
 return result.stdout;
}
let success = false;
try {
 run('python3', ['scripts/verify-signet-sources.py']);
 run('cargo', ['build', '--offline', '-j2', '-p', 'sapflow-signet-ffi']);
 const transport = join(root, 'crates/sapflow-signet-ffi');
 const pkg = mode === 'apple' ? apple : join(staging, 'SignetAuth');
 mkdirSync(join(pkg, 'Sources/SignetAuthFFI'), { recursive: true }); mkdirSync(join(pkg, 'Sources/SignetAuth'));
 cpSync(join(transport, 'swift/SignetAuth.swift'), join(pkg, 'Sources/SignetAuth/SignetAuth.swift'));
 cpSync(join(transport, 'include/SignetAuthFFI.h'), join(pkg, 'Sources/SignetAuthFFI/SignetAuthFFI.h'));
 writeFileSync(join(pkg, 'Sources/SignetAuthFFI/module.modulemap'), 'module SignetAuthFFI { header "SignetAuthFFI.h" export * link "sapflow_signet_ffi" }\n');
 writeFileSync(join(pkg, 'Package.swift'), '// swift-tools-version: 5.9\nimport PackageDescription\nlet package = Package(name: "SignetAuth", platforms: [.macOS(.v12)], products: [.library(name: "SignetAuth", targets: ["SignetAuth"])], targets: [.systemLibrary(name: "SignetAuthFFI"), .target(name: "SignetAuth", dependencies: ["SignetAuthFFI"])])\n');
 if (mode === 'apple') {
   // Xcode merges static XCFramework headers into one include directory.
   // Keep the original map name/module/relative header inside a unique module
   // directory so another SDK's flat module.modulemap remains untouched.
   const headersRoot = join(staging, 'headers');
   const headers = join(headersRoot, 'SignetAuthFFI'); mkdirSync(headers, {recursive: true});
   cpSync(join(transport, 'include/SignetAuthFFI.h'), join(headers, 'SignetAuthFFI.h'));
   cpSync(join(transport, 'include/module.modulemap'), join(headers, 'module.modulemap'));
   const libraries = [];
   for (const target of ['aarch64-apple-darwin', 'aarch64-apple-ios', 'aarch64-apple-ios-sim']) {
     run('cargo', ['build', '--offline', '--release', '-j2', '-p', 'sapflow-signet-ffi', '--target', target], {env: {...process.env, CARGO_BUILD_JOBS: '2', MACOSX_DEPLOYMENT_TARGET: '12.0', IPHONEOS_DEPLOYMENT_TARGET: '16.0'}});
     libraries.push('-library', join(root, 'target', target, 'release/libsapflow_signet_ffi.a'), '-headers', headersRoot);
   }
   run('xcodebuild', ['-create-xcframework', ...libraries, '-output', join(pkg, 'SignetAuthFFI.xcframework')]);
   rmSync(join(pkg, 'Sources/SignetAuthFFI'), {recursive: true});
   writeFileSync(join(pkg, 'Package.swift'), '// swift-tools-version: 5.9\nimport PackageDescription\nlet package = Package(name: "SignetAuth", platforms: [.iOS(.v16), .macOS(.v12)], products: [.library(name: "SignetAuth", targets: ["SignetAuth"])], targets: [.binaryTarget(name: "SignetAuthFFI", path: "SignetAuthFFI.xcframework"), .target(name: "SignetAuth", dependencies: ["SignetAuthFFI"])])\n');
   cpSync(join(root, 'LICENSE'), join(pkg, 'LICENSE'));
   cpSync(join(root, 'crates/sapflow-signet-ffi/THIRD_PARTY_LICENSES.txt'), join(pkg, 'THIRD_PARTY_LICENSES.txt'));
 }

 if (mode === 'apple') {
   const sdk = run('xcrun', ['--sdk', 'iphoneos', '--show-sdk-path'], {stdio: 'pipe', encoding: 'utf8'}).trim();
   // The Swift clang-link driver also needs the selected SDK in its environment.
   // Without this, Xcode16 can link an iOS target against the host SDK sysroot.
   const iosOptions = {env: {...process.env, SDKROOT: sdk}};
   const modules = join(staging, 'ios16-modules'); mkdirSync(modules);
   const authHeaders = join(pkg, 'SignetAuthFFI.xcframework/ios-arm64/Headers');
   const mediaFramework = join(root, 'target/native-packages/apple/SapflowFFI.xcframework/ios-arm64');
   const mediaHeaders = join(mediaFramework, 'Headers');
   const flags = ['-swift-version', '5', '-warnings-as-errors', '-target', 'arm64-apple-ios16.0', '-sdk', sdk];
   for (const [name, source, headers] of [['SignetAuth', join(pkg, 'Sources/SignetAuth/SignetAuth.swift'), authHeaders], ['Sapflow', join(root, 'target/native-packages/apple/Sources/Sapflow/Sapflow.swift'), mediaHeaders]]) {
     run('swiftc', [...flags, '-parse-as-library', '-emit-module', '-emit-object', '-module-name', name, '-I', headers, '-emit-module-path', join(modules, name+'.swiftmodule'), '-o', join(modules, name+'.o'), source], iosOptions);
   }
   run('swiftc', [...flags, '-typecheck', '-I', modules, '-I', authHeaders, '-I', mediaHeaders, ...consumerSources.map(name => join(root, 'mobile/ios/Sources', name+'.swift'))], iosOptions);
   const probe = join(modules, 'LinkProbe.swift');
   writeFileSync(probe, 'import Sapflow\nimport SignetAuth\npublic func coldAuthAndMediaLinkProbe() throws -> UInt32 { let owner = try NativeSignet.pairBunker(uri: "bunker://79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798?relay=wss://relay.example.invalid", deadlineMs: 1000); owner.requestClose(); return sdkAbiVersion() }\n');
   run('swiftc', [...flags, '-emit-library', '-I', modules, '-I', authHeaders, '-I', mediaHeaders, probe, join(modules, 'SignetAuth.o'), join(modules, 'Sapflow.o'), join(pkg, 'SignetAuthFFI.xcframework/ios-arm64/libsapflow_signet_ffi.a'), join(mediaFramework, 'libsapflow_ffi.a'), '-o', join(modules, 'libAuthMediaLinkProbe.dylib')], iosOptions);
 }
 const consumer = join(staging, 'Consumer'); mkdirSync(join(consumer, 'Sources/Consumer'), { recursive: true });
 cpSync(join(root, 'tests/native/SignetConsumer.swift'), join(consumer, 'Sources/Consumer/main.swift'));
 for (const name of consumerSources) {
   cpSync(join(root, 'mobile/ios/Sources', name+'.swift'), join(consumer, 'Sources/Consumer', name+'.swift'));
 }
 writeFileSync(join(consumer, 'Package.swift'), `// swift-tools-version: 5.9\nimport PackageDescription\nlet package = Package(name: "Consumer", platforms: [.macOS(.v12)], dependencies: [.package(path: ${JSON.stringify(pkg)}), .package(path: ${JSON.stringify(join(root, 'target/native-packages/apple'))})], targets: [.executableTarget(name: "Consumer", dependencies: [.product(name: "SignetAuth", package: ${JSON.stringify(mode === 'apple' ? 'signet-apple' : 'SignetAuth')}), .product(name: "Sapflow", package: "apple")])])\n`);
 run('swift', ['run', '--package-path', consumer, '--scratch-path', join(output, 'swift-build'), '-j', '2', ...(mode === 'apple' ? [] : ['-Xlinker', '-L'+join(root, 'target/debug'), '-Xlinker', '-rpath', '-Xlinker', join(root, 'target/debug')]), 'Consumer']);
 if (mode === 'apple') {
   writeFileSync(join(pkg, 'README.md'), 'Separate cold SignetAuth Swift package, ARM64 macOS/iOS/iOS Simulator. Clang headers use Headers/SignetAuthFFI/ to coexist with other static XCFrameworks. Existing Sapflow media package unchanged. Real signer consent/site authority/Keychain/app/device qualification remain open.\n');
   run('zip', ['-q', '-r', appleArchive, 'signet-apple'], {cwd: join(root, 'target/native-packages')});
 }
 success = true;
} finally {
 // Only this script's generated package/workspace, never the media package.
 if (success) rmSync(staging, { recursive: true });
}

// Reached only after every tool succeeded AND actual staging cleanup succeeded.
const completed = { headerLayoutVersion: mode === 'apple' ? 2 : 1, terminalExit: 0, combinedActualNativeConsumer: true, nativeSlicesBuilt: mode === 'apple', appOrDeviceChanged: false, stagingRemoved: true };
if (mode === 'apple') {
  const archive = {headerLayoutVersion: 2, archive: appleArchive, sha256: createHash('sha256').update(readFileSync(appleArchive)).digest('hex'), physicalQualified: false};
  writeFileSync(join(apple, 'verification.json'), JSON.stringify(archive, null, 2)+'\n');
  writeFileSync(archiveReceipt, JSON.stringify(archive, null, 2)+'\n');
}
writeFileSync(resultPath, JSON.stringify(completed, null, 2)+'\n');
