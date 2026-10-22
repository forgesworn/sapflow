//! Prepared publish-session replay fences, without credentials, I/O or clocks.
//!
//! The server advances the supplied clock, applies a command to a draft, and
//! durably saves its snapshot before publishing the draft. Expiry transitions
//! must be saved even when a following command is refused. A connection claim
//! has a separate reserve/persist/commit handshake: never admit media from a
//! reservation alone. Restoring metadata never restores runtime authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::StreamPath;

/// The snapshot format version. [`Authority::restore`] refuses any other.
pub const SNAPSHOT_VERSION: u32 = 1;
/// The most event rows an [`Authority`] can be configured to retain.
pub const MAX_EVENTS: usize = 4096;
/// The largest integer a JavaScript number holds exactly (2^53 - 1). It bounds
/// [`JsPositive`] and connection epochs.
pub const MAX_JS_INTEGER: u64 = 9_007_199_254_740_991;
/// The latest accepted time, in milliseconds since the Unix epoch:
/// 9999-12-31T23:59:59.999 UTC.
pub const MAX_EPOCH_MS: u64 = 253_402_300_799_999;
/// The longest reconnect grace a grant may carry, in milliseconds (one hour).
pub const MAX_RECONNECT_GRACE_MS: u32 = 3_600_000;

/// A session identifier: exactly 32 lowercase hexadecimal characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionId(String);

impl SessionId {
    /// Accepts 32 lowercase hexadecimal characters; anything else is
    /// [`Error::InvalidSessionId`].
    pub fn parse(value: &str) -> Result<Self, Error> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidSessionId);
        }
        Ok(Self(value.to_owned()))
    }
    /// The identifier as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A positive integer that a JavaScript number holds exactly, from 1 through
/// [`MAX_JS_INTEGER`], so it survives a round trip through JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct JsPositive(u64);

impl JsPositive {
    /// Accepts 1 through [`MAX_JS_INTEGER`]; anything else is
    /// [`Error::InvalidInteger`].
    pub fn new(value: u64) -> Result<Self, Error> {
        (1..=MAX_JS_INTEGER)
            .contains(&value)
            .then_some(Self(value))
            .ok_or(Error::InvalidInteger)
    }
    /// The wrapped integer.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A timestamp in milliseconds since the Unix epoch, from 1 through
/// [`MAX_EPOCH_MS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EpochMs(u64);

impl EpochMs {
    /// Accepts 1 through [`MAX_EPOCH_MS`]; anything else is
    /// [`Error::InvalidTimestamp`].
    pub fn new(value: u64) -> Result<Self, Error> {
        (1..=MAX_EPOCH_MS)
            .contains(&value)
            .then_some(Self(value))
            .ok_or(Error::InvalidTimestamp)
    }
    /// The timestamp in milliseconds.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The caller computes this over canonical validated data. A request digest
/// must include the operation kind, identity, revision and every request field.
/// It is an equality/replay fence, not authentication or proof of hashing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Wraps a 32-byte digest that the caller has already computed.
    pub fn new(value: [u8; 32]) -> Self {
        Self(value)
    }
    /// The 32 digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Fingerprint([REDACTED])")
    }
}

/// The exact identity of a session: its stream, event generation and session
/// id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionKey {
    /// The stream the session publishes to.
    pub path: StreamPath,
    /// Rises when a replacement session is created for the event. A lower
    /// generation than the stored one is stale.
    pub event_generation: JsPositive,
    /// The session identifier, unique across the retained event rows.
    pub session_id: SessionId,
}

/// The non-secret terms of a session grant. Credentials and keys are never
/// held here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantMetadata {
    /// When the grant expires. The session expires once the clock reaches this
    /// time.
    pub expires_at_ms: EpochMs,
    /// How long, in milliseconds, a dropped encoder may reconnect to the same
    /// stream. At most [`MAX_RECONNECT_GRACE_MS`], and fixed for the life of
    /// the session.
    pub reconnect_grace_ms: u32,
    /// Digest of the grant's immutable policy. A refresh must carry the same
    /// value.
    pub immutable_fingerprint: Fingerprint,
}

