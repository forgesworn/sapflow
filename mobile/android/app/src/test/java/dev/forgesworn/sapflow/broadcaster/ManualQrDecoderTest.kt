package dev.forgesworn.sapflow.broadcaster

import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import java.io.File
import java.util.Base64
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ManualQrDecoderTest {
    private val document = """{"version":1,"kind":"sapflow-manual-destination","name":"Generated QR destination","transport":"rtmp","endpoint":"rtmps://stream.example.invalid/live","streamKey":"generated-key-only","streamID":"","passphrase":"","allowPlaintext":false}""".toByteArray()
    private fun link(bytes: ByteArray = document) = ManualDestinationEnvelope.PREFIX + Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
    private fun image(text: String): ByteArray {
        val matrix = QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, 480, 480,
            mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.L, EncodeHintType.MARGIN to 4))
        return ByteArray(480 * 480) { index -> if (matrix[index % 480, index / 480]) 0 else 255.toByte() }
    }
    @Test fun actualPinnedDecoderReadsGeneratedQrInEveryCardinalOrientation() {
        val text = link(); var pixels = image(text); val decoder = ManualQrDecoder()
        repeat(4) {
            assertEquals(text, decoder.decode(pixels, 480, 480))
            pixels = ByteArray(pixels.size) { index -> pixels[(479 - index % 480) * 480 + index / 480] }
        }
    }
    @Test fun cameraScannerCannotAcceptForeignAuthorityOrMalformedProfiles() {
        val decoder = ManualQrDecoder()
        for (text in listOf("bunker://generated-not-an-import", "rtmp://stream.example.invalid/live",
            link("{\"version\":1,\"grant\":\"not-authority\"}".toByteArray()), link() + "=")) {
            assertNull(decoder.decode(image(text), 480, 480)); assertNull(ManualQrDecoder.validate(text))
        }
        assertNull(decoder.decode(ByteArray(480 * 480) { 255.toByte() }, 480, 480))
    }
    @Test fun qrByteCapIsIndependentOfCanonicalLinkAndJsonBounds() {
        var bytes = document
        while (link(bytes + 32).length <= 2048) bytes += 32
        // Prefix length means an unpadded canonical envelope cannot be 2048 bytes.
        val exact = link(bytes); assertEquals(2047, exact.length)
        assertEquals(exact, ManualQrDecoder.validate(exact))
        assertEquals(exact, ManualQrDecoder().decode(image(exact), 480, 480))
        val above = link(bytes + 32)
        assertEquals(2049, above.length)
        assertNotNull(ManualDestinationProfile.parse(ManualDestinationEnvelope.decode(above)))
        assertNull(ManualQrDecoder.validate(above)); assertNull(ManualQrDecoder().decode(image(above), 480, 480))
    }
    @Test fun sharedGeneratedCorpusRetainsStrictParserRulesWithSmallerQrCap() {
        val configured = System.getProperty("sapflow.fixtureRoot")?.let(::File)
        val roots = generateSequence(File(requireNotNull(System.getProperty("user.dir"))).absoluteFile) { it.parentFile }.take(8).toList()
        val file = configured?.resolve("tests/fixtures/manual-destination-link-v1.json")
            ?: roots.map { it.resolve("tests/fixtures/manual-destination-link-v1.json") }.first { it.isFile }
        assertTrue(file.length() in 1..1_048_576)
        val rows = V4vJson.parse(file.readText(), 1_048_576).getValue("cases").jsonArray
        assertTrue(rows.size >= 98)
        rows.forEach { row ->
            val value = row.jsonObject; val text = value.getValue("text").jsonPrimitive.content
            val wanted = value.getValue("accepted").jsonPrimitive.boolean && text.length <= 2048 && text.all { it.code in 0..127 }
            assertEquals(value.getValue("id").jsonPrimitive.content, wanted, ManualQrDecoder.validate(text) != null)
        }
    }
}
