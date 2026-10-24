// Build/stage alongside wasm-bindgen's generated sapflow_realtime.js module.
// No network, capture, media keys, logging or scheduling timer is introduced.
import { initSync, RealtimeQueue } from './sapflow_realtime.js'

const MAX_SAFE = Number.MAX_SAFE_INTEGER
function fail() { throw new Error('Invalid realtime queue operation') }
function integer(value, min, max) {
  if (!Number.isSafeInteger(value) || value < min || value > max) fail()
  return value
}
function time(value) {
  if (!Number.isFinite(value) || value < 0 || value > 9_007_199_254_740) fail()
  return value
}
function metadataCopy(value) {
  if (!value || ![null, Object.prototype].includes(Object.getPrototypeOf(value))) fail()
  const fields = Object.getOwnPropertyDescriptors(value)
  const keys = Reflect.ownKeys(fields)
  if (keys.length > 16) fail()
  const copy = Object.create(null)
  for (const name of keys) {
    if (typeof name !== 'string' || name.length === 0 || name.length > 32 || !('value' in fields[name])) fail()
    const field = fields[name].value
    if (!['number', 'boolean', 'string'].includes(typeof field) ||
        (typeof field === 'string' && field.length > 128) ||
        (typeof field === 'number' && (!Number.isFinite(field) || Math.abs(field) > MAX_SAFE))) fail()
    copy[name] = field
  }
  integer(copy.stream, 1, 0xffffffff)
  integer(copy.sequence, 0, 0xffffffff)
  if (typeof copy.video !== 'boolean' || typeof copy.key !== 'boolean' || (!copy.video && copy.key)) fail()
  time(copy.deadlineMs)
  return Object.freeze(copy)
}

/**
 * Instantiate a bounded opaque-frame queue. Supply the staged wasm bytes or a
 * precompiled WebAssembly.Module; this function never fetches a remote URL.
 * Each returned queue owns independent Rust state and a bounded ciphertext map.
 *
 * Metadata has stream, video, key, sequence and deadlineMs. Up to sixteen short
 * scalar fields are preserved for the caller's authenticated frame envelope.
 * Supply monotonic performance.now() milliseconds and call tick while idle.
 */
export async function createRealtimeQueue(wasmBytes, limits) {
  if (!(wasmBytes instanceof WebAssembly.Module) && !(wasmBytes instanceof ArrayBuffer) &&
      !(wasmBytes instanceof Uint8Array)) fail()
  if (!limits || typeof limits !== 'object') fail()
  const maxBytes = integer(limits.maxBytes, 1, 16 * 1024 * 1024)
  const maxFrames = integer(limits.maxFrames, 1, 4096)
  const maxStreams = integer(limits.maxStreams, 1, 256)
  const maxResidenceMs = time(limits.maxResidenceMs)
  if (maxResidenceMs < .001 || maxResidenceMs > 60_000) fail()
  initSync({ module: wasmBytes })
  let scheduler
  try { scheduler = new RealtimeQueue(maxBytes, maxFrames, maxStreams, maxResidenceMs) } catch { fail() }
  const payloads = new Map()
  let nextId = 1, closed = false
  function drain() {
    let id, count = 0
    while ((id = scheduler.takeDrop()) !== 0) { payloads.delete(id); count++ }
    return count
  }
  function open() { if (closed) fail() }
  function checked(code) { if (code < 0) fail(); return code }
  return Object.freeze({
    offer(metadata, ciphertext, nowMs) {
      open(); time(nowMs)
      const copy = metadataCopy(metadata)
      if (!(ciphertext instanceof Uint8Array) || ciphertext.byteLength === 0) fail()
      // Refuse impossible frames without allocating their duplicate payload.
      // Still tell Rust about an oversized frame so video recovery is fenced.
      integer(ciphertext.byteLength, 1, 0xffffffff)
      if (nextId > 0xffffffff) fail() // Never alias an ID after wrap.
      const id = nextId++
      const owned = ciphertext.byteLength <= maxBytes ? ciphertext.slice() : undefined
      const result = scheduler.offer(id, copy.stream, Number(copy.video), Number(copy.key),
        copy.sequence, ciphertext.byteLength, nowMs, copy.deadlineMs)
      const dropped = drain()
      checked(result)
      if (result === 1) payloads.set(id, { metadata: copy, ciphertext: owned })
      return { accepted: result === 1, dropped, waitingForKeyframe: scheduler.waiting(copy.stream) }
    },
    pop(nowMs) {
      open(); time(nowMs)
      const id = scheduler.pop(nowMs)
      drain(); checked(id)
      if (id === 0) return undefined
      const frame = payloads.get(id)
      payloads.delete(id)
      if (!frame) fail()
      return frame
    },
    tick(nowMs) {
      open(); time(nowMs)
      const code = scheduler.tick(nowMs)
      const dropped = drain()
      checked(code)
      return dropped
    },
    waitingForKeyframe(stream) {
      open(); integer(stream, 1, 0xffffffff)
      return scheduler.waiting(stream)
    },
    stats() {
      if (closed) return { bytes: 0, frames: 0, retainedPayloads: 0 }
      return { bytes: scheduler.bytes(), frames: scheduler.frames(), retainedPayloads: payloads.size }
    },
    close() {
      if (closed) return
      checked(scheduler.close()); drain(); payloads.clear(); scheduler.free(); closed = true
    },
  })
}
