//! Playlist text, in the shape gohlslib v2.4.0 writes it.
//!
//! One rendering per muxer state: every viewer gets the same bytes, with no
//! per-viewer session or query string in the URIs, so playlists and chunks
//! can be cached and shared.

use std::fmt::Write;
use std::time::Duration;

use crate::muxer::{Entry, Muxer};
use crate::time::rfc3339_millis;

pub const MEDIA_PLAYLIST: &str = "stream.m3u8";

pub fn init_name(prefix: &str) -> String {
    format!("{prefix}_init.mp4")
}

pub fn segment_name(prefix: &str, id: u64) -> String {
    format!("{prefix}_seg{id}.mp4")
}

pub fn part_name(prefix: &str, id: u64) -> String {
    format!("{prefix}_part{id}.mp4")
}

/// A chunk URL from a viewer, after checking it belongs to this muxer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chunk {
    Init,
    Segment(u64),
    Part(u64),
}

/// Parses `<prefix>_init.mp4`, `<prefix>_seg<N>.mp4` or `<prefix>_part<N>.mp4`.
pub fn parse_chunk(prefix: &str, name: &str) -> Option<Chunk> {
    let rest = name.strip_prefix(prefix)?.strip_prefix('_')?;
    let rest = rest.strip_suffix(".mp4")?;
    if rest == "init" {
        return Some(Chunk::Init);
    }
    let parse = |s: &str| -> Option<u64> {
        if s.is_empty() || s.len() > 19 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    if let Some(n) = rest.strip_prefix("seg") {
        return parse(n).map(Chunk::Segment);
    }
    if let Some(n) = rest.strip_prefix("part") {
        return parse(n).map(Chunk::Part);
    }
    None
}

/// Reads `_HLS_msn` and `_HLS_part` from a raw query string. Anything else
/// is ignored (`_HLS_skip` included: delta updates are not offered). A value
/// that is not a plain number is a bad request.
/// A blocking-reload parameter that is not a plain number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadQuery;

pub fn parse_blocking_query(query: Option<&str>) -> Result<(Option<u64>, Option<u64>), BadQuery> {
    let (mut msn, mut part) = (None, None);
    for pair in query.unwrap_or("").split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let target = match k {
            "_HLS_msn" => &mut msn,
            "_HLS_part" => &mut part,
            _ => continue,
        };
        if v.is_empty() || v.len() > 19 || !v.bytes().all(|b| b.is_ascii_digit()) {
            return Err(BadQuery);
        }
        *target = Some(v.parse().map_err(|_| BadQuery)?);
    }
    Ok((msn, part))
}

fn secs(d: Duration) -> String {
    format!("{:.5}", d.as_secs_f64())
}

/// The LL-HLS media playlist. Call only once `has_content()` is true.
pub fn media(m: &Muxer, prefix: &str) -> String {
    let mut s = String::with_capacity(4096);
    let part_target = m.part_target();
    let _ = write!(
        s,
        "#EXTM3U\n#EXT-X-VERSION:10\n#EXT-X-TARGETDURATION:{}\n\
         #EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK={}\n\
         #EXT-X-PART-INF:PART-TARGET={}\n#EXT-X-MEDIA-SEQUENCE:{}\n\
         #EXT-X-MAP:URI=\"{}\"\n",
        m.target_duration(),
        secs(part_target * 25 / 10),
        secs(part_target),
        m.media_sequence(),
        init_name(prefix),
    );

    let n = m.entries().len();
    for (i, entry) in m.entries().iter().enumerate() {
        match entry {
            Entry::Gap { duration } => {
                let _ = write!(s, "#EXT-X-GAP\n#EXTINF:{},\ngap.mp4\n", secs(*duration));
            }
            Entry::Segment(seg) => {
                let recent = n - i <= 2;
                if recent {
                    let _ = writeln!(s, "#EXT-X-PROGRAM-DATE-TIME:{}", rfc3339_millis(seg.start));
                    for p in &seg.parts {
                        write_part(&mut s, prefix, p.id, p.duration, p.independent);
                    }
                }
                let _ = write!(
                    s,
                    "#EXTINF:{},\n{}\n",
                    secs(seg.duration),
                    segment_name(prefix, seg.id)
                );
            }
        }
    }
    for p in m.open_parts() {
        write_part(&mut s, prefix, p.id, p.duration, p.independent);
    }
    // hls.js loops without a preload hint, so there is always one.
    let _ = writeln!(
        s,
        "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"{}\"",
        part_name(prefix, m.next_part_id())
    );
    s
}

fn write_part(s: &mut String, prefix: &str, id: u64, d: Duration, independent: bool) {
    let _ = write!(
        s,
        "#EXT-X-PART:DURATION={},URI=\"{}\"",
        secs(d),
        part_name(prefix, id)
    );
    if independent {
        s.push_str(",INDEPENDENT=YES");
    }
    s.push('\n');
}

