package dev.forgesworn.sapflow.broadcaster

import dev.forgesworn.sapflow.MediaCodec as RustCodec
import dev.forgesworn.sapflow.MediaSample
import dev.forgesworn.sapflow.NativeSrtCipher
import dev.forgesworn.sapflow.NativeSrtConfig
import dev.forgesworn.sapflow.NativeSrtEvent
import dev.forgesworn.sapflow.NativeSrtPublisher
import dev.forgesworn.sapflow.NativeSrtState
import java.net.Inet4Address
import java.net.StandardProtocolFamily
import java.nio.ByteBuffer
import java.nio.channels.DatagramChannel
import java.nio.channels.SelectionKey
import java.nio.channels.Selector
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** One own-SRT attempt. Every native protocol call runs on the UDP worker.
 * AES-CTR provides encryption, not peer authentication. No DNS or fallback.
 * Constructor/Prepare is dormant; explicit Start creates UDP and clock state. */
internal class SrtPublishingSink(
    private val destination: SrtDestination,
    private val streamId: String,
    passphrase: String,
    private val plaintext: Boolean,
    private val events: (PublishingAttemptEvent) -> Unit = {},
) : EncodedSink {
    private data class Codecs(val video: RustCodec?, val audio: RustCodec?)
    private val codecs = AtomicReference(Codecs(null, null))
    private val secret = AtomicReference(if (plaintext) null else passphrase.toByteArray(Charsets.UTF_8))
    private val message = AtomicReference("SRT destination prepared · press Start")
    override val status: String get() = message.get()
    private val closed = AtomicBoolean(false)
    private val selector = AtomicReference<Selector?>()
    private val channel = AtomicReference<DatagramChannel?>()
    private val offered = SingleSampleOffer<MediaSample>()
    @Volatile private var worker: Thread? = null
    private val shutdown = SrtShutdown()
    private val shutdownResult = AtomicReference<SrtShutdown.Result?>()
    override val diagnostics: String get() = "SRT shutdown ${shutdownResult.get()?.name ?: "not requested"}"
    private val timers = ScheduledThreadPoolExecutor(1) { task -> Thread(task, "sapflow-srt-deadline") }.apply { removeOnCancelPolicy = true }
    private val lifecycle = ExplicitAttempt(::startNetwork, ::closeNetwork)

    init { SrtDestination.validateCredentials(streamId, passphrase, plaintext) }

    override fun configure(video: RustCodec?, audio: RustCodec?) {
        if (!closed.get()) { codecs.set(Codecs(video, audio)); selector.get()?.wakeup() }
    }

    override fun start() = lifecycle.start()

    private fun startNetwork() {
        check(!closed.get()) { "SRT attempt closed" }
        val configureDeadlineUs = nowUs() + 15_000_000
        message.set("Starting one ${if (plaintext) "plaintext lab" else "AES-256"} SRT attempt")
        worker = Thread({ runNetwork(configureDeadlineUs) }, "sapflow-srt-udp").apply { start() }
    }

    override fun consume(sample: MediaSample) {
        if (closed.get()) return
        try {
            val ticket = offered.offer(sample, nowUs() + 1_000_000)
            selector.get()?.wakeup()
            if (!offered.await(ticket, 1000) && !closed.get()) fail("SRT sample admission exceeded one second · capture can continue")
        } catch (error: Exception) { fail("SRT attempt stopped: ${error.javaClass.simpleName} · capture can continue") }
    }

    private fun runNetwork(configureDeadlineUs: Long) {
        var native: NativeSrtPublisher? = null
        var alarm: ScheduledFuture<*>? = null
        var pendingWrite: ByteBuffer? = null
        var writeDeadlineUs: Long? = null
        var waitingIdr = true
        try {
            val readiness = Selector.open()
            selector.set(readiness)
            val socket = DatagramChannel.open(if (destination.address is Inet4Address) StandardProtocolFamily.INET else StandardProtocolFamily.INET6)
            channel.set(socket)
            socket.configureBlocking(false)
            socket.connect(destination.socketAddress)
            val key = socket.register(readiness, SelectionKey.OP_READ)
            val input = ByteBuffer.allocateDirect(1501)
            while (!closed.get()) {
                alarm?.cancel(false); alarm = null
                val now = nowUs()
                if (native == null) {
                    check(now < configureDeadlineUs) { "SRT codec configuration deadline exceeded" }
                    val descriptors = codecs.get()
                    if (descriptors.video != null && descriptors.audio != null) {
                        val config = NativeSrtConfig(
                            destination = destination.nativeAddress, streamId = streamId,
                            cipher = if (plaintext) null else NativeSrtCipher.AES256,
                            latencyMs = 120u, maxBufferedPackets = 128u,
                            maxBufferedBytes = 4u * 1024u * 1024u, maxPacketAgeMs = 1000u,
                            maxSampleResidenceMs = 1000u, bandwidthBytesPerSecond = 1_250_000uL,
                        )
                        val pass = secret.getAndSet(null)
                        try { native = NativeSrtPublisher(config, descriptors.video, descriptors.audio, pass, now.toULong()) }
                        finally { pass?.fill(0) }
                        message.set("Negotiating ${if (plaintext) "plaintext lab" else "AES-256"} SRT")
                    }
                }
                val publisher = native
                if (publisher != null) {
                    publisher.tick(nowUs().toULong())
                    for (index in 0 until 16) {
                        input.clear()
                        val source = socket.receive(input) ?: break
                        if (source == destination.socketAddress && input.position() in 1..1500) {
                            input.flip()
                            publisher.feed(destination.nativeAddress, input, nowUs().toULong())
                        }
                    }
                    check(publisher.state() != NativeSrtState.CLOSED) { "SRT origin closed the attempt" }
                }
                if (closed.get()) break
                offered.process(nowUs()) { sample ->
                    // Both tracks must be announced at construction, but no
                    // pre-handshake media or non-IDR video is retained here.
                    if (closed.get() || publisher == null || publisher.state() != NativeSrtState.CONNECTED) true
                    else if (waitingIdr && (!sample.isVideo() || !sample.isKeyframe())) true
                    else if (!publisher.isReady()) false
                    else {
                        publisher.push(sample, nowUs().toULong())
                        if (sample.isVideo() && sample.isKeyframe()) waitingIdr = false
                        true
                    }
                }
                if (publisher != null) {
                    var steps = 0
                    while (!closed.get() && steps++ < 16) {
                        if (pendingWrite == null) {
                            val packet = publisher.pollDatagram(nowUs().toULong())
                            if (packet != null) {
                                check(packet.bytes.size in 1..1500) { "SRT datagram output bound exceeded" }
                                pendingWrite = ByteBuffer.wrap(packet.bytes)
                                writeDeadlineUs = nowUs() + 100_000
                            } else if (!publisher.hasImmediateWork()) break
                        }
                        if (closed.get()) break
                        val datagram = pendingWrite ?: continue
                        if (nowUs() >= checkNotNull(writeDeadlineUs)) {
                            publisher.socketSendFailed(nowUs().toULong())
                            error("SRT UDP write deadline exceeded")
                        }
                        val size = datagram.remaining()
                        val sent = try { socket.write(datagram) } catch (error: Exception) {
                            publisher.socketSendFailed(nowUs().toULong()); throw error
                        }
                        if (sent == 0) break
                        if (sent != size) {
                            publisher.socketSendFailed(nowUs().toULong())
                            error("Partial SRT datagram write refused")
                        }
                        pendingWrite = null; writeDeadlineUs = null
                    }
                }
                if (publisher != null) {
                    // push/pollDatagram may generate one-shot startup facts.
                    // Drain after bounded work, before sleeping on readiness.
                    for (index in 0 until 16) {
                        when (val event = publisher.pollEvent()) {
                            is NativeSrtEvent.Connected -> {
                                message.set("SRT ${if (plaintext) "plaintext lab" else "AES-256"} handshake complete · media delivery unconfirmed")
                                events(PublishingAttemptEvent.Accepted)
                            }
                            NativeSrtEvent.Sending -> events(PublishingAttemptEvent.Sending)
                            NativeSrtEvent.PeerMediaProgress -> events(PublishingAttemptEvent.PeerMediaProgress(PeerProgressSource.SRT))
                            is NativeSrtEvent.Closed -> error("SRT closed: ${event.reason}")
                            is NativeSrtEvent.Failed -> error("SRT failed: ${event.reason}")
                            null -> break
                        }
                    }
                }
                key.interestOps(SelectionKey.OP_READ or if (pendingWrite != null) SelectionKey.OP_WRITE else 0)
                readiness.selectedKeys().clear()
                if (publisher != null && pendingWrite == null && publisher.hasImmediateWork()) {
                    // Real bounded work remains. Yield after 16 mux steps and
                    // service socket readiness before the next bounded batch.
                    readiness.selectNow(); Thread.yield(); continue
                }
                val deadline = listOfNotNull(
                    if (publisher == null) configureDeadlineUs else null,
                    // Immediate native mux deadlines must not spin while one
                    // UDP write waits. Its separate 100ms timer remains active.
                    if (pendingWrite == null) publisher?.nextDeadlineUs()?.toLong() else null,
                    writeDeadlineUs, offered.deadlineUs(),
                ).minOrNull()
                if (deadline != null) {
                    alarm = timers.schedule(Runnable { readiness.wakeup() }, (deadline - nowUs()).coerceAtLeast(0), TimeUnit.MICROSECONDS)
                }
                readiness.select()
            }
        } catch (error: Exception) {
            if (!closed.get()) fail("SRT stopped: ${error.message?.take(100) ?: error.javaClass.simpleName} · capture can continue")
        } finally {
            alarm?.cancel(false)
            offered.cancel()
            // Discard pending media. Only a freshly polled SHUTDOWN after
            // native cancellation can be submitted by this sole UDP owner.
            pendingWrite = null
            shutdownResult.set(shutdown.finish(::nowUs,
                stopNative = { native?.stop(nowUs().toULong()) },
                takeControl = { native?.pollDatagram(nowUs().toULong())?.bytes },
                write = { bytes -> channel.get()?.write(bytes) ?: 0 }))
            runCatching { native?.close() }
            secret.getAndSet(null)?.fill(0)
            runCatching { channel.getAndSet(null)?.close() }
            runCatching { selector.getAndSet(null)?.close() }
            timers.shutdownNow()
        }
    }

    private fun abortTransport() {
        shutdown.abort()
        closed.set(true)
        selector.get()?.wakeup()
        runCatching { channel.get()?.close() }
        worker?.takeIf { it !== Thread.currentThread() }?.interrupt()
    }

    private fun fail(reason: String) = failAttemptOnce(closed, notify = {
        message.set(reason)
        events(PublishingAttemptEvent.Failed())
    }, cleanup = {
        abortTransport()
        offered.cancel()
    })

    override fun close() = lifecycle.close()

    private fun closeNetwork() {
        val running = worker
        // Publish the original grace deadline before closed: the worker may
        // observe Stop and enter finally immediately after that atomic write.
        val deadline = if (running != null) shutdown.request(nowUs()) else null
        val normal = !closed.getAndSet(true)
        val abortAlarm = deadline?.let {
            runCatching {
                timers.schedule(Runnable { abortTransport() }, (it - nowUs()).coerceAtLeast(0), TimeUnit.MICROSECONDS)
            }.getOrElse { abortTransport(); null }
        }
        selector.get()?.wakeup()
        try {
            if (running == null) {
                shutdown.abort()
                offered.cancel()
            } else {
                // No early interrupt: interrupting an interruptible UDP channel
                // can close it before its owner submits the shutdown control.
                // The 100ms watchdog hard-aborts; this join retains its 1s bound.
                running.join(1000)
                check(!running.isAlive) { "SRT UDP worker exceeded its stop deadline" }
            }
            if (normal) message.set("SRT attempt stopped")
        } finally {
            abortAlarm?.cancel(false)
            if (running?.isAlive == true) abortTransport()
            secret.getAndSet(null)?.fill(0)
            timers.shutdownNow()
        }
    }

    companion object {
        private fun nowUs() = System.nanoTime() / 1000
    }
}
