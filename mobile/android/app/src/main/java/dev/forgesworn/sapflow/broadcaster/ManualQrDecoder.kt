package dev.forgesworn.sapflow.broadcaster

import com.google.zxing.BinaryBitmap
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader

/** Pure pinned core only. One reader/array owner; no runtime services or URI handling. */
internal class ManualQrDecoder {
    private val reader = QRCodeReader()
    fun decode(luma: ByteArray, width: Int, height: Int): String? {
        require(width in 1..640 && height in 1..480 && width * height <= luma.size && luma.size <= QrLuminance.MAX_PIXELS)
        val text = try {
            reader.decode(BinaryBitmap(HybridBinarizer(PlanarYUVLuminanceSource(luma, width, height, 0, 0, width, height, false)))).text
        } catch (_: ReaderException) { return null }
        finally { reader.reset() }
        return validate(text)
    }
    companion object {
        fun validate(text: String): String? {
            val bytes = try { ManualDestinationEnvelope.decodeQr(text) } catch (_: ManualProfileFailure) { return null }
            return try { ManualDestinationProfile.parse(bytes); text } catch (_: ManualProfileFailure) { null }
            finally { bytes.fill(0) }
        }
    }
}
