//! Native codec configuration and compressed-sample framing validation.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_client::queue::{EnqueueOutcome, QueueLimits, SampleFanout};
use sapflow_client::{AacConfig, CodecConfig, EncodedSample, H264Config, SampleTiming};
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    if data.len() > 16 * 1024 {
        return;
    }
    let Some((&head, input)) = data.split_first() else {
        return;
    };
    let split = usize::from(head).min(input.len());
    let _ = AacConfig::new(input);
    if let Ok(config) = H264Config::new(&input[..split], &input[split..]) {
        if let Ok(timing) = SampleTiming::new(0, 0, 3000, 90_000) {
            let _ = EncodedSample::new(
                Arc::new(CodecConfig::H264(config)),
                timing,
                head & 1 != 0,
                Arc::from(input),
            );
        }
    }
    // A valid configuration is also necessary: finding an entire valid SPS/PPS
    // by mutation alone would leave hostile AVCC sample admission unreachable.
    let config = H264Config::new(
        &[
            0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0,
            3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
        ],
        &[0x68, 0xef, 0xbc, 0x80],
    )
    .expect("known native encoder configuration");
    let timing = SampleTiming::new(0, 0, 3000, 90_000).expect("known video clock");
    let _ = EncodedSample::new(
        Arc::new(CodecConfig::H264(config)),
        timing,
        head & 1 != 0,
        Arc::from(input),
    );
    if let Some(fields) = input.get(..24) {
        let _ = SampleTiming::new(
            i64::from_le_bytes(fields[0..8].try_into().expect("PTS bytes")),
            i64::from_le_bytes(fields[8..16].try_into().expect("DTS bytes")),
            u32::from_le_bytes(fields[16..20].try_into().expect("duration bytes")),
            u32::from_le_bytes(fields[20..24].try_into().expect("timescale bytes")),
        );
    }
    if let (Ok(config), Ok(timing)) = (
        AacConfig::new(&[0x11, 0x90]),
        SampleTiming::new(0, 0, 1024, 48_000),
    ) {
        let _ = EncodedSample::new(
            Arc::new(CodecConfig::Aac(config)),
            timing,
            head & 1 != 0,
            Arc::from(input),
        );
    }
    // Exercise terminal branch fences amid ordinary bounded queue operations.
    // Clearing, draining and later keyframes must never reopen an admission.
    let limits = QueueLimits::new(1024, 16, 1_000_000_000, 1_000_000_000)
        .expect("bounded native queue fixture");
    let mut queues = SampleFanout::new(limits, limits, false);
    let config = Arc::new(CodecConfig::Aac(
        AacConfig::new(&[0x11, 0x88]).expect("AAC"),
    ));
    for (index, operation) in input.iter().take(256).enumerate() {
        match operation % 5 {
            0 => queues.publishing().close_admission(),
            1 => queues.recording().close_admission(),
            2 => queues.clear(),
            3 => {
                let _ = queues.publishing().pop(index as u64);
                let _ = queues.recording().pop(index as u64);
            }
            _ => {
                let time = index as i64 * 1024;
                let sample = Arc::new(
                    EncodedSample::new(
                        config.clone(),
                        SampleTiming::new(time, time, 1024, 48_000).expect("bounded clock"),
                        false,
                        Arc::from([1]),
                    )
                    .expect("AAC sample"),
                );
                let (published, recorded) =
                    queues.push(sample, index as u64).expect("forward clock");
                if queues.publishing().admission_closed() {
                    assert_eq!(published, EnqueueOutcome::AdmissionClosed);
                }
                if queues.recording().admission_closed() {
                    assert_eq!(recorded, EnqueueOutcome::AdmissionClosed);
                }
            }
        }
    }
});