/// Where a stored session is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// Provisioned, and neither revoked nor expired.
    Prepared,
    /// Ended by a revoke command. Terminal: only a higher event generation can
    /// replace it.
    Revoked,
    /// Reached its expiry time. Terminal in the same way as a revoked session.
    Expired,
}

/// The saved record of one event's current session. Holds no secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRow {
    /// The stream path. There is one row per event.
    pub path: StreamPath,
    /// The event generation of the retained session.
    pub event_generation: JsPositive,
    /// The identifier of the retained session.
    pub session_id: SessionId,
    /// The revision of the last command applied. A lower revision is refused
    /// as stale.
    pub revision: JsPositive,
    /// Fingerprint of the last command applied, used to recognise an exact
    /// retry.
    pub request_fingerprint: Fingerprint,
    /// Whether the session is prepared, revoked or expired.
    pub status: SessionStatus,
    /// The grant terms, kept after the session is revoked or expires. `None`
    /// only for a revocation recorded without a matching provisioned session.
    pub grant: Option<GrantMetadata>,
    /// Per-event high-water mark, retained even across replacement sessions.
    pub connection_epoch: u64,
}

impl SnapshotRow {
    /// The row's identity as a [`SessionKey`].
    pub fn key(&self) -> SessionKey {
        SessionKey {
            path: self.path.clone(),
            event_generation: self.event_generation,
            session_id: self.session_id.clone(),
        }
    }
}

/// The persistent state of an [`Authority`], for the server to save and later
/// restore. It holds no secrets and no runtime authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The format version. It must equal [`SNAPSHOT_VERSION`].
    pub version: u32,
    /// Zero is allowed only as the initial clock floor; grant timestamps are positive.
    pub clock_floor_ms: u64,
    /// One row per event, at most the capacity it is restored with.
    pub rows: Vec<SnapshotRow>,
}

/// The payload of a provision or refresh command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provision {
    /// The session being provisioned or refreshed.
    pub key: SessionKey,
    /// The command's revision. Lower than the stored revision is refused as
    /// stale, equal is an exact retry and higher is a change.
    pub revision: JsPositive,
    /// The grant terms to store.
    pub grant: GrantMetadata,
    /// Fingerprint of the whole request, used to recognise an exact retry.
    pub request_fingerprint: Fingerprint,
}

/// The payload of a revoke command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revoke {
    /// The session being revoked.
    pub key: SessionKey,
    /// The command's revision. Lower than the stored revision is refused as
    /// stale.
    pub revision: JsPositive,
    /// Fingerprint of the whole request, used to recognise an exact retry.
    pub request_fingerprint: Fingerprint,
}

/// A change to apply to an [`Authority`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Creates or explicitly replaces a lower event generation; same identity
    /// at a higher revision has the same immutable-policy rules as Refresh.
    Provision(Provision),
    /// Requires an existing exact identity; never creates or replaces a session.
    Refresh(Provision),
    /// Ends a session for good. For an identity that was never provisioned it
    /// records a denial instead.
    Revoke(Revoke),
}

/// A claim on a session's publishing slot, reserved and then committed. It is
/// valid only while it matches the current owner, so a stale lease cannot
/// affect a replacement.
#[derive(Clone)]
pub struct ConnectionLease {
    /// The session the lease is for.
    pub key: SessionKey,
    /// The per-event epoch this claim reserved. It is never reused.
    pub connection_epoch: u64,
    // A failed unpublished draft may reserve an epoch that was never saved.
    // Its late cleanup must not match another draft's reservation of that
    // number. This runtime identity is deliberately absent from snapshots.
    reservation: Arc<()>,
}

impl PartialEq for ConnectionLease {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.connection_epoch == other.connection_epoch
            && Arc::ptr_eq(&self.reservation, &other.reservation)
    }
}
impl Eq for ConnectionLease {}

impl fmt::Debug for ConnectionLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionLease")
            .field("key", &self.key)
            .field("connection_epoch", &self.connection_epoch)
            .finish_non_exhaustive()
    }
}

/// How an accepted command affected the stored state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// The command changed the stored state.
    Applied,
    /// An exact repeat of an earlier command; nothing changed.
    Idempotent,
}

