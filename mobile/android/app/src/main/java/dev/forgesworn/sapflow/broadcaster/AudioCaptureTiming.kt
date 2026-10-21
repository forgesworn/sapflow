package dev.forgesworn.sapflow.broadcaster

/** Hardware capture position is independent of how quickly the client reads.
 * A cursor outside the actual capture ring cannot identify retained PCM safely.
 * Refuse that epoch rather than moving old PCM to the present or hiding loss. */
internal class AudioCaptureTiming(private val epochNs: Long, private val capacityFrames: Long) {
    private var readFrames = 0L
    private var lastPosition = -1L
    private var lastTimestampNs = -1L
    var backlogFrames = 0L
        private set
    var timestampAgeUs = 0L
        private set
    val framesRead get() = readFrames
    private var phase = "none"
    private var beforeCursor = 0L
    private var beforePosition = 0L
    private var beforeStampNs = 0L
    private var beforeNowNs = 0L
    private var afterCursor = 0L
    private var afterCount = 0
    private var afterPosition = 0L
    private var afterStampNs = 0L
    private var afterNowNs = 0L
    val observation get() = "phase=$phase before readCursor=$beforeCursor count=0 hwPosition=$beforePosition stampNs=$beforeStampNs nowNs=$beforeNowNs stampAgeNs=${beforeNowNs - beforeStampNs}; after readCursor=$afterCursor count=$afterCount hwPosition=$afterPosition stampNs=$afterStampNs nowNs=$afterNowNs stampAgeNs=${afterNowNs - afterStampNs}; capacityFrames=$capacityFrames epochNs=$epochNs"

    init { require(epochNs >= 0 && capacityFrames in 1..Int.MAX_VALUE.toLong()) }

    fun beforeRead(position: Long, timestampNs: Long, nowNs: Long) {
        observe("before", 0, position, timestampNs, nowNs)
        validate(position, timestampNs, nowNs)
        val produced = Math.addExact(position, (nowNs - timestampNs) * 48_000 / 1_000_000_000)
        backlogFrames = maxOf(0, produced - readFrames)
        require(backlogFrames <= capacityFrames) { "Microphone capture overrun or uncertain PCM position; $observation estimatedBacklog=$backlogFrames" }
    }

    private fun observe(phase: String, count: Int, position: Long, timestampNs: Long, nowNs: Long) {
        this.phase = phase
        if (phase == "before") {
            beforeCursor = readFrames; beforePosition = position; beforeStampNs = timestampNs; beforeNowNs = nowNs
        } else {
            afterCursor = readFrames; afterCount = count; afterPosition = position; afterStampNs = timestampNs; afterNowNs = nowNs
        }
    }

    private fun validate(position: Long, timestampNs: Long, nowNs: Long) {
        require(position >= 0 && timestampNs >= epochNs - 100_000_000L)
        require(nowNs >= timestampNs && nowNs - timestampNs <= 100_000_000L) { "Microphone hardware timestamp is stale or unavailable; $observation" }
        require(position >= lastPosition && timestampNs >= lastTimestampNs) { "Microphone hardware clock regressed; $observation" }
        lastPosition = position; lastTimestampNs = timestampNs
    }

    fun read(count: Int, position: Long, timestampNs: Long, nowNs: Long): Long {
        require(count.toLong() in 1..capacityFrames)
        observe("after", count, position, timestampNs, nowNs)
        validate(position, timestampNs, nowNs)
        val afterRead = Math.addExact(readFrames, count.toLong())
        // Account for timestamp age without pretending it measures exact loss.
        val produced = Math.addExact(position, (nowNs - timestampNs) * 48_000 / 1_000_000_000)
        backlogFrames = maxOf(0, produced - afterRead)
        require(backlogFrames <= capacityFrames) { "Microphone capture overrun or uncertain PCM position; $observation estimatedBacklog=$backlogFrames" }
        val deltaFrames = readFrames - position
        require(deltaFrames in -(capacityFrames + 4800)..(capacityFrames + 4800)) { "Microphone position arithmetic bound exceeded" }
        val firstNs = Math.addExact(timestampNs, deltaFrames * 1_000_000_000L / 48_000)
        require(firstNs >= epochNs) { "Microphone PCM predates the capture epoch" }
        readFrames = afterRead
        timestampAgeUs = (nowNs - timestampNs) / 1000
        return (firstNs - epochNs) / 1000
    }
}
