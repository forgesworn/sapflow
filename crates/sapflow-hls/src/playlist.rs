//! Playlist text, in the shape gohlslib v2.4.0 writes it.
//!
//! One rendering per muxer state: every viewer gets the same bytes, with no
//! per-viewer session or query string in the URIs, so playlists and chunks
//! can be cached and shared.

use std::fmt::Write;
use std::time::Duration;

use crate::muxer::{Entry, Muxer};
use crate::time::rfc3339_millis;

/// The media playlist's URI, relative to the multivariant playlist.
pub const MEDIA_PLAYLIST: &str = "stream.m3u8";

/// `<prefix>_init.mp4`: the init segment of codec epoch 0.
pub fn init_name(prefix: &str) -> String {
    format!("{prefix}_init.mp4")
}

/// The init segment's name for codec `epoch`: `<prefix>_init.mp4` for 0,
/// otherwise `<prefix>_init<epoch>.mp4`.
pub fn epoch_init_name(prefix: &str, epoch: u64) -> String {
    if epoch == 0 {
        init_name(prefix)
    } else {
        format!("{prefix}_init{epoch}.mp4")
    }
}

/// `<prefix>_seg<id>.mp4`.
pub fn segment_name(prefix: &str, id: u64) -> String {
    format!("{prefix}_seg{id}.mp4")
}

/// `<prefix>_part<id>.mp4`.
pub fn part_name(prefix: &str, id: u64) -> String {
    format!("{prefix}_part{id}.mp4")
}

/// A chunk URL from a viewer, after checking it belongs to this muxer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chunk {
    /// `<prefix>_init.mp4`: the init segment of codec epoch 0.
    Init,
    /// `<prefix>_init<N>.mp4`: the init segment of codec epoch N.
    InitEpoch(u64),
    /// `<prefix>_seg<N>.mp4`: the segment with id N.
    Segment(u64),
    /// `<prefix>_part<N>.mp4`: the part with id N.
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

/// A blocking-reload parameter that is not a plain number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadQuery;

/// Reads `_HLS_msn` and `_HLS_part` from a raw query string, in that order.
/// Anything else is ignored (`_HLS_skip` included: delta updates are not
/// offered). A value that is not a plain number is a bad request.
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

/// A duration as `EXTINF` prints it, in units of 10 µs, so the muxer's
/// window rule counts exactly what a client adds up.
pub(crate) fn extinf_units(d: Duration) -> u64 {
    fixed5(&secs(d)).unwrap_or(u64::MAX)
}

/// The LL-HLS media playlist. Call only once `has_content()` is true.
pub fn media(m: &Muxer, prefix: &str) -> String {
    media_with_mode(m, prefix, true)
}

