//! Lab publisher using our own MPEG-TS writer and SRT caller, as the apps do.
//! Run: cargo run -p sapflow-client --example srt-publish-lab -- \
//!   127.0.0.1:8890 publish:live/event-a:user:pass video.h264 audio.aac 9000
//!
//! Inputs are encoder output files: H.264 Annex B with access unit
//! delimiters and no B-frames at 25 fps, and AAC-LC in ADTS. For example:
//!   ffmpeg -f lavfi -i testsrc2=size=640x360:rate=25 -t 20 -c:v libx264 \
//!     -bf 0 -g 25 -bsf:v h264_metadata=aud=insert video.h264
//!   ffmpeg -f lavfi -i sine=sample_rate=48000 -t 20 -c:a aac audio.aac
//! Samples are submitted in real time. After the given milliseconds the
//! publisher stops as the apps' Stop does: cancel at once, discarding unsent
//! data, then send one best-effort SHUTDOWN. LAB ONLY: plaintext stream ID.
use std::error::Error;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sapflow_client::media::{AacConfig, CodecConfig, EncodedSample, H264Config, SampleTiming};
use sapflow_client::mpegts::MpegTsConfig;
use sapflow_client::srt::{PublisherConfig, PublisherEvent, SrtPublisher};
use sapflow_srt::{CallerConfig, CallerEvent};

const FPS: i64 = 25;

fn nals(annex_b: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= annex_b.len() {
        if annex_b[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &s)| {
            let mut end = starts.get(n + 1).map_or(annex_b.len(), |&next| next - 3);
            while end > s && annex_b[end - 1] == 0 {
                end -= 1;
            }
            &annex_b[s..end]
        })
        .filter(|nal| !nal.is_empty())
        .collect()
}

/// (SPS, PPS, access units as AVCC with keyframe flags).
type Video = (Vec<u8>, Vec<u8>, Vec<(bool, Vec<u8>)>);

fn video(annex_b: &[u8]) -> Result<Video, Box<dyn Error>> {
    let (mut sps, mut pps) = (None, None);
    let mut units: Vec<(bool, Vec<u8>)> = Vec::new();
    for nal in nals(annex_b) {
        match nal[0] & 0x1f {
            7 => {
                sps.get_or_insert_with(|| nal.to_vec());
            }
            8 => {
                pps.get_or_insert_with(|| nal.to_vec());
            }
            kind => {
                if kind == 9 {
                    units.push((false, Vec::new()));
                }
                let Some((key, unit)) = units.last_mut() else {
                    return Err("stream must start with an access unit delimiter".into());
                };
                *key |= kind == 5;
                unit.extend((nal.len() as u32).to_be_bytes());
                unit.extend_from_slice(nal);
            }
        }
    }
    Ok((
        sps.ok_or("no SPS")?,
        pps.ok_or("no PPS")?,
        units.into_iter().filter(|(_, u)| u.len() > 6).collect(),
    ))
}

/// (AAC config, raw AAC frames without their ADTS headers).
type Audio = (AacConfig, Vec<Vec<u8>>);

