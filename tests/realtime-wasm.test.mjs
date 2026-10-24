import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { createRealtimeQueue } from '../target/realtime-browser/browser.mjs'

const wasm = await readFile(new URL('../target/realtime-browser/sapflow_realtime_bg.wasm', import.meta.url))
const limits = { maxBytes: 100, maxFrames: 4, maxStreams: 4, maxResidenceMs: 100 }
const metadata = (sequence, other = {}) => ({ stream: 1, video: false, key: false, sequence, deadlineMs: 100, ...other })

test('compiled WASM owns independent queues and returns copied ciphertext with metadata', async () => {
  const a = await createRealtimeQueue(wasm, limits), b = await createRealtimeQueue(wasm, limits)
  try {
    const bytes = new Uint8Array([1, 2, 3])
    assert.equal(a.offer(metadata(0, { timestamp: 12345 }), bytes, 0).accepted, true)
    bytes.fill(9)
    assert.deepEqual(a.stats(), { bytes: 3, frames: 1, retainedPayloads: 1 })
    assert.equal(b.pop(0), undefined)
    const received = a.pop(0)
    assert.deepEqual(received.ciphertext, new Uint8Array([1, 2, 3]))
    assert.equal(received.metadata.timestamp, 12345)
    assert.equal(a.stats().retainedPayloads, 0)
  } finally { a.close(); b.close() }
})

test('compiled WASM releases expired and evicted payloads and fences broken video', async () => {
  const q = await createRealtimeQueue(wasm, { ...limits, maxFrames: 2 })
  try {
    q.offer(metadata(0, { stream: 2, video: true, key: true }), new Uint8Array(20), 0)
    q.offer(metadata(1, { stream: 2, video: true }), new Uint8Array(20), 0)
    const audio = q.offer(metadata(0), new Uint8Array(10), 0)
    assert.equal(audio.accepted, true)
    assert.equal(audio.dropped, 2)
    assert.equal(q.waitingForKeyframe(2), true)
    assert.deepEqual(q.stats(), { bytes: 10, frames: 1, retainedPayloads: 1 })
    assert.equal(q.tick(100), 1)
    assert.deepEqual(q.stats(), { bytes: 0, frames: 0, retainedPayloads: 0 })
  } finally { q.close() }
})

test('compiled WASM refuses unsafe JS integers, clocks, metadata and remote initialisers', async () => {
  await assert.rejects(createRealtimeQueue('https://invalid.example/queue.wasm', limits), /Invalid realtime/)
  await assert.rejects(createRealtimeQueue(wasm, { ...limits, maxBytes: 0x100000000 }), /Invalid realtime/)
  const q = await createRealtimeQueue(wasm, limits)
  try {
    for (const now of [NaN, Infinity, -1, Number.MAX_SAFE_INTEGER]) {
      assert.throws(() => q.offer(metadata(0), new Uint8Array(1), now), /Invalid realtime/)
    }
    for (const fields of [{ stream: 0 }, { sequence: 2 ** 32 }, { video: 1 }, { key: true },
      { deadlineMs: NaN }, { note: 'x'.repeat(129) }, { payload: new Uint8Array(1000) }]) {
      assert.throws(() => q.offer(metadata(0, fields), new Uint8Array(1), 0), /Invalid realtime/)
    }
    q.offer(metadata(0), new Uint8Array(1), 1)
    assert.throws(() => q.pop(0), /Invalid realtime/)
    assert.equal(q.stats().retainedPayloads, 1)
  } finally { q.close() }
})

test('oversized video rejection releases dependencies; close releases all and stays closed', async () => {
  const q = await createRealtimeQueue(wasm, limits)
  q.offer(metadata(0, { stream: 2, video: true, key: true }), new Uint8Array(10), .123)
  const rejected = q.offer(metadata(1, { stream: 2, video: true }), new Uint8Array(101), .124)
  assert.deepEqual(rejected, { accepted: false, dropped: 1, waitingForKeyframe: true })
  q.offer(metadata(0), new Uint8Array(10), .125)
  q.close(); q.close()
  assert.deepEqual(q.stats(), { bytes: 0, frames: 0, retainedPayloads: 0 })
  assert.throws(() => q.pop(.126), /Invalid realtime/)
})
