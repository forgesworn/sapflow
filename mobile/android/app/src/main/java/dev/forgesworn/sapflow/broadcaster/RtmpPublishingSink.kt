package dev.forgesworn.sapflow.broadcaster

import android.net.DnsResolver
import android.net.InetAddresses
import android.os.CancellationSignal
import android.os.SystemClock
import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NativeRtmpPublisher
import dev.forgesworn.sapflow.NativeRtmpEvent
import dev.forgesworn.sapflow.NativeRtmpState
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.nio.ByteBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.locks.ReentrantLock
import javax.net.ssl.SSLSocket
import javax.net.ssl.SSLSocketFactory
import kotlin.concurrent.withLock

/** One explicitly selected attempt. Platform TLS validates CA chains and the
 * destination hostname; there is no trust override or plaintext fallback.
 * Failed attempts stop sending while independent capture/recording can continue. */
internal class RtmpPublishingSink(private val destination: RtmpDestination, streamKey: String,
    private val events: (PublishingAttemptEvent) -> Unit = {},
    private val intent: CaptureIntent = CaptureIntent.DEFAULT) : EncodedSink, SocketOutputProvider {
    private val outputCounter = SocketOutputCounter(if (destination.tls) SocketOutputAccounting.RTMPS_APPLICATION else SocketOutputAccounting.RTMP_APPLICATION)
    override val socketOutput: SocketOutputSource get() = outputCounter
    private val message = AtomicReference("Destination prepared · press Start to connect")
    override val status: String get() = message.get()
    private val closed = AtomicBoolean(false)
    private val ready = AtomicBoolean(false)
    private val accepted = AtomicBoolean(false)
    private val startupReported = AtomicBoolean(false)
    private val peerReported = AtomicBoolean(false)
    private val firstKeyframe = FirstKeyframeGate(intent.hasVideo)
    private val socket = AtomicReference<Socket?>()
    private val tcpSocket = AtomicReference<Socket?>()
    private val dnsCancellation = CancellationSignal()
    private val lock = ReentrantLock()
    private val timers = ScheduledThreadPoolExecutor(1) { command -> Thread(command, "sapflow-rtmp-deadline") }.apply {
        removeOnCancelPolicy = true
    }
    private var startedMs = 0L
    private var preparedKey = streamKey
    private var publisher: NativeRtmpPublisher? = null
    private var reader: Thread? = null
    private var nativeDeadline: ScheduledFuture<*>? = null // lock only
    private val deadlineGeneration = AtomicLong()
    private val lockDelay = DelayDiagnostics()
    private val writeDelay = DelayDiagnostics()
    private val consumeDelay = DelayDiagnostics()
    private val queuedWireBytes = AtomicLong()
    override val diagnostics: String get() = "RTMP consume ${consumeDelay.text()} · lock ${lockDelay.text()} · write ${writeDelay.text()} · native wire ${queuedWireBytes.get()}B"
    private val lifecycle = ExplicitAttempt(::startNetwork, ::closeNetwork)

    override fun start() = lifecycle.start()

    private fun startNetwork() {
        check(!closed.get()) { "Attempt closed" }
        startedMs = SystemClock.elapsedRealtime()
        publisher = NativeRtmpPublisher(destination.app, preparedKey, destination.tcUrl, 0uL, startedMs.toULong()).apply {
            // Immutable declared topology, never inferred from first CSD arrival.
            // A partial first CSD arrival must never turn this into video-only.
            declareTracks(intent.hasVideo, intent.hasAudio)
        }
        preparedKey = ""
        lock.withLock { armNativeDeadline() }
        message.set("Connecting one ${if (destination.tls) "RTMPS" else "development RTMP"} attempt")
        reader = Thread(::readLoop, "sapflow-rtmp-reader").apply { start() }
    }

    override fun consume(sample: MediaSample) {
        if (closed.get() || !ready.get()) return
        val startedNs = System.nanoTime()
        try {
            if (!firstKeyframe.admit(sample::isVideo, sample::isKeyframe)) return
            lock.withLock {
                lockDelay.record(System.nanoTime() - startedNs)
                if (closed.get() || !ready.get()) return
                // The origin may be silent after publication. Refresh the
                // transport clock before assigning owned wire-media deadlines.
                val snapshot = publisher!!.sendAndTakeOutput(sample, nowMs())
                check(snapshot.state == NativeRtmpState.PUBLISHING) { "Origin stopped publication" }
                reportProgress(snapshot.startupSubmitted, snapshot.peerMediaProgress)
                queuedWireBytes.set(snapshot.queuedOutputBytes.toLong())
                // These fields are Kotlin values from the SAME native lock,
                // avoiding separate sample/state/queue/deadline crossings.
                armNativeDeadline(snapshot.nextDeadlineMs)
                writeOutput(snapshot.output)
                firstKeyframe.committed()
            }
        } catch (error: Exception) {
            fail("Attempt stopped: ${error.javaClass.simpleName}. Capture can continue.")
        } finally {
            consumeDelay.record(System.nanoTime() - startedNs)
            if (closed.get()) runCatching { releasePublisher() }
        }
    }

    private fun readLoop() {
        try {
            val addresses = resolve(destination.host)
            var connected: Socket? = null
            val deadline = SystemClock.elapsedRealtime() + 3000
            for (address in addresses.take(4)) {
                if (closed.get()) return
                val remaining = deadline - SystemClock.elapsedRealtime()
                if (remaining <= 0) break
                val candidate = Socket()
                tcpSocket.set(candidate)
                socket.set(candidate)
                try {
                    candidate.connect(InetSocketAddress(address, destination.port), remaining.toInt())
                    candidate.tcpNoDelay = true
                    connected = candidate
                    break
                } catch (_: Exception) { runCatching { candidate.close() } }
            }
            var transport = connected ?: error("Destination connection failed")
            if (destination.tls) {
                val tls = (SSLSocketFactory.getDefault() as SSLSocketFactory)
                    .createSocket(transport, destination.host, destination.port, true) as SSLSocket
                socket.set(tls)
                tls.sslParameters = tls.sslParameters.apply { endpointIdentificationAlgorithm = "HTTPS" }
                tls.soTimeout = 5000
                val handshakeDeadline = timers.schedule(Runnable { abortTransport() }, 5, TimeUnit.SECONDS)
                try { tls.startHandshake() } finally { handshakeDeadline.cancel(false) }
                transport = tls
            }
            if (closed.get()) return
            socket.set(transport)
            lock.withLock { flushOutput() }
            val bytes = ByteBuffer.allocateDirect(64 * 1024)
            val input = ByteArray(64 * 1024)
            while (!closed.get()) {
                try {
                    // After publication the origin may be silent indefinitely.
                    // Block on readiness, not a periodic idle polling wake-up.
                    transport.soTimeout = if (ready.get()) 0 else maxOf(1, 15_000 - (SystemClock.elapsedRealtime() - startedMs)).toInt()
                    val count = transport.getInputStream().read(input)
                    if (count < 0) error("Origin closed the connection")
                    if (count == 0) continue
                    bytes.clear(); bytes.put(input, 0, count); bytes.flip()
                    lock.withLock {
                        if (closed.get()) return
                        publisher!!.feed(bytes, nowMs())
                        flushOutput()
                        updateState()
                        pollProgress()
                    }
                } catch (_: SocketTimeoutException) {
                    lock.withLock {
                        if (closed.get()) return
                        publisher!!.tick(nowMs())
                        flushOutput()
                        updateState()
                        pollProgress()
                    }
                }
            }
        } catch (error: Exception) {
            if (!closed.get()) fail("Attempt stopped: ${error.javaClass.simpleName}.")
        } finally {
            abortTransport()
            if (closed.get()) runCatching { releasePublisher() }
        }
    }

    private fun updateState() {
        when (publisher!!.state()) {
            NativeRtmpState.PUBLISHING -> {
                ready.set(true)
                if (accepted.compareAndSet(false, true)) events(PublishingAttemptEvent.Accepted)
                message.set("Origin accepted ${if (destination.tls) "RTMPS" else "development RTMP"} · media delivery unconfirmed")
            }
            NativeRtmpState.CLOSED, NativeRtmpState.FAILED -> error("Origin stopped publication")
            else -> message.set("Negotiating one ${if (destination.tls) "RTMPS" else "development RTMP"} attempt")
        }
    }

    /** Current publisher's native facts only; no socket/write/encode inference.
     * Events can race Accepted across reader/drainer threads; control latches
     * them under its exact attempt token before applying the ordered policy. */
    private fun reportProgress(startup: Boolean, peer: Boolean) {
        if (startup && startupReported.compareAndSet(false, true)) events(PublishingAttemptEvent.Sending)
        if (peer && peerReported.compareAndSet(false, true))
            events(PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.RTMP))
    }
    private fun pollProgress() {
        repeat(16) {
            when (publisher!!.pollEvent()) {
                NativeRtmpEvent.SENDING -> reportProgress(true, false)
                NativeRtmpEvent.PEER_MEDIA_PROGRESS -> reportProgress(true, true)
                null -> return
            }
        }
    }

    /** Called under lock by a control/network worker, never a codec callback. */
    private fun flushOutput() {
        val output = publisher!!.takeOutput()
        queuedWireBytes.set(publisher!!.queuedOutputBytes().toLong())
        // Cancel a stale queue timer before a possibly blocking socket write.
        // Taken bytes are now protected by the separate write watchdog.
        armNativeDeadline()
        writeOutput(output)
    }

    private fun writeOutput(output: ByteArray) {
        if (output.isEmpty()) return
        val transport = socket.get() ?: error("Transport unavailable")
        val deadline = timers.schedule(Runnable { abortTransport() }, 2, TimeUnit.SECONDS)
        val writeStartedNs = System.nanoTime()
        try { outputCounter.write(output.size) { transport.getOutputStream().write(output); output.size } }
        finally { writeDelay.record(System.nanoTime() - writeStartedNs); deadline.cancel(false) }
    }

    /** Exactly one native absolute-deadline timer, with no idle polling. Timer
     * callbacks never wait behind a socket writer; a due stale-media deadline
     * aborts that attempt and releases it after the writer unwinds. */
    private fun armNativeDeadline(deadline: ULong? = publisher?.nextDeadlineMs()) {
        val generation = deadlineGeneration.incrementAndGet()
        nativeDeadline?.cancel(false)
        nativeDeadline = null
        if (deadline == null) return
        if (closed.get()) return
        val delay = (deadline.toLong() - SystemClock.elapsedRealtime()).coerceAtLeast(0)
        nativeDeadline = timers.schedule(Runnable {
            if (closed.get() || deadlineGeneration.get() != generation) return@Runnable
            if (!lock.tryLock()) {
                if (deadlineGeneration.get() == generation) fail("Attempt stopped: native wire deadline expired. Capture can continue.")
                return@Runnable
            }
            try {
                if (closed.get() || deadlineGeneration.get() != generation) return@Runnable
                publisher!!.tick(nowMs())
                updateState()
                pollProgress()
                armNativeDeadline()
            } catch (_: Exception) {
                fail("Attempt stopped: native wire deadline expired. Capture can continue.")
            } finally { lock.unlock() }
        }, delay, TimeUnit.MILLISECONDS)
    }

    private fun resolve(host: String): List<InetAddress> {
        if (InetAddresses.isNumericAddress(host)) return listOf(InetAddresses.parseNumericAddress(host))
        val done = CountDownLatch(1)
        val answer = AtomicReference<List<InetAddress>>(emptyList())
        DnsResolver.getInstance().query(null, host, DnsResolver.FLAG_EMPTY, Executor { it.run() }, dnsCancellation,
            object : DnsResolver.Callback<List<InetAddress>> {
                override fun onAnswer(addresses: List<InetAddress>, rcode: Int) { if (rcode == 0) answer.set(addresses); done.countDown() }
                override fun onError(error: DnsResolver.DnsException) { done.countDown() }
            })
        check(done.await(3, TimeUnit.SECONDS) && answer.get().isNotEmpty()) { "Destination DNS failed or timed out" }
        return answer.get()
    }

    private fun fail(reason: String) = failAttemptOnce(closed, notify = {
        message.set(reason)
        events(PublishingAttemptEvent.Failed())
    }, cleanup = {
        deadlineGeneration.incrementAndGet()
        ready.set(false)
        dnsCancellation.cancel()
        abortTransport()
        timers.shutdownNow()
        // Called under the publishing worker's lock or by the reader after its
        // lock scope. A future consume sees Closed and releases its handle.
        releasePublisher()
    })

    override fun close() = lifecycle.close()

    private fun closeNetwork() {
        closed.set(true)
        deadlineGeneration.incrementAndGet()
        ready.set(false)
        dnsCancellation.cancel()
        abortTransport()
        timers.shutdownNow()
        reader?.interrupt()
        reader?.join(1000)
        val released = releasePublisher()
        preparedKey = ""
        check(reader?.isAlive != true && released) { "RTMP worker cleanup exceeded its deadline" }
        outputCounter.seal()
        message.set("RTMP attempt stopped")
    }

    /** Socket/DNS cancellation happens before this bounded lock attempt. If a
     * writer remains stalled, stop reports the deadline instead of waiting
     * indefinitely. The worker retries release after its IO unwinds. */
    private fun releasePublisher(): Boolean {
        return lock.cleanupWithin(100) {
            publisher?.let { runCatching { it.cancel() }; it.close() }
            publisher = null
        }
    }

    /** Close raw TCP first so TLS close-notify cannot wait behind a stalled
     * encrypted write. Both references belong to this one non-retrying attempt. */
    private fun abortTransport() {
        val tcp = tcpSocket.getAndSet(null)
        runCatching { tcp?.close() }
        val current = socket.getAndSet(null)
        if (current !== tcp) runCatching { current?.close() }
    }

    companion object {
        private fun nowMs() = SystemClock.elapsedRealtime().toULong()
    }
}
