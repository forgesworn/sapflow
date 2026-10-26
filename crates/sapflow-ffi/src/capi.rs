#![allow(unsafe_code)]

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;
use std::sync::Arc;

use crate::*;

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SapflowBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SapflowFanoutOutcome {
    pub publishing_queued: bool,
    pub recording_queued: bool,
    pub publishing_waiting_keyframe: bool,
    pub publishing_overflows: u64,
    pub recording_failed: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SapflowMediaSampleResult {
    pub handle: *mut c_void,
    pub len: u64,
    pub pts: i64,
    pub dts: i64,
    pub duration: u32,
    pub timescale: u32,
    pub is_video: bool,
    pub is_keyframe: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SapflowOptionalU64 {
    pub value: u64,
    pub present: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SapflowRecordingOutput {
    pub bytes: SapflowBuffer,
    pub checkpoint: *mut c_void,
    pub kind: u8,
    pub rewrite_header: bool,
    pub present: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SapflowRtmpSendOutput {
    pub output: SapflowBuffer,
    pub queued_output_bytes: u64,
    pub next_deadline_ms: u64,
    pub state: u8,
    pub has_next_deadline: bool,
    pub startup_submitted: bool,
    pub peer_media_progress: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SapflowSrtDatagram {
    pub destination: SapflowBuffer,
    pub bytes: SapflowBuffer,
    pub present: bool,
}

fn media_sample_metadata(sample: &MediaSample) -> SapflowMediaSampleResult {
    SapflowMediaSampleResult {
        handle: std::ptr::null_mut(),
        len: sample.len(),
        pts: sample.pts(),
        dts: sample.dts(),
        duration: sample.duration(),
        timescale: sample.timescale(),
        is_video: sample.is_video(),
        is_keyframe: sample.is_keyframe(),
    }
}

fn media_sample_result(sample: Arc<MediaSample>) -> SapflowMediaSampleResult {
    let metadata = media_sample_metadata(&sample);
    SapflowMediaSampleResult {
        handle: Arc::into_raw(sample) as *mut c_void,
        ..metadata
    }
}

fn optional_sample_result(sample: Option<Arc<MediaSample>>) -> SapflowMediaSampleResult {
    sample.map_or_else(Default::default, media_sample_result)
}

fn recording_output<T>(
    bytes: Vec<u8>,
    kind: NativeRecordingChunkKind,
    rewrite_header: bool,
    checkpoint: Option<Arc<T>>,
) -> Result<SapflowRecordingOutput, String> {
    let bytes = owned(bytes)?;
    Ok(SapflowRecordingOutput {
        bytes,
        checkpoint: checkpoint.map_or(std::ptr::null_mut(), |value| {
            Arc::into_raw(value) as *mut c_void
        }),
        kind: kind as u8,
        rewrite_header,
        present: true,
    })
}

fn srt_datagram_result(value: Option<NativeSrtDatagram>) -> Result<SapflowSrtDatagram, String> {
    let Some(value) = value else {
        return Ok(SapflowSrtDatagram::default());
    };
    let destination = owned(value.destination.into_bytes())?;
    match owned(value.bytes) {
        Ok(bytes) => Ok(SapflowSrtDatagram {
            destination,
            bytes,
            present: true,
        }),
        Err(error) => {
            sapflow_buffer_free(destination);
            Err(error)
        }
    }
}

fn owned(mut bytes: Vec<u8>) -> Result<SapflowBuffer, String> {
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err("BufferTooLarge".into());
    }
    if bytes.is_empty() {
        return Ok(SapflowBuffer::default());
    }
    let mut bytes = std::mem::take(&mut bytes).into_boxed_slice();
    let out = SapflowBuffer {
        ptr: bytes.as_mut_ptr(),
        len: bytes.len(),
    };
    std::mem::forget(bytes);
    Ok(out)
}

fn set_error(out: *mut SapflowBuffer, message: String) {
    if out.is_null() {
        return;
    }
    let value = owned(message.into_bytes()).unwrap_or_default();
    unsafe { out.write(value) };
}

unsafe fn input<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], String> {
    if len > MAX_INPUT_BYTES || (ptr.is_null() && len != 0) {
        return Err("Argument".into());
    }
    if len == 0 {
        Ok(&[])
    } else {
        Ok(unsafe { slice::from_raw_parts(ptr, len) })
    }
}

#[derive(Default)]
struct Writer(Vec<u8>);
impl Writer {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }
    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }
    fn u32(&mut self, value: u32) {
        self.0.extend(value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.0.extend(value.to_le_bytes());
    }
    fn i64(&mut self, value: i64) {
        self.0.extend(value.to_le_bytes());
    }
    fn bytes(&mut self, value: &[u8]) {
        self.u64(value.len() as u64);
        self.0.extend_from_slice(value);
    }
    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }
    fn option_u64(&mut self, value: Option<u64>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.u64(value);
        }
    }
    fn handle<T>(&mut self, value: Arc<T>) {
        self.u64(Arc::into_raw(value) as usize as u64);
    }
    fn option_handle<T>(&mut self, value: Option<Arc<T>>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.handle(value);
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or("Argument")?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, String> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("Argument".into()),
        }
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| "Argument")?,
        ))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| "Argument")?,
        ))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| "Argument")?,
        ))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let len = usize::try_from(self.u64()?).map_err(|_| "Argument")?;
        Ok(self.take(len)?.to_vec())
    }
    fn string(&mut self) -> Result<String, String> {
        String::from_utf8(self.bytes()?).map_err(|_| "Argument".into())
    }
    fn option_i64(&mut self) -> Result<Option<i64>, String> {
        if self.bool()? {
            self.i64().map(Some)
        } else {
            Ok(None)
        }
    }
    fn handle<T>(&mut self) -> Result<Arc<T>, String> {
        let ptr = self.u64()? as usize as *const T;
        if ptr.is_null() {
            return Err("Argument".into());
        }
        unsafe {
            Arc::increment_strong_count(ptr);
            Ok(Arc::from_raw(ptr))
        }
    }
    fn option_handle<T>(&mut self) -> Result<Option<Arc<T>>, String> {
        if self.bool()? {
            self.handle().map(Some)
        } else {
            Ok(None)
        }
    }
    fn finish(self) -> Result<(), String> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err("Argument".into())
        }
    }
}

unsafe fn object<'a, T>(handle: *const c_void) -> Result<&'a T, String> {
    unsafe { handle.cast::<T>().as_ref() }.ok_or_else(|| "Argument".into())
}
unsafe fn clone_object<T>(handle: *const c_void) -> Result<Arc<T>, String> {
    let ptr = handle.cast::<T>();
    if ptr.is_null() {
        return Err("Argument".into());
    }
    unsafe {
        Arc::increment_strong_count(ptr);
        Ok(Arc::from_raw(ptr))
    }
}
fn handle_output<T>(value: Arc<T>) -> Vec<u8> {
    let mut w = Writer::default();
    w.handle(value);
    w.0
}
fn unit() -> Vec<u8> {
    Vec::new()
}
fn scalar_u64(value: u64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}
fn scalar_bool(value: bool) -> Vec<u8> {
    vec![u8::from(value)]
}

