# Encrypted own-SRT origin

Sapflow uses its own SRT implementation. Configure `SAPFLOW_SRT_PASSPHRASE`
independently of `SAPFLOW_PUBLISH_PASS`, and optionally
`SAPFLOW_SRT_PBKEYLEN=16` (the default, AES128) or `32` (AES256).
The earlier explicit `SAPFLOW_SRT_CIPHER=aes128` or `aes256` setting remains
supported; if both key-size settings are present they must agree.
The caller must use the same transport secret and cipher. Provision the
transport secret separately; never derive it from the clear-text stream ID.

A transport passphrase must contain 10 through 79 bytes and no NUL. An empty
value is invalid, rather than disabling encryption. A cipher without a
passphrase, an unsupported key size or conflicting key-size settings cause
startup failure. The removed `srt-rs` implementation is refused explicitly;
the old `SAPFLOW_SRT_IMPL=own` setting is accepted for compatibility.
`SAPFLOW_SRT_PBKEYLEN` only chooses the encryption key size. Without a
passphrase it permits plaintext, matching the existing origin configuration;
set a passphrase to require encryption. The named `SAPFLOW_SRT_CIPHER` alias
requires a passphrase.
Flags of the same name
are supported, but environment provisioning avoids putting the secret in a
command line. Setting neither option preserves the existing plaintext
own-listener behaviour. Configuring a secret requires encryption and rejects
plaintext, wrong-secret and wrong-cipher callers without downgrade.

Publish credentials still authorise the stream separately. The own listener
checks publish admission before key derivation, then accepts key negotiation
before creating the publisher queue/task. If key negotiation fails, the
provisional stream claim is immediately released. No media is submitted, so
there is no healthy/publish/unpublish callback or recording-complete event;
the recorder waits for its first media part before creating a file. Ordinary
connection/leave log messages may still describe that provisional claim.

The owned environment secret moves directly into the existing core
`CryptoConfig` zeroising allocation, rather than a generic configuration map.
Clones share that allocation and debug output is redacted. Configuration
errors do not echo the secret. This does not erase operating-system environment
or argument storage, the caller's input copies, or temporary inline CLI parsing
copies. The configured secret remains available for the listener's lifetime.
The first profile is one explicitly provisioned transport secret per origin;
per-event secret provisioning is not provided by these server settings.

AES-CTR encrypts media but does not authenticate data packets. SRT's stream ID
and publish credentials are not made confidential by media encryption. This
configuration wiring does not establish production security, sustained phone
performance or impaired-network acceptance. Core independent interoperability
and native device-to-origin evidence remain separate gates.

Focused regressions exercise actual own Caller/Listener negotiation with the
server-generated configuration: AES128/AES256 media delivery, wrong secret,
wrong cipher and missing encryption, plus configuration bounds and redaction.

Configuration and protocol regressions cover these settings in the server
test suite. Source tests and physical device qualification remain separate;
the mobile Stop-test APK retains its frozen source and artefact receipt.
