# Saved manual destinations and reviewed file import

Manual profiles describe an SRT or RTMP/RTMPS destination. They carry no V4V
connector configuration, creator identity, grant, event authority or billing
entitlement. The authenticated V4V flow remains separate and always free.

Both apps require an explicit user action to load or import a profile. A file
is read under a 16,384-byte limit, validated, then held for review. Review shows
the transport, destination host and port, and encryption setting. It does not
display the application path, stream key, stream ID, passphrase or provider file
name. Apply only fills the manual destination fields. It does not save secrets,
prepare capture, connect a socket or start a broadcast. Prepare and Start remain
separate actions.
The imported name is validated for format compatibility but is not displayed or
copied into the editable saved-profile name. Choose a local name when saving.

If the file requests plaintext RTMP or SRT, the reviewer must separately permit
plaintext before Apply. A boolean in an imported file cannot supply that consent.
For an RTMPS endpoint, Apply clears the plaintext setting even when the file's
`allowPlaintext` flag is true; changing that endpoint later cannot inherit a
plaintext permission from the file.
Cancel discards the pending review. Changes to the destination, capture
preparation, app foreground state or the current import invalidate stale results.
A file arriving after another preparation begins cannot alter that capture.
Android's own external document picker retains its original launch ticket
through that expected departure. Its result waits for foreground resumption
and the same idle admission before reading; an ordinary background transition
does not preserve a completed review or an unrelated file operation.
Android applies an original five-second logical read deadline. A timed-out
provider cannot supply a later successful review. Timeout does not prove that
OS reading, closure or provider cancellation has finished: the single IO slot
remains occupied until actual cleanup, preventing replacement workers from
accumulating behind a stalled provider. Capture and broadcasting use their
existing independent owners.

## Version 1 file format

The input is one UTF-8 JSON object with exactly these fields. Unknown, missing
or duplicate fields, including duplicate names expressed with JSON escapes, are
rejected. The version must be the lexical JSON integer `1`; `1.0`, `"1"` and
`true` are not versions. Unsupported versions are refused without migration.

| Field | Value |
| --- | --- |
| `version` | Integer `1` |
| `kind` | Exact string `sapflow-manual-destination` |
| `name` | Printable ASCII name, 1 to 48 characters after trimming ASCII spaces |
| `transport` | Exact string `rtmp` or `srt`; `rtmp` also selects RTMPS according to the endpoint scheme |
| `endpoint` | Application URL for RTMP(S), or numeric unicast SRT address with explicit port |
| `streamKey` | Required non-empty RTMP key, at most 4,096 UTF-8 bytes; empty for SRT |
| `streamID` | SRT stream ID, at most 512 UTF-8 bytes; empty for RTMP(S) |
| `passphrase` | Encrypted SRT: 10 to 79 printable ASCII bytes; empty for RTMP(S) or explicitly requested plaintext SRT |
| `allowPlaintext` | Boolean requesting the plaintext transport setting; independent review consent is still required |

Endpoint, key and stream ID reject control characters. Endpoints reject embedded
credentials, surrounding whitespace, query strings and fragments. RTMP(S) needs an application path and
refuses dot-path components and invalid explicit ports. SRT accepts numeric
unicast IPv4/IPv6 with an explicit non-zero port; it performs no hostname
resolution while importing. Version 1 excludes IPv4 addresses in `0/8` and
`224/3`, and dotted IPv4-embedded IPv6 forms such as `::ffff:192.0.2.1`.
Use an ordinary unicast IPv4 address or hexadecimal IPv6 address instead.
The native destination validators enforce their
remaining bounds. A profile cannot request DNS SRT, transport failover, a TLS
exception, an encoder change or a background permission.

This generated example uses a reserved address and a non-service test secret:

```json
{
  "version": 1,
  "kind": "sapflow-manual-destination",
  "name": "Generated lab destination",
  "transport": "srt",
  "endpoint": "srt://192.0.2.1:8890",
  "streamKey": "",
  "streamID": "publish:generated-session",
  "passphrase": "generated-lab-passphrase",
  "allowPlaintext": false
}
```

The file itself contains credentials. Obtain it through an appropriate private
channel and retain or remove the source file using its owner's normal file
controls. Import does not erase a provider's original. There is no automatic
credential export, QR scan, deep-link handler or unsolicited Intent import in
this packet.

## Local storage and evidence

Saving is an explicit action separate from Apply. Each store admits at most 16
profiles. Secrets use platform-protected local storage, rather than plaintext
preferences or diagnostic output. A corrupt or inaccessible existing store is
reported; it is not silently replaced by an empty collection. Loading and
deleting profiles use the same capture-idle and stale-result rules as import.

Android commits the complete profile collection in one encrypted document.
New iOS Keychain values bind each secret to its exact profile ID, endpoint,
transport and plaintext setting. If metadata and protected values differ after
an interrupted save, Load refuses that pairing. Historical raw Keychain values
remain readable; their next explicit Save writes the bound format. The source
and host tests do not establish physical crash durability or repair an already
inconsistent legacy value.

The shared generated vectors live in
[`tests/fixtures/manual-destination-v1.json`](../tests/fixtures/manual-destination-v1.json).
They exercise the same accepted and refused wire documents on both platforms.
Additional platform tests cover storage and lifecycle boundaries. Their results
are recorded in [mobile delivery status](mobile-delivery-status.md).

With the native packages and platform build prerequisites already available,
run the host gates from the repository root:

```sh
bash mobile/ios/test-manual-destination.sh
mobile/android/build-debug.sh --offline
```

The shared corpus is also copied into the iOS unit-test bundle so its test does
not depend on a host-machine file path when the bundle is later executed on
iOS. Seeded byte-mutation tests are finite local robustness checks; they do not
establish long-running fuzz or physical acceptance.

Host parser/store tests and unsigned app builds do not prove physical document
provider behaviour, Keychain/Keystore acceptance, background interruption,
credential lifetime or a live destination. Those remain qualification gates.