/// The multivariant playlist pointing at the one media playlist.
pub fn multivariant(m: &Muxer) -> String {
    let (peak, avg) = m.bandwidth();
    let v = m.video();
    let mut codecs = v.codec_string();
    if let Some(a) = m.audio() {
        codecs.push(',');
        codecs.push_str(&a.codec_string());
    }
    let mut s = format!(
        "#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-INDEPENDENT-SEGMENTS\n\n\
         #EXT-X-STREAM-INF:BANDWIDTH={peak},AVERAGE-BANDWIDTH={avg},CODECS=\"{codecs}\",RESOLUTION={}x{}",
        v.width, v.height
    );
    if let Some(fps) = v.fps.filter(|f| f.is_finite() && *f > 0.0) {
        let _ = write!(s, ",FRAME-RATE={fps:.3}");
    }
    let _ = write!(s, "\n{MEDIA_PLAYLIST}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muxer::tests::{aac, feed, video};
    use crate::muxer::{Muxer, MuxerConfig};

    #[test]
    fn media_playlist_has_the_ios_shape() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed(&mut m, 0..(30 * 3 + 7), 30);
        let pl = media(&m, "p1");
        let lines: Vec<&str> = pl.lines().collect();
        assert_eq!(lines[0], "#EXTM3U");
        assert!(pl.contains("#EXT-X-TARGETDURATION:1\n"));
        assert!(pl.contains("#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=0.50000\n"));
        assert!(pl.contains("#EXT-X-PART-INF:PART-TARGET=0.20000\n"));
        assert!(pl.contains("#EXT-X-MEDIA-SEQUENCE:0\n"));
        assert!(pl.contains("#EXT-X-MAP:URI=\"p1_init.mp4\"\n"));
        assert_eq!(pl.matches("#EXT-X-GAP\n").count(), 7);
        assert!(pl.contains("#EXTINF:1.00000,\np1_seg7.mp4\n"));
        assert!(pl.contains("p1_seg9.mp4"));
        // Parts only for the last two segments and the open one.
        assert!(!pl.contains("URI=\"p1_part0.mp4\""));
        assert!(pl.contains("#EXT-X-PART:DURATION=0.20000,URI=\"p1_part5.mp4\",INDEPENDENT=YES\n"));
        assert!(
            pl.contains("#EXT-X-PART:DURATION=0.20000,URI=\"p1_part15.mp4\",INDEPENDENT=YES\n")
        );
        assert!(pl.ends_with("#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"p1_part16.mp4\"\n"));
        assert!(!pl.contains("CAN-SKIP-UNTIL"));
        assert_eq!(pl.matches("#EXT-X-PROGRAM-DATE-TIME:").count(), 2);
    }

    #[test]
    fn multivariant_names_codecs_and_resolution() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed(&mut m, 0..61, 30);
        let pl = multivariant(&m);
        assert!(pl.contains("CODECS=\"avc1.4d400d,mp4a.40.2\""));
        assert!(pl.contains("RESOLUTION="));
        assert!(pl.ends_with("\nstream.m3u8\n"));
    }

    #[test]
    fn blocking_query() {
        assert_eq!(parse_blocking_query(None), Ok((None, None)));
        assert_eq!(
            parse_blocking_query(Some("_HLS_msn=12&_HLS_part=3")),
            Ok((Some(12), Some(3)))
        );
        assert_eq!(
            parse_blocking_query(Some("_HLS_skip=YES&_HLS_msn=4")),
            Ok((Some(4), None))
        );
        assert_eq!(parse_blocking_query(Some("x=1")), Ok((None, None)));
        for bad in [
            "_HLS_msn=",
            "_HLS_msn=-1",
            "_HLS_part=a",
            "_HLS_msn=99999999999999999999",
        ] {
            assert_eq!(parse_blocking_query(Some(bad)), Err(BadQuery), "{bad}");
        }
    }

    #[test]
    fn chunk_names_round_trip_and_reject_strangers() {
        assert_eq!(parse_chunk("ab", &init_name("ab")), Some(Chunk::Init));
        assert_eq!(
            parse_chunk("ab", &segment_name("ab", 12)),
            Some(Chunk::Segment(12))
        );
        assert_eq!(parse_chunk("ab", &part_name("ab", 0)), Some(Chunk::Part(0)));
        for bad in [
            "cd_seg1.mp4",
            "ab_seg.mp4",
            "ab_seg-1.mp4",
            "ab_seg+1.mp4",
            "ab_part1.ts",
            "ab_seg99999999999999999999.mp4",
            "ab_init.mp4x",
            "abseg1.mp4",
        ] {
            assert_eq!(parse_chunk("ab", bad), None, "{bad}");
        }
    }
}