fn source_info(w: &mut Writer, x: NativeAacSampleSourceInfo) {
    w.u64(x.producer_generation);
    w.i64(x.native_pts_us);
    w.i64(x.original_pts);
    w.i64(x.original_dts);
    w.u32(x.original_duration);
    w.u32(x.original_timescale);
}
fn fanout_outcome(x: FanoutOutcome) -> Vec<u8> {
    let mut w = Writer::default();
    w.bool(x.publishing_queued);
    w.bool(x.recording_queued);
    w.bool(x.publishing_waiting_keyframe);
    w.u64(x.publishing_overflows);
    w.bool(x.recording_failed);
    w.0
}
fn queue_stats(x: NativeQueueStats) -> Vec<u8> {
    let mut w = Writer::default();
    for value in [
        x.publishing_bytes,
        x.recording_bytes,
        x.publishing_drops,
        x.recording_drops,
        x.publishing_high_water_bytes,
        x.recording_high_water_bytes,
        x.publishing_media_ms,
        x.publishing_high_water_media_ms,
        x.publishing_overflows,
        x.publishing_overflow_bytes,
        x.publishing_overflow_samples,
        x.publishing_overflow_media,
        x.publishing_overflow_residence,
    ] {
        w.u64(value);
    }
    w.bool(x.publishing_waiting_keyframe);
    w.bool(x.recording_failed);
    w.0
}
fn recording_facts(w: &mut Writer, x: NativeRecordingCheckpointFacts) {
    w.u32(x.sequence);
    w.u64(x.audio_units);
    w.u64(x.video_units);
    w.u64(x.audio_start);
    w.u64(x.audio_end);
    w.u64(x.audio_container_end);
    w.u32(x.final_trailing_frames);
    w.bool(x.complete_audio_eof);
    w.option_u64(x.video_start);
    w.option_u64(x.video_end);
    w.u32(x.first_leading_frames);
    w.u64(x.completed_generation_count);
}
fn programme_facts(w: &mut Writer, x: NativeProgrammeCheckpointFacts) {
    w.u32(x.sequence);
    w.u64(x.audio_units);
    w.u64(x.audio_start);
    w.u64(x.audio_end);
    w.u64(x.audio_container_end);
    w.u32(x.leading_frames);
    w.u32(x.trailing_frames);
    w.bool(x.window_geometry_complete);
}
fn programme_start(w: &mut Writer, x: NativeProgrammeWindowStart) {
    w.u64(x.generation);
    w.u64(x.coded_origin);
    w.u64(x.preparation_frames);
    w.u64(x.measured_displacement);
    w.u64(x.discard_prefix_frames);
    w.u64(x.movie_delay_ticks);
}
fn queued_frames(w: &mut Writer, x: NativeProgrammeQueuedFrames) {
    w.u64(x.preparation);
    w.u64(x.programme_raw);
    w.u64(x.programme_zero);
    w.u64(x.block_padding);
    w.u64(x.codec_drain);
}
fn programme_end(w: &mut Writer, x: NativeProgrammeGenerationEnd) {
    w.u64(x.generation);
    queued_frames(w, x.queued);
    w.u64(x.coded_frames);
}
fn programme_extent(x: NativeProgrammePresentationExtent) -> Vec<u8> {
    let mut w = Writer::default();
    w.u32(x.movie_timescale);
    w.u64(x.duration_ticks);
    w.u64(x.duration_ms_ceil);
    w.bool(x.window_geometry_complete);
    w.0
}
fn recording_extent(x: NativeRecordingPresentationExtent) -> Vec<u8> {
    let mut w = Writer::default();
    w.u32(x.movie_timescale);
    w.u64(x.duration_ticks);
    w.u64(x.duration_ms_ceil);
    w.bool(x.complete_audio_eof);
    w.0
}
#[allow(clippy::too_many_arguments)]
fn recording_info(
    w: &mut Writer,
    facts: NativeRecordingCheckpointFacts,
    media_epoch_ns: u64,
    initial_header_len: u32,
    initial_header_sha256: Vec<u8>,
    recording_binding: Vec<u8>,
    source_file_binding: Vec<u8>,
    committed_offset: u64,
    fragment_region_bytes: u64,
    fragment_region_sha256: Vec<u8>,
) {
    recording_facts(w, facts);
    w.u64(media_epoch_ns);
    w.u32(initial_header_len);
    w.bytes(&initial_header_sha256);
    w.bytes(&recording_binding);
    w.bytes(&source_file_binding);
    w.u64(committed_offset);
    w.u64(fragment_region_bytes);
    w.bytes(&fragment_region_sha256);
}
fn encoded_recording_info(w: &mut Writer, x: EncodedRecordingCheckpointInfo) {
    recording_info(
        w,
        x.facts,
        x.media_epoch_ns,
        x.initial_header_len,
        x.initial_header_sha256,
        x.recording_binding,
        x.source_file_binding,
        x.committed_offset,
        x.fragment_region_bytes,
        x.fragment_region_sha256,
    );
}
fn decoded_recording_info(w: &mut Writer, x: DecodedRecordingCheckpointInfo) {
    recording_info(
        w,
        x.facts,
        x.media_epoch_ns,
        x.initial_header_len,
        x.initial_header_sha256,
        x.recording_binding,
        x.source_file_binding,
        x.committed_offset,
        x.fragment_region_bytes,
        x.fragment_region_sha256,
    );
}
#[allow(clippy::too_many_arguments)]
fn programme_info(
    w: &mut Writer,
    facts: NativeProgrammeCheckpointFacts,
    start: NativeProgrammeWindowStart,
    window_end: Option<NativeProgrammeGenerationEnd>,
    media_epoch_ns: u64,
    initial_header_len: u32,
    initial_header_sha256: Vec<u8>,
    recording_binding: Vec<u8>,
    source_file_binding: Vec<u8>,
    committed_offset: u64,
    fragment_region_bytes: u64,
    fragment_region_sha256: Vec<u8>,
) {
    programme_facts(w, facts);
    programme_start(w, start);
    w.bool(window_end.is_some());
    if let Some(end) = window_end {
        programme_end(w, end);
    }
    w.u64(media_epoch_ns);
    w.u32(initial_header_len);
    w.bytes(&initial_header_sha256);
    w.bytes(&recording_binding);
    w.bytes(&source_file_binding);
    w.u64(committed_offset);
    w.u64(fragment_region_bytes);
    w.bytes(&fragment_region_sha256);
}
fn encoded_programme_info(w: &mut Writer, x: EncodedProgrammeCheckpointInfo) {
    programme_info(
        w,
        x.facts,
        x.start,
        x.window_end,
        x.media_epoch_ns,
        x.initial_header_len,
        x.initial_header_sha256,
        x.recording_binding,
        x.source_file_binding,
        x.committed_offset,
        x.fragment_region_bytes,
        x.fragment_region_sha256,
    );
}
fn decoded_programme_info(w: &mut Writer, x: DecodedProgrammeCheckpointInfo) {
    programme_info(
        w,
        x.facts,
        x.start,
        x.window_end,
        x.media_epoch_ns,
        x.initial_header_len,
        x.initial_header_sha256,
        x.recording_binding,
        x.source_file_binding,
        x.committed_offset,
        x.fragment_region_bytes,
        x.fragment_region_sha256,
    );
}
fn recording_stats(x: NativeRecordingStats) -> Vec<u8> {
    let mut w = Writer::default();
    for value in [
        x.buffered_bytes,
        x.buffered_samples,
        x.queued_output_bytes,
        x.accepted_samples,
        x.fragments,
        x.duration_ms,
    ] {
        w.u64(value);
    }
    w.bool(x.finished);
    w.0
}
fn rtmp_send(x: NativeRtmpSendOutput) -> Vec<u8> {
    let mut w = Writer::default();
    w.bytes(&x.output);
    w.u64(x.queued_output_bytes);
    w.option_u64(x.next_deadline_ms);
    w.u8(x.state as u8);
    w.bool(x.startup_submitted);
    w.bool(x.peer_media_progress);
    w.0
}
fn session_action(w: &mut Writer, x: NativeSessionAction) {
    match x {
        NativeSessionAction::None => w.u8(0),
        NativeSessionAction::Connect => w.u8(1),
        NativeSessionAction::RetryAt { at_ns } => {
            w.u8(2);
            w.u64(at_ns);
        }
        NativeSessionAction::AwaitGraceDeadline { at_ns } => {
            w.u8(3);
            w.u64(at_ns);
        }
        NativeSessionAction::EndDestination => w.u8(4),
        NativeSessionAction::GraceExpired => w.u8(5),
        NativeSessionAction::RetryBudgetExhausted => w.u8(6),
    }
}
fn session_snapshot(x: NativeSessionSnapshot) -> Vec<u8> {
    let mut w = Writer::default();
    w.u64(x.generation);
    w.u64(x.attempt_epoch);
    w.u8(x.media_readiness as u8);
    w.u8(x.retry_budget as u8);
    w.bool(x.transport_recovery_source.is_some());
    if let Some(source) = x.transport_recovery_source {
        w.u8(source as u8);
    }
    w.u64(x.network_recoveries);
    w.option_u64(x.outage_deadline_ns);
    w.option_u64(x.next_deadline_ns);
    w.u8(x.state as u8);
    w.bool(x.transport_connected);
    w.option_u64(x.reconnect_deadline_ns);
    for value in [
        x.transport_failures,
        x.retry_attempts,
        x.media_confirmations,
        x.stale_callbacks,
    ] {
        w.u64(value);
    }
    w.0
}
fn srt_stats(x: NativeSrtStats) -> Vec<u8> {
    let mut w = Writer::default();
    for value in [
        x.accepted_samples,
        x.accepted_sample_bytes,
        x.buffered_sample_bytes,
        x.buffered_sample_duration_ns,
        x.discarded_sample_bytes,
        x.pending_chunk_bytes,
        x.pending_chunk_capacity,
        x.discarded_pending_chunk_bytes,
        x.buffered_packets,
        x.buffered_bytes,
        x.peak_buffered_bytes,
        x.sent_packets,
        x.sent_bytes,
        x.retransmitted_packets,
        x.retransmitted_bytes,
        x.acknowledged_bytes,
        x.abandoned_bytes,
        x.encrypted_packets,
        x.km_requests,
        x.km_responses,
        x.rtt_us,
        x.malformed_datagrams,
        x.stray_datagrams,
        x.rejected_datagrams,
        x.backpressure_events,
        x.ffi_input_handle_clones,
        x.sdk_input_handle_clones,
        x.ts_output_allocations,
        x.elementary_staging_copy_bytes,
        x.packet_payload_copy_bytes,
        x.writer_payload_copy_bytes,
        x.ts_serialised_bytes,
        x.returned_datagrams,
        x.returned_datagram_bytes,
    ] {
        w.u64(value);
    }
    w.0
}
fn srt_event(x: NativeSrtEvent) -> Vec<u8> {
    let mut w = Writer::default();
    match x {
        NativeSrtEvent::Sending => w.u8(0),
        NativeSrtEvent::PeerMediaProgress => w.u8(1),
        NativeSrtEvent::Connected {
            latency_ms,
            max_payload_bytes,
        } => {
            w.u8(2);
            w.u32(latency_ms);
            w.u32(max_payload_bytes);
        }
        NativeSrtEvent::Failed { reason } => {
            w.u8(3);
            w.u8(reason as u8);
        }
        NativeSrtEvent::Closed { reason, peer_code } => {
            w.u8(4);
            w.u8(reason as u8);
            w.bool(peer_code.is_some());
            if let Some(code) = peer_code {
                w.u32(code);
            }
        }
    }
    w.0
}
fn read_generation(r: &mut Reader<'_>) -> Result<NativeAudioGenerationReceipt, String> {
    Ok(NativeAudioGenerationReceipt {
        generation: r.u64()?,
        input_frames: r.u64()?,
        raw_frames: r.u64()?,
        zero_frames: r.u64()?,
        input_rate: r.u32()?,
        encoded_frames: r.u64()?,
        leading_frames: r.u32()?,
        trailing_frames: r.u32()?,
        source_mapping: match r.u8()? {
            0 => NativeRecordingSourceMapping::Nearest,
            1 => NativeRecordingSourceMapping::Floor,
            _ => return Err("Argument".into()),
        },
    })
}
fn read_file_receipt(r: &mut Reader<'_>) -> Result<NativeAudioFileReceipt, String> {
    Ok(NativeAudioFileReceipt {
        generation_count: r.u64()?,
        source_frames_at_48k: r.u64()?,
        intentional_standby_frames: r.u64()?,
        encoded_frames: r.u64()?,
        first_leading_frames: r.u32()?,
        last_trailing_frames: r.u32()?,
    })
}
fn read_programme_start(r: &mut Reader<'_>) -> Result<NativeProgrammeWindowStart, String> {
    Ok(NativeProgrammeWindowStart {
        generation: r.u64()?,
        coded_origin: r.u64()?,
        preparation_frames: r.u64()?,
        measured_displacement: r.u64()?,
        discard_prefix_frames: r.u64()?,
        movie_delay_ticks: r.u64()?,
    })
}
fn read_programme_end(r: &mut Reader<'_>) -> Result<NativeProgrammeGenerationEnd, String> {
    Ok(NativeProgrammeGenerationEnd {
        generation: r.u64()?,
        queued: NativeProgrammeQueuedFrames {
            preparation: r.u64()?,
            programme_raw: r.u64()?,
            programme_zero: r.u64()?,
            block_padding: r.u64()?,
            codec_drain: r.u64()?,
        },
        coded_frames: r.u64()?,
    })
}
fn read_srt_config(r: &mut Reader<'_>) -> Result<NativeSrtConfig, String> {
    Ok(NativeSrtConfig {
        destination: r.string()?,
        stream_id: r.string()?,
        cipher: if r.bool()? {
            Some(match r.u8()? {
                0 => NativeSrtCipher::Aes128,
                1 => NativeSrtCipher::Aes256,
                _ => return Err("Argument".into()),
            })
        } else {
            None
        },
        latency_ms: r.u32()?,
        max_buffered_packets: r.u32()?,
        max_buffered_bytes: r.u32()?,
        max_packet_age_ms: r.u32()?,
        max_sample_residence_ms: r.u32()?,
        bandwidth_bytes_per_second: r.u64()?,
    })
}

