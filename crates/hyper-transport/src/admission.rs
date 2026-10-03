//! Admission of connections by identity (mantle note 32 T9–T11; focal `admission.rs`, 27 §3.1 P5,
//! the audit's F20), owned by the endpoint: no shared handle, no lock.
//!
//! A connection is admitted twice. Before its handshake it takes one of a bounded number of
//! **pending** places, which authenticated connections never use, so handshakes that never
//! authenticate cannot take the capacity of peers that have. Once its certificate names a peer it is
//! charged to that **identity**:
//!
//! - an identity holds at most `per_identity` connections; one past the bound replaces the one of
//!   that identity used longest ago, which is closed. A peer that restarted or moved leaves a
//!   connection behind until its idle timeout; refusing the newcomer would make it wait that out;
//! - the connections held in all are bounded by `connections`, met after the replacement rule,
//!   never before it: an identity at its own bound reaches its replacement however full the
//!   endpoint is, and only a connection that would be one more is refused (F20);
//! - the identities are bounded, and an identity past its own bound displaces only itself.
//!
//! Every refusal is typed and counted. focal's fixed bounds (4 per node, 16 per participant) are
//! excluded (note 32 T9): every bound here is the owner's configuration.

use crate::{PeerId, Refusal};

/// The bounds of admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionLimits {
    /// Inbound handshakes in progress.
    pub pending: usize,
    /// Identities holding a connection.
    pub identities: usize,
    /// Connections held in all, once authenticated, and dials in progress.
    pub connections: usize,
    /// Connections one identity holds.
    pub per_identity: usize,
}

impl AdmissionLimits {
    fn valid(&self) -> bool {
        self.pending > 0 && self.identities > 0 && self.connections > 0 && self.per_identity > 0
    }
}

/// What admission holds, and what it refused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdmissionStats {
    /// Handshakes in progress.
    pub pending: usize,
    /// Identities holding connections.
    pub identities: usize,
    /// Connections held.
    pub connections: usize,
    /// Connections admitted so far.
    pub admitted: u64,
    /// Connections that replaced one of their identity's.
    pub replaced: u64,
    /// Handshakes refused a pending place.
    pub refused_pending: u64,
    /// Connections refused for the identity bound.
    pub refused_identities: u64,
    /// Connections refused for the connection bound.
    pub refused_connections: u64,
    /// Handshakes whose certificate named no peer.
    pub refused_identity: u64,
}

/// A connection, as admission knows it: the endpoint's key for it.
pub(crate) type ConnectionKey = usize;

#[derive(Debug)]
struct Identity {
    peer: PeerId,
    /// The identity's connections and when each was last used, by the endpoint's counter.
    held: Vec<(ConnectionKey, u64)>,
}

/// One endpoint's admission.
#[derive(Debug)]
pub(crate) struct Admission {
    limits: AdmissionLimits,
    /// Sorted by peer.
    identities: Vec<Identity>,
    stats: AdmissionStats,
}

