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

pub fn epoch_init_name(prefix: &str, epoch: u64) -> String {
    if epoch == 0 {
        init_name(prefix)
    } else {
        format!("{prefix}_init{epoch}.mp4")
    }
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
    InitEpoch(u64),
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
    if let Some(n) = rest.strip_prefix("init") {
        return parse(n).map(Chunk::InitEpoch);
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
         #EXT-X-DISCONTINUITY-SEQUENCE:{}\n#EXT-X-MAP:URI=\"{}\"\n",
        m.target_duration(),
        // Apple HLS authoring specification 14.3 requires at least three
        // Part Target Durations, stronger than the HLS draft's two-target floor.
        secs(part_target * 3),
        secs(part_target),
        m.media_sequence(),
        m.discontinuity_sequence(),
        epoch_init_name(
            prefix,
            m.entries()
                .iter()
                .find_map(|entry| match entry {
                    Entry::Segment(segment) => Some(segment.initialization.id),
                    Entry::Gap { .. } => None,
                })
                .unwrap_or(m.current_initialization_id())
        ),
    );

    let mut initialization = m
        .entries()
        .iter()
        .find_map(|entry| match entry {
            Entry::Segment(segment) => Some(segment.initialization.id),
            Entry::Gap { .. } => None,
        })
        .unwrap_or(m.current_initialization_id());

    let n = m.entries().len();
    // RFC 8216bis 6.2.2 uses media duration, not a count of parents, for
    // recent part tags. Keep whole parent part sets while their end lies
    // within three target durations of the published edge (inclusive).
    // Count published open-part spans too, including any decode-time gaps.
    let part_window = Duration::from_secs(m.target_duration().saturating_mul(3));
    let mut age = m
        .open_parts()
        .iter()
        .fold(Duration::ZERO, |sum, part| sum.saturating_add(part.span));
    let mut parts_from = n;
    for (i, entry) in m.entries().iter().enumerate().rev() {
        if age > part_window {
            break;
        }
        parts_from = i;
        age = age.saturating_add(entry.duration());
    }
    for (i, entry) in m.entries().iter().enumerate() {
        match entry {
            Entry::Gap { duration } => {
                let _ = write!(s, "#EXT-X-GAP\n#EXTINF:{},\ngap.mp4\n", secs(*duration));
            }
            Entry::Segment(seg) => {
                // Before the segment's date and parts, never among them.
                if seg.discontinuity {
                    s.push_str("#EXT-X-DISCONTINUITY\n");
                }
                if initialization != seg.initialization.id {
                    let _ = writeln!(
                        s,
                        "#EXT-X-MAP:URI=\"{}\"",
                        epoch_init_name(prefix, seg.initialization.id)
                    );
                    initialization = seg.initialization.id;
                }
                // Preserve the existing PDT policy here; correcting its
                // arrival-time mapping is a separate change.
                if n - i <= 2 {
                    let _ = writeln!(s, "#EXT-X-PROGRAM-DATE-TIME:{}", rfc3339_millis(seg.start));
                }
                if i >= parts_from {
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
    if m.open_discontinuity() && !m.open_parts().is_empty() {
        s.push_str("#EXT-X-DISCONTINUITY\n");
    }
    if let Some(part) = m.open_parts().first()
        && initialization != part.initialization.id
    {
        let _ = writeln!(
            s,
            "#EXT-X-MAP:URI=\"{}\"",
            epoch_init_name(prefix, part.initialization.id)
        );
        initialization = part.initialization.id;
    }
    for p in m.open_parts() {
        write_part(&mut s, prefix, p.id, p.duration, p.independent);
    }
    // RFC 8216bis 6.2.5: a hinted part may cross a discontinuity/MAP.
    // Hint its known initialization too; the binding MAP is emitted only
    // when a real part is present, never for an empty codec epoch.
    if initialization != m.current_initialization_id() {
        let _ = writeln!(
            s,
            "#EXT-X-PRELOAD-HINT:TYPE=MAP,URI=\"{}\"",
            epoch_init_name(prefix, m.current_initialization_id())
        );
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

/// Checks the renders of one stream's media playlist against the rules a
/// client holds them to (RFC 8216bis 4.4.3.1, 4.4.3.3, 4.4.3.7, 4.4.4.9):
///
/// - `EXT-X-TARGETDURATION` and `PART-TARGET` never change;
/// - every `EXTINF`, rounded to the nearest second, is at most the target;
/// - no part is longer than `PART-TARGET`, and every part but the last of
///   its segment is at least 85% of it (the last part after the final
///   `EXTINF` is exempt: it may yet be the last of its segment);
/// - an `EXT-X-DISCONTINUITY` sits before a segment's parts, never among
///   them, and every media sequence number keeps one discontinuity
///   sequence number from render to render.
///
/// Durations are compared as the printed decimals, as a client reads them.
/// For tests and fuzzing; `scripts/check-playlist.py` is the same check.
#[derive(Debug, Default)]
pub struct RuleCheck {
    part_target: Option<u64>,
    target: Option<u64>,
    discontinuity: std::collections::BTreeMap<u64, u64>,
    media_identity: std::collections::BTreeMap<u64, (String, String)>,
    first_msn: Option<u64>,
}

/// A decimal with up to 5 places, in units of 0.00001.
fn fixed5(v: &str) -> Result<u64, String> {
    let (whole, frac) = v.split_once('.').unwrap_or((v, ""));
    if frac.len() > 5 {
        return Err(format!("more than 5 decimals: {v}"));
    }
    format!("{whole}{frac:0<5}")
        .parse()
        .map_err(|_| format!("bad decimal {v}"))
}

impl RuleCheck {
    pub fn check(&mut self, pl: &str) -> Result<(), String> {
        let tag = |name: &str| pl.lines().find_map(|l| l.strip_prefix(name));
        let part_target = fixed5(tag("#EXT-X-PART-INF:PART-TARGET=").ok_or("no PART-TARGET")?)?;
        let target: u64 = tag("#EXT-X-TARGETDURATION:")
            .and_then(|v| v.parse().ok())
            .ok_or("no TARGETDURATION")?;
        let mut msn: u64 = tag("#EXT-X-MEDIA-SEQUENCE:")
            .and_then(|v| v.parse().ok())
            .ok_or("no MEDIA-SEQUENCE")?;
        if self.first_msn.is_some_and(|previous| msn < previous) {
            return Err("MEDIA-SEQUENCE decreased".into());
        }
        self.first_msn = Some(msn);
        let mut dseq: u64 = match tag("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            Some(v) => v.parse().map_err(|_| "bad DISCONTINUITY-SEQUENCE")?,
            None => 0,
        };
        if *self.part_target.get_or_insert(part_target) != part_target {
            return Err(format!("PART-TARGET changed to {part_target}"));
        }
        if *self.target.get_or_insert(target) != target {
            return Err(format!("TARGETDURATION changed to {target}"));
        }
        let parts = |run: &[u64], open: bool| -> Result<(), String> {
            for (i, &d) in run.iter().enumerate() {
                if d > part_target {
                    return Err(format!("part {d} longer than PART-TARGET {part_target}"));
                }
                let last = i + 1 == run.len();
                if !last && d * 100 < part_target * 85 {
                    let what = if open { "open segment" } else { "segment" };
                    return Err(format!("part {d} under 85% of {part_target} ({what})"));
                }
            }
            Ok(())
        };
        let mut run = Vec::new();
        let mut map = String::new();
        let mut pending_uri = None;
        for l in pl.lines() {
            if let Some(value) = l.strip_prefix("#EXT-X-MAP:") {
                map = value.to_owned();
            }
            if !l.is_empty()
                && !l.starts_with('#')
                && let Some(id) = pending_uri.take()
            {
                let identity = (l.to_owned(), map.clone());
                if self
                    .media_identity
                    .entry(id)
                    .or_insert_with(|| identity.clone())
                    != &identity
                {
                    return Err(format!(
                        "media sequence mismatch {id}: URI or initialization changed"
                    ));
                }
            }
            if let Some(rest) = l.strip_prefix("#EXT-X-PART:DURATION=") {
                run.push(fixed5(rest.split(',').next().unwrap_or_default())?);
            } else if l == "#EXT-X-DISCONTINUITY" {
                if !run.is_empty() {
                    return Err(format!("discontinuity among the parts of segment {msn}"));
                }
                dseq += 1;
            } else if let Some(rest) = l.strip_prefix("#EXTINF:") {
                parts(&run, false)?;
                run.clear();
                let d = fixed5(rest.trim_end_matches(','))?;
                // Rounded to the nearest second, halves up.
                if (d + 50_000) / 100_000 > target {
                    return Err(format!(
                        "segment {msn} lasts {d}, over TARGETDURATION {target}"
                    ));
                }
                if *self.discontinuity.entry(msn).or_insert(dseq) != dseq {
                    return Err(format!(
                        "segment {msn} changed discontinuity sequence to {dseq}"
                    ));
                }
                pending_uri = Some(msn);
                msn += 1;
            }
        }
        parts(&run, true)
    }
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
         #EXT-X-STREAM-INF:BANDWIDTH={peak},AVERAGE-BANDWIDTH={avg},CODECS=\"{codecs}\""
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
        assert!(pl.contains("#EXT-X-TARGETDURATION:2\n"));
        assert!(pl.contains("#EXT-X-DISCONTINUITY-SEQUENCE:0\n"));
        assert!(!pl.contains("#EXT-X-DISCONTINUITY\n"));
        assert!(pl.contains("#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=0.70200\n"));
        assert!(pl.contains("#EXT-X-PART-INF:PART-TARGET=0.23400\n"));
        assert!(pl.contains("#EXT-X-MEDIA-SEQUENCE:0\n"));
        assert!(pl.contains("#EXT-X-MAP:URI=\"p1_init.mp4\"\n"));
        assert_eq!(pl.matches("#EXT-X-GAP\n").count(), 7);
        assert!(pl.contains("#EXTINF:1.00000,\np1_seg7.mp4\n"));
        assert!(pl.contains("p1_seg9.mp4"));
        // All three completed segments remain within the six-second part
        // window, in addition to the published open part.
        assert!(pl.contains("URI=\"p1_part0.mp4\""));
        assert!(pl.contains("#EXT-X-PART:DURATION=0.20000,URI=\"p1_part5.mp4\",INDEPENDENT=YES\n"));
        assert!(
            pl.contains("#EXT-X-PART:DURATION=0.20000,URI=\"p1_part15.mp4\",INDEPENDENT=YES\n")
        );
        assert!(pl.ends_with("#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"p1_part16.mp4\"\n"));
        assert!(!pl.contains("CAN-SKIP-UNTIL"));
        assert_eq!(pl.matches("#EXT-X-PROGRAM-DATE-TIME:").count(), 2);
    }

    #[test]
    fn apple_part_hold_back_is_at_least_three_printed_part_targets() {
        // Apple HLS authoring specification 14.3 makes the three-target
        // minimum mandatory. Check the decimals actually seen by clients,
        // with different learned targets and after the live window rolls.
        for part_min_ms in [200, 350, 1000] {
            let config = MuxerConfig {
                part_min: Duration::from_millis(part_min_ms),
                ..MuxerConfig::default()
            };
            let mut m = Muxer::new(config, video(), Some(aac()));
            feed(&mut m, 0..97, 30);
            let mut initial = None;
            for end in [97, 30 * 30 + 7] {
                if end > 97 {
                    feed(&mut m, 97..end, 30);
                }
                let pl = media(&m, "p");
                let target = fixed5(
                    pl.lines()
                        .find_map(|line| line.strip_prefix("#EXT-X-PART-INF:PART-TARGET="))
                        .expect("PART-TARGET"),
                )
                .expect("decimal target");
                let hold_back = fixed5(
                    pl.lines()
                        .find_map(|line| line.strip_prefix("#EXT-X-SERVER-CONTROL:"))
                        .expect("SERVER-CONTROL")
                        .split(',')
                        .find_map(|attribute| attribute.strip_prefix("PART-HOLD-BACK="))
                        .expect("PART-HOLD-BACK"),
                )
                .expect("decimal hold-back");
                assert!(target > 0);
                assert!(
                    hold_back >= 3 * target,
                    "Apple 14.3: {hold_back} < 3 * {target}"
                );
                assert_eq!(
                    *initial.get_or_insert((target, hold_back)),
                    (target, hold_back)
                );
            }
        }
    }

    #[test]
    fn part_tag_history_follows_duration_and_the_published_open_edge() {
        // Ten completed parents. A one-second GOP has target 2 and a
        // six-second history; a three-second GOP has target 4 and twelve.
        // At the exact boundary keep the older parent's entire part set.
        for (gop, boundary_first, open_first, open_count, history_secs) in
            [(30, 15, 20, 31, 6), (90, 75, 90, 61, 12)]
        {
            let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
            let end = gop * 10;
            feed(&mut m, 0..end + 1, gop);
            assert!(m.open_parts().is_empty());
            let pl = media(&m, "p");
            let first = pl
                .lines()
                .find(|line| line.starts_with("#EXT-X-PART:"))
                .expect("part history");
            assert!(first.contains(&format!("URI=\"p_part{boundary_first}.mp4\"")));
            assert!(first.contains("INDEPENDENT=YES"));

            // Publishing one 200 ms open part advances the edge and evicts
            // the older parent's tags, without waiting for another segment.
            feed(&mut m, end + 1..end + 7, gop);
            assert_eq!(m.open_parts().len(), 1);
            let pl = media(&m, "p");
            let parts: Vec<_> = pl
                .lines()
                .filter(|line| line.starts_with("#EXT-X-PART:"))
                .collect();
            assert_eq!(parts.len(), open_count);
            assert!(parts[0].contains(&format!("URI=\"p_part{open_first}.mp4\"")));
            assert!(!pl.contains(&format!("URI=\"p_part{boundary_first}.mp4\"")));
            // The default resource window still serves the removed part
            // throughout the following three target durations.
            feed(&mut m, end + 7..end + 7 + history_secs * 30, gop);
            assert!(matches!(
                m.part(boundary_first),
                crate::PartLookup::Found(_)
            ));
        }
    }

    #[test]
    fn rule_check_catches_each_rule() {
        let head = |target: u64, msn: u64, dseq: u64| {
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-PART-INF:PART-TARGET=0.23400\n\
                 #EXT-X-MEDIA-SEQUENCE:{msn}\n#EXT-X-DISCONTINUITY-SEQUENCE:{dseq}\n"
            )
        };
        let part = |d: &str| format!("#EXT-X-PART:DURATION={d},URI=\"p\"\n");
        let seg = |d: &str| format!("#EXTINF:{d},\ns\n");
        let ok = format!(
            "{}{}{}{}#EXT-X-DISCONTINUITY\n{}{}{}",
            head(3, 10, 0),
            part("0.20000"),
            part("0.03300"),
            seg("3.49000"),
            part("0.19890"),
            seg("1.0"),
            part("0.01000"),
        );
        let mut c = RuleCheck::default();
        assert_eq!(c.check(&ok), Ok(()));
        // The same window a segment later: segment 11 is now first, its
        // discontinuity still tagged, the sequence unchanged.
        let rolled = format!(
            "{}#EXT-X-DISCONTINUITY\n{}{}",
            head(3, 11, 0),
            part("0.2"),
            seg("1.0")
        );
        assert_eq!(c.check(&rolled), Ok(()));
        // Once it leaves, the sequence must have counted it.
        let gone = format!("{}{}", head(3, 12, 0), seg("1.0"));
        assert_eq!(c.check(&gone), Ok(()));
        let wrong = format!("{}{}", head(3, 11, 0), seg("1.0"));
        assert!(
            c.check(&wrong).is_err(),
            "segment 11 lost its discontinuity"
        );
        let mut fresh = RuleCheck::default();
        for bad in [
            format!("{}{}", head(3, 0, 0), seg("3.50000")),
            format!("{}{}{}", head(3, 0, 0), part("0.23401"), seg("1")),
            format!(
                "{}{}{}{}",
                head(3, 0, 0),
                part("0.19889"),
                part("0.2"),
                seg("1")
            ),
            format!("{}{}{}", head(3, 0, 0), part("0.19889"), part("0.2")),
            format!(
                "{}{}#EXT-X-DISCONTINUITY\n{}{}",
                head(3, 0, 0),
                part("0.2"),
                part("0.2"),
                seg("1")
            ),
            "#EXTM3U\n".to_string(),
        ] {
            assert!(RuleCheck::default().check(&bad).is_err(), "{bad}");
        }
        assert_eq!(
            fresh.check(&format!("{}{}", head(3, 0, 0), seg("1"))),
            Ok(())
        );
        assert!(
            fresh
                .check(&format!("{}{}", head(4, 1, 0), seg("1")))
                .is_err()
        );
    }

    #[test]
    fn multivariant_names_codecs_and_resolution() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed(&mut m, 0..61, 30);
        let pl = multivariant(&m);
        assert!(pl.contains("CODECS=\"avc1.4d400d,mp4a.40.2\""));
        // A single live rendition can change aspect at a codec epoch. A fixed
        // master RESOLUTION would ask clients to retain that old rectangle.
        assert!(!pl.contains("RESOLUTION="));
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
