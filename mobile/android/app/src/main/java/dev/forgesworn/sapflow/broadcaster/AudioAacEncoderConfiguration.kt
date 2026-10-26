package dev.forgesworn.sapflow.broadcaster

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Build

/** Shared native request. Cold observations never enable source/trim authority. */
internal object AudioAacEncoderConfiguration {
    // String constants avoid referencing API35 fields below the supported API.
    private const val BATCH_MAX = "buffer-batch-max-output-size"
    private const val BATCH_THRESHOLD = "buffer-batch-threshold-output-size"
    private fun <T> read(block: () -> T): AudioAacProfile.Fact<T> = try {
        AudioAacProfile.known(block())
    } catch (_: ClassCastException) { AudioAacProfile.missing(AudioAacProfile.State.WRONG_TYPE) }
    catch (_: Exception) { AudioAacProfile.missing(AudioAacProfile.State.READ_FAILED) }
    private fun name(block: () -> String): AudioAacProfile.Fact<String> {
        val fact = read(block)
        return if (fact.state == AudioAacProfile.State.VALUE && (fact.value?.length ?: 0) !in 1..256)
            AudioAacProfile.missing(AudioAacProfile.State.UNSUPPORTED) else fact
    }
    private fun integer(format: MediaFormat, key: String): AudioAacProfile.Fact<Int> = try {
        if (!format.containsKey(key)) AudioAacProfile.missing(AudioAacProfile.State.ABSENT)
        else read { format.getInteger(key) }
    } catch (_: Exception) { AudioAacProfile.missing(AudioAacProfile.State.READ_FAILED) }
    fun format(value: MediaFormat?, unknown: AudioAacProfile.State = AudioAacProfile.State.READ_FAILED): AudioAacProfile.Format {
        if (value == null) return AudioAacProfile.Format(AudioAacProfile.missing(unknown), AudioAacProfile.missing(unknown),
            AudioAacProfile.missing(unknown), AudioAacProfile.missing(unknown), AudioAacProfile.missing(unknown), AudioAacProfile.missing(unknown))
        val mime = try {
            if (!value.containsKey(MediaFormat.KEY_MIME)) AudioAacProfile.missing<String>(AudioAacProfile.State.ABSENT)
            else name { checkNotNull(value.getString(MediaFormat.KEY_MIME)) }
        } catch (_: Exception) { AudioAacProfile.missing<String>(unknown) }
        val csd = try {
            if (!value.containsKey("csd-0")) AudioAacProfile.missing<String>(AudioAacProfile.State.ABSENT)
            else {
                val bytes = checkNotNull(value.getByteBuffer("csd-0")).duplicate()
                if (bytes.remaining() != 2) AudioAacProfile.missing<String>(AudioAacProfile.State.UNSUPPORTED)
                else AudioAacProfile.known("%02x%02x".format(bytes.get().toInt() and 255, bytes.get().toInt() and 255))
            }
        } catch (_: ClassCastException) { AudioAacProfile.missing<String>(AudioAacProfile.State.WRONG_TYPE) }
        catch (_: Exception) { AudioAacProfile.missing<String>(unknown) }
        val max: AudioAacProfile.Fact<Int> = if (Build.VERSION.SDK_INT >= 35) integer(value, BATCH_MAX) else AudioAacProfile.missing(AudioAacProfile.State.API_UNAVAILABLE)
        val threshold: AudioAacProfile.Fact<Int> = if (Build.VERSION.SDK_INT >= 35) integer(value, BATCH_THRESHOLD) else AudioAacProfile.missing(AudioAacProfile.State.API_UNAVAILABLE)
        return AudioAacProfile.Format(mime, integer(value, MediaFormat.KEY_SAMPLE_RATE), integer(value, MediaFormat.KEY_CHANNEL_COUNT), csd, max, threshold)
    }
    private fun returned(block: () -> MediaFormat): AudioAacProfile.Format = try { format(block()) } catch (_: Exception) { format(null) }
    fun configure(codec: MediaCodec, bitrate: Int): AudioAacProfile.Setup {
        codec.configure(MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC,48_000,1).apply {
            setInteger(MediaFormat.KEY_AAC_PROFILE,MediaCodecInfo.CodecProfileLevel.AACObjectLC)
            setInteger(MediaFormat.KEY_BIT_RATE,bitrate);setInteger(MediaFormat.KEY_MAX_INPUT_SIZE,2048)
        },null,null,MediaCodec.CONFIGURE_FLAG_ENCODE)
        val api = Build.VERSION.SDK_INT
        val unavailable = AudioAacProfile.State.API_UNAVAILABLE
        val info = read { codec.codecInfo }
        val caps = read { checkNotNull(info.value).getCapabilitiesForType(MediaFormat.MIMETYPE_AUDIO_AAC) }
        val supported = if (api >= 29) read { checkNotNull(caps.value).isFeatureSupported(MediaCodecInfo.CodecCapabilities.FEATURE_MultipleFrames) }
            else AudioAacProfile.missing<Boolean>(unavailable)
        val required = if (api >= 29) read { checkNotNull(caps.value).isFeatureRequired(MediaCodecInfo.CodecCapabilities.FEATURE_MultipleFrames) }
            else AudioAacProfile.missing<Boolean>(unavailable)
        val selection = AudioAacProfile.Selection(api, name { codec.name },
            if (api >= 29) name { codec.canonicalName } else AudioAacProfile.missing(unavailable),
            read { checkNotNull(info.value).isEncoder }, supported, required, AudioAacProfile.Request(bitrate))
        return AudioAacProfile.Setup(selection, returned { codec.inputFormat }, returned { codec.outputFormat })
    }
}
