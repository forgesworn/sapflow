//! Clock-driven worker checks. No sleeps, extra server threads or live services.
use super::*;
use crate::test_support::{Scratch, app};
use sapflow_core::fragment::FragmentQuota;
use sapflow_hls::{Muxer, MuxerConfig};
use std::net::TcpStream as StdStream;
use std::time::SystemTime;

fn fixture(timeouts: Timeouts) -> (Worker, StdStream, Scratch, usize) {
    let (app, scratch) = app(false);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("owned loopback fixture");
    listener
        .set_nonblocking(true)
        .expect("nonblocking fixture listener");
    let client = StdStream::connect(listener.local_addr().expect("fixture address"))
        .expect("fixture connect");
    let mut worker = Worker::new(app, listener, timeouts).expect("owned worker");
    // A successful client connect does not guarantee that a nonblocking
    // accept sees readiness on its first attempt, especially in parallel tests.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Events::with_capacity(4);
    loop {
        worker.accept();
        if !worker.conns.is_empty() {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "listener never became ready");
        worker
            .poll
            .poll(&mut events, Some(remaining))
            .expect("fixture listener readiness");
    }
    let id = *worker.conns.keys().next().expect("accepted connection");
    (worker, client, scratch, id)
}

fn charged_body() -> (Arc<FragmentQuota>, sapflow_hls::muxer::SegmentData, Vec<u8>) {
    let quota = Arc::new(FragmentQuota::new(16 * 1024));
    let audio = sapflow_hls::codec::AacConfig::from_audio_specific_config(&[0x11, 0x88])
        .expect("supported structural audio config");
    let mut muxer =
        Muxer::with_fragment_quota(MuxerConfig::default(), None, Some(audio), quota.clone())
            .expect("bounded fixture");
    for frame in 0..12 {
        muxer
            .push_audio_at(
                frame * 1024 * 90_000 / 48_000,
                Bytes::from_static(&[0x5a; 128]),
                SystemTime::UNIX_EPOCH,
            )
            .expect("structural generated payload");
    }
    let PartLookup::Found(part) = muxer.part(0) else {
        panic!("encoded fixture part")
    };
    let data =
        sapflow_hls::muxer::SegmentData::from_chunks(vec![part.data.clone(), part.data.clone()])
            .expect("shared body chunks");
    let expected = data
        .chunks()
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect();
    drop(part);
    drop(muxer);
    assert!(quota.used() > 0);
    (quota, data, expected)
}

struct LimitedWriter {
    limit: usize,
    bytes: Vec<u8>,
}
impl Write for LimitedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        panic!("scatter writes required")
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn write_vectored(&mut self, chunks: &[IoSlice<'_>]) -> io::Result<usize> {
        let count = self.limit.min(chunks.iter().map(|chunk| chunk.len()).sum());
        let mut left = count;
        for chunk in chunks {
            let take = left.min(chunk.len());
            self.bytes.extend_from_slice(&chunk[..take]);
            left -= take;
        }
        Ok(count)
    }
}

#[test]
fn stalled_response_releases_its_payload_at_the_absolute_deadline() {
    let duration = Duration::from_millis(40);
    let (mut worker, _client, _scratch, id) = fixture(Timeouts {
        body: duration,
        idle: Duration::from_secs(3600),
        ..Timeouts::default()
    });
    let (quota, data, _) = charged_body();
    let now = Instant::now();
    let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
    conn.last_progress = now;
    respond(
        conn,
        Response::ok(data, "video/mp4", CHUNK_CACHE),
        false,
        now,
    );
    worker.sweep(now + duration - Duration::from_millis(1));
    assert!(worker.conns.contains_key(&id));
    assert!(quota.used() > 0);
    worker.sweep(now + duration);
    assert!(
        !worker.conns.contains_key(&id),
        "idle timeout cannot reclaim this stalled body yet"
    );
    assert_eq!(quota.used(), 0);
    assert_eq!(
        worker.slow_closes,
        (1, 0),
        "counted as a body-deadline close"
    );
}

#[test]
fn no_progress_close_is_counted_only_when_bytes_were_still_queued() {
    let idle = Duration::from_millis(40);
    for queued in [true, false] {
        let (mut worker, _client, _scratch, id) = fixture(Timeouts {
            idle,
            body: Duration::from_secs(3600),
            ..Timeouts::default()
        });
        let (_quota, data, _) = charged_body();
        let now = Instant::now();
        let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
        conn.last_progress = now;
        if queued {
            respond(
                conn,
                Response::ok(data, "video/mp4", CHUNK_CACHE),
                false,
                now,
            );
            assert!(conn.output.remaining() > 0);
        }
        worker.sweep(now + idle);
        assert!(
            !worker.conns.contains_key(&id),
            "idle close, queued={queued}"
        );
        // A keep-alive connection idling with nothing queued is not slow.
        let expected = if queued { (0, 1) } else { (0, 0) };
        assert_eq!(worker.slow_closes, expected, "queued={queued}");
    }
}

