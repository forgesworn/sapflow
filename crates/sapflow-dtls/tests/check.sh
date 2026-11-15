#!/usr/bin/env bash
# The local gate for this crate, as CI runs it: formatting, clippy three
# ways (the workspace as CI runs it, this crate with its test helpers, and
# the library alone), the tests, and the fuzz crate, each for the default
# build (as WHIP builds the crate) and again with the experimental
# hello-verify feature, so the cookie code does not rot. Pass a target
# triple to cross-build, for example aarch64-apple-darwin.
set -uo pipefail
t=()
[ -n "${1:-}" ] && t=(--target "$1")
status=0
step() { echo "== $*"; "$@" 2>&1 | grep -E '^(error|warning)|^test result|FAILED|panicked' -A6 | head -80; [ "${PIPESTATUS[0]}" = 0 ] || status=1; }
step cargo fmt --all -- --check
step cargo clippy --workspace --all-targets "${t[@]}" -- -D warnings
for f in "" ",hello-verify"; do
  step cargo clippy -p sapflow-dtls --all-targets --features "testing$f" "${t[@]}" -- -D warnings
  step cargo clippy -p sapflow-dtls --lib --features "${f#,}" "${t[@]}" -- -D warnings
  step cargo test -p sapflow-dtls --features "testing$f" "${t[@]}"
  step cargo check --manifest-path fuzz/Cargo.toml --bins --features "${f#,}" "${t[@]}"
done
exit $status
