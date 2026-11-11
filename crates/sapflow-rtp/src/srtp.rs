//! SRTP and SRTCP (RFC 3711) in the two DTLS-SRTP profiles WebRTC
//! publishers offer: `SRTP_AES128_CM_HMAC_SHA1_80` (RFC 5764 section
//! 4.1.2) and `SRTP_AEAD_AES_128_GCM` (RFC 7714).
//!
//! - [`split_keying_material`] turns the DTLS exporter output (RFC 5764
//!   section 4.2) into our write key and the peer's.
//! - [`Inbound`] unprotects RTP and RTCP from the peer, with one replay
//!   window per SSRC for each.
//! - [`Outbound`] protects the RTCP sapflow sends, with an SRTCP index that
//!   refuses to wrap.
//!
//! Session keys are derived once (key derivation rate 0) and kept as ready
//! cipher instances. Packets are processed in place in the caller's buffer;
//! after [`Inbound::new`] the unprotect paths allocate nothing. Master and
//! session keys, salts and key schedules are zeroed on drop; copies a caller
//! keeps are its own to clear.
//!
//! The replay window defaults to 1024 packets (RFC 3711's minimum is 64).
//! Sapflow waits up to 200 ms for a NACKed retransmission, and a sender
//! without RTX (OBS) resends on the original SSRC and index; at 5 Mbit/s
//! that is a hundred or more packets back. An index never received inside
//! the window is accepted; a duplicate of a received one is refused.
//!
//! Stream slots are bounded and never freed: [`Config::max_streams`] (8 by
//! default) SSRCs for RTP and, separately, for RTCP. A slot is taken by an
//! SSRC's first authenticated packet and kept for the life of the context,
//! so once all are taken a packet from a ninth distinct SSRC is refused
//! with [`SrtpError::TooManyStreams`] for the rest of the session. Slots are
//! not reused, because forgetting an SSRC's replay window would let old
//! packets from it be replayed. A WHIP publisher needs at most a video, an
//! RTX and an audio SSRC.
//!
//! Master key identifiers (MKI) are not supported; the DTLS stage refuses a
//! `use_srtp` extension that asks for one.

mod replay;
#[cfg(test)]
mod tests;
mod transform;

use aes::Aes128Enc;
use aes_gcm::KeyInit;
use replay::ReplayWindow;
use std::fmt;
use transform::{CmKeys, GcmKeys};
use zeroize::Zeroizing;

/// Smallest replay window (RFC 3711 section 3.3.2).
pub const MIN_REPLAY_WINDOW: u32 = 64;
/// Largest replay window: half the sequence space, beyond which index
/// estimation cannot place a packet anyway.
pub const MAX_REPLAY_WINDOW: u32 = 32_768;
/// Default replay window, sized for retransmissions on the original SSRC.
pub const DEFAULT_REPLAY_WINDOW: u32 = 1024;
/// Largest SSRC count per direction and kind.
pub const MAX_STREAMS: usize = 64;
/// Highest SRTCP index before the master key must be replaced.
const MAX_SRTCP_INDEX: u32 = 0x7fff_ffff;

/// Why an SRTP operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrtpError {
    /// A configuration value, key or salt length, or keying material length
    /// is wrong.
    Config,
    /// Too short to hold the header and trailer, or not an RTP header.
    Malformed,
    /// A packet from a new SSRC when every stream slot is taken.
    TooManyStreams,
    /// An index that has already been received.
    Replayed,
    /// An index behind the replay window, or before the stream began.
    TooOld,
    /// The authentication tag does not match.
    Authentication,
    /// The index space for this key is used up; the session must rekey.
    Exhausted,
    /// No room in the buffer for the SRTP trailer.
    BufferTooSmall,
}
impl fmt::Display for SrtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SRTP: {self:?}")
    }
}
impl std::error::Error for SrtpError {}