fn debug_error<E: std::fmt::Debug>(error: E) -> String {
    format!("{error:?}")
}

fn hot<T>(out: *mut T, error: *mut SapflowBuffer, call: impl FnOnce() -> Result<T, String>) -> i32 {
    if out.is_null() {
        set_error(error, "Argument".into());
        return 1;
    }
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(value)) => {
            unsafe { out.write(value) };
            0
        }
        Ok(Err(message)) => {
            set_error(error, message);
            1
        }
        Err(_) => {
            set_error(error, "Panic".into());
            1
        }
    }
}

fn hot_unit(error: *mut SapflowBuffer, call: impl FnOnce() -> Result<(), String>) -> i32 {
    let mut out = 0u8;
    hot(&mut out, error, || {
        call()?;
        Ok(0)
    })
}

fn call(operation: u32, handle: *const c_void, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut r = Reader::new(bytes);
    let value = unsafe {
        match operation {
            1 => sdk_abi_version().to_le_bytes().to_vec(),
            2 => handle_output(EncodedBuffer::new(&r.bytes()?).map_err(debug_error)?),
            3 => scalar_u64(object::<EncodedBuffer>(handle)?.len()),
            4 => scalar_bool(object::<EncodedBuffer>(handle)?.is_empty()),
            5 => vec![
                object::<EncodedBuffer>(handle)?
                    .byte_at(r.u64()?)
                    .map_err(debug_error)?,
            ],
            6 => handle_output(MediaCodec::h264(&r.bytes()?, &r.bytes()?).map_err(debug_error)?),
            7 => handle_output(MediaCodec::aac(&r.bytes()?).map_err(debug_error)?),
            8 => {
                let bytes = r.bytes()?;
                handle_output(
                    object::<MediaCodec>(handle)?
                        .sample(&bytes, r.i64()?, r.i64()?, r.u32()?, r.u32()?, r.bool()?)
                        .map_err(debug_error)?,
                )
            }
            9 => {
                let generation = r.u64()?;
                let native = r.i64()?;
                let publishing = r.option_i64()?;
                handle_output(
                    object::<MediaSample>(handle)?
                        .aac_timing_views(generation, native, publishing)
                        .map_err(debug_error)?,
                )
            }
            10 => {
                let mut w = Writer::default();
                let value = object::<MediaSample>(handle)?.aac_source_info();
                w.bool(value.is_some());
                if let Some(value) = value {
                    source_info(&mut w, value);
                }
                w.0
            }
            11 => scalar_u64(object::<MediaSample>(handle)?.len()),
            12 => scalar_bool(object::<MediaSample>(handle)?.is_empty()),
            13 => scalar_bool(object::<MediaSample>(handle)?.is_video()),
            14 => scalar_bool(object::<MediaSample>(handle)?.is_keyframe()),
            15 => object::<MediaSample>(handle)?.pts().to_le_bytes().to_vec(),
            16 => object::<MediaSample>(handle)?.dts().to_le_bytes().to_vec(),
            17 => scalar_u64(u64::from(object::<MediaSample>(handle)?.duration())),
            18 => scalar_u64(u64::from(object::<MediaSample>(handle)?.timescale())),
            19 => {
                let mut w = Writer::default();
                source_info(
                    &mut w,
                    object::<NativeAacTimingViews>(handle)?.source_info(),
                );
                w.0
            }
            20 => handle_output(object::<NativeAacTimingViews>(handle)?.recording_sample()),
            21 => {
                let mut w = Writer::default();
                w.option_handle(object::<NativeAacTimingViews>(handle)?.publishing_sample());
                w.0
            }
            22 => handle_output(
                NativeSampleFanout::new(r.u64()?, r.u64()?, r.u32()?, r.bool()?)
                    .map_err(debug_error)?,
            ),
            23 => handle_output(
                NativeSampleFanout::with_aac_timing_views(r.u64()?, r.u64()?, r.u32()?, r.bool()?)
                    .map_err(debug_error)?,
            ),
            24 => {
                let views = r.handle()?;
                fanout_outcome(
                    object::<NativeSampleFanout>(handle)?
                        .push_aac_timing_views(views, r.u64()?)
                        .map_err(debug_error)?,
                )
            }
            25 => {
                let sample = r.handle()?;
                fanout_outcome(
                    object::<NativeSampleFanout>(handle)?
                        .push(sample, r.u64()?)
                        .map_err(debug_error)?,
                )
            }
            26 | 27 => {
                let now = r.u64()?;
                let value = if operation == 26 {
                    object::<NativeSampleFanout>(handle)?.pop_publishing(now)
                } else {
                    object::<NativeSampleFanout>(handle)?.pop_recording(now)
                }
                .map_err(debug_error)?;
                let mut w = Writer::default();
                w.option_handle(value);
                w.0
            }
            28 => queue_stats(
                object::<NativeSampleFanout>(handle)?
                    .stats()
                    .map_err(debug_error)?,
            ),
            29 => {
                object::<NativeSampleFanout>(handle)?
                    .close_publishing_admission()
                    .map_err(debug_error)?;
                unit()
            }
            30 => {
                object::<NativeSampleFanout>(handle)?
                    .close_recording_admission()
                    .map_err(debug_error)?;
                unit()
            }
            31 => {
                object::<NativeSampleFanout>(handle)?
                    .cancel()
                    .map_err(debug_error)?;
                unit()
            }
            32 => {
                let mut w = Writer::default();
                recording_facts(&mut w, object::<NativeRecordingCheckpoint>(handle)?.facts());
                w.0
            }
            33 => recording_extent(
                object::<NativeRecordingCheckpoint>(handle)?
                    .presentation_extent()
                    .map_err(debug_error)?,
            ),
            34 => {
                let mut w = Writer::default();
                decoded_recording_info(
                    &mut w,
                    object::<DecodedRecordingCheckpoint>(handle)?.info(),
                );
                w.0
            }
            35 => object::<DecodedRecordingCheckpoint>(handle)?.header(),
            36 => object::<DecodedRecordingCheckpoint>(handle)?.fragment_region_sha256(),
            37 => {
                let mut w = Writer::default();
                programme_facts(&mut w, object::<NativeProgrammeCheckpoint>(handle)?.facts());
                w.0
            }
            38 => {
                let mut w = Writer::default();
                programme_start(&mut w, object::<NativeProgrammeCheckpoint>(handle)?.start());
                w.0
            }
            39 => {
                let mut w = Writer::default();
                let value = object::<NativeProgrammeCheckpoint>(handle)?.window_end();
                w.bool(value.is_some());
                if let Some(value) = value {
                    programme_end(&mut w, value);
                }
                w.0
            }
            40 => programme_extent(
                object::<NativeProgrammeCheckpoint>(handle)?
                    .presentation_extent()
                    .map_err(debug_error)?,
            ),
            41 => {
                let mut w = Writer::default();
                decoded_programme_info(
                    &mut w,
                    object::<DecodedProgrammeCheckpoint>(handle)?.info(),
                );
                w.0
            }
            42 => object::<DecodedProgrammeCheckpoint>(handle)?.header(),
            43 => object::<DecodedProgrammeCheckpoint>(handle)?.fragment_region_sha256(),
            44 => programme_extent(
                object::<DecodedProgrammeCheckpoint>(handle)?
                    .presentation_extent()
                    .map_err(debug_error)?,
            ),
            45 => handle_output(
                NativeRecordingMuxer::new(r.bool()?, r.bool()?, r.u64()?, r.u64()?, r.u32()?)
                    .map_err(debug_error)?,
            ),
            46 => handle_output(
                NativeRecordingMuxer::with_native_aac_timing(
                    r.bool()?,
                    r.bool()?,
                    r.u64()?,
                    r.u64()?,
                    r.u32()?,
                )
                .map_err(debug_error)?,
            ),
            47 => handle_output(
                NativeRecordingMuxer::with_programme_aac_timing(r.u64()?, r.u64()?, r.u32()?)
                    .map_err(debug_error)?,
            ),
            48 => {
                object::<NativeRecordingMuxer>(handle)?
                    .begin_programme_window(read_programme_start(&mut r)?)
                    .map_err(debug_error)?;
                unit()
            }
            49 => {
                object::<NativeRecordingMuxer>(handle)?
                    .close_programme_window(read_programme_end(&mut r)?)
                    .map_err(debug_error)?;
                unit()
            }
            50 => {
                let value = object::<NativeRecordingMuxer>(handle)?
                    .take_programme_output()
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.bool(value.is_some());
                if let Some(value) = value {
                    w.bytes(&value.bytes);
                    w.u8(value.kind as u8);
                    w.option_handle(value.checkpoint);
                }
                w.0
            }
            51 => object::<NativeRecordingMuxer>(handle)?
                .programme_checkpoint_header(r.handle()?)
                .map_err(debug_error)?,
            52 => {
                let token = r.handle()?;
                let a = r.bytes()?;
                let b = r.bytes()?;
                let c = r.bytes()?;
                let value = object::<NativeRecordingMuxer>(handle)?
                    .encode_programme_checkpoint(token, a, b, c)
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.bytes(&value.bytes);
                encoded_programme_info(&mut w, value.info);
                w.0
            }
            53 => {
                let outcome = match r.u8()? {
                    0 => NativeProgrammeCompletion::CompleteSubmittedWindow,
                    1 => NativeProgrammeCompletion::IncompletePrefix,
                    _ => return Err("Argument".into()),
                };
                object::<NativeRecordingMuxer>(handle)?
                    .finish_programme(outcome)
                    .map_err(debug_error)?;
                unit()
            }
            54 => {
                object::<NativeRecordingMuxer>(handle)?
                    .begin_native_audio(r.u32()?)
                    .map_err(debug_error)?;
                unit()
            }
            55 => {
                object::<NativeRecordingMuxer>(handle)?
                    .end_native_audio_generation(read_generation(&mut r)?)
                    .map_err(debug_error)?;
                unit()
            }
            56 => {
                object::<NativeRecordingMuxer>(handle)?
                    .complete_native_audio(read_file_receipt(&mut r)?)
                    .map_err(debug_error)?;
                unit()
            }
            57 => {
                object::<NativeRecordingMuxer>(handle)?
                    .push(r.handle()?)
                    .map_err(debug_error)?;
                unit()
            }
            58 => {
                let value = object::<NativeRecordingMuxer>(handle)?
                    .take_output()
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.bool(value.is_some());
                if let Some(value) = value {
                    w.bytes(&value.bytes);
                    w.bool(value.rewrite_header);
                }
                w.0
            }
            59 => {
                let value = object::<NativeRecordingMuxer>(handle)?
                    .take_native_output()
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.bool(value.is_some());
                if let Some(value) = value {
                    w.bytes(&value.bytes);
                    w.u8(value.kind as u8);
                    w.option_handle(value.checkpoint);
                }
                w.0
            }
            60 => object::<NativeRecordingMuxer>(handle)?
                .checkpoint_header(r.handle()?)
                .map_err(debug_error)?,
            61 => {
                let token = r.handle()?;
                let a = r.bytes()?;
                let b = r.bytes()?;
                let c = r.bytes()?;
                let value = object::<NativeRecordingMuxer>(handle)?
                    .encode_checkpoint(token, a, b, c)
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.bytes(&value.bytes);
                encoded_recording_info(&mut w, value.info);
                w.0
            }
            62 => {
                object::<NativeRecordingMuxer>(handle)?
                    .finish()
                    .map_err(debug_error)?;
                unit()
            }
            63 => {
                let outcome = match r.u8()? {
                    0 => NativeRecordingCompletion::Complete,
                    1 => NativeRecordingCompletion::IncompletePrefix,
                    _ => return Err("Argument".into()),
                };
                object::<NativeRecordingMuxer>(handle)?
                    .finish_native(outcome)
                    .map_err(debug_error)?;
                unit()
            }
            64 => {
                object::<NativeRecordingMuxer>(handle)?
                    .cancel()
                    .map_err(debug_error)?;
                unit()
            }
            65 => recording_stats(
                object::<NativeRecordingMuxer>(handle)?
                    .stats()
                    .map_err(debug_error)?,
            ),
            66 => {
                let bytes = r.bytes()?;
                let a = r.bytes()?;
                let b = r.bytes()?;
                handle_output(decode_recording_checkpoint(bytes, a, b).map_err(debug_error)?)
            }
            67 => {
                validate_recording_checkpoint_successor(r.handle()?, r.handle()?)
                    .map_err(debug_error)?;
                unit()
            }
            68 => {
                let bytes = r.bytes()?;
                let a = r.bytes()?;
                let b = r.bytes()?;
                handle_output(decode_programme_checkpoint(bytes, a, b).map_err(debug_error)?)
            }
            69 => {
                validate_programme_checkpoint_successor(r.handle()?, r.handle()?)
                    .map_err(debug_error)?;
                unit()
            }
            70 => handle_output(
                NativeRtmpPublisher::new(r.string()?, r.string()?, r.string()?, r.u64()?, r.u64()?)
                    .map_err(debug_error)?,
            ),
            71 => {
                object::<NativeRtmpPublisher>(handle)?
                    .declare_tracks(r.bool()?, r.bool()?)
                    .map_err(debug_error)?;
                unit()
            }
            72 => {
                let value = object::<NativeRtmpPublisher>(handle)?
                    .poll_event()
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.bool(value.is_some());
                if let Some(value) = value {
                    w.u8(value as u8);
                }
                w.0
            }
            73 => {
                let bytes = r.bytes()?;
                object::<NativeRtmpPublisher>(handle)?
                    .feed(&bytes, r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            74 => {
                object::<NativeRtmpPublisher>(handle)?
                    .tick(r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            75 => {
                let mut w = Writer::default();
                w.option_u64(
                    object::<NativeRtmpPublisher>(handle)?
                        .next_deadline_ms()
                        .map_err(debug_error)?,
                );
                w.0
            }
            76 => object::<NativeRtmpPublisher>(handle)?
                .take_output()
                .map_err(debug_error)?,
            77 => scalar_u64(
                object::<NativeRtmpPublisher>(handle)?
                    .queued_output_bytes()
                    .map_err(debug_error)?,
            ),
            78 => vec![
                object::<NativeRtmpPublisher>(handle)?
                    .state()
                    .map_err(debug_error)? as u8,
            ],
            79 => {
                object::<NativeRtmpPublisher>(handle)?
                    .send(r.handle()?)
                    .map_err(debug_error)?;
                unit()
            }
            80 => {
                let sample = r.handle()?;
                rtmp_send(
                    object::<NativeRtmpPublisher>(handle)?
                        .send_and_take_output(sample, r.u64()?)
                        .map_err(debug_error)?,
                )
            }
            81 => {
                object::<NativeRtmpPublisher>(handle)?
                    .finish()
                    .map_err(debug_error)?;
                unit()
            }
            82 => {
                object::<NativeRtmpPublisher>(handle)?
                    .cancel()
                    .map_err(debug_error)?;
                unit()
            }
            83 => handle_output(NativeSession::new(r.u64()?).map_err(debug_error)?),
            84 => scalar_u64(
                object::<NativeSession>(handle)?
                    .start(r.u64()?)
                    .map_err(debug_error)?,
            ),
            85 => {
                let mut w = Writer::default();
                session_action(
                    &mut w,
                    object::<NativeSession>(handle)?
                        .prepared(r.u64()?)
                        .map_err(debug_error)?,
                );
                w.0
            }
            86 => {
                let generation = r.u64()?;
                object::<NativeSession>(handle)?
                    .connected(generation, r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            87 => {
                let generation = r.u64()?;
                let attempt = r.u64()?;
                let mut w = Writer::default();
                session_action(
                    &mut w,
                    object::<NativeSession>(handle)?
                        .sending(generation, attempt, r.u64()?)
                        .map_err(debug_error)?,
                );
                w.0
            }
            88 => {
                let generation = r.u64()?;
                let attempt = r.u64()?;
                let now = r.u64()?;
                let source = match r.u8()? {
                    0 => NativeTransportRecoverySource::SrtPeerAcknowledgedWatermark,
                    1 => NativeTransportRecoverySource::RtmpPeerAcknowledgedWatermark,
                    _ => return Err("Argument".into()),
                };
                let mut w = Writer::default();
                session_action(
                    &mut w,
                    object::<NativeSession>(handle)?
                        .transport_recovered(generation, attempt, now, source)
                        .map_err(debug_error)?,
                );
                w.0
            }
            89 => {
                let generation = r.u64()?;
                object::<NativeSession>(handle)?
                    .media_confirmed(generation, r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            90 => {
                let generation = r.u64()?;
                let attempt = r.u64()?;
                object::<NativeSession>(handle)?
                    .media_confirmed_for_attempt(generation, attempt, r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            91..=93 => {
                let generation = r.u64()?;
                let now = r.u64()?;
                let value = match operation {
                    91 => object::<NativeSession>(handle)?.replace_transport(generation, now),
                    92 => object::<NativeSession>(handle)?.transport_failed(generation, now),
                    _ => object::<NativeSession>(handle)?.tick(generation, now),
                }
                .map_err(debug_error)?;
                let mut w = Writer::default();
                session_action(&mut w, value);
                w.0
            }
            94 => scalar_u64(
                object::<NativeSession>(handle)?
                    .interrupt(r.u64()?)
                    .map_err(debug_error)?,
            ),
            95 => {
                let value = object::<NativeSession>(handle)?
                    .stop(r.u64()?)
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.u64(value.generation);
                session_action(&mut w, value.action);
                w.0
            }
            96 => {
                object::<NativeSession>(handle)?
                    .finish_stop(r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            97 => scalar_u64(
                object::<NativeSession>(handle)?
                    .cancel(r.u64()?)
                    .map_err(debug_error)?,
            ),
            98 => session_snapshot(
                object::<NativeSession>(handle)?
                    .snapshot()
                    .map_err(debug_error)?,
            ),
            99 => {
                let config = read_srt_config(&mut r)?;
                let video = r.option_handle()?;
                let audio = r.option_handle()?;
                let secret = if r.bool()? { Some(r.bytes()?) } else { None };
                handle_output(
                    NativeSrtPublisher::new(config, video, audio, secret, r.u64()?)
                        .map_err(debug_error)?,
                )
            }
            100 => {
                let sample = r.handle()?;
                object::<NativeSrtPublisher>(handle)?
                    .push(sample, r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            101 => {
                let source = r.string()?;
                let bytes = r.bytes()?;
                object::<NativeSrtPublisher>(handle)?
                    .feed(source, &bytes, r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            102 => {
                object::<NativeSrtPublisher>(handle)?
                    .tick(r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            103 => {
                let value = object::<NativeSrtPublisher>(handle)?
                    .poll_datagram(r.u64()?)
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.bool(value.is_some());
                if let Some(value) = value {
                    w.string(&value.destination);
                    w.bytes(&value.bytes);
                }
                w.0
            }
            104 => scalar_bool(
                object::<NativeSrtPublisher>(handle)?
                    .is_ready()
                    .map_err(debug_error)?,
            ),
            105 => scalar_bool(
                object::<NativeSrtPublisher>(handle)?
                    .has_immediate_work()
                    .map_err(debug_error)?,
            ),
            106 => {
                let mut w = Writer::default();
                w.option_u64(
                    object::<NativeSrtPublisher>(handle)?
                        .next_deadline_us()
                        .map_err(debug_error)?,
                );
                w.0
            }
            107 => vec![
                object::<NativeSrtPublisher>(handle)?
                    .state()
                    .map_err(debug_error)? as u8,
            ],
            108 => {
                let value = object::<NativeSrtPublisher>(handle)?
                    .poll_event()
                    .map_err(debug_error)?;
                let mut w = Writer::default();
                w.bool(value.is_some());
                if let Some(value) = value {
                    w.0.extend(srt_event(value));
                }
                w.0
            }
            109 => {
                object::<NativeSrtPublisher>(handle)?
                    .stop(r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            110 => {
                object::<NativeSrtPublisher>(handle)?
                    .socket_send_failed(r.u64()?)
                    .map_err(debug_error)?;
                unit()
            }
            111 => srt_stats(
                object::<NativeSrtPublisher>(handle)?
                    .stats()
                    .map_err(debug_error)?,
            ),
            _ => return Err("Argument".into()),
        }
    };
    r.finish()?;
    Ok(value)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_encoded_buffer_new(
    bytes: *const u8,
    len: usize,
    out: *mut *mut c_void,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        Ok(
            Arc::into_raw(EncodedBuffer::new(unsafe { input(bytes, len)? }).map_err(debug_error)?)
                as *mut c_void,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_media_codec_sample(
    codec: *const c_void,
    bytes: *const u8,
    len: usize,
    pts: i64,
    dts: i64,
    duration: u32,
    timescale: u32,
    keyframe: bool,
    out: *mut *mut c_void,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let bytes = unsafe { input(bytes, len)? };
        Ok(Arc::into_raw(
            unsafe { object::<MediaCodec>(codec)? }
                .sample(bytes, pts, dts, duration, timescale, keyframe)
                .map_err(debug_error)?,
        ) as *mut c_void)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_encoded_buffer_scalar(
    buffer: *const c_void,
    operation: u32,
    index: u64,
    out: *mut u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let buffer = unsafe { object::<EncodedBuffer>(buffer)? };
        match operation {
            3 => Ok(buffer.len()),
            4 => Ok(u64::from(buffer.is_empty())),
            5 => Ok(u64::from(buffer.byte_at(index).map_err(debug_error)?)),
            _ => Err("Argument".into()),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_media_sample_scalar(
    sample: *const c_void,
    operation: u32,
    out: *mut u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let sample = unsafe { object::<MediaSample>(sample)? };
        match operation {
            11 => Ok(sample.len()),
            12 => Ok(u64::from(sample.is_empty())),
            13 => Ok(u64::from(sample.is_video())),
            14 => Ok(u64::from(sample.is_keyframe())),
            15 => Ok(sample.pts() as u64),
            16 => Ok(sample.dts() as u64),
            17 => Ok(u64::from(sample.duration())),
            18 => Ok(u64::from(sample.timescale())),
            _ => Err("Argument".into()),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_media_sample_metadata(
    sample: *const c_void,
    out: *mut SapflowMediaSampleResult,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        Ok(media_sample_metadata(unsafe {
            object::<MediaSample>(sample)?
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_aac_timing_views_sample(
    views: *const c_void,
    publishing: bool,
    out: *mut SapflowMediaSampleResult,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let views = unsafe { object::<NativeAacTimingViews>(views)? };
        Ok(if publishing {
            optional_sample_result(views.publishing_sample())
        } else {
            media_sample_result(views.recording_sample())
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_sample_fanout_push(
    fanout: *const c_void,
    sample: *const c_void,
    now_ns: u64,
    out: *mut SapflowFanoutOutcome,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let outcome = unsafe { object::<NativeSampleFanout>(fanout)? }
            .push(unsafe { clone_object(sample)? }, now_ns)
            .map_err(debug_error)?;
        Ok(SapflowFanoutOutcome {
            publishing_queued: outcome.publishing_queued,
            recording_queued: outcome.recording_queued,
            publishing_waiting_keyframe: outcome.publishing_waiting_keyframe,
            publishing_overflows: outcome.publishing_overflows,
            recording_failed: outcome.recording_failed,
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_sample_fanout_push_aac_timing_views(
    fanout: *const c_void,
    views: *const c_void,
    now_ns: u64,
    out: *mut SapflowFanoutOutcome,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let outcome = unsafe { object::<NativeSampleFanout>(fanout)? }
            .push_aac_timing_views(unsafe { clone_object(views)? }, now_ns)
            .map_err(debug_error)?;
        Ok(SapflowFanoutOutcome {
            publishing_queued: outcome.publishing_queued,
            recording_queued: outcome.recording_queued,
            publishing_waiting_keyframe: outcome.publishing_waiting_keyframe,
            publishing_overflows: outcome.publishing_overflows,
            recording_failed: outcome.recording_failed,
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_sample_fanout_pop(
    fanout: *const c_void,
    recording: bool,
    now_ns: u64,
    out: *mut SapflowMediaSampleResult,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let fanout = unsafe { object::<NativeSampleFanout>(fanout)? };
        let sample = if recording {
            fanout.pop_recording(now_ns)
        } else {
            fanout.pop_publishing(now_ns)
        }
        .map_err(debug_error)?;
        Ok(optional_sample_result(sample))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_recording_muxer_push(
    muxer: *const c_void,
    sample: *const c_void,
    error: *mut SapflowBuffer,
) -> i32 {
    hot_unit(error, || {
        unsafe { object::<NativeRecordingMuxer>(muxer)? }
            .push(unsafe { clone_object(sample)? })
            .map_err(debug_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_recording_muxer_take_output(
    muxer: *const c_void,
    out: *mut SapflowRecordingOutput,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let value = unsafe { object::<NativeRecordingMuxer>(muxer)? }
            .take_output()
            .map_err(debug_error)?;
        value.map_or_else(
            || Ok(SapflowRecordingOutput::default()),
            |value| {
                let kind = if value.rewrite_header {
                    NativeRecordingChunkKind::FinalHeader
                } else {
                    NativeRecordingChunkKind::Fragment
                };
                recording_output(
                    value.bytes,
                    kind,
                    value.rewrite_header,
                    None::<Arc<NativeRecordingCheckpoint>>,
                )
            },
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_recording_muxer_take_native_output(
    muxer: *const c_void,
    out: *mut SapflowRecordingOutput,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let value = unsafe { object::<NativeRecordingMuxer>(muxer)? }
            .take_native_output()
            .map_err(debug_error)?;
        value.map_or_else(
            || Ok(SapflowRecordingOutput::default()),
            |value| recording_output(value.bytes, value.kind, false, value.checkpoint),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_recording_muxer_take_programme_output(
    muxer: *const c_void,
    out: *mut SapflowRecordingOutput,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let value = unsafe { object::<NativeRecordingMuxer>(muxer)? }
            .take_programme_output()
            .map_err(debug_error)?;
        value.map_or_else(
            || Ok(SapflowRecordingOutput::default()),
            |value| recording_output(value.bytes, value.kind, false, value.checkpoint),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_rtmp_publisher_feed(
    publisher: *const c_void,
    bytes: *const u8,
    len: usize,
    now_ms: u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot_unit(error, || {
        unsafe { object::<NativeRtmpPublisher>(publisher)? }
            .feed(unsafe { input(bytes, len)? }, now_ms)
            .map_err(debug_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_rtmp_publisher_send(
    publisher: *const c_void,
    sample: *const c_void,
    error: *mut SapflowBuffer,
) -> i32 {
    hot_unit(error, || {
        unsafe { object::<NativeRtmpPublisher>(publisher)? }
            .send(unsafe { clone_object(sample)? })
            .map_err(debug_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_rtmp_publisher_take_output(
    publisher: *const c_void,
    out: *mut SapflowBuffer,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        owned(
            unsafe { object::<NativeRtmpPublisher>(publisher)? }
                .take_output()
                .map_err(debug_error)?,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_rtmp_publisher_send_and_take_output(
    publisher: *const c_void,
    sample: *const c_void,
    now_ms: u64,
    out: *mut SapflowRtmpSendOutput,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let value = unsafe { object::<NativeRtmpPublisher>(publisher)? }
            .send_and_take_output(unsafe { clone_object(sample)? }, now_ms)
            .map_err(debug_error)?;
        Ok(SapflowRtmpSendOutput {
            output: owned(value.output)?,
            queued_output_bytes: value.queued_output_bytes,
            next_deadline_ms: value.next_deadline_ms.unwrap_or(0),
            state: value.state as u8,
            has_next_deadline: value.next_deadline_ms.is_some(),
            startup_submitted: value.startup_submitted,
            peer_media_progress: value.peer_media_progress,
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_rtmp_publisher_next_deadline_ms(
    publisher: *const c_void,
    out: *mut SapflowOptionalU64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let value = unsafe { object::<NativeRtmpPublisher>(publisher)? }
            .next_deadline_ms()
            .map_err(debug_error)?;
        Ok(SapflowOptionalU64 {
            value: value.unwrap_or(0),
            present: value.is_some(),
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_rtmp_publisher_queued_output_bytes(
    publisher: *const c_void,
    out: *mut u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        unsafe { object::<NativeRtmpPublisher>(publisher)? }
            .queued_output_bytes()
            .map_err(debug_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_rtmp_publisher_state(
    publisher: *const c_void,
    out: *mut u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        Ok(unsafe { object::<NativeRtmpPublisher>(publisher)? }
            .state()
            .map_err(debug_error)? as u64)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_srt_publisher_push(
    publisher: *const c_void,
    sample: *const c_void,
    now_us: u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot_unit(error, || {
        unsafe { object::<NativeSrtPublisher>(publisher)? }
            .push(unsafe { clone_object(sample)? }, now_us)
            .map_err(debug_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_srt_publisher_feed(
    publisher: *const c_void,
    source: *const u8,
    source_len: usize,
    bytes: *const u8,
    len: usize,
    now_us: u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot_unit(error, || {
        let source = std::str::from_utf8(unsafe { input(source, source_len)? })
            .map_err(|_| "Argument")?
            .to_owned();
        unsafe { object::<NativeSrtPublisher>(publisher)? }
            .feed(source, unsafe { input(bytes, len)? }, now_us)
            .map_err(debug_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_srt_publisher_poll_datagram(
    publisher: *const c_void,
    now_us: u64,
    out: *mut SapflowSrtDatagram,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        srt_datagram_result(
            unsafe { object::<NativeSrtPublisher>(publisher)? }
                .poll_datagram(now_us)
                .map_err(debug_error)?,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_srt_publisher_is_ready(
    publisher: *const c_void,
    out: *mut u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        Ok(u64::from(
            unsafe { object::<NativeSrtPublisher>(publisher)? }
                .is_ready()
                .map_err(debug_error)?,
        ))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_srt_publisher_has_immediate_work(
    publisher: *const c_void,
    out: *mut u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        Ok(u64::from(
            unsafe { object::<NativeSrtPublisher>(publisher)? }
                .has_immediate_work()
                .map_err(debug_error)?,
        ))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_srt_publisher_next_deadline_us(
    publisher: *const c_void,
    out: *mut SapflowOptionalU64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        let value = unsafe { object::<NativeSrtPublisher>(publisher)? }
            .next_deadline_us()
            .map_err(debug_error)?;
        Ok(SapflowOptionalU64 {
            value: value.unwrap_or(0),
            present: value.is_some(),
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_srt_publisher_state(
    publisher: *const c_void,
    out: *mut u64,
    error: *mut SapflowBuffer,
) -> i32 {
    hot(out, error, || {
        Ok(unsafe { object::<NativeSrtPublisher>(publisher)? }
            .state()
            .map_err(debug_error)? as u64)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn sapflow_abi_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_call(
    operation: u32,
    handle: *const c_void,
    input_ptr: *const u8,
    input_len: usize,
    out: *mut SapflowBuffer,
    error: *mut SapflowBuffer,
) -> i32 {
    if out.is_null() {
        set_error(error, "Argument".into());
        return 1;
    }
    unsafe { out.write(SapflowBuffer::default()) };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let bytes = unsafe { input(input_ptr, input_len)? };
        call(operation, handle, bytes)
    }));
    match result {
        Ok(Ok(value)) => match owned(value) {
            Ok(value) => {
                unsafe { out.write(value) };
                0
            }
            Err(message) => {
                set_error(error, message);
                1
            }
        },
        Ok(Err(message)) => {
            set_error(error, message);
            1
        }
        Err(_) => {
            set_error(error, "Panic".into());
            1
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sapflow_buffer_free(buffer: SapflowBuffer) {
    if !buffer.ptr.is_null() {
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                buffer.ptr, buffer.len,
            )));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_object_free(kind: u32, handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    macro_rules! release {
        ($type:ty) => {
            unsafe { drop(Arc::from_raw(handle.cast::<$type>())) }
        };
    }
    match kind {
        1 => release!(EncodedBuffer),
        2 => release!(MediaCodec),
        3 => release!(MediaSample),
        4 => release!(NativeAacTimingViews),
        5 => release!(NativeSampleFanout),
        6 => release!(NativeRecordingCheckpoint),
        7 => release!(DecodedRecordingCheckpoint),
        8 => release!(NativeProgrammeCheckpoint),
        9 => release!(DecodedProgrammeCheckpoint),
        10 => release!(NativeRecordingMuxer),
        11 => release!(NativeRtmpPublisher),
        12 => release!(NativeSession),
        13 => release!(NativeSrtPublisher),
        _ => {}
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn invoke(operation: u32, handle: *const c_void, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        let mut out = SapflowBuffer::default();
        let mut error = SapflowBuffer::default();
        let status = unsafe {
            sapflow_call(
                operation,
                handle,
                bytes.as_ptr(),
                bytes.len(),
                &mut out,
                &mut error,
            )
        };
        let take = |buffer: SapflowBuffer| unsafe {
            let value = if buffer.ptr.is_null() {
                Vec::new()
            } else {
                slice::from_raw_parts(buffer.ptr, buffer.len).to_vec()
            };
            sapflow_buffer_free(buffer);
            value
        };
        if status == 0 {
            Ok(take(out))
        } else {
            Err(String::from_utf8(take(error)).unwrap())
        }
    }

    #[test]
    fn buffer_ownership_and_errors_match_direct_api() {
        let mut source = vec![1, 2, 3];
        let created = invoke(2, std::ptr::null(), {
            let mut w = Writer::default();
            w.bytes(&source);
            w.0
        })
        .unwrap();
        source.fill(0);
        let handle = u64::from_le_bytes(created.try_into().unwrap()) as usize as *mut c_void;
        assert_eq!(invoke(3, handle, vec![]).unwrap(), 3u64.to_le_bytes());
        let mut w = Writer::default();
        w.u64(1);
        assert_eq!(invoke(5, handle, w.0).unwrap(), [2]);
        assert_eq!(
            invoke(2, std::ptr::null(), 0u64.to_le_bytes().to_vec()),
            Err("EmptyBuffer".into())
        );
        unsafe {
            sapflow_object_free(1, handle);
        }
    }

    #[test]
    fn malformed_calls_and_panics_do_not_cross_the_abi() {
        assert_eq!(invoke(0, std::ptr::null(), vec![]), Err("Argument".into()));
        assert_eq!(invoke(2, std::ptr::null(), vec![1]), Err("Argument".into()));
        assert_eq!(invoke(3, std::ptr::null(), vec![]), Err("Argument".into()));
        let mut out = SapflowBuffer::default();
        let status = unsafe {
            sapflow_call(
                1,
                std::ptr::null(),
                std::ptr::null(),
                1,
                &mut out,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, 1);
    }

    unsafe fn copied(buffer: SapflowBuffer) -> Vec<u8> {
        let bytes = if buffer.ptr.is_null() {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(buffer.ptr, buffer.len) }.to_vec()
        };
        sapflow_buffer_free(buffer);
        bytes
    }

    #[test]
    fn direct_media_results_cache_metadata_and_own_caller_bytes() {
        let mut encoded = [9, 1, 2, 3, 9];
        let mut handle = std::ptr::null_mut();
        let mut error = SapflowBuffer::default();
        assert_eq!(
            unsafe {
                sapflow_encoded_buffer_new(encoded[1..4].as_ptr(), 3, &mut handle, &mut error)
            },
            0
        );
        encoded.fill(0);
        let mut value = 0;
        assert_eq!(
            unsafe { sapflow_encoded_buffer_scalar(handle, 5, 1, &mut value, &mut error) },
            0
        );
        assert_eq!(value, 2);
        unsafe { sapflow_object_free(1, handle) };

        let codec = Arc::into_raw(MediaCodec::aac(&[0x11, 0x88]).unwrap()) as *const c_void;
        let mut payload = [9, 1, 2, 3, 9];
        let mut created: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                sapflow_media_codec_sample(
                    codec,
                    payload[1..4].as_ptr(),
                    3,
                    12,
                    12,
                    1024,
                    48_000,
                    false,
                    &mut created,
                    &mut error,
                )
            },
            0
        );
        payload.fill(0);
        let mut metadata = SapflowMediaSampleResult::default();
        assert_eq!(
            unsafe { sapflow_media_sample_metadata(created, &mut metadata, &mut error) },
            0
        );
        assert_eq!(metadata.len, 3);
        assert_eq!((metadata.pts, metadata.dts), (12, 12));
        assert_eq!((metadata.duration, metadata.timescale), (1024, 48_000));
        assert!(!metadata.is_video && !metadata.is_keyframe);
        let sample = unsafe { object::<MediaSample>(created) }.unwrap();
        assert_eq!(sample.sample.payload().as_ref(), &[1, 2, 3]);

        let fanout = Arc::into_raw(NativeSampleFanout::new(1024, 1024, 1000, false).unwrap())
            as *const c_void;
        let mut outcome = SapflowFanoutOutcome::default();
        assert_eq!(
            unsafe { sapflow_sample_fanout_push(fanout, created, 0, &mut outcome, &mut error) },
            0
        );
        let mut popped = SapflowMediaSampleResult::default();
        assert_eq!(
            unsafe { sapflow_sample_fanout_pop(fanout, false, 1, &mut popped, &mut error) },
            0
        );
        assert!(!popped.handle.is_null());
        assert_eq!(popped.len, 3);
        assert_eq!((popped.pts, popped.dts), (12, 12));
        unsafe {
            sapflow_object_free(3, popped.handle);
            sapflow_object_free(3, created);
            sapflow_object_free(5, fanout as *mut c_void);
            sapflow_object_free(2, codec as *mut c_void);
        }
    }

    #[test]
    fn direct_recording_outputs_move_bytes_and_preserve_optional_checkpoints() {
        let sample = MediaCodec::aac(&[0x11, 0x88])
            .unwrap()
            .sample(&[1, 2, 3], 0, 0, 1024, 48_000, false)
            .unwrap();
        let sample = Arc::into_raw(sample) as *const c_void;
        let ordinary = NativeRecordingMuxer::new(false, true, 0, 4096, 100).unwrap();
        let generic = NativeRecordingMuxer::new(false, true, 0, 4096, 100).unwrap();
        generic
            .push(unsafe { clone_object(sample) }.unwrap())
            .unwrap();
        let ordinary = Arc::into_raw(ordinary) as *const c_void;
        let generic = Arc::into_raw(generic) as *const c_void;
        let mut error = SapflowBuffer::default();
        assert_eq!(
            unsafe { sapflow_recording_muxer_push(ordinary, sample, &mut error) },
            0
        );
        let mut output = SapflowRecordingOutput::default();
        assert_eq!(
            unsafe { sapflow_recording_muxer_take_output(ordinary, &mut output, &mut error) },
            0
        );
        assert!(output.present && output.checkpoint.is_null());
        let direct = unsafe { copied(output.bytes) };
        let expected = invoke(58, generic, vec![]).unwrap();
        let mut reader = Reader::new(&expected);
        assert!(reader.bool().unwrap());
        assert_eq!(direct, reader.bytes().unwrap());
        assert!(!reader.bool().unwrap());
        reader.finish().unwrap();
        unsafe { sapflow_object_free(10, generic as *mut c_void) };
        assert_eq!(
            unsafe { sapflow_recording_muxer_take_output(ordinary, &mut output, &mut error) },
            0
        );
        assert!(!output.present);

        let native =
            NativeRecordingMuxer::with_native_aac_timing(false, true, 0, 4096, 100).unwrap();
        native.begin_native_audio(0).unwrap();
        let native = Arc::into_raw(native) as *const c_void;
        assert_eq!(
            unsafe { sapflow_recording_muxer_push(native, sample, &mut error) },
            0
        );
        assert_eq!(
            unsafe { sapflow_recording_muxer_take_native_output(native, &mut output, &mut error) },
            0
        );
        assert!(output.present && output.checkpoint.is_null());
        sapflow_buffer_free(output.bytes);
        assert_eq!(invoke(63, native, vec![1]).unwrap(), Vec::<u8>::new());
        let mut checkpoints = 0;
        loop {
            assert_eq!(
                unsafe {
                    sapflow_recording_muxer_take_native_output(native, &mut output, &mut error)
                },
                0
            );
            if !output.present {
                break;
            }
            sapflow_buffer_free(output.bytes);
            if !output.checkpoint.is_null() {
                checkpoints += 1;
                unsafe { sapflow_object_free(6, output.checkpoint) };
            }
        }
        assert_eq!(checkpoints, 1);
        unsafe {
            sapflow_object_free(10, ordinary as *mut c_void);
            sapflow_object_free(10, native as *mut c_void);
            sapflow_object_free(3, sample as *mut c_void);
        }
    }

    #[test]
    fn direct_protocol_scalars_and_owned_outputs_match_generic_calls() {
        let direct = Arc::into_raw(
            NativeRtmpPublisher::new(
                "live".into(),
                "key".into(),
                "rtmp://origin.invalid/live".into(),
                0,
                0,
            )
            .unwrap(),
        ) as *const c_void;
        let generic = Arc::into_raw(
            NativeRtmpPublisher::new(
                "live".into(),
                "key".into(),
                "rtmp://origin.invalid/live".into(),
                0,
                0,
            )
            .unwrap(),
        ) as *const c_void;
        let mut error = SapflowBuffer::default();
        let mut scalar = 0;
        assert_eq!(
            unsafe { sapflow_rtmp_publisher_queued_output_bytes(direct, &mut scalar, &mut error) },
            0
        );
        assert_eq!(invoke(77, generic, vec![]).unwrap(), scalar.to_le_bytes());
        assert_eq!(
            unsafe { sapflow_rtmp_publisher_state(direct, &mut scalar, &mut error) },
            0
        );
        assert_eq!(invoke(78, generic, vec![]).unwrap(), [scalar as u8]);
        let mut output = SapflowBuffer::default();
        assert_eq!(
            unsafe { sapflow_rtmp_publisher_take_output(direct, &mut output, &mut error) },
            0
        );
        assert_eq!(
            unsafe { copied(output) },
            invoke(76, generic, vec![]).unwrap()
        );
        unsafe {
            sapflow_object_free(11, direct as *mut c_void);
            sapflow_object_free(11, generic as *mut c_void);
        }

        let config = || NativeSrtConfig {
            destination: "127.0.0.1:9000".into(),
            stream_id: "publish:test".into(),
            ..NativeSrtConfig::default()
        };
        let direct = Arc::into_raw(
            NativeSrtPublisher::new(
                config(),
                None,
                Some(MediaCodec::aac(&[0x11, 0x88]).unwrap()),
                None,
                1_000_000,
            )
            .unwrap(),
        ) as *const c_void;
        let generic = Arc::into_raw(
            NativeSrtPublisher::new(
                config(),
                None,
                Some(MediaCodec::aac(&[0x11, 0x88]).unwrap()),
                None,
                1_000_000,
            )
            .unwrap(),
        ) as *const c_void;
        let mut datagram = SapflowSrtDatagram::default();
        assert_eq!(
            unsafe {
                sapflow_srt_publisher_poll_datagram(direct, 1_000_000, &mut datagram, &mut error)
            },
            0
        );
        assert!(datagram.present);
        assert_eq!(unsafe { copied(datagram.destination) }, b"127.0.0.1:9000");
        let direct_bytes = unsafe { copied(datagram.bytes) };
        let expected = invoke(103, generic, 1_000_000u64.to_le_bytes().to_vec()).unwrap();
        let mut reader = Reader::new(&expected);
        assert!(reader.bool().unwrap());
        assert_eq!(reader.string().unwrap(), "127.0.0.1:9000");
        assert_eq!(direct_bytes.len(), reader.bytes().unwrap().len());
        assert_eq!(&direct_bytes[..4], &[128, 0, 0, 0]);
        reader.finish().unwrap();
        for len in [1316, 1500] {
            let bytes = vec![0; len];
            assert_eq!(
                unsafe {
                    sapflow_srt_publisher_feed(
                        direct,
                        b"127.0.0.1:9001".as_ptr(),
                        14,
                        bytes.as_ptr(),
                        len,
                        1_000_000,
                        &mut error,
                    )
                },
                0
            );
        }
        assert_eq!(
            unsafe { object::<NativeSrtPublisher>(direct) }
                .unwrap()
                .stats()
                .unwrap()
                .stray_datagrams,
            2
        );
        unsafe {
            sapflow_object_free(13, direct as *mut c_void);
            sapflow_object_free(13, generic as *mut c_void);
        }
    }

    #[test]
    fn media_and_fanout_parity_through_c_dispatcher() {
        let mut w = Writer::default();
        w.bytes(&[0x11, 0x88]);
        let codec = invoke(7, std::ptr::null(), w.0).unwrap();
        let codec = u64::from_le_bytes(codec.try_into().unwrap()) as usize as *mut c_void;
        let mut w = Writer::default();
        w.bytes(&[1, 2]);
        w.i64(0);
        w.i64(0);
        w.u32(1024);
        w.u32(48_000);
        w.bool(false);
        let sample = invoke(8, codec, w.0).unwrap();
        let sample_ptr =
            u64::from_le_bytes(sample.try_into().unwrap()) as usize as *const MediaSample;
        let mut w = Writer::default();
        w.u64(1024);
        w.u64(1024);
        w.u32(1000);
        w.bool(false);
        let fanout = invoke(22, std::ptr::null(), w.0).unwrap();
        let fanout = u64::from_le_bytes(fanout.try_into().unwrap()) as usize as *mut c_void;
        let mut w = Writer::default();
        w.u64(sample_ptr as usize as u64);
        w.u64(0);
        let outcome = invoke(25, fanout, w.0).unwrap();
        assert_eq!(&outcome[..3], &[1, 1, 0]);
        let mut w = Writer::default();
        w.u64(1);
        let popped = invoke(26, fanout, w.0).unwrap();
        assert_eq!(popped[0], 1);
        unsafe {
            sapflow_object_free(
                3,
                popped[1..9].as_ptr().cast::<u64>().read_unaligned() as usize as *mut c_void,
            );
            sapflow_object_free(3, sample_ptr as *mut c_void);
            sapflow_object_free(5, fanout);
            sapflow_object_free(2, codec);
        }
    }
}