fn bump(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

impl Admission {
    pub(crate) fn new(limits: AdmissionLimits) -> Result<Self, Refusal> {
        if !limits.valid() {
            return Err(Refusal::Configuration);
        }
        Ok(Self {
            limits,
            identities: Vec::with_capacity(limits.identities),
            stats: AdmissionStats::default(),
        })
    }
    pub(crate) fn limits(&self) -> AdmissionLimits {
        self.limits
    }
    pub(crate) fn stats(&self) -> AdmissionStats {
        AdmissionStats {
            identities: self.identities.len(),
            ..self.stats
        }
    }
    /// A place for one inbound handshake, held until it authenticates or ends.
    pub(crate) fn begin(&mut self) -> Result<(), Refusal> {
        if self.stats.pending >= self.limits.pending {
            bump(&mut self.stats.refused_pending);
            return Err(Refusal::Pending);
        }
        self.stats.pending = self.stats.pending.saturating_add(1);
        Ok(())
    }
    /// An inbound handshake's place given back.
    pub(crate) fn end_pending(&mut self) {
        self.stats.pending = self.stats.pending.saturating_sub(1);
    }
    /// Whether one more connection may be dialed: dials count against the connection bound.
    pub(crate) fn may_dial(&self, dialing: usize) -> Result<(), Refusal> {
        if self.stats.connections.saturating_add(dialing) >= self.limits.connections {
            return Err(Refusal::Connections);
        }
        Ok(())
    }
    /// A certificate named no peer.
    pub(crate) fn unidentified(&mut self) {
        bump(&mut self.stats.refused_identity);
    }
    /// Charge `connection` to `peer` at `tick`. Returns the connection of that identity this one
    /// replaces, which the caller closes.
    pub(crate) fn admit(
        &mut self,
        peer: PeerId,
        connection: ConnectionKey,
        tick: u64,
    ) -> Result<Option<ConnectionKey>, Refusal> {
        let found = self
            .identities
            .binary_search_by_key(&peer, |identity| identity.peer);
        let held = found
            .ok()
            .and_then(|at| self.identities.get(at))
            .map_or(0, |identity| identity.held.len());
        if held == 0 && found.is_err() && self.identities.len() >= self.limits.identities {
            bump(&mut self.stats.refused_identities);
            return Err(Refusal::Identities);
        }
        // The replacement rule first: an identity at its bound takes its own place back however
        // full the endpoint is.
        let replacing = held >= self.limits.per_identity;
        if !replacing && self.stats.connections >= self.limits.connections {
            bump(&mut self.stats.refused_connections);
            return Err(Refusal::Connections);
        }
        let at = match found {
            Ok(at) => at,
            Err(at) => {
                let identity = Identity {
                    peer,
                    held: Vec::with_capacity(self.limits.per_identity),
                };
                self.identities.insert(at, identity);
                at
            }
        };
        let identity = self.identities.get_mut(at).ok_or(Refusal::Identities)?;
        let replaced = replacing.then(|| least_used(&mut identity.held)).flatten();
        identity.held.push((connection, tick));
        if replaced.is_some() {
            bump(&mut self.stats.replaced);
        } else {
            self.stats.connections = self.stats.connections.saturating_add(1);
        }
        bump(&mut self.stats.admitted);
        Ok(replaced)
    }
    /// The connections charged to `peer`.
    pub(crate) fn connections_of(&self, peer: PeerId) -> impl Iterator<Item = ConnectionKey> + '_ {
        self.identities
            .binary_search_by_key(&peer, |identity| identity.peer)
            .ok()
            .and_then(|at| self.identities.get(at))
            .into_iter()
            .flat_map(|identity| identity.held.iter().map(|(connection, _)| *connection))
    }
    /// `connection` of `peer` was used at `tick`.
    pub(crate) fn used(&mut self, peer: PeerId, connection: ConnectionKey, tick: u64) {
        let Ok(at) = self
            .identities
            .binary_search_by_key(&peer, |identity| identity.peer)
        else {
            return;
        };
        if let Some(entry) = self.identities.get_mut(at).and_then(|identity| {
            identity
                .held
                .iter_mut()
                .find(|(held, _)| *held == connection)
        }) {
            entry.1 = tick;
        }
    }
    /// `connection` of `peer` ended: its charge is given back.
    pub(crate) fn release(&mut self, peer: PeerId, connection: ConnectionKey) {
        let Ok(at) = self
            .identities
            .binary_search_by_key(&peer, |identity| identity.peer)
        else {
            return;
        };
        let Some(identity) = self.identities.get_mut(at) else {
            return;
        };
        let before = identity.held.len();
        identity.held.retain(|(held, _)| *held != connection);
        let removed = before.saturating_sub(identity.held.len());
        self.stats.connections = self.stats.connections.saturating_sub(removed);
        if identity.held.is_empty() {
            self.identities.remove(at);
        }
    }
}

/// Remove and return the connection used longest ago.
fn least_used(held: &mut Vec<(ConnectionKey, u64)>) -> Option<ConnectionKey> {
    let at = held
        .iter()
        .enumerate()
        .min_by_key(|(_, (_, used))| *used)
        .map(|(at, _)| at)?;
    Some(held.remove(at).0)
}

#[cfg(test)]
mod tests {
    //! focal `tests.rs` `mod admission`, on the admission alone; the same rules over real
    //! connections are in `tests/exchange.rs`.
    use super::*;

    fn bounds() -> AdmissionLimits {
        AdmissionLimits {
            pending: 8,
            identities: 8,
            connections: 16,
            per_identity: 2,
        }
    }