/// A DTLS-SRTP protection profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// `SRTP_AES128_CM_HMAC_SHA1_80` (RFC 5764 section 4.1.2), `0x0001`.
    Aes128CmHmacSha1_80,
    /// `SRTP_AEAD_AES_128_GCM` (RFC 7714 section 14.2), `0x0007`.
    AeadAes128Gcm,
}
impl Profile {
    /// The `use_srtp` identifier.
    pub const fn dtls_id(self) -> u16 {
        match self {
            Self::Aes128CmHmacSha1_80 => 0x0001,
            Self::AeadAes128Gcm => 0x0007,
        }
    }
    /// The profile for a `use_srtp` identifier, if supported.
    pub const fn from_dtls_id(id: u16) -> Option<Self> {
        match id {
            0x0001 => Some(Self::Aes128CmHmacSha1_80),
            0x0007 => Some(Self::AeadAes128Gcm),
            _ => None,
        }
    }
    /// Master key length in octets.
    pub const fn master_key_len(self) -> usize {
        16
    }
    /// Master salt length in octets.
    pub const fn master_salt_len(self) -> usize {
        match self {
            Self::Aes128CmHmacSha1_80 => 14,
            Self::AeadAes128Gcm => 12,
        }
    }
    /// Exporter output length: both directions' keys and salts.
    pub const fn keying_material_len(self) -> usize {
        2 * (self.master_key_len() + self.master_salt_len())
    }
    /// Octets SRTP adds to an RTP packet.
    pub const fn rtp_overhead(self) -> usize {
        match self {
            Self::Aes128CmHmacSha1_80 => transform::CM_TAG,
            Self::AeadAes128Gcm => transform::GCM_TAG,
        }
    }
    /// Octets SRTCP adds to an RTCP packet.
    pub const fn rtcp_overhead(self) -> usize {
        self.rtp_overhead() + transform::ESRTCP
    }
}

/// A master key and salt for one direction. Zeroed on drop; `Debug` shows
/// only the profile.
pub struct MasterKey {
    profile: Profile,
    key: Zeroizing<[u8; 16]>,
    salt: Zeroizing<[u8; 14]>,
}
impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MasterKey")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}
impl MasterKey {
    /// A master key and salt of the profile's lengths.
    pub fn new(profile: Profile, key: &[u8], salt: &[u8]) -> Result<Self, SrtpError> {
        if key.len() != profile.master_key_len() || salt.len() != profile.master_salt_len() {
            return Err(SrtpError::Config);
        }
        let mut k = Zeroizing::new([0u8; 16]);
        k.copy_from_slice(key);
        let mut s = Zeroizing::new([0u8; 14]);
        s.get_mut(..salt.len())
            .ok_or(SrtpError::Config)?
            .copy_from_slice(salt);
        Ok(Self {
            profile,
            key: k,
            salt: s,
        })
    }
    /// The profile.
    pub fn profile(&self) -> Profile {
        self.profile
    }
    fn salt(&self) -> &[u8] {
        self.salt
            .get(..self.profile.master_salt_len())
            .unwrap_or_default()
    }
}

/// Our side of the DTLS handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The DTLS client.
    Client,
    /// The DTLS server, as sapflow is.
    Server,
}

/// Both directions' master keys from one DTLS-SRTP handshake.
#[derive(Debug)]
pub struct DtlsSrtpKeys {
    /// Ours: protects what we send ([`Outbound`]).
    pub local: MasterKey,
    /// The peer's: unprotects what we receive ([`Inbound`]).
    pub remote: MasterKey,
}

