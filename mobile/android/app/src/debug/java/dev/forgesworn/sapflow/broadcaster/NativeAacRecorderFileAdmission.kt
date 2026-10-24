// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import java.security.MessageDigest

/** Fixed generated Apple segment-file profile, qualification only. Parsing
 * yields untrusted file observations. Admission compares them to geometry and
 * original packets already checked against the separate SDK file/SFCP2.
 * Neither class is a native source, EOF, trim or recovery capability. */
internal object NativeAacRecorderFileAdmission {
    const val MAX_BYTES = 4 * 1024 * 1024
    data class Packet(val offset: Int, val size: Int, val dts: Long, val pts: Long,
        val duration: Int, val sha256: String)
    data class FileObservation(val sha256: String, val packets: List<Packet>,
        val movieDuration: Long = 0, val edits: Int = 0)
    data class OriginalPacket(val size: Int, val dts: Long, val pts: Long,
        val duration: Int, val sha256: String)
    data class Source(val sdkFileSHA256: String, val generation: Long, val head: Int,
        val tail: Int, val programmeFrames: Long, val mediaTime: Long,
        val packets: List<OriginalPacket>)
    data class Registration(val fileSHA256: String, val sdkFileSHA256: String,
        val sourceGeneration: Long)

    fun digest(bytes: ByteArray): String {
        val hex = "0123456789abcdef"
        return buildString { for (b in MessageDigest.getInstance("SHA-256").digest(bytes)) {
            val v = b.toInt() and 255; append(hex[v ushr 4]); append(hex[v and 15])
        } }
    }
    private fun hash(value: String) = value.matches(Regex("[a-f0-9]{64}"))
    fun admit(bytes: ByteArray, registration: Registration, source: Source,
        checkpoint: ByteArray? = null): FileObservation {
        // A native recorder counterpart has no SFCP2. Even a valid token from
        // its SDK partner is not a file binding for this distinct native file.
        require(checkpoint == null) { "Native recorder checkpoint forbidden" }
        require(hash(registration.fileSHA256) && hash(source.sdkFileSHA256))
        require(registration.sdkFileSHA256 == source.sdkFileSHA256 &&
            registration.sourceGeneration == source.generation && source.generation > 0)
        require(source.head in 0..8191 && source.tail in 0..1023 && source.programmeFrames in 1L..48000L)
        require(source.packets.size in 1..128)
        val first = source.packets.first().dts
        require(first >= 0 && first <= Long.MAX_VALUE - 128 * 1024 - 8191)
        require(source.mediaTime == first + source.head)
        require(source.packets.size * 1024L == source.head + source.programmeFrames + source.tail)
        val actual = inspect(bytes)
        require(actual.sha256 == registration.fileSHA256) { "Native file identity changed" }
        require(actual.packets.size == source.packets.size)
        for (i in actual.packets.indices) {
            val a = actual.packets[i]; val b = source.packets[i]
            require(hash(b.sha256) && b.dts == first + i * 1024L && b.pts == b.dts && b.duration == 1024)
            require(a.size == b.size && a.sha256 == b.sha256 && a.dts == b.dts && a.pts == b.pts && a.duration == b.duration) {
                "Native recorder packet/index/coded axis differs from checked SDK source"
            }
        }
        return actual
    }

    private data class Box(val start: Int, val end: Int, val tag: String) { val body get() = start + 8 }
    private class Parser(val bytes: ByteArray) {
        private fun range(at: Int, n: Int, end: Int) {
            require(at >= 0 && n >= 0 && end <= bytes.size && at.toLong() + n <= end) { "Truncated child field" }
        }
        fun u32(at: Int, end: Int): Long {
            range(at, 4, end); var value = 0L
            for (i in 0..3) value = (value shl 8) or (bytes[at + i].toLong() and 255)
            return value
        }
        fun u64(at: Int, end: Int): Long {
            range(at, 8, end); require(bytes[at].toInt() and 128 == 0) { "Unsigned clock not representable" }
            return (u32(at, end) shl 32) or u32(at + 4, end)
        }
        fun text(at: Int, n: Int, end: Int): String { range(at, n, end); return String(bytes, at, n, Charsets.US_ASCII) }
        fun boxes(start: Int, end: Int): List<Box> {
            range(start, 0, end); val result = mutableListOf<Box>(); var at = start
            while (at < end) {
                require(result.size < 260); val size = u32(at, end)
                require(size >= 8 && size <= end.toLong() - at) { "Unsupported or overflowing box size" }
                val tag = text(at + 4, 4, end)
                result.add(Box(at, at + size.toInt(), tag)); at += size.toInt()
            }
            require(at == end); return result
        }
        fun children(box: Box, tags: List<String>): List<Box> {
            val result = boxes(box.body, box.end)
            require(result.map { it.tag } == tags) { "Unsupported native child profile" }; return result
        }
        fun full(box: Box, version: Int, flags: Int, bodyBytes: Int) {
            require(box.end - box.body == bodyBytes)
            require(u32(box.body, box.end) == (version.toLong() shl 24) + flags)
        }
        fun zeroTable(box: Box, sizeTable: Boolean = false) {
            full(box, 0, 0, if (sizeTable) 12 else 8)
            require(u32(box.body + 4, box.end) == 0L)
            if (sizeTable) require(u32(box.body + 8, box.end) == 0L)
        }
    }