#[test]
fn trickling_progress_cannot_extend_the_body_deadline_or_change_accepted_bytes() {
    let duration = Duration::from_millis(40);
    let (mut worker, _client, _scratch, id) = fixture(Timeouts {
        body: duration,
        idle: Duration::from_millis(15),
        ..Timeouts::default()
    });
    let (quota, data, expected_body) = charged_body();
    let now = Instant::now();
    let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
    conn.last_progress = now;
    respond(
        conn,
        Response::ok(data, "video/mp4", CHUNK_CACHE),
        false,
        now,
    );
    let mut expected = conn.output.head.to_vec();
    expected.extend(expected_body);
    let mut writer = LimitedWriter {
        limit: conn.output.head.len() + 3,
        bytes: Vec::new(),
    };
    conn.output
        .write_to(&mut writer)
        .expect("header and body prefix");
    writer.limit = 1;
    for ms in [10, 20, 30, 39] {
        let conn = &mut worker.conns.get_mut(&id).expect("still alive").0;
        assert_eq!(
            conn.output
                .write_to(&mut writer)
                .expect("trickled body byte"),
            1
        );
        conn.last_progress = now + Duration::from_millis(ms);
        worker.sweep(now + Duration::from_millis(ms));
        assert!(worker.conns.contains_key(&id));
        assert!(quota.used() > 0);
    }
    assert_eq!(writer.bytes, expected[..writer.bytes.len()]);
    worker.sweep(now + duration);
    assert!(
        !worker.conns.contains_key(&id),
        "progress one millisecond ago cannot retain the body"
    );
    assert_eq!(quota.used(), 0);
    assert_eq!(
        writer.bytes,
        expected[..writer.bytes.len()],
        "closing must not append another response or corrupt accepted bytes"
    );
}

#[test]
fn completed_body_releases_bytes_and_keep_alive_response_gets_a_fresh_deadline() {
    let duration = Duration::from_millis(40);
    let (mut worker, _client, _scratch, id) = fixture(Timeouts {
        body: duration,
        idle: Duration::from_secs(3600),
        ..Timeouts::default()
    });
    let (quota, data, expected_body) = charged_body();
    let now = Instant::now();
    let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
    conn.last_progress = now;
    respond(
        conn,
        Response::ok(data, "video/mp4", CHUNK_CACHE),
        false,
        now,
    );
    let mut expected = conn.output.head.to_vec();
    expected.extend(expected_body);
    let mut writer = LimitedWriter {
        limit: usize::MAX,
        bytes: Vec::new(),
    };
    while !conn.output.is_empty() {
        conn.output.write_to(&mut writer).expect("normal response");
    }
    assert_eq!(writer.bytes, expected);
    assert_eq!(quota.used(), 0);
    worker.sweep(now + duration);
    assert!(worker.conns.contains_key(&id));
    let (quota, data, _) = charged_body();
    let next = now + duration;
    respond(
        &mut worker.conns.get_mut(&id).expect("reused connection").0,
        Response::ok(data, "video/mp4", CHUNK_CACHE),
        false,
        next,
    );
    worker.sweep(next + duration - Duration::from_millis(1));
    assert!(
        worker.conns.contains_key(&id),
        "deadline belongs to the new response"
    );
    worker.sweep(next + duration);
    assert_eq!(quota.used(), 0);
}

#[test]
fn flush_checks_the_absolute_clock_between_successful_writes() {
    let (mut worker, _client, _scratch, id) = fixture(Timeouts::default());
    let (quota, data, expected_body) = charged_body();
    let now = Instant::now();
    let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
    respond(
        conn,
        Response::ok(data, "video/mp4", CHUNK_CACHE),
        false,
        now,
    );
    let mut expected = conn.output.head.to_vec();
    expected.extend(expected_body);
    let mut writer = LimitedWriter {
        limit: 1,
        bytes: Vec::new(),
    };
    let mut ms = 0;
    let error = conn
        .output
        .flush_to(&mut writer, Duration::from_millis(7), || {
            let value = now + Duration::from_millis(ms);
            ms += 1;
            value
        })
        .expect_err("continuous successful writes must still expire");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(writer.bytes, expected[..writer.bytes.len()]);
    assert!(writer.bytes.len() < WRITES_PER_TURN);
    worker.close(id);
    assert_eq!(quota.used(), 0);
}