/// Splits exporter keying material ("EXTRACTOR-dtls_srtp", RFC 5764
/// section 4.2) into client and server write keys and salts:
/// `client_key || server_key || client_salt || server_salt`. The server
/// writes with the server's and reads with the client's, and the client
/// the other way round.
pub fn split_keying_material(
    profile: Profile,
    role: Role,
    material: &[u8],
) -> Result<DtlsSrtpKeys, SrtpError> {
    if material.len() != profile.keying_material_len() {
        return Err(SrtpError::Config);
    }
    let k = profile.master_key_len();
    let s = profile.master_salt_len();
    let part = |from: usize, len: usize| material.get(from..from + len).ok_or(SrtpError::Config);
    let client = MasterKey::new(profile, part(0, k)?, part(2 * k, s)?)?;
    let server = MasterKey::new(profile, part(k, k)?, part(2 * k + s, s)?)?;
    Ok(match role {
        Role::Server => DtlsSrtpKeys {
            local: server,
            remote: client,
        },
        Role::Client => DtlsSrtpKeys {
            local: client,
            remote: server,
        },
    })
}

/// Limits for an [`Inbound`] context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Replay window in packets: a multiple of 64 from 64 to 32768.
    pub replay_window: u32,
    /// SSRCs tracked for RTP, and separately for RTCP, 1 to 64. Slots are
    /// never freed: once they are all taken, a packet from one more SSRC is
    /// refused before any cryptography for the rest of the session.
    pub max_streams: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            replay_window: DEFAULT_REPLAY_WINDOW,
            max_streams: 8,
        }
    }
}
impl Config {
    fn validate(&self) -> Result<(), SrtpError> {
        let window_ok = (MIN_REPLAY_WINDOW..=MAX_REPLAY_WINDOW).contains(&self.replay_window)
            && self.replay_window.is_multiple_of(64);
        if !window_ok || !(1..=MAX_STREAMS).contains(&self.max_streams) {
            return Err(SrtpError::Config);
        }
        Ok(())
    }
}

/// The session keys for one direction, RTP and RTCP.
enum Transform {
    Cm {
        rtp: Box<CmKeys>,
        rtcp: Box<CmKeys>,
    },
    Gcm {
        rtp: Box<GcmKeys>,
        rtcp: Box<GcmKeys>,
    },
}
impl Transform {
    fn derive(key: &MasterKey) -> Result<Self, SrtpError> {
        let master =
            Aes128Enc::new_from_slice(key.key.as_slice()).map_err(|_| SrtpError::Config)?;
        let salt = key.salt();
        Ok(match key.profile {
            Profile::Aes128CmHmacSha1_80 => Self::Cm {
                rtp: Box::new(CmKeys::derive(&master, salt, transform::RTP_LABELS)?),
                rtcp: Box::new(CmKeys::derive(&master, salt, transform::RTCP_LABELS)?),
            },
            Profile::AeadAes128Gcm => Self::Gcm {
                rtp: Box::new(GcmKeys::derive(&master, salt, transform::RTP_LABELS)?),
                rtcp: Box::new(GcmKeys::derive(&master, salt, transform::RTCP_LABELS)?),
            },
        })
    }
    fn profile(&self) -> Profile {
        match self {
            Self::Cm { .. } => Profile::Aes128CmHmacSha1_80,
            Self::Gcm { .. } => Profile::AeadAes128Gcm,
        }
    }
}

/// Per-SSRC replay state, allocated up front.
struct Streams {
    slots: Vec<(Option<u32>, ReplayWindow)>,
}
impl Streams {
    fn new(config: &Config) -> Self {
        Self {
            slots: (0..config.max_streams)
                .map(|_| (None, ReplayWindow::new(config.replay_window)))
                .collect(),
        }
    }
    fn find(&self, ssrc: u32) -> Option<usize> {
        self.slots.iter().position(|(s, _)| *s == Some(ssrc))
    }
    fn free(&self) -> Option<usize> {
        self.slots.iter().position(|(s, _)| s.is_none())
    }
    /// The slot for `ssrc`, or a free one, and whether it is new.
    fn slot(&self, ssrc: u32) -> Result<(usize, bool), SrtpError> {
        match self.find(ssrc) {
            Some(n) => Ok((n, false)),
            None => self
                .free()
                .map(|n| (n, true))
                .ok_or(SrtpError::TooManyStreams),
        }
    }
    fn window(&self, n: usize) -> Result<&ReplayWindow, SrtpError> {
        self.slots
            .get(n)
            .map(|(_, w)| w)
            .ok_or(SrtpError::Malformed)
    }
    fn accept(&mut self, n: usize, ssrc: u32, index: u64) {
        if let Some((s, w)) = self.slots.get_mut(n) {
            *s = Some(ssrc);
            w.accept(index);
        }
    }
    fn tracked(&self) -> usize {
        self.slots.iter().filter(|(s, _)| s.is_some()).count()
    }
}

