package dev.forgesworn.sapflow.broadcaster

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat

/** Identical native AAC request shared by capture and the generated-only probe.
 * This request is not a guarantee of the codec's output packing or trim facts. */
internal object AudioAacEncoderConfiguration {
    fun configure(codec: MediaCodec, bitrate: Int) {
        codec.configure(MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC,48_000,1).apply {
            setInteger(MediaFormat.KEY_AAC_PROFILE,MediaCodecInfo.CodecProfileLevel.AACObjectLC)
            setInteger(MediaFormat.KEY_BIT_RATE,bitrate);setInteger(MediaFormat.KEY_MAX_INPUT_SIZE,2048)
        },null,null,MediaCodec.CONFIGURE_FLAG_ENCODE)
    }
}