/// The result of an accepted [`Command`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Whether the command changed the state or repeated an earlier one.
    pub disposition: Disposition,
    /// A previous reserved or active owner must be cancelled by the server.
    pub ended: Option<ConnectionLease>,
    /// Whether the session may now be claimed. False after a revoke, and for
    /// an exact retry of a restored session, which stays inactive until a
    /// command at a higher revision is applied.
    pub admission_enabled: bool,
}

/// The result of [`Authority::advance_clock`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockAdvance {
    /// The clock value now in force: the later of the supplied time and the
    /// previous floor.
    pub effective_now: EpochMs,
    /// True if the supplied time was earlier than the previous floor and so
    /// was ignored.
    pub clamped: bool,
    /// At most one owner per bounded event row; no internal pending-effect queue.
    pub ended: Vec<ConnectionLease>,
}

/// Why an operation was refused. `Display` prints the variant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A session id is not 32 lowercase hexadecimal characters.
    InvalidSessionId,
    /// An integer is outside 1 through [`MAX_JS_INTEGER`].
    InvalidInteger,
    /// A timestamp is outside 1 through [`MAX_EPOCH_MS`], or a command was
    /// applied before the clock was first advanced.
    InvalidTimestamp,
    /// The reconnect grace is above [`MAX_RECONNECT_GRACE_MS`].
    InvalidGrace,
    /// The event capacity is not 1 through [`MAX_EVENTS`].
    InvalidCapacity,
    /// The snapshot version is not [`SNAPSHOT_VERSION`].
    UnsupportedSnapshot,
    /// The snapshot is not internally consistent, or does not fit the
    /// configured capacity.
    InvalidSnapshot,
    /// The authority already retains its maximum number of event rows.
    /// Terminal rows are never evicted to make room.
    Capacity,
    /// The command's event generation is lower than the stored one.
    StaleGeneration,
    /// The session id clashes with another identity: it differs at the same
    /// generation, is reused at another generation, or belongs to another
    /// event.
    SessionConflict,
    /// The command's revision is lower than the stored one.
    StaleRevision,
    /// The revision matches the stored one but the request differs from the
    /// one applied.
    RequestConflict,
    /// The stored session is revoked or expired, or the new grant's expiry is
    /// already past, so it cannot be provisioned or refreshed.
    Terminal,
    /// A refresh changed the immutable policy or the reconnect grace.
    ConfigurationChanged,
    /// A refresh set an earlier expiry than the stored one.
    ExpiryReduced,
    /// A refresh did not match an existing session identity exactly.
    UnknownSession,
    /// The session cannot be claimed: it is not the event's current session,
    /// it is not prepared (revoked or expired), or it was restored from a
    /// snapshot and not refreshed since.
    AdmissionDisabled,
    /// Another connection holds or has reserved the session.
    AlreadyClaimed,
    /// The connection epoch cannot be raised without passing
    /// [`MAX_JS_INTEGER`].
    EpochExhausted,
    /// The lease is not the current owner: it was released, ended or replaced.
    StaleLease,
    /// The claim is already committed, so it is no longer awaiting commit.
    ClaimNotReserved,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug)]
struct Owner {
    lease: ConnectionLease,
    committed: bool,
}

#[derive(Clone, Debug)]
struct Row {
    metadata: SnapshotRow,
    enabled: bool,
    owner: Option<Owner>,
}

/// The in-memory authority over prepared publish sessions: one row per event,
/// a clock floor that never goes back, and at most one connection claim per
/// event. Save [`Authority::snapshot`] durably as the module documentation
/// describes.
#[derive(Clone, Debug)]
pub struct Authority {
    max_events: usize,
    clock_floor_ms: u64,
    rows: BTreeMap<String, Row>,
}

impl Authority {
    /// Creates an empty authority that retains up to `max_events` event rows,
    /// which must be 1 through [`MAX_EVENTS`]. Its clock starts at zero, so
    /// call [`Authority::advance_clock`] before applying commands.
    pub fn new(max_events: usize) -> Result<Self, Error> {
        if !(1..=MAX_EVENTS).contains(&max_events) {
            return Err(Error::InvalidCapacity);
        }
        Ok(Self {
            max_events,
            clock_floor_ms: 0,
            rows: BTreeMap::new(),
        })
    }

