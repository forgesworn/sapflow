# Encrypted own-SRT origin

`SAPFLOW_SRT_IMPL=own` opts into the original Sapflow SRT implementation.
The default implementation remains unchanged. For this listener, configure
`SAPFLOW_SRT_PASSPHRASE` independently of `SAPFLOW_PUBLISH_PASS`, and optionally
`SAPFLOW_SRT_CIPHER=aes256` (the default with a passphrase) or `aes128`.
The caller must use the same transport secret and cipher. Provision the
transport secret separately; never derive it from the clear-text stream ID.

A transport passphrase must contain 10 through 79 bytes and no NUL. An empty
value is invalid, rather than disabling encryption. A cipher without a
passphrase, an unsupported cipher, or either encryption setting with the
default `srt-rs` implementation causes startup failure. Flags of the same name
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

Local validation: `cargo test --offline -p sapflow-server --bin sapflow config::
-j 2` passed all eight configuration tests; the `srt_own::` filter passed its
eight-case protocol matrix. Strict binary Clippy and the debug origin binary
build also passed. No active server was restarted by these checks, and no
physical-device result is implied.
