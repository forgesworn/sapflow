import test from 'node:test';
import assert from 'node:assert/strict';
import {applyKotlinStatusLeases, originalRustCallHelper} from '../../scripts/native-kotlin-status-leases.mjs';

test('transforms only the exact pinned helper and preserves surrounding bindings', () => {
  const source = `before\n${originalRustCallHelper}\nafter`;
  const result = applyKotlinStatusLeases(source);
  assert.ok(result.startsWith('before\n'));
  assert.ok(result.endsWith('\nafter'));
  assert.ok(result.includes('internal object SapflowRustCallStatusPool'));
  assert.ok(result.includes('stack.release(status, enteredDepth)'));
  assert.ok(!result.includes(originalRustCallHelper));
});

test('refuses missing, changed, duplicate or previously transformed helpers', () => {
  for (const source of ['', originalRustCallHelper.replace('var status', 'val status'),
    originalRustCallHelper + originalRustCallHelper,
    applyKotlinStatusLeases(originalRustCallHelper)]) {
    assert.throws(() => applyKotlinStatusLeases(source));
  }
});