#[test]
fn a_continuously_writable_response_yields_and_resumes_without_losing_bytes() {
    let (mut worker, _client, _scratch, id) = fixture(Timeouts::default());
    let (quota, data, expected_body) = charged_body();
    let now = Instant::now();
    let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
    respond(
        conn,
        Response::ok(data, "video/mp4", CHUNK_CACHE),
        false,
        now,
    );
    let mut expected = conn.output.head.to_vec();
    expected.extend(expected_body);
    let mut writer = LimitedWriter {
        limit: 1,
        bytes: Vec::new(),
    };
    let (progress, again) = conn
        .output
        .flush_to(&mut writer, BODY_TIMEOUT, || now)
        .expect("finite write turn");
    assert_eq!(progress, Some(now));
    assert!(again);
    assert_eq!(writer.bytes.len(), WRITES_PER_TURN);
    assert!(quota.used() > 0);
    while !conn.output.is_empty() {
        conn.output
            .flush_to(&mut writer, BODY_TIMEOUT, || now)
            .expect("resume next turn");
    }
    assert_eq!(writer.bytes, expected);
    assert_eq!(quota.used(), 0);
}

#[test]
fn parked_waiting_is_excluded_and_head_releases_media_without_a_body_transfer() {
    let duration = Duration::from_millis(40);
    let (mut worker, _client, _scratch, id) = fixture(Timeouts {
        body: duration,
        idle: Duration::from_millis(15),
        ..Timeouts::default()
    });
    let now = Instant::now();
    let path = sapflow_core::StreamPath::parse("live/event-parked-deadline").expect("fixture path");
    let (stream, _, _) = worker
        .app
        .streams
        .claim(&path, MuxerConfig::default(), crate::stream::SourceKind::Ts)
        .expect("parked presentation");
    let seen = stream.version();
    let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
    conn.last_progress = now;
    conn.parked = Some(Parked {
        request: Request::Multivariant(stream),
        head_only: false,
        deadline: now + Duration::from_secs(1),
        seen,
    });
    worker.parked.insert(id);
    let ready = now + duration * 5;
    worker.sweep(ready);
    assert!(
        worker.conns.contains_key(&id),
        "ready-response deadline does not consume parked LL-HLS waiting"
    );
    let (quota, data, _) = charged_body();
    let conn = &mut worker.conns.get_mut(&id).expect("parked connection").0;
    conn.parked = None;
    worker.parked.remove(&id);
    respond(
        conn,
        Response::ok(data, "video/mp4", CHUNK_CACHE),
        false,
        ready,
    );
    conn.last_progress = ready;
    worker.sweep(ready + Duration::from_millis(10));
    assert!(worker.conns.contains_key(&id));
    assert!(quota.used() > 0);
    // A HEAD response owns only its header, despite declaring the media length.
    let (head_quota, head_data, expected_body) = charged_body();
    let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
    respond(
        conn,
        Response::ok(head_data, "video/mp4", CHUNK_CACHE),
        true,
        ready,
    );
    assert_eq!(
        quota.used(),
        0,
        "replaced test response releases its owners"
    );
    assert_eq!(
        head_quota.used(),
        0,
        "HEAD releases the unused media immediately"
    );
    let expected_head = conn.output.head.to_vec();
    assert!(
        String::from_utf8_lossy(&expected_head)
            .to_ascii_lowercase()
            .contains(&format!("content-length: {}", expected_body.len()))
    );
    let mut writer = LimitedWriter {
        limit: usize::MAX,
        bytes: Vec::new(),
    };
    conn.output
        .flush_to(&mut writer, duration, || ready)
        .expect("HEAD response");
    assert_eq!(writer.bytes, expected_head);
}

#[cfg(unix)]
#[test]
fn real_socket_backpressure_releases_the_backing_and_keeps_exact_framed_prefix() {
    let duration = Duration::from_secs(3600);
    let (mut worker, mut client, _scratch, id) = fixture(Timeouts {
        body: duration,
        idle: duration * 2,
        ..Timeouts::default()
    });
    rustix::net::sockopt::set_socket_recv_buffer_size(&client, 4096)
        .expect("small receiver window");
    let (quota, small, _) = charged_body();
    let chunk = small.chunks()[0].clone();
    let pattern = chunk.to_vec();
    let large = sapflow_hls::muxer::SegmentData::from_chunks(vec![chunk; 2048])
        .expect("large shared response");
    drop(small);
    let now = Instant::now();
    let conn = &mut worker.conns.get_mut(&id).expect("connection").0;
    rustix::net::sockopt::set_socket_send_buffer_size(&conn.sock, 4096).expect("small send queue");
    respond(
        conn,
        Response::ok(large, "video/mp4", CHUNK_CACHE),
        false,
        now,
    );
    let head = conn.output.head.to_vec();
    flush(conn, duration).expect("actual partial socket write");
    assert!(
        !conn.output.is_empty(),
        "a non-reading socket must retain an incomplete body"
    );
    assert!(quota.used() > 0);
    worker.sweep(now + duration);
    assert!(!worker.conns.contains_key(&id));
    assert_eq!(
        quota.used(),
        0,
        "kernel buffers do not retain the application backing"
    );
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("bounded readback");
    let mut received = Vec::new();
    client
        .read_to_end(&mut received)
        .expect("framed prefix then connection close");
    assert!(received.starts_with(&head));
    for (index, byte) in received[head.len()..].iter().enumerate() {
        assert_eq!(*byte, pattern[index % pattern.len()]);
    }
}
