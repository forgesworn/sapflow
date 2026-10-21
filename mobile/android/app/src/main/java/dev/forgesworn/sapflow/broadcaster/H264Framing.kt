package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer

/** Bounded framing conversion only. Rust validates SPS/PPS and complete AVCC. */
internal class H264Framing(private val maxBytes: Int = 4 * 1024 * 1024) {
    private var scratch: ByteBuffer? = null

    fun sample(input: ByteBuffer): ByteBuffer {
        require(input.hasRemaining() && input.remaining() <= maxBytes) { "Encoded video exceeds the sample bound" }
        val source = input.slice()
        if (isAvcc(source)) {
            // Already four-byte AVCC. The borrowed direct view is copied only
            // once for ownership, synchronously inside Rust's sample method.
            return source
        }
        require(startCode(source, 0) != 0) { "Unknown H264 framing" }
        val target = scratch ?: ByteBuffer.allocateDirect(maxBytes).also { scratch = it }
        target.clear()
        var offset = 0
        var slices = 0
        while (offset < source.limit()) {
            val prefix = startCode(source, offset)
            require(prefix != 0) { "Malformed Annex-B framing" }
            val begin = offset + prefix
            var end = begin
            while (end < source.limit() && startCode(source, end) == 0) end++
            require(end > begin) { "Empty Annex-B NAL" }
            val kind = source.get(begin).toInt() and 31
            // Repeated parameter sets are carried by the descriptor, never
            // smuggled into the validated AVCC access unit.
            if (kind != 7 && kind != 8) {
                require(target.remaining() >= 4 + end - begin) { "AVCC conversion exceeds the sample bound" }
                target.putInt(end - begin)
                val nal = source.duplicate().apply { position(begin); limit(end) }
                target.put(nal)
                if (kind == 1 || kind == 5) slices++
            }
            offset = end
        }
        require(slices > 0) { "Encoded video contains no picture" }
        target.flip()
        return target
    }

    fun clear() { scratch = null }

    companion object {
        private fun isAvcc(bytes: ByteBuffer): Boolean {
            var offset = 0
            while (offset < bytes.limit()) {
                if (bytes.limit() - offset < 4) return false
                val size = bytes.getInt(offset)
                if (size <= 0 || size > bytes.limit() - offset - 4) return false
                offset += 4 + size
            }
            return offset > 0
        }

        private fun startCode(bytes: ByteBuffer, at: Int): Int {
            if (at + 3 > bytes.limit() || bytes.get(at) != 0.toByte() || bytes.get(at + 1) != 0.toByte()) return 0
            if (bytes.get(at + 2) == 1.toByte()) return 3
            if (at + 4 <= bytes.limit() && bytes.get(at + 2) == 0.toByte() && bytes.get(at + 3) == 1.toByte()) return 4
            return 0
        }

        fun parameterSet(input: ByteBuffer?, kind: Int): ByteBuffer? {
            if (input == null) return null
            val bytes = input.slice()
            require(bytes.remaining() <= 8192) { "Codec configuration exceeds the bound" }
            var offset = 0
            if (bytes.hasRemaining() && bytes.get(0).toInt() and 31 == kind && startCode(bytes, 0) == 0) {
                return copyConfig(bytes)
            }
            while (offset < bytes.limit()) {
                val prefix = startCode(bytes, offset)
                if (prefix == 0) return null
                val begin = offset + prefix
                var end = begin
                while (end < bytes.limit() && startCode(bytes, end) == 0) end++
                if (end > begin && bytes.get(begin).toInt() and 31 == kind) {
                    return copyConfig(bytes.duplicate().apply { position(begin); limit(end) }.slice())
                }
                offset = end
            }
            return null
        }

        private fun copyConfig(bytes: ByteBuffer): ByteBuffer {
            require(bytes.remaining() in 1..4096) { "Parameter set exceeds the bound" }
            return ByteBuffer.allocateDirect(bytes.remaining()).apply { put(bytes); flip() }
        }
    }
}
