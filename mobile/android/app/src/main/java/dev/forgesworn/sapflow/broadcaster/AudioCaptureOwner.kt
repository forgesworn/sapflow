package dev.forgesworn.sapflow.broadcaster

import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.MediaCodec as PlatformCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.MediaRecorder
import android.os.Handler
import dev.forgesworn.sapflow.MediaCodec as RustCodec
import dev.forgesworn.sapflow.MediaSample
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlin.math.sqrt

/** Real run-level audio owner. Platform generations are exclusive; the Rust
 * descriptor and show cursor survive a cold privacy replacement. Native stop
 * and release execute only after producer, callback and handoff receipts join.
 * The controller never infers actual closure from a request or timer. */
internal class AudioCaptureOwner(
    private val context: Context,
    private val preparation: CaptureStartAdmission.Preparation,
    private val epoch: Long,
    private val bitrate: Int,
    private val pin: AudioInputPin,
    private val owner: Handler,
    private val audioCallbacks: Handler,
    private val serials: AtomicLong,
    private val runAdmission: CameraRunAdmission,
    private val rawAllowed: () -> Boolean,
    private val replacementAdmission: (PrivacyStandbyPolicy.Command,Long) -> PrivacyStandbyPolicy.NativeAdmission,
    private val onFormat: (RustCodec) -> Unit,
    private val onSample: (MediaSample, Long, Long, Boolean) -> Unit,
    private val onValidated: () -> Unit,
    private val onPriming: () -> Unit,
    private val onFailure: (String) -> Unit,
    private val onRoute: (String) -> Unit,
) {
    private enum class Mode { RAW, ZERO, ALIGNED_RAW, RETIRING }
    private val lifecycle = AudioGenerationLifecycle(serials.get())
    private val timeline = AudioGenerationClock()
    val descriptor: RustCodec = RustCodec.aac(ByteBuffer.allocateDirect(2).apply { put(0x11); put(0x88.toByte()); flip() })
    private val native = ThreadPoolExecutor(1,1,0,TimeUnit.MILLISECONDS,ArrayBlockingQueue(3),
        { work -> Thread(work,"sapflow-audio-native-close") },ThreadPoolExecutor.AbortPolicy())
    @Volatile private var active: Generation? = null
    @Volatile private var stopped = false
    private val nativeFailure=AudioNativeFailure(runAdmission)
    private var started = false
    private var epochNs = 0L
    private var replacement: PrivacyStandbyPolicy.Command? = null
    private var replacementOldClosed: ((Long) -> Unit)? = null
    private var replacementReady: ((Long) -> Unit)? = null
    private var closeReply: ((Boolean) -> Unit)? = null
    private var zeroReadyDelivered = false
    @Volatile var level = 0f; private set
    @Volatile var routeName = "Not verified"; private set
    @Volatile var diagnostics = "Audio owner prepared"; private set
    @Volatile var encoderName = "Not selected"; private set
    val failed get() = nativeFailure.reason != null || lifecycle.isFailed()
    val failureReason get() = nativeFailure.reason
    val actualClosed get() = active?.let { lifecycle.actualClosed(it.ticket) } != false
    val currentGeneration get() = active?.ticket?.serial ?: 0L
    val outstanding get() = active?.encoded?.outstanding ?: 0

    private inner class Generation(val ticket: AudioGenerationLifecycle.Generation, val clock: AudioGenerationClock.Generation, zero: Boolean) {
        @Volatile var mode = if (zero) Mode.ZERO else Mode.RAW
        val beganZero = zero
        val everRaw = AtomicBoolean(false)
        val reading = AtomicBoolean(false)
        val pumping = AtomicBoolean(false)
        val inputs = ArrayBlockingQueue<Int>(32)
        val pool = PcmBlockPool()
        val zeroBytes = ByteBuffer.allocateDirect(2048).apply { repeat(256) { putLong(0) }; flip() }
        val acceptedFrames = AtomicLong()
        val lastInputUs = AtomicLong(clock.inputStartUs)
        val rawFrames = AtomicLong()
        val outputUnits = AtomicLong()
        val workers = AudioWorkerCompletion()
        var codec: PlatformCodec? = null
        var encoded: EncodedAudioEpoch? = null
        var microphone: AudioRecord? = null
        @Volatile var route: AudioInputRouting? = null
        var reader: Thread? = null
        var pump: Thread? = null
        var recordStopReturned = true
        var producerJoinStarted = false
        var formatReady = false
        @Volatile var wantsEos=false
        val eosWakePosted=AtomicBoolean(false)
        var eosSent = false
        var quiescing = false
        var handoffSealed = false
        var gracefulTail = false
        var resumeReady: (() -> Unit)? = null
        var firstHardwareFrame = false
        var inputTick = AudioGenerationClock.tick(clock.inputStartUs)
        val alignment = ResumePcmAlignment()
    }

    /** Canonical descriptor is allocated once; actual native CSD must still
     * validate before any sample can be constructed or local proof reported. */
    fun prepare(initialZero: Boolean) { check(active == null); create(initialZero,0) }

    private fun create(zero: Boolean, originUs: Long) {
        check(!stopped && !failed && (active == null || actualClosed)) { "Previous native AAC generation has not actually closed" }
        val ticket = checkNotNull(lifecycle.begin()) { "AAC generation capacity is unavailable" }
        check(serials.compareAndSet(ticket.serial-1,ticket.serial)) { "AAC generation serial changed" }
        val clock = timeline.begin(ticket.serial,originUs,zero)
        val g = Generation(ticket,clock,zero);check(nativeFailure.bind(g));active = g; zeroReadyDelivered = false
        try {
            val codec = PlatformCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC); g.codec = codec; encoderName = codec.name
            val encoded = EncodedAudioEpoch(audioCallbacks,owner,descriptor,timeline,clock,ticket.serial,
                onInput = { index ->
                    if (active === g && lifecycle.owns(ticket)) {
                        if (!g.inputs.offer(index)) report(g,"AAC input buffer bound exceeded")
                        else if(g.wantsEos && g.eosWakePosted.compareAndSet(false,true)) owner.post {
                            g.eosWakePosted.set(false);if(active===g)maybeSendEos(g)
                        }
                    }
                },
                onFormat = { config -> if (active === g && !lifecycle.actualClosed(ticket)) {
                    g.formatReady = true; onFormat(config); notifyZeroReady(g)
                } },
                onSample = { sample,tick,generation ->
                    if (active === g && !lifecycle.actualClosed(ticket)) onSample(sample,tick,generation,g.beganZero && !g.everRaw.get())
                },
                onValidated = onValidated,onPriming = onPriming,
                onRawOutput = { g.outputUnits.incrementAndGet() },
                onEos = { if (active === g) act(g,lifecycle.eos(ticket)) },
                onFailure = { reason -> report(g,reason) },
                onHandoffIdle = { if (active === g && g.handoffSealed && (g.encoded?.outstanding ?: 0) == 0) act(g,lifecycle.handoffIdle(ticket)) })
            g.encoded = encoded
            codec.setCallback(encoded,audioCallbacks)
            codec.configure(MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC,48_000,1).apply {
                setInteger(MediaFormat.KEY_AAC_PROFILE,MediaCodecInfo.CodecProfileLevel.AACObjectLC)
                setInteger(MediaFormat.KEY_BIT_RATE,bitrate);setInteger(MediaFormat.KEY_MAX_INPUT_SIZE,2048)
            },null,null,PlatformCodec.CONFIGURE_FLAG_ENCODE)
            codec.start()
            if (!zero) prepareMicrophone(g)
            if (started) beginGeneration(g)
        } catch (error: Exception) { report(g,"Audio native preparation failed: ${error.javaClass.simpleName}") }
    }

    private fun prepareMicrophone(g: Generation) {
        check(context.checkSelfPermission(android.Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) { "Microphone permission is unavailable" }
        val minimum = AudioRecord.getMinBufferSize(48_000,AudioFormat.CHANNEL_IN_MONO,AudioFormat.ENCODING_PCM_16BIT)
        require(minimum > 0) { "48 kHz mono microphone capture is unavailable" }
        val record = AudioRecord.Builder().setAudioSource(MediaRecorder.AudioSource.MIC)
            .setAudioFormat(AudioFormat.Builder().setSampleRate(48_000).setChannelMask(AudioFormat.CHANNEL_IN_MONO).setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
            .setBufferSizeInBytes(maxOf(minimum*2,8192)).build()
        g.microphone = record
        require(record.state == AudioRecord.STATE_INITIALIZED && record.sampleRate == 48_000 && record.channelCount == 1 && record.audioFormat == AudioFormat.ENCODING_PCM_16BIT) { "Microphone changed the native PCM format" }
        val identity = AudioInputOwner(preparation,epoch,g.ticket.serial)
        g.route = AudioInputRouting.attach(context,record,audioCallbacks,identity,pin,failed = {
            // Exact immutable generation, never a mutable controller.current.
            report(g,g.route?.failureDescription ?: "Microphone route changed")
        },changed = { id -> owner.post {
            if (active === g && g.route?.permitsPcm() == true) {
                routeName = AudioInputRouting.inventory(context).firstOrNull { it.id == id }?.name ?: "Verified microphone"
                onRoute(routeName)
            }
        } })
    }

    fun start(mediaEpochNs: Long, standby: PrivacyStandbyPolicy.Command?) {
        check(!started && !stopped); require(mediaEpochNs >= 0)
        epochNs = mediaEpochNs; replacement = standby; started = true
        beginGeneration(checkNotNull(active)); notifyZeroReady(checkNotNull(active))
    }
    private fun beginGeneration(g: Generation) {
        g.encoded!!.begin(); g.workers.begin(); g.pumping.set(true)
        if (g.mode == Mode.RAW) startReader(g) else { g.workers.readerCompleted(); g.pool.finishReading() }
        g.pump = Thread({ pump(g) },"sapflow-aac-input-${g.ticket.serial}").apply { start() }
    }

    /** Called after policy's synchronous raw fence. Unknown old closure blocks
     * create(), never merely a callback returning 'closed=true'. */
    fun standby(command: PrivacyStandbyPolicy.Command, oldClosed: (Long) -> Unit, zeroReady: (Long) -> Unit) {
        if (stopped || failed) return
        replacement = command; replacementOldClosed = oldClosed; replacementReady = zeroReady
        val g = active ?: return
        retire(g,false,command.deadlineNs)
    }
    fun bindInitialStandby(command: PrivacyStandbyPolicy.Command, zeroReady: (Long) -> Unit) {
        replacement = command; replacementReady = zeroReady; notifyZeroReady(checkNotNull(active))
    }
    private fun notifyZeroReady(g: Generation) {
        if (!zeroReadyDelivered && started && g.formatReady && g.beganZero && !g.everRaw.get() && replacement != null && active === g) {
            zeroReadyDelivered = true; replacementReady?.invoke(g.ticket.serial)
        }
    }
    fun prepareResume(command: PrivacyStandbyPolicy.Command, ready: () -> Unit): Boolean {
        val g = active ?: return false
        if (stopped || failed || g.mode != Mode.ZERO || g.microphone != null || lifecycle.currentStage() != AudioGenerationLifecycle.Stage.ACTIVE) return false
        // Silence keeps its original safe token until policy commits Resume.
        g.resumeReady = ready
        try { prepareMicrophone(g); g.firstHardwareFrame = false; startReader(g) }
        catch (error: Exception) { report(g,"Microphone Resume failed: ${error.javaClass.simpleName}"); return false }
        diagnostics = "Preparing verified microphone for privacy revision ${command.revision}"
        return true
    }
    fun commitResume(generation: Long): Boolean {
        val g = active ?: return false
        if (g.ticket.serial != generation || !g.firstHardwareFrame || stopped || failed || !rawAllowed()) return false
        g.mode = Mode.ALIGNED_RAW; return true
    }

    private fun startReader(g: Generation) {
        val record = checkNotNull(g.microphone); val route = checkNotNull(g.route)
        g.workers.readerBegan()
        g.reading.set(true)
        g.reader = Thread({
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_AUDIO)
            var block: PcmBlockPool.Block? = null
            try {
                synchronized(record) { if (g.reading.get()) record.startRecording() }
                check(record.recordingState == AudioRecord.RECORDSTATE_RECORDING && route.verify()) { "Actual microphone route is unavailable" }
                val timing = AudioCaptureTiming(epochNs,record.bufferSizeInFrames.toLong());val stamp=AudioTimestamp();val began=System.nanoTime()
                var nextDiagnostic=0L
                while (g.reading.get()) {
                    if (!route.admitReader(g.reading)) break
                    check(!failed && runAdmission.admit(System.nanoTime())) { "Capture run denied raw input" }
                    val next=g.pool.acquireReader() ?: break;block=next
                    val before=record.getTimestamp(stamp,AudioTimestamp.TIMEBASE_MONOTONIC)
                    if (before==AudioRecord.SUCCESS) timing.beforeRead(stamp.framePosition,stamp.nanoTime,System.nanoTime())
                    else check(timing.framesRead==0L && System.nanoTime()-began<=record.bufferSizeInFrames*1_000_000_000L/48_000) { "Microphone timestamp unavailable within capture capacity" }
                    val count=record.read(next.buffer,next.buffer.capacity(),AudioRecord.READ_BLOCKING);val now=System.nanoTime()
                    if (!g.reading.get()) break
                    check(!failed && runAdmission.admit(now));require(count>0 && count%2==0) { "Microphone read failed" }
                    check(record.getTimestamp(stamp,AudioTimestamp.TIMEBASE_MONOTONIC)==AudioRecord.SUCCESS) { "Microphone timestamp unavailable after read" }
                    val pts=timing.read(count/2,stamp.framePosition,stamp.nanoTime,System.nanoTime())
                    if (!route.admitReader(g.reading)) break
                    check(!failed && runAdmission.admit(System.nanoTime()))
                    // Resume may prepare bounded PCM while media remains zero;
                    // it is never queued to AAC until the exact policy commits.
                    if(g.mode==Mode.ZERO || !rawAllowed()){g.pool.release(next);block=null}
                    else { g.pool.publish(next,count,pts,epochNs+pts*1000);block=null }
                    g.rawFrames.addAndGet(count/2L)
                    if (!g.firstHardwareFrame) { g.firstHardwareFrame=true;owner.post { if(active===g && g.reading.get()) g.resumeReady?.invoke() } }
                    if(now>=nextDiagnostic){nextDiagnostic=now+200_000_000;diagnostics="PCM ${g.rawFrames.get()}f · input ${g.acceptedFrames.get()}f · output ${g.outputUnits.get()}AU · native stamp age ${timing.timestampAgeUs}us/backlog ${timing.backlogFrames}f · ${g.encoded?.diagnostics}"}
                }
            } catch(error:Exception){if(g.reading.get())report(g,"Microphone reader failed: ${error.message ?: error.javaClass.simpleName}")}
            finally {
                block?.let { runCatching { g.pool.release(it) }.onFailure { report(g,"PCM release failed") } }
                g.pool.finishReading();owner.post { if(active===g){g.workers.readerCompleted();joinProducers(g)} }
            }
        },"sapflow-microphone-${g.ticket.serial}").apply{start()}
    }

    private fun pump(g: Generation) {
        android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_AUDIO)
        var held: PcmBlockPool.Block?=null;var heldOffset=0;var borrowedInput=-1
        val cadence = ReplacementCadence(Math.addExact(epochNs,Math.multiplyExact(g.clock.inputStartUs,1000)),1024,48_000)
        try {
            while(g.pumping.get()) {
                if(active!==g)break
                val mode=g.mode
                if(mode==Mode.RETIRING && !g.gracefulTail)break
                val index=g.inputs.poll(20,TimeUnit.MILLISECONDS) ?: continue
                borrowedInput=index
                val codec=checkNotNull(g.codec);val input=codec.getInputBuffer(index) ?: error("AAC input buffer unavailable")
                require(input.capacity()>=2048);input.clear()
                var bytes=0;var ptsUs=g.lastInputUs.get()
                if(mode==Mode.ZERO) {
                    val safe=checkNotNull(replacement) { "Silence has no exact privacy owner" }
                    when(zeroAction(g,safe)) {
                        PrivacyNativeWork.Action.RETRY -> { check(g.inputs.offer(index));borrowedInput=-1;TimeUnit.MILLISECONDS.sleep(5);continue }
                        PrivacyNativeWork.Action.CANCEL -> { check(g.inputs.offer(index));borrowedInput=-1;break }
                        PrivacyNativeWork.Action.FAULT -> { report(g,"Silence authority expired or faulted");break }
                        PrivacyNativeWork.Action.SUBMIT -> Unit
                    }
                    var due=cadence.claim(System.nanoTime())
                    while(due<0 && g.pumping.get() && g.mode==Mode.ZERO){TimeUnit.NANOSECONDS.sleep(minOf(2_000_000L,(cadence.nextNs-System.nanoTime()).coerceAtLeast(1)));due=cadence.claim(System.nanoTime())}
                    if(!g.pumping.get() || g.mode!=Mode.ZERO) { check(g.inputs.offer(index));borrowedInput=-1;continue }
                    g.zeroBytes.position(0);g.zeroBytes.limit(2048);input.put(g.zeroBytes);level=0f;bytes=2048;ptsUs=AudioGenerationClock.microseconds(g.inputTick)
                    when(zeroAction(g,safe)) {
                        PrivacyNativeWork.Action.RETRY -> { check(g.inputs.offer(index));borrowedInput=-1;continue }
                        PrivacyNativeWork.Action.CANCEL -> { check(g.inputs.offer(index));borrowedInput=-1;break }
                        PrivacyNativeWork.Action.FAULT -> { report(g,"Silence deadline expired before native input");break }
                        PrivacyNativeWork.Action.SUBMIT -> Unit
                    }
                } else {
                    if(held==null){held=g.pool.acquireEncoder(100,System::nanoTime);heldOffset=0}
                    val block=held
                    if(block==null){check(g.inputs.offer(index));borrowedInput=-1;if(mode==Mode.RETIRING && g.pool.finished)break;continue}
                    g.pool.checkAge(block,System.nanoTime());check(g.route?.awaitQueuedPcm(g.pool,block)==true) { "Microphone route unverified before AAC" }
                    check(runAdmission.admit(System.nanoTime()))
                    when(rawAction(g)) {
                        AudioRawAdmission.Action.WAIT -> { check(g.inputs.offer(index));borrowedInput=-1;TimeUnit.MILLISECONDS.sleep(5);continue }
                        AudioRawAdmission.Action.CANCEL -> { check(g.inputs.offer(index));borrowedInput=-1;break }
                        AudioRawAdmission.Action.FAULT -> error("Capture run failed before raw AAC input")
                        else -> Unit
                    }
                    val oldOffset=heldOffset;val oldTick=g.inputTick
                    if(mode==Mode.ALIGNED_RAW) {
                        val alignment=g.alignment;alignment.plan(g.inputTick,block.ptsUs,heldOffset,block.bytes)
                        heldOffset=alignment.offsetBytes
                        if(alignment.outputBytes==0){g.pool.release(block);held=null;check(g.inputs.offer(index));borrowedInput=-1;continue}
                        repeat(alignment.zeroFrames){input.putShort(0)}
                        val take=alignment.copyBytes
                        block.buffer.position(heldOffset);block.buffer.limit(heldOffset+take);input.put(block.buffer);heldOffset+=take
                        bytes=alignment.outputBytes;ptsUs=AudioGenerationClock.microseconds(g.inputTick)
                    } else {
                        block.buffer.position(0);block.buffer.limit(block.bytes);input.put(block.buffer);bytes=block.bytes;ptsUs=block.ptsUs;heldOffset=block.bytes
                        g.inputTick=AudioGenerationClock.tick(ptsUs)
                    }
                    var square=0.0;input.order(ByteOrder.nativeOrder());for(at in 0 until bytes step 2){val x=input.getShort(at)/32768.0;square+=x*x};level=sqrt(square/(bytes/2)).toFloat().coerceIn(0f,1f)
                    g.pool.checkAge(block,System.nanoTime())
                    when(rawAction(g)) {
                        AudioRawAdmission.Action.WAIT -> {
                            heldOffset=oldOffset;g.inputTick=oldTick
                            check(g.inputs.offer(index));borrowedInput=-1;TimeUnit.MILLISECONDS.sleep(5);continue
                        }
                        AudioRawAdmission.Action.CANCEL -> { check(g.inputs.offer(index));borrowedInput=-1;break }
                        AudioRawAdmission.Action.FAULT -> error("Capture run failed before raw AAC input")
                        else -> Unit
                    }
                    g.everRaw.set(true)
                }
                if(mode==Mode.ZERO) {
                    val safe=checkNotNull(replacement)
                    when(zeroAction(g,safe)) {
                        PrivacyNativeWork.Action.RETRY -> { check(g.inputs.offer(index));borrowedInput=-1;continue }
                        PrivacyNativeWork.Action.CANCEL -> { check(g.inputs.offer(index));borrowedInput=-1;break }
                        PrivacyNativeWork.Action.FAULT -> { report(g,"Silence deadline expired at native input admission");break }
                        PrivacyNativeWork.Action.SUBMIT -> Unit
                    }
                }
                check(!failed && runAdmission.admit(System.nanoTime())) { "Capture run failed before native AAC input" }
                if(g.mode==Mode.RETIRING && !g.gracefulTail)break
                codec.queueInputBuffer(index,0,bytes,ptsUs,0);borrowedInput=-1
                held?.let { if(heldOffset==it.bytes){g.pool.release(it);held=null} }
                g.acceptedFrames.addAndGet(bytes/2L);g.inputTick=Math.addExact(g.inputTick,bytes/2L)
                g.lastInputUs.set(ptsUs+bytes/2L*1_000_000/48_000)
            }
        }catch(error:Exception){if(g.pumping.get() && g.mode!=Mode.RETIRING)report(g,"AAC input failed: ${error.message ?: error.javaClass.simpleName}")}
        finally{
            // Even an expected privacy/Stop fence must return a dequeued index
            // so EOS can use a single-buffer native encoder without stalling.
            if(borrowedInput>=0 && !g.inputs.offer(borrowedInput))report(g,"AAC input index return exceeded its bound")
            held?.let{runCatching{g.pool.release(it)}.onFailure{report(g,"PCM pump release failed")}}
            owner.post{if(active===g){g.workers.pumpCompleted();joinProducers(g)}}
        }
    }

    private fun zeroAction(g:Generation,command:PrivacyStandbyPolicy.Command)=PrivacyNativeWork.action(
        replacementAdmission(command,System.nanoTime()),active===g,g.mode==Mode.ZERO && g.pumping.get(),failed || !runAdmission.admit(System.nanoTime()))
    private fun rawAction(g:Generation)=AudioRawAdmission.action(rawAllowed(),g.mode==Mode.RETIRING,g.gracefulTail,failed || !runAdmission.admit(System.nanoTime()))

    fun close(graceful: Boolean, deadlineNs: Long, complete: (Boolean) -> Unit) {
        if(closeReply!=null)return
        stopped=true;closeReply=complete
        val g=active
        if(g==null){complete(true);native.shutdown();return}
        if(lifecycle.actualClosed(g.ticket)){complete(true);native.shutdown();return}
        retire(g,graceful,deadlineNs)
    }
    private fun retire(g:Generation,graceful:Boolean,deadlineNs:Long){
        if(active!==g || lifecycle.currentStage()!=AudioGenerationLifecycle.Stage.ACTIVE)return
        g.gracefulTail=graceful && !failed
        act(g,lifecycle.retire(g.ticket,g.acceptedFrames.get()>0,System.nanoTime(),deadlineNs))
    }
    private fun act(g:Generation,action:AudioGenerationLifecycle.Action){
        if(active!==g)return
        when(action){
            AudioGenerationLifecycle.Action.NONE->Unit
            AudioGenerationLifecycle.Action.STOP_PRODUCERS->{
                g.mode=Mode.RETIRING
                val route=g.route
                if(route?.stopProducer(g.reading)==true)report(g,route.failureDescription) else g.reading.set(false)
                if(!g.gracefulTail){g.pool.cancel();g.pumping.set(false);g.pump?.interrupt()}
                g.reader?.interrupt();g.recordStopReturned=g.microphone==null
                val record=g.microphone
                if(record!=null)native.execute{
                    val ok=runCatching{synchronized(record){if(record.recordingState==AudioRecord.RECORDSTATE_RECORDING)record.stop()}}.isSuccess
                    owner.post{if(active===g){g.recordStopReturned=ok;if(!ok)report(g,"AudioRecord stop is unconfirmed");joinProducers(g)}}
                }
                if(g.reader==null)g.workers.readerCompleted();if(g.pump==null)g.workers.pumpCompleted()
                joinProducers(g)
            }
            AudioGenerationLifecycle.Action.SEND_EOS->{g.wantsEos=true;maybeSendEos(g)}
            AudioGenerationLifecycle.Action.QUIESCE->{
                if(g.quiescing)return;g.quiescing=true
                val encoded=g.encoded
                if(encoded==null){act(g,lifecycle.callbacksClosed(g.ticket));act(g,lifecycle.handoffIdle(g.ticket))}
                else encoded.quiesce({g.codec?.stop()}){ stopped ->
                    if(active===g){if(!stopped && g.acceptedFrames.get()>0)report(g,"AAC native stop was not confirmed")
                        act(g,lifecycle.callbacksClosed(g.ticket));g.handoffSealed=true;encoded.finishHandoff(true);if(encoded.outstanding==0)act(g,lifecycle.handoffIdle(g.ticket))}
                }
            }
            AudioGenerationLifecycle.Action.RELEASE->native.execute{
                var ok=true
                runCatching{g.codec?.release()}.onFailure{ok=false}
                runCatching{g.microphone?.release()}.onFailure{ok=false}
                owner.post{if(active===g)act(g,lifecycle.released(g.ticket,ok,System.nanoTime()))}
            }
            AudioGenerationLifecycle.Action.CLOSED->{
                timeline.closed(g.clock)
                if(stopped){closeReply?.invoke(true);native.shutdown()}
                else{
                    replacementOldClosed?.invoke(g.ticket.serial)
                    if(failed)return
                    val cmd=replacement ?:return
                    when(replacementAdmission(cmd,System.nanoTime())) {
                        PrivacyStandbyPolicy.NativeAdmission.SUBMIT -> create(true,timeline.replacementStartUs((System.nanoTime()-epochNs)/1000))
                        PrivacyStandbyPolicy.NativeAdmission.FAULT -> report(g,"Privacy deadline expired before fresh AAC creation")
                        else -> Unit // Stop/Auto/stale command owns actual clean closure; no successor codec.
                    }
                }
            }
            AudioGenerationLifecycle.Action.FAIL->{report(g,"Native AAC cleanup is unconfirmed; owner retained");closeReply?.invoke(false)}
        }
    }
    private fun joinProducers(g:Generation){
        if(active!==g || lifecycle.currentStage()!=AudioGenerationLifecycle.Stage.PRODUCERS || !g.recordStopReturned || !g.workers.stopped || g.producerJoinStarted)return
        g.producerJoinStarted=true
        native.execute{
            runCatching{g.reader?.join(1000);g.pump?.join(1000)}
            val done=g.reader?.isAlive!=true && g.pump?.isAlive!=true
            owner.post{if(active===g){if(!done){report(g,"Audio producer threads remain retained");closeReply?.invoke(false)}else act(g,lifecycle.producersClosed(g.ticket))}}
        }
    }
    private fun maybeSendEos(g:Generation){
        if(active!==g || lifecycle.currentStage()!=AudioGenerationLifecycle.Stage.EOS || g.eosSent)return
        val index=g.inputs.poll() ?:return
        try{g.codec!!.queueInputBuffer(index,0,0,g.lastInputUs.get(),PlatformCodec.BUFFER_FLAG_END_OF_STREAM);g.eosSent=true
            owner.postDelayed({if(active===g)act(g,lifecycle.abortEos(g.ticket))},500)
        }catch(error:Exception){report(g,"AAC EOS failed");act(g,lifecycle.abortEos(g.ticket))}
    }
    private fun report(g:Generation,reason:String){
        if(active!==g || !nativeFailure.fail(g,reason))return
        lifecycle.fail(g.ticket);g.reading.set(false)
        owner.post{if(active===g)onFailure(reason)}
    }
}
