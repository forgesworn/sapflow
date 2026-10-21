// Reproducible, fail-closed transformation of the pinned UniFFI helper.
// The native ABI and generated method bodies are unchanged.
import {readFileSync} from 'node:fs';

export const originalRustCallHelper = `private inline fun <U, E: kotlin.Exception> uniffiRustCallWithError(errorHandler: UniffiRustCallStatusErrorHandler<E>, callback: (UniffiRustCallStatus) -> U): U {
    var status = UniffiRustCallStatus()
    val return_value = callback(status)
    uniffiCheckCallStatus(errorHandler, status)
    return return_value
}`;

export function applyKotlinStatusLeases(source) {
  if (source.includes('internal object SapflowRustCallStatusPool')) {
    throw Error('Kotlin status leases were already applied; regenerate first');
  }
  if (source.split(originalRustCallHelper).length !== 2) {
    throw Error('Pinned UniFFI Rust-call helper did not match exactly once; review generator changes');
  }
  const replacement = readFileSync(new URL('./native-kotlin-status-leases.kt', import.meta.url), 'utf8').trimEnd();
  return source.replace(originalRustCallHelper, replacement);
}
