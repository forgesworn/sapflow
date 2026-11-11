# Test fixtures

`openssl/` holds eight runs of OpenSSL 4.0.3 `s_client -dtls1_2` against
`examples/udp_server.rs` on 127.0.0.1, made by `tests/openssl_interop.sh`
(acceptance A2.1). The server was seeded, so its certificate and randomness
are reproducible, and the client used a fresh self-signed P-256 test
certificate. Per run:

- `<run>.record`: every datagram in order, one per line, as
  `<direction> <milliseconds since start> <hex>`. `C` is from OpenSSL, `S`
  from the server, `D` a server datagram the example dropped on purpose,
  `T` a server timer firing.
- `<run>.keys`: the client certificate's SHA-256 fingerprint, the keying
  material OpenSSL printed, and the keying material the server exported.
- `<run>.keylog`: OpenSSL's `-keylogfile` (`CLIENT_RANDOM` and the master
  secret).

`openssl-keymatexport.txt` lists every run's two keying materials side by
side.

The unit tests in `src/tests.rs` replay each record against an identically
seeded server and require the same datagrams and the same keying material;
derive the exporter output and the record keys from OpenSSL's master secret
and open OpenSSL's own encrypted Finished and close_notify with them; and
feed both halves of the keying material to `sapflow-rtp`.

These are throwaway loopback sessions. The keys protect nothing else.

To recapture, run from the repository root:

```sh
bash crates/sapflow-dtls/tests/openssl_interop.sh 47010 crates/sapflow-dtls/tests/fixtures/openssl
```

which also rewrites `openssl-keymatexport.txt`. The replay test assumes
port 47010 (the seed includes it). Then rewrite the fuzz seeds, since the
`openssl-` seeds and `dtls_server`'s interop mode read these runs:
`cargo test -p sapflow-dtls --lib -- --ignored write_seeds`.