    /// Rebuilds an authority from a saved [`Snapshot`], refusing one that is
    /// inconsistent. Restored sessions have admission disabled and no
    /// connection owner: runtime authority is never restored.
    pub fn restore(snapshot: Snapshot, max_events: usize) -> Result<Self, Error> {
        let mut authority = Self::new(max_events)?;
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(Error::UnsupportedSnapshot);
        }
        if snapshot.clock_floor_ms > MAX_EPOCH_MS
            || snapshot.rows.len() > max_events
            || (snapshot.clock_floor_ms == 0 && !snapshot.rows.is_empty())
        {
            return Err(Error::InvalidSnapshot);
        }
        authority.clock_floor_ms = snapshot.clock_floor_ms;
        let mut session_ids = BTreeSet::new();
        for row in snapshot.rows {
            if row.connection_epoch > MAX_JS_INTEGER
                || !session_ids.insert(row.session_id.as_str().to_owned())
            {
                return Err(Error::InvalidSnapshot);
            }
            match (&row.grant, row.status) {
                (Some(grant), status) => {
                    if grant.reconnect_grace_ms > MAX_RECONNECT_GRACE_MS
                        || (status == SessionStatus::Prepared
                            && grant.expires_at_ms.get() <= authority.clock_floor_ms)
                        || (status == SessionStatus::Expired
                            && grant.expires_at_ms.get() > authority.clock_floor_ms)
                    {
                        return Err(Error::InvalidSnapshot);
                    }
                }
                (None, SessionStatus::Revoked) => {}
                _ => return Err(Error::InvalidSnapshot),
            }
            let key = row.path.event_id().to_owned();
            if authority
                .rows
                .insert(
                    key,
                    Row {
                        metadata: row,
                        enabled: false,
                        owner: None,
                    },
                )
                .is_some()
            {
                return Err(Error::InvalidSnapshot);
            }
        }
        Ok(authority)
    }

    /// The current state as a [`Snapshot`] for the server to save durably.
    /// Runtime owners and admission flags are not included.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            version: SNAPSHOT_VERSION,
            clock_floor_ms: self.clock_floor_ms,
            rows: self.rows.values().map(|row| row.metadata.clone()).collect(),
        }
    }

    /// The stored row for the event at `path`, if there is one.
    pub fn row(&self, path: &StreamPath) -> Option<&SnapshotRow> {
        self.rows.get(path.event_id()).map(|row| &row.metadata)
    }

    /// The earliest expiry among prepared sessions, for the server's expiry
    /// timer; `None` if there are none.
    pub fn next_expiry(&self) -> Option<EpochMs> {
        self.rows
            .values()
            .filter(|row| row.metadata.status == SessionStatus::Prepared)
            .filter_map(|row| row.metadata.grant.as_ref().map(|grant| grant.expires_at_ms))
            .min()
    }

    /// Whether `key` is the event's current session, still prepared and
    /// enabled, so it may be claimed.
    pub fn admission_enabled(&self, key: &SessionKey) -> bool {
        self.rows.get(key.path.event_id()).is_some_and(|row| {
            row.metadata.key() == *key
                && row.enabled
                && row.metadata.status == SessionStatus::Prepared
        })
    }

    /// Call before every administrative operation and admission, and from the
    /// server's expiry timer. The server also enforces its monotonic fast gate.
    pub fn advance_clock(&mut self, now: EpochMs) -> ClockAdvance {
        let clamped = now.get() < self.clock_floor_ms;
        self.clock_floor_ms = self.clock_floor_ms.max(now.get());
        let mut ended = Vec::new();
        for row in self.rows.values_mut() {
            if row.metadata.status == SessionStatus::Prepared
                && row
                    .metadata
                    .grant
                    .as_ref()
                    .is_some_and(|grant| grant.expires_at_ms.get() <= self.clock_floor_ms)
            {
                row.metadata.status = SessionStatus::Expired;
                row.enabled = false;
                if let Some(owner) = row.owner.take() {
                    ended.push(owner.lease);
                }
            }
        }
        ClockAdvance {
            effective_now: EpochMs(self.clock_floor_ms),
            clamped,
            ended,
        }
    }

    /// Applies a command to the stored state. It is refused until
    /// [`Authority::advance_clock`] has been given a time, and a refused
    /// command changes nothing.
    pub fn apply(&mut self, command: Command) -> Result<ApplyOutcome, Error> {
        if self.clock_floor_ms == 0 {
            return Err(Error::InvalidTimestamp);
        }
        // The private control route addresses a retained session by ID alone.
        // Two current event rows must never make that lookup ambiguous.
        let key = match &command {
            Command::Provision(value) | Command::Refresh(value) => &value.key,
            Command::Revoke(value) => &value.key,
        };
        if self
            .rows
            .values()
            .any(|row| row.metadata.path != key.path && row.metadata.session_id == key.session_id)
        {
            return Err(Error::SessionConflict);
        }
        match command {
            Command::Provision(value) => self.provision(value, false),
            Command::Refresh(value) => self.provision(value, true),
            Command::Revoke(value) => self.revoke(value),
        }
    }

    fn compare(row: &SnapshotRow, key: &SessionKey) -> Result<bool, Error> {
        if key.event_generation < row.event_generation {
            return Err(Error::StaleGeneration);
        }
        if key.event_generation == row.event_generation {
            if key.session_id != row.session_id {
                return Err(Error::SessionConflict);
            }
            Ok(true)
        } else {
            if key.session_id == row.session_id {
                return Err(Error::SessionConflict);
            }
            Ok(false)
        }
    }

    fn provision(&mut self, value: Provision, refresh: bool) -> Result<ApplyOutcome, Error> {
        if value.grant.reconnect_grace_ms > MAX_RECONNECT_GRACE_MS {
            return Err(Error::InvalidGrace);
        }
        let event = value.key.path.event_id().to_owned();
        if let Some(row) = self.rows.get_mut(&event) {
            let same = Self::compare(&row.metadata, &value.key)?;
            if refresh && !same {
                return Err(Error::UnknownSession);
            }
            if same {
                if value.revision < row.metadata.revision {
                    return Err(Error::StaleRevision);
                }
                if value.revision == row.metadata.revision {
                    if value.request_fingerprint != row.metadata.request_fingerprint
                        || row.metadata.grant.as_ref() != Some(&value.grant)
                    {
                        return Err(Error::RequestConflict);
                    }
                    return Ok(ApplyOutcome {
                        disposition: Disposition::Idempotent,
                        ended: None,
                        admission_enabled: row.enabled,
                    });
                }
                if row.metadata.status != SessionStatus::Prepared {
                    return Err(Error::Terminal);
                }
                let previous = row.metadata.grant.as_ref().ok_or(Error::InvalidSnapshot)?;
                if previous.immutable_fingerprint != value.grant.immutable_fingerprint
                    || previous.reconnect_grace_ms != value.grant.reconnect_grace_ms
                {
                    return Err(Error::ConfigurationChanged);
                }
                if value.grant.expires_at_ms < previous.expires_at_ms {
                    return Err(Error::ExpiryReduced);
                }
            }
        } else {
            if refresh {
                return Err(Error::UnknownSession);
            }
            if self.rows.len() >= self.max_events {
                return Err(Error::Capacity);
            }
        }
        if value.grant.expires_at_ms.get() <= self.clock_floor_ms {
            return Err(Error::Terminal);
        }
        let mut ended = None;
        let (connection_epoch, owner) = match self.rows.get_mut(&event) {
            Some(row) if row.metadata.key() == value.key => {
                (row.metadata.connection_epoch, row.owner.take())
            }
            Some(row) => {
                ended = row.owner.take().map(|owner| owner.lease);
                (row.metadata.connection_epoch, None)
            }
            None => (0, None),
        };
        self.rows.insert(
            event,
            Row {
                metadata: SnapshotRow {
                    path: value.key.path,
                    event_generation: value.key.event_generation,
                    session_id: value.key.session_id,
                    revision: value.revision,
                    request_fingerprint: value.request_fingerprint,
                    status: SessionStatus::Prepared,
                    grant: Some(value.grant),
                    connection_epoch,
                },
                enabled: true,
                owner,
            },
        );
        Ok(ApplyOutcome {
            disposition: Disposition::Applied,
            ended,
            admission_enabled: true,
        })
    }

    fn revoke(&mut self, value: Revoke) -> Result<ApplyOutcome, Error> {
        let event = value.key.path.event_id().to_owned();
        if let Some(row) = self.rows.get(&event) {
            if Self::compare(&row.metadata, &value.key)? {
                if value.revision < row.metadata.revision {
                    return Err(Error::StaleRevision);
                }
                if value.revision == row.metadata.revision {
                    if row.metadata.status != SessionStatus::Revoked
                        || value.request_fingerprint != row.metadata.request_fingerprint
                    {
                        return Err(Error::RequestConflict);
                    }
                    return Ok(ApplyOutcome {
                        disposition: Disposition::Idempotent,
                        ended: None,
                        admission_enabled: false,
                    });
                }
            }
        } else if self.rows.len() >= self.max_events {
            return Err(Error::Capacity);
        }
        let previous = self.rows.remove(&event);
        let (grant, connection_epoch, ended) = previous.map_or((None, 0, None), |row| {
            let same = row.metadata.key() == value.key;
            (
                if same { row.metadata.grant } else { None },
                row.metadata.connection_epoch,
                row.owner.map(|owner| owner.lease),
            )
        });
        self.rows.insert(
            event,
            Row {
                metadata: SnapshotRow {
                    path: value.key.path,
                    event_generation: value.key.event_generation,
                    session_id: value.key.session_id,
                    revision: value.revision,
                    request_fingerprint: value.request_fingerprint,
                    status: SessionStatus::Revoked,
                    grant,
                    connection_epoch,
                },
                enabled: false,
                owner: None,
            },
        );
        Ok(ApplyOutcome {
            disposition: Disposition::Applied,
            ended,
            admission_enabled: false,
        })
    }

    /// Reserve on a draft, durably persist its snapshot, then commit. Cancelling
    /// a reservation burns its epoch permanently; it must never be reused.
    pub fn reserve_claim(&mut self, key: &SessionKey) -> Result<ConnectionLease, Error> {
        if !self.admission_enabled(key) {
            return Err(Error::AdmissionDisabled);
        }
        let row = self
            .rows
            .get_mut(key.path.event_id())
            .ok_or(Error::UnknownSession)?;
        if row.owner.is_some() {
            return Err(Error::AlreadyClaimed);
        }
        let epoch = row
            .metadata
            .connection_epoch
            .checked_add(1)
            .filter(|epoch| *epoch <= MAX_JS_INTEGER)
            .ok_or(Error::EpochExhausted)?;
        row.metadata.connection_epoch = epoch;
        let lease = ConnectionLease {
            key: key.clone(),
            connection_epoch: epoch,
            reservation: Arc::new(()),
        };
        row.owner = Some(Owner {
            lease: lease.clone(),
            committed: false,
        });
        Ok(lease)
    }

    /// The adapter must have durably saved this reservation before committing.
    pub fn commit_claim(&mut self, lease: &ConnectionLease) -> Result<(), Error> {
        if !self.admission_enabled(&lease.key) {
            return Err(Error::AdmissionDisabled);
        }
        let row = self
            .rows
            .get_mut(lease.key.path.event_id())
            .ok_or(Error::StaleLease)?;
        let owner = row
            .owner
            .as_mut()
            .filter(|owner| owner.lease == *lease)
            .ok_or(Error::StaleLease)?;
        if owner.committed {
            return Err(Error::ClaimNotReserved);
        }
        owner.committed = true;
        Ok(())
    }

    /// Whether `lease` is the committed owner of a session that still admits
    /// media.
    pub fn lease_active(&self, lease: &ConnectionLease) -> bool {
        self.admission_enabled(&lease.key)
            && self
                .rows
                .get(lease.key.path.event_id())
                .and_then(|row| row.owner.as_ref())
                .is_some_and(|owner| owner.committed && owner.lease == *lease)
    }

    /// Exact identity and epoch only; an old callback cannot free a replacement.
    pub fn release_claim(&mut self, lease: &ConnectionLease) -> bool {
        let Some(row) = self.rows.get_mut(lease.key.path.event_id()) else {
            return false;
        };
        if row
            .owner
            .as_ref()
            .is_some_and(|owner| owner.lease == *lease)
        {
            row.owner = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
