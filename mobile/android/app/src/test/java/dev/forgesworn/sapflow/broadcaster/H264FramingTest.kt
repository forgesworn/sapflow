package dev.forgesworn.sapflow.broadcaster

import java.nio.ByteBuffer
import org.junit.Assert.*
import org.junit.Test

class H264FramingTest {
    private fun bytes(vararg bytes: Int) = ByteBuffer.allocateDirect(bytes.size).apply {
        bytes.forEach { put(it.toByte()) }; flip()
    }
    private fun array(buffer: ByteBuffer): ByteArray = ByteArray(buffer.remaining()).also { buffer.duplicate().get(it) }

    @Test fun annexBIsConvertedWithOneReusableBufferAndParameterSetsRemoved() {
        val framing = H264Framing(64)
        val input = bytes(0, 0, 0, 1, 0x67, 0x64, 0, 0, 1, 0x68, 0x12, 0, 0, 0, 1, 0x65, 0x45)
        val output = framing.sample(input)
        assertArrayEquals(byteArrayOf(0, 0, 0, 2, 0x65, 0x45), array(output))
        val next = framing.sample(bytes(0, 0, 1, 0x41, 0x23))
        assertSame(output, next)
        assertArrayEquals(byteArrayOf(0, 0, 0, 2, 0x41, 0x23), array(next))
        assertEquals(0, input.position())
    }

    @Test fun existingAvccRemainsABorrowedDirectViewIncludingSingleByteNal() {
        val input = bytes(0, 0, 0, 1, 0x65, 0, 0, 0, 2, 0x06, 0x12)
        val output = H264Framing().sample(input)
        assertTrue(output.isDirect)
        input.put(4, 0x41)
        assertEquals(0x41.toByte(), output.get(4))
    }

    @Test fun parametersAreBoundedAndFoundInCombinedCodecConfiguration() {
        val input = bytes(0, 0, 0, 1, 0x67, 0x64, 0, 0, 1, 0x68, 0x12)
        assertArrayEquals(byteArrayOf(0x67, 0x64), array(H264Framing.parameterSet(input, 7)!!))
        assertArrayEquals(byteArrayOf(0x68, 0x12), array(H264Framing.parameterSet(input, 8)!!))
        assertNull(H264Framing.parameterSet(input, 5))
        assertThrows(IllegalArgumentException::class.java) { H264Framing.parameterSet(ByteBuffer.allocate(8193), 7) }
    }

    @Test fun malformedOversizedAndConfigurationOnlySamplesAreRefused() {
        for (input in listOf(bytes(), bytes(0, 0, 1), bytes(0, 0, 1, 0x67, 0x64), bytes(1, 2, 3), bytes(0, 0, 0, 8, 0x65))) {
            assertThrows(IllegalArgumentException::class.java) { H264Framing(32).sample(input) }
        }
        assertThrows(IllegalArgumentException::class.java) { H264Framing(8).sample(bytes(0, 0, 1, 0x65, 1, 2, 3, 4, 5)) }
        // Three-byte Annex-B prefixes require one additional AVCC prefix byte.
        assertThrows(IllegalArgumentException::class.java) { H264Framing(8).sample(bytes(0, 0, 1, 0x65, 1, 2, 3, 4)) }
    }
}
