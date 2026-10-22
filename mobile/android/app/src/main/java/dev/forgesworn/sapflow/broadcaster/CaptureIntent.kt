package dev.forgesworn.sapflow.broadcaster

/** Immutable pre-Prepare topology. Audio-only has no video geometry or pose.
 * The 1 Mbit/s audio grant floor is an engineering transport budget, not the
 * AAC bitrate or a physically qualified peak guarantee. */
internal sealed class CaptureIntent {
    data class AudioVideo(val profile: CaptureProfile = CaptureProfile.DEFAULT) : CaptureIntent()
    data object AudioOnly48kMono : CaptureIntent()
    val hasVideo: Boolean get() = this is AudioVideo
    val requiredPermissions: List<String> get() = if (hasVideo) listOf("android.permission.CAMERA", "android.permission.RECORD_AUDIO") else listOf("android.permission.RECORD_AUDIO")
    fun permissionsGranted(granted: (String) -> Boolean): Boolean = requiredPermissions.all(granted)
    val hasAudio: Boolean get() = true
    val videoProfile: CaptureProfile? get() = (this as? AudioVideo)?.profile
    val audioBps: Int get() = 96_000
    val elementaryBps: Int get() = (videoProfile?.videoBps ?: 0) + audioBps
    val minimumPublishBps: Long get() = videoProfile?.minimumPublishBps ?: 1_000_000L
    val profileId: String get() = videoProfile?.name ?: "AUDIO_ONLY_48K_MONO"
    val label: String get() = videoProfile?.label ?: "Microphone only"
    fun requireGrant(maxPublishBps: Long) {
        require(maxPublishBps >= minimumPublishBps) { "$label needs a grant of at least $minimumPublishBps bits/s; choose before Prepare" }
    }
    fun srtPacingBytesPerSecond(maxPublishBps: Long): ULong {
        requireGrant(maxPublishBps)
        return minOf(1_250_000L, maxPublishBps / 8).toULong()
    }
    fun codecsReady(videoPresent: Boolean, audioPresent: Boolean): Boolean {
        require(hasVideo || !videoPresent) { "Unexpected video configuration" }
        return (!hasVideo || videoPresent) && audioPresent
    }
    fun requireTrackMimes(mimes: List<String?>) {
        require(mimes.size == if (hasVideo) 2 else 1)
        require(mimes.count { it == "audio/mp4a-latm" } == 1 && mimes.count { it == "video/avc" } == (if (hasVideo) 1 else 0))
    }
    fun tailComplete(videoEnded: Boolean, audioEnded: Boolean): Boolean = (!hasVideo || videoEnded) && audioEnded
    fun matchesTracks(video: Boolean, audio: Boolean): Boolean = video == hasVideo && audio == hasAudio
    companion object { val DEFAULT: CaptureIntent = AudioVideo() }
}