    #[test]
    fn pending_places_are_bounded_and_given_back() {
        assert_eq!(
            Admission::new(AdmissionLimits {
                pending: 0,
                ..bounds()
            })
            .err(),
            Some(Refusal::Configuration)
        );
        let mut admission = Admission::new(AdmissionLimits {
            pending: 2,
            ..bounds()
        })
        .unwrap();
        admission.begin().unwrap();
        admission.begin().unwrap();
        assert_eq!(admission.begin(), Err(Refusal::Pending));
        assert_eq!(
            (admission.stats().pending, admission.stats().refused_pending),
            (2, 1)
        );
        admission.end_pending();
        admission.begin().unwrap();
        assert_eq!(admission.begin(), Err(Refusal::Pending));
        admission.end_pending();
        admission.end_pending();
        assert_eq!(
            (admission.stats().pending, admission.stats().connections),
            (0, 0)
        );
    }

    /// The audit's F20: with the endpoint full, an identity at its own bound still reaches its
    /// replacement; only a connection that would be one more is refused.
    #[test]
    fn a_full_endpoint_still_replaces_an_identity_s_own_connection() {
        let mut admission = Admission::new(AdmissionLimits {
            connections: 3,
            ..bounds()
        })
        .unwrap();
        assert_eq!(admission.admit(1, 10, 1), Ok(None));
        assert_eq!(admission.admit(1, 11, 2), Ok(None));
        assert_eq!(admission.admit(2, 20, 3), Ok(None));
        assert_eq!(admission.admit(2, 21, 4), Err(Refusal::Connections));
        assert_eq!(admission.stats().refused_connections, 1);
        // Identity 1 at its bound replaces the connection it used least, though the endpoint is
        // full.
        admission.used(1, 10, 5);
        assert_eq!(admission.admit(1, 12, 6), Ok(Some(11)));
        let stats = admission.stats();
        assert_eq!(
            (stats.connections, stats.replaced, stats.admitted),
            (3, 1, 4)
        );
    }

    #[test]
    fn an_identity_past_its_bound_loses_the_connection_it_used_least() {
        let mut admission = Admission::new(bounds()).unwrap();
        admission.admit(1, 10, 1).unwrap();
        admission.admit(1, 11, 2).unwrap();
        // The first is used after the second was opened: the second is now the one used least.
        admission.used(1, 10, 3);
        assert_eq!(admission.admit(1, 12, 4), Ok(Some(11)));
        // Clients that leave without closing: one short-lived client after another is always
        // admitted, each replacing what the last left behind.
        for round in 0..40u64 {
            let replaced = admission
                .admit(1, 100 + usize::try_from(round).unwrap(), 10 + round)
                .unwrap();
            assert!(replaced.is_some(), "round {round}");
        }
        let stats = admission.stats();
        assert_eq!(
            (stats.identities, stats.connections, stats.replaced),
            (1, 2, 41)
        );
    }

    #[test]
    fn identities_are_bounded_and_one_identity_cannot_take_another_s_place() {
        let mut admission = Admission::new(AdmissionLimits {
            identities: 2,
            ..bounds()
        })
        .unwrap();
        for connection in 0..5 {
            admission
                .admit(1, connection, u64::try_from(connection).unwrap())
                .unwrap();
        }
        admission.admit(2, 50, 9).unwrap();
        let stats = admission.stats();
        assert_eq!(
            (stats.identities, stats.replaced, stats.connections),
            (2, 3, 3)
        );
        assert_eq!(admission.admit(3, 60, 10), Err(Refusal::Identities));
        assert_eq!(admission.stats().refused_identities, 1);
        // An identity that leaves frees its place.
        admission.release(2, 50);
        assert_eq!(admission.stats().identities, 1);
        assert_eq!(admission.admit(3, 60, 11), Ok(None));
        assert_eq!(admission.stats().connections, 3);
    }

    #[test]
    fn dials_count_against_the_connection_bound() {
        let mut admission = Admission::new(AdmissionLimits {
            connections: 2,
            ..bounds()
        })
        .unwrap();
        admission.may_dial(1).unwrap();
        assert_eq!(admission.may_dial(2), Err(Refusal::Connections));
        admission.admit(1, 1, 1).unwrap();
        assert_eq!(admission.may_dial(1), Err(Refusal::Connections));
    }
}
