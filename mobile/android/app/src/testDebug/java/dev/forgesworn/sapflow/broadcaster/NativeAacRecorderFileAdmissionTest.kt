// SPDX-License-Identifier: MIT
package dev.forgesworn.sapflow.broadcaster

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import java.nio.ByteBuffer
import java.nio.ByteOrder

class NativeAacRecorderFileAdmissionTest {
    private val sdkHash = "ab".repeat(32)
    private fun u32(value: Int) = ByteBuffer.allocate(4).order(ByteOrder.BIG_ENDIAN).putInt(value).array()
    private fun u64(value: Long) = ByteBuffer.allocate(8).order(ByteOrder.BIG_ENDIAN).putLong(value).array()
    private fun box(tag: String, vararg bodies: ByteArray): ByteArray {
        val body = bodies.fold(ByteArray(0)) { a, b -> a + b }
        return u32(body.size + 8) + tag.toByteArray(Charsets.US_ASCII) + body
    }
    private fun hex(value: String) = ByteArray(value.length / 2) { value.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
    private fun body(size: Int, vararg fields: Pair<Int, Int>) = ByteArray(size).also { bytes ->
        for ((offset, value) in fields) u32(value).copyInto(bytes, offset)
    }
    private data class Fixture(val bytes: ByteArray, val source: NativeAacRecorderFileAdmission.Source) {
        fun registration(bytes: ByteArray = this.bytes) = NativeAacRecorderFileAdmission.Registration(
            NativeAacRecorderFileAdmission.digest(bytes), source.sdkFileSHA256, source.generation)
    }
    private fun fixture(count: Int = 5, programme: Long = 2765, tail: Int = 307): Fixture {
        val ftyp = hex("0000001c6674797069736f350000000169736f6d69736f35686c7366")
        val stsd = box("stsd", u32(0), u32(1), hex(
            "0000004b6d703461000000000000000100000000000000000001001000000000bb800000" +
            "000000276573647300000000031900020004114015000000000000000000000005021188060102"))
        val tables = box("stbl", stsd, box("stts", body(8)), box("stsc", body(8)), box("stsz", body(12)), box("stco", body(8)))
        val references = box("dinf", box("dref", u32(0), u32(1), box("url ", u32(1))))
        val media = box("mdia", box("mdhd", body(24, 12 to 48000)),
            box("hdlr", body(24).also { "soun".toByteArray().copyInto(it, 8) }),
            box("minf", box("smhd", body(8)), references, tables))
        val moov = box("moov", box("mvhd", body(100, 12 to 720000)),
            box("trak", box("tkhd", body(84, 0 to 1, 12 to 1)), media),
            box("mvex", box("trex", body(24, 4 to 1, 8 to 1))))
        var bytes = ftyp + moov
        val expected = mutableListOf<NativeAacRecorderFileAdmission.OriginalPacket>()
        for (i in 0 until count) {
            val payload = ByteArray(4 + i % 3) { (17 * i + 3 * it + 1).toByte() }
            val dts = 8192L + i * 1024L
            val mfhd = box("mfhd", u32(0), u32(i + 1))
            val tfhd = box("tfhd", u32(0x020018), u32(1), u32(1024), u32(payload.size))
            val tfdt = box("tfdt", u32(0x01000000), u64(dts))
            val moof = box("moof", mfhd, box("traf", tfhd, tfdt, box("trun", u32(1), u32(1), u32(104))))
            bytes += moof + box("mdat", payload)
            expected.add(NativeAacRecorderFileAdmission.OriginalPacket(payload.size, dts, dts, 1024, NativeAacRecorderFileAdmission.digest(payload)))
        }
        return Fixture(bytes, NativeAacRecorderFileAdmission.Source(sdkHash, 2, 2048, tail, programme, 10240, expected))
    }
    private fun admit(f: Fixture, bytes: ByteArray = f.bytes) = NativeAacRecorderFileAdmission.admit(bytes, f.registration(bytes), f.source)
    private fun tagAt(bytes: ByteArray, tag: String): Int {
        val needle = tag.toByteArray(Charsets.US_ASCII)
        return (4..bytes.size - 4).first { at -> (0..3).all { bytes[at + it] == needle[it] } } - 4
    }

    @Test fun selectedShortAndLongFilesRetainOriginalAxisAndWholePackets() {
        for (f in listOf(fixture(), fixture(49, 48000, 128))) {
            val result = admit(f)
            assertEquals(f.source.packets.size, result.packets.size)
            assertEquals(8192L, result.packets.first().dts)
            assertEquals(8192L + 1024 * (f.source.packets.size - 1), result.packets.last().pts)
            assertEquals(f.source.packets.map { it.sha256 }, result.packets.map { it.sha256 })
            assertEquals(0L, result.movieDuration); assertEquals(0, result.edits)
        }
    }
    @Test fun nativeFileHashAndExternalSourceIdentityMustBothMatch() {
        val f = fixture()
        assertThrows(IllegalArgumentException::class.java) { NativeAacRecorderFileAdmission.admit(f.bytes, f.registration().copy(fileSHA256 = "00".repeat(32)), f.source) }
        assertThrows(IllegalArgumentException::class.java) { NativeAacRecorderFileAdmission.admit(f.bytes, f.registration().copy(sdkFileSHA256 = "00".repeat(32)), f.source) }
        assertThrows(IllegalArgumentException::class.java) { NativeAacRecorderFileAdmission.admit(f.bytes, f.registration().copy(sourceGeneration = 3), f.source) }
    }
    @Test fun foreignCheckpointIsRefusedForNativeFileEvenWithValidSource() {
        val f = fixture()
        assertThrows(IllegalArgumentException::class.java) { NativeAacRecorderFileAdmission.admit(f.bytes, f.registration(), f.source, byteArrayOf(1)) }
        assertThrows(IllegalArgumentException::class.java) { NativeAacRecorderFileAdmission.admit(f.bytes, f.registration(), f.source, ByteArray(0)) }
    }
    @Test fun changedPayloadIsRefusedEvenWhenFileHashWasRecomputed() {
        val f = fixture(); val bytes = f.bytes.copyOf(); bytes[tagAt(bytes, "mdat") + 8] = 99
        assertThrows(IllegalArgumentException::class.java) { admit(f, bytes) }
    }
    @Test fun packetOrderAndCountCannotBeChangedByRegistration() {
        val f = fixture()
        val reordered = f.source.copy(packets = f.source.packets.reversed())
        assertThrows(IllegalArgumentException::class.java) { NativeAacRecorderFileAdmission.admit(f.bytes, f.registration(), reordered) }
        val missing = fixture(4, 1741, 307)
        assertThrows(IllegalArgumentException::class.java) { NativeAacRecorderFileAdmission.admit(missing.bytes, missing.registration(), f.source) }
    }
    @Test fun rawCodedTimeCannotBeRebasedOrChanged() {
        val f = fixture(); val bytes = f.bytes.copyOf(); val at = tagAt(bytes, "tfdt")
        u64(0).copyInto(bytes, at + 12)
        assertThrows(IllegalArgumentException::class.java) { admit(f, bytes) }
        val overflow = f.bytes.copyOf(); u64(Long.MAX_VALUE).copyInto(overflow, at + 12)
        assertThrows(IllegalArgumentException::class.java) { admit(f, overflow) }
    }
    @Test fun childBoundsNeverBorrowSiblingBytes() {
        val f = fixture()
        for (tag in listOf("mvhd", "tkhd", "mdhd", "hdlr", "stsd", "tfhd", "tfdt", "trun")) {
            val bytes = f.bytes.copyOf(); u32(12).copyInto(bytes, tagAt(bytes, tag))
            assertThrows(IllegalArgumentException::class.java) { admit(f, bytes) }
        }
        for (n in listOf(0, 1, 7, 28, f.bytes.size - 1)) {
            assertThrows(IllegalArgumentException::class.java) { admit(f, f.bytes.copyOf(n)) }
        }
    }
    @Test fun editsHybridTablesAndUnexpectedSampleEntryAreUnsupported() {
        val f = fixture()
        val edits = f.bytes.copyOf(); "edts".toByteArray().copyInto(edits, tagAt(edits, "mvex") + 4)
        assertThrows(IllegalArgumentException::class.java) { admit(f, edits) }
        val hybrid = f.bytes.copyOf(); u32(1).copyInto(hybrid, tagAt(hybrid, "stts") + 12)
        assertThrows(IllegalArgumentException::class.java) { admit(f, hybrid) }
        val wrongAsc = f.bytes.copyOf(); val entry = tagAt(wrongAsc, "mp4a"); wrongAsc[entry + 69] = 0x11
        assertThrows(IllegalArgumentException::class.java) { admit(f, wrongAsc) }
    }
    @Test fun fragmentOffsetFlagsAndActualMdatCoverageAreChecked() {
        val f = fixture()
        for ((tag, offset, value) in listOf(Triple("trun", 8, 0xf01), Triple("trun", 16, 103), Triple("tfhd", 8, 0x020038), Triple("tfhd", 20, 1))) {
            val bytes = f.bytes.copyOf(); u32(value).copyInto(bytes, tagAt(bytes, tag) + offset)
            assertThrows(IllegalArgumentException::class.java) { admit(f, bytes) }
        }
    }
    @Test fun malformedHeadTailOrProgrammeGeometryCannotReachAdmission() {
        val f = fixture()
        for (s in listOf(f.source.copy(head = -1), f.source.copy(tail = 1024), f.source.copy(mediaTime = 10239), f.source.copy(programmeFrames = 2764), f.source.copy(generation = 0))) {
            assertThrows(IllegalArgumentException::class.java) { NativeAacRecorderFileAdmission.admit(f.bytes, f.registration(), s) }
        }
    }
}

/** Jazzer-compatible pure parser target. No dependency, codec or test result is
 * implied. The decoded structure remains untrusted without separate admission. */
internal object NativeAacRecorderFileAdmissionFuzz {
    @JvmStatic fun fuzzerTestOneInput(bytes: ByteArray) {
        if (bytes.size > 65536) return
        try { NativeAacRecorderFileAdmission.inspect(bytes) }
        catch (_: IllegalArgumentException) { /* Expected bounded refusal only. */ }
    }
}