    /** Reject unsupported profiles rather than guess omitted flags, edits,
     * sample tables, external references or generic AAC packet cardinality. */
    fun inspect(bytes: ByteArray): FileObservation {
        require(bytes.size in 1..MAX_BYTES)
        val p = Parser(bytes); val top = p.boxes(0, bytes.size)
        require(top.size in 4..258 && top[0].tag == "ftyp" && top[1].tag == "moov" && (top.size - 2) % 2 == 0)
        val ftyp = top[0]
        require(bytes.copyOfRange(ftyp.start, ftyp.end).contentEquals(hexBytes("0000001c6674797069736f350000000169736f6d69736f35686c7366")))
        val movie = p.children(top[1], listOf("mvhd", "trak", "mvex"))
        p.full(movie[0], 0, 0, 100)
        require(p.u32(movie[0].body + 12, movie[0].end) == 720000L && p.u32(movie[0].body + 16, movie[0].end) == 0L)
        val track = p.children(movie[1], listOf("tkhd", "mdia"))
        p.full(track[0], 0, 1, 84)
        require(p.u32(track[0].body + 12, track[0].end) == 1L && p.u32(track[0].body + 20, track[0].end) == 0L)
        val media = p.children(track[1], listOf("mdhd", "hdlr", "minf"))
        p.full(media[0], 0, 0, 24)
        require(p.u32(media[0].body + 12, media[0].end) == 48000L && p.u32(media[0].body + 16, media[0].end) == 0L)
        require(media[1].end - media[1].body in 24..128 && p.u32(media[1].body, media[1].end) == 0L)
        require(p.text(media[1].body + 8, 4, media[1].end) == "soun")
        val minf = p.children(media[2], listOf("smhd", "dinf", "stbl"))
        p.full(minf[0], 0, 0, 8)
        val dref = p.children(minf[1], listOf("dref")).single()
        require(dref.end - dref.body == 20 && p.u32(dref.body, dref.end) == 0L && p.u32(dref.body + 4, dref.end) == 1L)
        val url = p.boxes(dref.body + 8, dref.end).single(); require(url.tag == "url "); p.full(url, 0, 1, 4)
        val stbl = p.children(minf[2], listOf("stsd", "stts", "stsc", "stsz", "stco"))
        require(stbl[0].end - stbl[0].body == 83 && p.u32(stbl[0].body, stbl[0].end) == 0L && p.u32(stbl[0].body + 4, stbl[0].end) == 1L)
        // Exact observed mono AAC-LC ASC1188 sample-entry profile. This also
        // excludes encrypted/unknown sample entries and unbounded ES descriptors.
        require(bytes.copyOfRange(stbl[0].body + 8, stbl[0].end).contentEquals(hexBytes(
            "0000004b6d703461000000000000000100000000000000000001001000000000bb800000" +
            "000000276573647300000000031900020004114015000000000000000000000005021188060102")))
        p.zeroTable(stbl[1]); p.zeroTable(stbl[2]); p.zeroTable(stbl[3], true); p.zeroTable(stbl[4])
        val trex = p.children(movie[2], listOf("trex")).single(); p.full(trex, 0, 0, 24)
        require(p.u32(trex.body + 4, trex.end) == 1L && p.u32(trex.body + 8, trex.end) == 1L)
        require((12..20 step 4).all { p.u32(trex.body + it, trex.end) == 0L })
        val packets = mutableListOf<Packet>(); var firstDts = 0L
        for (i in 2 until top.size step 2) {
            val moof = top[i]; val mdat = top[i + 1]; require(moof.tag == "moof" && mdat.tag == "mdat" && moof.end == mdat.start)
            val parts = p.children(moof, listOf("mfhd", "traf")); p.full(parts[0], 0, 0, 8)
            require(p.u32(parts[0].body + 4, parts[0].end) == packets.size + 1L)
            val traf = p.children(parts[1], listOf("tfhd", "tfdt", "trun"))
            p.full(traf[0], 0, 0x020018, 16)
            require(p.u32(traf[0].body + 4, traf[0].end) == 1L && p.u32(traf[0].body + 8, traf[0].end) == 1024L)
            val size = p.u32(traf[0].body + 12, traf[0].end); require(size in 1L..65536L)
            p.full(traf[1], 1, 0, 12); val dts = p.u64(traf[1].body + 4, traf[1].end)
            if (packets.isEmpty()) { firstDts = dts; require(firstDts <= Long.MAX_VALUE - 128 * 1024L) }
            require(dts == firstDts + packets.size * 1024L)
            p.full(traf[2], 0, 1, 12); require(p.u32(traf[2].body + 4, traf[2].end) == 1L)
            val offset = p.u32(traf[2].body + 8, traf[2].end)
            require(offset == (mdat.body - moof.start).toLong() && size == (mdat.end - mdat.body).toLong())
            packets.add(Packet(mdat.body, size.toInt(), dts, dts, 1024, digest(bytes.copyOfRange(mdat.body, mdat.end))))
        }
        require(packets.size in 1..128)
        return FileObservation(digest(bytes), packets)
    }
    private fun hexBytes(value: String) = ByteArray(value.length / 2) { value.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