/// Unprotects SRTP and SRTCP from the peer.
pub struct Inbound {
    transform: Transform,
    rtp: Streams,
    rtcp: Streams,
}
impl fmt::Debug for Inbound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inbound")
            .field("profile", &self.transform.profile())
            .field("rtp_streams", &self.rtp.tracked())
            .field("rtcp_streams", &self.rtcp.tracked())
            .finish_non_exhaustive()
    }
}

impl Inbound {
    /// A context for the peer's master key (`DtlsSrtpKeys::remote`).
    pub fn new(key: &MasterKey, config: Config) -> Result<Self, SrtpError> {
        Self::with_transform(Transform::derive(key)?, config)
    }

    fn with_transform(transform: Transform, config: Config) -> Result<Self, SrtpError> {
        config.validate()?;
        Ok(Self {
            transform,
            rtp: Streams::new(&config),
            rtcp: Streams::new(&config),
        })
    }

    /// The profile.
    pub fn profile(&self) -> Profile {
        self.transform.profile()
    }

    /// Unprotects the SRTP packet filling `buf`, in place, and returns the
    /// length of the RTP packet now at its start. On an error nothing in
    /// the context changes and the buffer should be dropped.
    pub fn unprotect_rtp(&mut self, buf: &mut [u8]) -> Result<usize, SrtpError> {
        let header_len = crate::rtp::header_len(buf).map_err(|_| SrtpError::Malformed)?;
        let seq = crate::rtp::be16(buf, 2).ok_or(SrtpError::Malformed)?;
        let ssrc = crate::rtp::be32(buf, 8).ok_or(SrtpError::Malformed)?;
        let (slot, new) = self.rtp.slot(ssrc)?;
        let index = if new {
            // A new stream starts with ROC 0 (RFC 3711 section 3.3.1).
            u64::from(seq)
        } else {
            let window = self.rtp.window(slot)?;
            let index = replay::estimate(window.top().unwrap_or(0), seq)?;
            window.check(index)?;
            index
        };
        let len = match &self.transform {
            Transform::Cm { rtp, .. } => {
                transform::cm_rtp_unprotect(rtp, buf, header_len, ssrc, index)?
            }
            Transform::Gcm { rtp, .. } => {
                transform::gcm_rtp_unprotect(rtp, buf, header_len, ssrc, index)?
            }
        };
        self.rtp.accept(slot, ssrc, index);
        Ok(len)
    }

    /// Unprotects the SRTCP packet filling `buf`, in place, and returns the
    /// length of the RTCP now at its start. Packets with the E flag clear
    /// are authenticated and left unencrypted, as RFC 3711 section 3.4
    /// allows. Each sending SSRC (the first header's) has its own replay
    /// window over the SRTCP index.
    pub fn unprotect_rtcp(&mut self, buf: &mut [u8]) -> Result<usize, SrtpError> {
        let (tag_len, word_last) = match self.transform {
            Transform::Cm { .. } => (transform::CM_TAG, false),
            Transform::Gcm { .. } => (transform::GCM_TAG, true),
        };
        let (_, index) = transform::srtcp_word(buf, tag_len, word_last)?;
        let index = u64::from(index);
        let ssrc = transform::rtcp_ssrc(buf)?;
        let (slot, new) = self.rtcp.slot(ssrc)?;
        if !new {
            self.rtcp.window(slot)?.check(index)?;
        }
        let len = match &self.transform {
            Transform::Cm { rtcp, .. } => transform::cm_rtcp_unprotect(rtcp, buf)?,
            Transform::Gcm { rtcp, .. } => transform::gcm_rtcp_unprotect(rtcp, buf)?,
        };
        self.rtcp.accept(slot, ssrc, index);
        Ok(len)
    }
}

