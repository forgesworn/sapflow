#!/usr/bin/env bash
# The local gate for this crate, as CI runs it: formatting, clippy three
# ways (the workspace as CI runs it, this crate with its test helpers, and
# the library alone), the tests, and the fuzz crate. Pass a target triple
# to cross-build, for example aarch64-apple-darwin.
set -uo pipefail
t=()
[ -n "${1:-}" ] && t=(--target "$1")
status=0
step() { echo "== $*"; "$@" 2>&1 | grep -E '^(error|warning)|^test result|FAILED|panicked' -A6 | head -80; [ "${PIPESTATUS[0]}" = 0 ] || status=1; }
step cargo fmt --all -- --check
step cargo clippy --workspace --all-targets "${t[@]}" -- -D warnings
step cargo clippy -p sapflow-dtls --all-targets --features testing "${t[@]}" -- -D warnings
step cargo clippy -p sapflow-dtls --lib "${t[@]}" -- -D warnings
step cargo test -p sapflow-dtls --features testing "${t[@]}"
step cargo check --manifest-path fuzz/Cargo.toml --bins "${t[@]}"
exit $status