fn audio(adts: &[u8]) -> Result<Audio, Box<dyn Error>> {
    let mut frames = Vec::new();
    let mut config = None;
    let mut at = 0;
    while at + 7 <= adts.len() {
        let h = &adts[at..];
        if h[0] != 0xff || h[1] & 0xf0 != 0xf0 {
            return Err("ADTS sync lost".into());
        }
        let header = if h[1] & 1 == 1 { 7 } else { 9 };
        let length =
            (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
        if length <= header || at + length > adts.len() {
            break;
        }
        if config.is_none() {
            let object = u16::from((h[2] >> 6) + 1);
            let rate = u16::from((h[2] >> 2) & 0xf);
            let channels = u16::from(((h[2] & 1) << 2) | (h[3] >> 6));
            let asc = (object << 11) | (rate << 7) | (channels << 3);
            config = Some(AacConfig::new(&asc.to_be_bytes())?);
        }
        frames.push(adts[at + header..at + length].to_vec());
        at += length;
    }
    Ok((config.ok_or("no ADTS frames")?, frames))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [destination, stream_id, video_path, audio_path, stop_ms] = &args[..] else {
        return Err(
            "usage: srt-publish-lab <ip:port> <stream-id> <video.h264> <audio.aac> <stop-ms>"
                .into(),
        );
    };
    let destination: SocketAddr = destination.parse()?;
    let stop_after = Duration::from_millis(stop_ms.parse()?);
    let (sps, pps, units) = video(&std::fs::read(video_path)?)?;
    let (aac, frames) = audio(&std::fs::read(audio_path)?)?;
    let h264 = H264Config::new(&sps, &pps)?;
    let video_codec = Arc::new(CodecConfig::H264(h264.clone()));
    let audio_codec = Arc::new(CodecConfig::Aac(aac));
    let rate = i64::from(aac.sample_rate());
    let mut samples = Vec::new();
    for (i, (key, unit)) in units.into_iter().enumerate() {
        let ticks = i as i64 * 90_000 / FPS;
        let timing = SampleTiming::new(ticks, ticks, (90_000 / FPS) as u32, 90_000)?;
        samples.push(Arc::new(EncodedSample::new(
            video_codec.clone(),
            timing,
            key,
            unit.into(),
        )?));
    }
    for (j, frame) in frames.into_iter().enumerate() {
        let ticks = j as i64 * 1024;
        let timing = SampleTiming::new(ticks, ticks, 1024, rate as u32)?;
        samples.push(Arc::new(EncodedSample::new(
            audio_codec.clone(),
            timing,
            false,
            frame.into(),
        )?));
    }
    samples.sort_by_key(|s| s.timing().decode_time_ns());
    let config = PublisherConfig {
        mux: MpegTsConfig {
            video: Some(h264),
            audio: Some(aac),
            ..MpegTsConfig::default()
        },
        caller: CallerConfig {
            stream_id: stream_id.clone(),
            ..CallerConfig::default()
        },
        ..PublisherConfig::default()
    };
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut publisher = SrtPublisher::new(config, destination, Instant::now())?;
    let mut buffer = [0u8; 2048];
    let mut next = 0;
    let mut started: Option<Instant> = None;
    let give_up = Instant::now() + stop_after + Duration::from_secs(10);
    loop {
        let now = Instant::now();
        if now >= give_up {
            return Err("lab publisher exceeded its run budget".into());
        }
        publisher.handle_timeout(now);
        while let Some(event) = publisher.poll_event() {
            match event {
                PublisherEvent::Transport(CallerEvent::Connected { .. }) => {
                    started.get_or_insert(now);
                }
                PublisherEvent::Transport(CallerEvent::Closed { reason }) => {
                    return Err(format!("SRT closed: {reason:?}").into());
                }
                PublisherEvent::Failed(error) => return Err(format!("{error:?}").into()),
                _ => {}
            }
        }
        if let Some(t0) = started {
            if now.saturating_duration_since(t0) >= stop_after || next == samples.len() {
                // The apps' Stop: immediate cancel, then one SHUTDOWN datagram.
                let buffered = publisher.stats().transport.send.buffered_packets;
                publisher.stop(now);
                if let Some((peer, bytes)) = publisher.poll_transmit(now) {
                    socket.send_to(&bytes, peer)?;
                }
                println!("stopped samples_sent={next} buffered_packets_discarded={buffered}");
                return Ok(());
            }
            while next < samples.len()
                && publisher.is_ready()
                && t0 + Duration::from_nanos(samples[next].timing().decode_time_ns()) <= now
            {
                if publisher.push(now, samples[next].clone()).is_err() {
                    break;
                }
                next += 1;
            }
        }
        while let Some((peer, bytes)) = publisher.poll_transmit(Instant::now()) {
            socket.send_to(&bytes, peer)?;
        }
        let due = started
            .zip(samples.get(next))
            .map(|(t0, s)| t0 + Duration::from_nanos(s.timing().decode_time_ns()));
        let wake = [publisher.poll_timeout(), due]
            .into_iter()
            .flatten()
            .min()
            .map_or(Duration::from_millis(5), |at| {
                at.saturating_duration_since(Instant::now())
            })
            .clamp(Duration::from_micros(200), Duration::from_millis(5));
        socket.set_read_timeout(Some(wake))?;
        if let Ok((len, peer)) = socket.recv_from(&mut buffer) {
            publisher.handle_datagram(Instant::now(), peer, &buffer[..len]);
        }
    }
}