/// Protects the SRTCP sapflow sends (and SRTP, for tests).
pub struct Outbound {
    transform: Transform,
    /// The next SRTCP index.
    rtcp_index: u32,
    #[cfg(any(test, feature = "testing"))]
    senders: Vec<(u32, u64)>,
}
impl fmt::Debug for Outbound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outbound")
            .field("profile", &self.transform.profile())
            .field("rtcp_index", &self.rtcp_index)
            .finish_non_exhaustive()
    }
}

impl Outbound {
    /// A context for our master key (`DtlsSrtpKeys::local`).
    pub fn new(key: &MasterKey) -> Result<Self, SrtpError> {
        Ok(Self::with_transform(Transform::derive(key)?))
    }

    fn with_transform(transform: Transform) -> Self {
        Self {
            transform,
            rtcp_index: 0,
            #[cfg(any(test, feature = "testing"))]
            senders: Vec::new(),
        }
    }

    /// The profile.
    pub fn profile(&self) -> Profile {
        self.transform.profile()
    }

    /// Protects (encrypts and authenticates) the RTCP packet in
    /// `buf[..len]` in place, appending the SRTCP trailer, and returns the
    /// SRTCP length. `buf` needs [`Profile::rtcp_overhead`] spare octets
    /// after `len`. Refuses once the 31-bit index is spent.
    pub fn protect_rtcp(&mut self, buf: &mut [u8], len: usize) -> Result<usize, SrtpError> {
        self.protect_rtcp_with(buf, len, true)
    }

    pub(crate) fn protect_rtcp_with(
        &mut self,
        buf: &mut [u8],
        len: usize,
        encrypt: bool,
    ) -> Result<usize, SrtpError> {
        let index = self.rtcp_index;
        if index > MAX_SRTCP_INDEX {
            return Err(SrtpError::Exhausted);
        }
        let out = match &self.transform {
            Transform::Cm { rtcp, .. } => {
                transform::cm_rtcp_protect(rtcp, buf, len, index, encrypt)?
            }
            Transform::Gcm { rtcp, .. } => {
                transform::gcm_rtcp_protect(rtcp, buf, len, index, encrypt)?
            }
        };
        self.rtcp_index = index + 1;
        Ok(out)
    }

    /// Protects the RTP packet in `buf[..len]` in place, appending the tag.
    /// Sapflow sends no RTP: this is for tests and fuzz seeds, and does not
    /// stop a caller reusing an index.
    #[cfg(any(test, feature = "testing"))]
    pub fn protect_rtp(&mut self, buf: &mut [u8], len: usize) -> Result<usize, SrtpError> {
        let packet = buf.get(..len).ok_or(SrtpError::BufferTooSmall)?;
        let header_len = crate::rtp::header_len(packet).map_err(|_| SrtpError::Malformed)?;
        let seq = crate::rtp::be16(packet, 2).ok_or(SrtpError::Malformed)?;
        let ssrc = crate::rtp::be32(packet, 8).ok_or(SrtpError::Malformed)?;
        let index = match self.senders.iter().find(|(s, _)| *s == ssrc) {
            Some((_, top)) => replay::estimate(*top, seq)?,
            None => u64::from(seq),
        };
        let out = match &self.transform {
            Transform::Cm { rtp, .. } => {
                transform::cm_rtp_protect(rtp, buf, len, header_len, ssrc, index)?
            }
            Transform::Gcm { rtp, .. } => {
                transform::gcm_rtp_protect(rtp, buf, len, header_len, ssrc, index)?
            }
        };
        match self.senders.iter_mut().find(|(s, _)| *s == ssrc) {
            Some((_, top)) => *top = (*top).max(index),
            None => self.senders.push((ssrc, index)),
        }
        Ok(out)
    }
}
