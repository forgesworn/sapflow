#!/usr/bin/env bash
# A2.1: a black-box OpenSSL DTLS 1.2 client against the sans-IO server on
# loopback, for both SRTP profiles. Prints both sides' keying material and
# fails unless they match byte for byte. Run from the repository root:
#
#   bash crates/sapflow-dtls/tests/openssl_interop.sh [port] [out-dir]
#
# TARGET=<triple> cross-builds the example (for example
# aarch64-apple-darwin); OPENSSL=<path> picks the openssl binary.
#
# With an out-dir it keeps each run's record (datagrams both ways, for the
# offline replay test) and s_client's key log, as fixtures. The server is
# seeded (fixed test randomness) so a kept record replays exactly.
set -euo pipefail
port=${1:-47010}
out=${2:-}
openssl=${OPENSSL:-openssl}
work=$(mktemp -d)
cleanup() {
  if [ -d "$HOME/.Trash" ]; then mv "$work" "$HOME/.Trash/sapflow-dtls-interop-$(basename "$work")"; else rm -rf "$work"; fi
}
trap cleanup EXIT
if [ -n "${TARGET:-}" ]; then
  cargo build -q -p sapflow-dtls --features testing --example udp_server --target "$TARGET"
  bin=target/$TARGET/debug/examples/udp_server
else
  cargo build -q -p sapflow-dtls --features testing --example udp_server
  bin=target/debug/examples/udp_server
fi
"$openssl" req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -subj /CN=openssl-test-client -days 2 -keyout "$work/client.key" -out "$work/client.pem" 2>/dev/null
fp=$("$openssl" x509 -in "$work/client.pem" -noout -fingerprint -sha256 | sed 's/^.*=//')
seed=5361706c6f77445444534f70656e53534c496e7465726f7000000000000000${port: -2}
status=0
run() {
  local name=$1 profiles=$2 len=$3; shift 3
  local rec="$work/$name.record"
  "$bin" --bind "127.0.0.1:$port" --expect "$fp" --seed "$seed" --record "$rec" "$@" >"$work/$name.server" 2>&1 &
  local pid=$!
  for _ in $(seq 50); do
    grep -q '^listening' "$work/$name.server" 2>/dev/null && break
    sleep 0.1
  done
  (sleep 2) | "$openssl" s_client -dtls1_2 -connect "127.0.0.1:$port" -use_srtp "$profiles" \
    -keymatexport EXTRACTOR-dtls_srtp -keymatexportlen "$len" \
    -cert "$work/client.pem" -key "$work/client.key" -keylogfile "$work/$name.keylog" \
    >"$work/$name.client" 2>&1 || true
  wait "$pid" || true
  local theirs ours
  theirs=$(grep -i 'Keying material:' "$work/$name.client" | sed 's/.*: *//' | tr 'a-f' 'A-F' || true)
  ours=$(grep '^keying material:' "$work/$name.server" | sed 's/.*: *//' || true)
  echo "== $name ($profiles, $len bytes) $*"
  grep -E '^(profile|state|stats)' "$work/$name.server" || true
  grep -E 'SRTP Extension negotiated|Protocol *:|Cipher *:|Extended master secret' "$work/$name.client" | sed 's/^ *//' || true
  echo "openssl:  $theirs"
  echo "sapflow:  $ours"
  if [ -n "$theirs" ] && [ "$theirs" = "$ours" ]; then
    echo "MATCH"
  else
    echo "MISMATCH"; status=1
    sed -n '1,40p' "$work/$name.server"; grep -iE 'error|alert|SRTP|Cipher|verify' "$work/$name.client" | head -20
  fi
  if [ -n "$out" ]; then
    mkdir -p "$out"
    cp "$rec" "$out/$name.record"
    cp "$work/$name.keylog" "$out/$name.keylog" 2>/dev/null || true
    printf '%s\n%s\n%s\n' "$fp" "$theirs" "$ours" >"$out/$name.keys"
    {
      echo "run $name: -use_srtp $profiles -keymatexportlen $len${*:+; server $*}"
      echo "  client certificate sha-256 $fp"
      echo "  openssl $theirs"
      echo "  sapflow $ours"
      if [ -n "$theirs" ] && [ "$theirs" = "$ours" ]; then echo "  MATCH"; else echo "  MISMATCH"; fi
      echo
    } >>"$work/summary"
  fi
}
run gcm SRTP_AEAD_AES_128_GCM 56
run cm80 SRTP_AES128_CM_SHA1_80 60
run cm80-mtu300-drop SRTP_AES128_CM_SHA1_80 60 --mtu 300 --drop-first-flight
run both SRTP_AES128_CM_SHA1_80:SRTP_AEAD_AES_128_GCM 56
run gcm-mtu300 SRTP_AEAD_AES_128_GCM 56 --mtu 300
run gcm-drop SRTP_AEAD_AES_128_GCM 56 --drop-first-flight
run gcm-cookie SRTP_AEAD_AES_128_GCM 56 --hello-verify
run gcm-drop-final SRTP_AEAD_AES_128_GCM 56 --drop-final-flight
version=$("$openssl" version)
echo "$version"
if [ -n "$out" ]; then
  {
    echo "# A2.1: DTLS-SRTP keying material, $version, s_client as a"
    echo "# black box against crates/sapflow-dtls/examples/udp_server.rs on 127.0.0.1,"
    echo "# captured $(date -u +%Y-%m-%d) by tests/openssl_interop.sh. Each run:"
    echo "#   openssl s_client -dtls1_2 -connect 127.0.0.1:$port -use_srtp <profiles> \\"
    echo "#     -keymatexport EXTRACTOR-dtls_srtp -keymatexportlen <length> \\"
    echo "#     -cert <test P-256 cert> -key <its key> -keylogfile <file>"
    echo "# 'openssl' is what s_client printed; 'sapflow' is what the server exported."
    echo "# Throwaway loopback sessions with a seeded test server: not secrets."
    echo
    cat "$work/summary"
  } >"$(dirname "$out")/openssl-keymatexport.txt"
fi
exit $status