/// Complete segments remain available when a deployment disables part delivery.
/// The segment timeline, gaps and codec-epoch maps are shared with LL-HLS.
pub fn media_with_mode(m: &Muxer, prefix: &str, low_latency: bool) -> String {
    let mut s = String::with_capacity(4096);
    let part_target = m.part_target();
    let _ = write!(
        s,
        "#EXTM3U\n#EXT-X-VERSION:10\n#EXT-X-TARGETDURATION:{}\n",
        m.target_duration(),
    );
    if low_latency {
        let _ = write!(
            s,
            "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK={}\n\
             #EXT-X-PART-INF:PART-TARGET={}\n",
            // Apple HLS authoring specification 14.3 requires at least three
            // Part Target Durations, stronger than the HLS draft's two-target floor.
            secs(part_target * 3),
            secs(part_target),
        );
    }
    let _ = write!(
        s,
        "#EXT-X-MEDIA-SEQUENCE:{}\n#EXT-X-DISCONTINUITY-SEQUENCE:{}\n#EXT-X-MAP:URI=\"{}\"\n",
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
    // Part tags for the segments whose end lies within three target
    // durations of the published edge (RFC 8216bis 6.2.2), decided in one
    // place with the muxer's part retention.
    let parts_from = m.part_tags_from();
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
                if low_latency && i >= parts_from {
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
    if !low_latency {
        return s;
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
/// client holds them to (RFC 8216bis 4.4.3.1, 4.4.3.3, 4.4.3.7, 4.4.4.9,
/// 6.2.2):
///
/// - `EXT-X-TARGETDURATION` and `PART-TARGET` never change;
/// - every `EXTINF`, rounded to the nearest second, is at most the target;
/// - no part is longer than `PART-TARGET`, and every part but the last of
///   its segment is at least 85% of it (the last part after the final
///   `EXTINF` is exempt: it may yet be the last of its segment). 4.4.4.9
///   also exempts `INDEPENDENT=YES` parts; this check allows that only in a
///   segment (or the open run of parts) whose every part is independent, as
///   in audio-only, and holds video to the stricter rule Apple's players
///   were built against;
/// - once segments have been removed (`EXT-X-MEDIA-SEQUENCE` above 0), the
///   segments listed last at least three target durations (6.2.2's segment
///   rule), unless the render lists [`crate::muxer::MAX_WINDOW_ENTRIES`]
///   entries or more, the muxer's entry cap, or the caller says the muxer's
///   byte cap cut it ([`RuleCheck::check_with_byte_cap`],
///   [`crate::Muxer::window_byte_capped`]);
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
    /// Test-only: leave out the live-window rule, for synthetic renders
    /// about the other rules.
    no_live_window: bool,
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
    /// Without the live-window rule, for synthetic windows about the others.
    #[cfg(test)]
    fn without_live_window() -> Self {
        Self {
            no_live_window: true,
            ..Self::default()
        }
    }

    /// Checks one more render, returning a description of the first rule it
    /// breaks. Rules that span renders (the targets, and what each media
    /// sequence number points at) are held against the renders seen by earlier
    /// calls.
    pub fn check(&mut self, pl: &str) -> Result<(), String> {
        self.check_with_byte_cap(pl, false)
    }

    /// [`RuleCheck::check`] for a render whose muxer says its byte cap cut
    /// the window short ([`crate::Muxer::window_byte_capped`]), which the
    /// render cannot show: then a window under three target durations is
    /// accepted.
    pub fn check_with_byte_cap(&mut self, pl: &str, byte_capped: bool) -> Result<(), String> {
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
        let first_msn = msn;
        let mut listed = 0u64;
        let mut entries = 0usize;
        let parts = |run: &[(u64, bool)], open: bool| -> Result<(), String> {
            // Decided per segment, so an audio-only epoch after an A/V one
            // keeps its exemption while the A/V parts are still listed.
            let audio_only = run.iter().all(|&(_, independent)| independent);
            for (i, &(d, independent)) in run.iter().enumerate() {
                if d > part_target {
                    return Err(format!("part {d} longer than PART-TARGET {part_target}"));
                }
                let last = i + 1 == run.len();
                let exempt = last || (independent && audio_only);
                if !exempt && d * 100 < part_target * 85 {
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
                run.push((
                    fixed5(rest.split(',').next().unwrap_or_default())?,
                    rest.split(',').any(|a| a == "INDEPENDENT=YES"),
                ));
            } else if l == "#EXT-X-DISCONTINUITY" {
                if !run.is_empty() {
                    return Err(format!("discontinuity among the parts of segment {msn}"));
                }
                dseq += 1;
            } else if let Some(rest) = l.strip_prefix("#EXTINF:") {
                parts(&run, false)?;
                run.clear();
                let d = fixed5(rest.trim_end_matches(','))?;
                listed += d;
                entries += 1;
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
        if !self.no_live_window
            && !byte_capped
            && first_msn > 0
            && listed < target * 3 * 100_000
            && entries < crate::muxer::MAX_WINDOW_ENTRIES
        {
            return Err(format!(
                "segments removed down to {listed}, under three TARGETDURATIONs of {target}"
            ));
        }
        parts(&run, true)
    }
}

/// The multivariant playlist pointing at the one media playlist.
pub fn multivariant(m: &Muxer) -> String {
    let (peak, avg) = m.bandwidth();
    let mut codecs = m.video().map_or_else(String::new, |v| v.codec_string());
    if let Some(a) = m.audio_config() {
        if !codecs.is_empty() {
            codecs.push(',');
        }
        codecs.push_str(&a.codec_string());
    }
    let mut s = format!(
        "#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-INDEPENDENT-SEGMENTS\n\n\
         #EXT-X-STREAM-INF:BANDWIDTH={peak},AVERAGE-BANDWIDTH={avg},CODECS=\"{codecs}\""
    );
    if let Some(fps) = m
        .video()
        .and_then(|v| v.fps)
        .filter(|f| f.is_finite() && *f > 0.0)
    {
        let _ = write!(s, ",FRAME-RATE={fps:.3}");
    }
    let _ = write!(s, "\n{MEDIA_PLAYLIST}\n");
    s
}

/// RFC 8216bis 6.2.2's retention rules, as a client sees them. For parts:
/// "Clients MUST be able to download the Partial Segment for at least three
/// Target Durations after the EXT-X-PART tag is removed from the Playlist."
/// For segments, in a playlist without `EXT-X-ENDLIST`: "clients MUST be
/// able to download the segment until the time at which it first appeared
/// in the Playlist plus the segment's Availability Duration", which is "the
/// duration of the segment plus the duration of the longest-duration
/// Playlist distributed by the server containing that segment".
///
/// Time is the published media: every `EXTINF` the window has listed, once
/// each, plus the open parts' spans. That is the muxer's own clock, read
/// from what it lists, so this check is not independent of how the muxer
/// measures time; a test should also measure on its input timestamps, as
/// `part_retention_tests` does. A segment first appears at the end of its
/// own `EXTINF`; a playlist's duration is its `EXTINF` total. Call
/// [`RetentionCheck::check`] after every push, so that no entry joins and
/// leaves the window between calls.
///
/// A part whose tag a render stops listing must be served by
/// [`Muxer::part`] until the published media is three target durations
/// further on, and a segment that has left the window must be served by
/// [`Muxer::segment`] until its Availability Duration has passed, unless
/// the muxer counted it in [`Muxer::parts_released_early`] or
/// [`Muxer::segments_released_early`] by then. For tests and fuzzing.
#[derive(Debug, Default)]
pub struct RetentionCheck {
    listed: std::collections::BTreeSet<u64>,
    /// Part id, and the published media when its tag left.
    removed: std::collections::BTreeMap<u64, Duration>,
    /// Segment id: when it first appeared, its duration, and the longest
    /// playlist that listed it.
    segments: std::collections::BTreeMap<u64, (Duration, Duration, Duration)>,
    counted: u64,
    closed: Duration,
    published: Duration,
    released_early: (u64, u64),
    shortest: Option<Duration>,
    least_margin: Option<Duration>,
}

impl RetentionCheck {
    /// Checks the muxer after one more push, returning a description of the
    /// first part listed but not served, or part or segment served too
    /// briefly.
    pub fn check(&mut self, m: &Muxer) -> Result<(), String> {
        let total = m
            .entries()
            .iter()
            .fold(Duration::ZERO, |sum, e| sum.saturating_add(e.duration()));
        for (i, entry) in m.entries().iter().enumerate() {
            let msn = m.media_sequence() + i as u64;
            if msn >= self.counted {
                self.closed = self.closed.saturating_add(entry.duration());
                self.counted = msn + 1;
                if let Entry::Segment(s) = entry {
                    self.segments
                        .insert(msn, (self.closed, s.duration, Duration::ZERO));
                }
            }
            if let Some(seen) = self.segments.get_mut(&msn) {
                seen.2 = seen.2.max(total);
            }
        }
        let open = m
            .open_parts()
            .iter()
            .fold(Duration::ZERO, |sum, part| sum.saturating_add(part.span));
        self.published = self.published.max(self.closed.saturating_add(open));
        let early = (m.parts_released_early(), m.segments_released_early());
        let (parts_excused, segments_excused) = (
            early.0 != self.released_early.0,
            early.1 != self.released_early.1,
        );
        self.released_early = early;
        let listed: std::collections::BTreeSet<u64> = if m.has_content() {
            media(m, "r")
                .lines()
                .filter_map(|l| l.strip_prefix("#EXT-X-PART:"))
                .filter_map(|l| l.split("URI=\"r_part").nth(1))
                .filter_map(|r| r.split('.').next()?.parse().ok())
                .collect()
        } else {
            self.listed.clone()
        };
        for &id in &listed {
            if !matches!(m.part(id), crate::PartLookup::Found(_)) {
                return Err(format!("part {id} listed but not served"));
            }
        }
        for &id in self.listed.difference(&listed) {
            self.removed.entry(id).or_insert(self.published);
        }
        self.listed = listed;
        let window = Duration::from_secs(m.target_duration().saturating_mul(3));
        let mut gone = Vec::new();
        for (&id, &left) in &self.removed {
            if matches!(m.part(id), crate::PartLookup::Found(_)) {
                continue;
            }
            let held = self.published.saturating_sub(left);
            if held < window && !parts_excused {
                return Err(format!(
                    "part {id} gone {held:?} after its tag left, under {window:?}"
                ));
            }
            if !parts_excused {
                self.shortest = Some(self.shortest.map_or(held, |s| s.min(held)));
            }
            gone.push(id);
        }
        for id in gone {
            self.removed.remove(&id);
        }
        let mut gone = Vec::new();
        for (&id, &(appeared, duration, longest)) in &self.segments {
            if id >= m.media_sequence() || m.segment(id).is_some() {
                continue;
            }
            let until = appeared.saturating_add(duration).saturating_add(longest);
            if self.published < until && !segments_excused {
                return Err(format!(
                    "segment {id} gone at {:?}, before {until:?}",
                    self.published
                ));
            }
            if !segments_excused {
                let margin = self.published.saturating_sub(until);
                self.least_margin = Some(self.least_margin.map_or(margin, |s| s.min(margin)));
            }
            gone.push(id);
        }
        for id in gone {
            self.segments.remove(&id);
        }
        Ok(())
    }

    /// The shortest time, in published media, a part was still served
    /// after its tag left, over the parts that have gone in their time.
    pub fn shortest_held(&self) -> Option<Duration> {
        self.shortest
    }

    /// The least time, in published media, a segment was still served past
    /// the end of its Availability Duration, over the segments that have
    /// gone in their time.
    pub fn least_segment_margin(&self) -> Option<Duration> {
        self.least_margin
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muxer::tests::{aac, feed, video};
    use crate::muxer::{Muxer, MuxerConfig};

    #[test]
    fn whole_segment_mode_keeps_the_completed_timeline_without_part_advertising() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed(&mut m, 0..(30 * 3 + 7), 30);
        let low = media(&m, "p1");
        let whole = media_with_mode(&m, "p1", false);
        for tag in [
            "#EXT-X-PART",
            "#EXT-X-SERVER-CONTROL",
            "#EXT-X-PRELOAD-HINT",
        ] {
            assert!(
                !whole.contains(tag),
                "{tag} must not advertise partial delivery"
            );
        }
        let closed = |text: &str| {
            text.lines()
                .filter(|line| {
                    line.starts_with("#EXTINF:") || (!line.starts_with('#') && !line.is_empty())
                })
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(closed(&whole), closed(&low));
        for tag in [
            "#EXT-X-TARGETDURATION:",
            "#EXT-X-MEDIA-SEQUENCE:",
            "#EXT-X-DISCONTINUITY-SEQUENCE:",
            "#EXT-X-MAP:",
        ] {
            assert_eq!(
                whole.lines().find(|line| line.starts_with(tag)),
                low.lines().find(|line| line.starts_with(tag))
            );
        }
        assert!(whole.ends_with("p1_seg9.mp4\n"));
    }

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
    fn rule_check_exempts_audio_only_independent_parts_and_holds_the_live_window() {
        let head = |msn: u64| {
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PART-INF:PART-TARGET=0.23400\n\
                 #EXT-X-MEDIA-SEQUENCE:{msn}\n"
            )
        };
        let part = |d: &str, independent: bool| {
            format!(
                "#EXT-X-PART:DURATION={d},URI=\"p\"{}\n",
                if independent { ",INDEPENDENT=YES" } else { "" }
            )
        };
        // A short part before others in its segment. RFC 8216bis 4.4.4.9
        // exempts it if INDEPENDENT=YES: allowed in a segment whose every
        // part is independent (as in audio-only), refused beside a part
        // that is not (held to Apple's stricter rule).
        let render = |first: bool, second: bool| {
            format!(
                "{}{}{}#EXTINF:1.0,\ns\n",
                head(0),
                part("0.12000", first),
                part("0.20000", second)
            )
        };
        assert!(RuleCheck::default().check(&render(false, false)).is_err());
        assert_eq!(RuleCheck::default().check(&render(true, true)), Ok(()));
        assert!(
            RuleCheck::default().check(&render(true, false)).is_err(),
            "a short keyframe part in video"
        );
        // Decided per segment: an audio-only segment after an A/V one keeps
        // its exemption, and an A/V one after it is still held to the rule.
        let av = format!(
            "{}{}#EXTINF:0.4,\ns\n",
            part("0.23400", true),
            part("0.16600", false)
        );
        let audio = format!(
            "{}{}#EXTINF:0.3,\nt\n",
            part("0.12000", true),
            part("0.18000", true)
        );
        let short_av = format!(
            "{}{}#EXTINF:0.3,\nu\n",
            part("0.12000", true),
            part("0.18000", false)
        );
        assert_eq!(
            RuleCheck::default().check(&format!("{}{av}{audio}", head(0))),
            Ok(())
        );
        assert!(
            RuleCheck::default()
                .check(&format!("{}{audio}{short_av}", head(0)))
                .is_err()
        );
        // Three seconds listed after removals: under three targets of 2 s
        // (RFC 8216bis 6.2.2).
        let thin = format!("{}#EXTINF:1.5,\ns\n#EXTINF:1.5,\nt\n", head(4));
        assert!(RuleCheck::default().check(&thin).is_err());
        // Nothing removed yet: a short playlist is fine.
        let young = format!("{}#EXTINF:1.5,\ns\n", head(0));
        assert_eq!(RuleCheck::default().check(&young), Ok(()));
        let full = format!(
            "{}#EXTINF:2.0,\ns\n#EXTINF:2.0,\nt\n#EXTINF:2.0,\nu\n",
            head(4)
        );
        assert_eq!(RuleCheck::default().check(&full), Ok(()));
        // The muxer's entry cap: a render listing that many entries passes
        // even under three target durations; one fewer does not.
        let many = |n: usize| {
            let mut pl = head(4);
            for _ in 0..n {
                pl.push_str("#EXTINF:0.02000,\ns\n");
            }
            pl
        };
        let cap = crate::muxer::MAX_WINDOW_ENTRIES;
        assert_eq!(RuleCheck::default().check(&many(cap)), Ok(()));
        assert!(RuleCheck::default().check(&many(cap - 1)).is_err());
        // The byte cap does not show in a render: the muxer's signal says so.
        assert_eq!(
            RuleCheck::default().check_with_byte_cap(&thin, true),
            Ok(())
        );
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
        let mut c = RuleCheck::without_live_window();
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
        let mut fresh = RuleCheck::without_live_window();
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
            assert!(
                RuleCheck::without_live_window().check(&bad).is_err(),
                "{bad}"
            );
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
