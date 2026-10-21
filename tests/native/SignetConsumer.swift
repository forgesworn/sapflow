import Foundation
import SignetAuth
import Sapflow

// Generated-only, offline native consumers. No signer/relay/account or media.
let uri = "bunker://79dff8f82963424e0bb02708a22e44b4980893e3a4be0fa3cb60a43b946764e3?relay=wss://relay.example.invalid&secret=generated-only"
let owner = try NativeSignet.pairBunker(uri: uri, deadlineMs: 5_000)
owner.requestClose()
let closed = try owner.waitClosed(timeoutMs: 2_000)
precondition(closed)
do { try owner.connect(requestId: 1); fatalError("closed auth owner accepted work") }
catch SignetError.Closed { }
do { _ = try SignetCredential.restore(bytes: Data("{}".utf8)); fatalError("bad credential accepted") }
catch SignetError.InvalidCredential { }
let credentialJSON = "{\"version\":1,\"persona\":\"" + "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798" + "\",\"app_secret_key\":\"" + String(repeating: "1", count: 64) + "\",\"bunker_uri\":\"" + uri.components(separatedBy: "&secret=")[0] + "\"}"
let credential = try SignetCredential.restore(bytes: Data(credentialJSON.utf8))
precondition(credential.encoded() == Data(credentialJSON.utf8))
let restored = try NativeSignet.restore(credential: credential, deadlineMs: 5_000)
restored.requestClose()
let restoredClosed = try restored.waitClosed(timeoutMs: 2_000)
precondition(restoredClosed)
// Both actual native libraries in one process. Auth does not replace Sapflow.
precondition(sdkAbiVersion() > 0)
let bytes = try EncodedBuffer(bytes: Data([1, 2, 3]))
precondition(bytes.len() == 3)
print("combined Sapflow + SignetAuth offline consumer PASS")
