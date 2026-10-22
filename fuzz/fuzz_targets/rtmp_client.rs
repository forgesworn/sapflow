//! Hostile receiver bytes before and after a valid publication exchange.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_rtmp::client::{Publisher, PublisherConfig, PublisherState};
use sapflow_rtmp::{Connection, Event};

fuzz_target!(|data: &[u8]| {
    let Some((&head, input)) = data.split_first() else {
        return;
    };
    let mut config = PublisherConfig::new(
        "live".into(),
        "synthetic".into(),
        "rtmp://example.invalid/live".into(),
    );
    config.max_input_bytes = 64 * 1024;
    config.max_output_bytes = 64 * 1024;
    let Ok(mut client) = Publisher::new(config, u64::from(head), 0) else {
        return;
    };
    if head & 1 != 0 {
        let mut server = Connection::new(1);
        for _ in 0..8 {
            if server.feed(&client.take_output()).is_err() {
                return;
            }
            while let Ok(Some(event)) = server.poll() {
                if matches!(event, Event::Publish { .. }) {
                    server.accept_publish();
                }
            }
            if client.feed(&server.take_output(), 0).is_err() {
                return;
            }
            if client.state() == PublisherState::Publishing {
                break;
            }
        }
        if client.state() == PublisherState::Publishing {
            let _ = client.send_aac_config(&[0x11, 0x90]);
        }
    }
    for (i, part) in input.chunks(usize::from(head >> 1).max(1)).enumerate() {
        let now = i as u64 + 1;
        if client.tick(now).is_err() {
            break;
        }
        if client.state() == PublisherState::Publishing
            && client.send_aac(now as u32, part).is_err()
        {
            break;
        }
        if client.feed(part, now).is_err() {
            break;
        }
        // Exercise both prompt writes and an owned wire queue that ages while
        // hostile controls arrive. Draining must not hide the media deadline.
        if head & 2 == 0 {
            let _ = client.take_output();
        }
    }
    client.cancel();
    assert!(client.take_output().is_empty());
});
