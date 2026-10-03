//! SWIM/Lifeguard membership (§4.8, Part 4 "Cluster plane") — the failure-detection and membership
//! view every node maintains, the input to the configuration group's neighbourhood and to placement.
//! Degenerate on a laptop: one member, the local node, always alive.
//!
//! Evidence: SWIM (Das, Gupta, Motivala — *SWIM: Scalable Weakly-consistent Infection-style Process
//! Group Membership Protocol*, DSN 2002; tier A) and Lifeguard (Dadgar, Phillips, Currey — *Lifeguard:
//! Local Health Awareness for More Accurate Failure Detection*, DSN/ISSRE 2018; tier A). This slice is
//! the **membership view and its incarnation-based update merge** — the weakly-consistent, CRDT-like
//! state SWIM disseminates by gossip — with **self-refutation**: a node that hears itself suspected or
//! declared dead raises its incarnation and re-asserts that it is alive, so a false suspicion cannot
//! stick. The failure detector (the ping / ping-request / suspicion-timer machine) and gossip
//! dissemination (piggybacking updates on pings, the Lifeguard local-health multiplier) are the next
//! slices; they drive this view and ride the control plane's datagrams.
//!
//! The merge is a pure, deterministic state machine — no clock, no I/O — so it is oracle-tested at
//! N=1 and over a simulated exchange before any timer or datagram is involved.
//!
//! **The view is bounded** by the owner's placement: how many hosts this node can know, itself
//! included ([`Membership::capacity`]). An update about a member past it is refused, typed
//! ([`Full`]), whoever names it. A dead member's record stays until the detector forgets it
//! ([`Membership::forget`]), once gossip of it from before its death can no longer arrive
//! (`docs/timing.md` §2.7): a record that went earlier would let such gossip, at an incarnation at or
//! below the death's, add the member back.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

use crate::HostId;

/// The view's refusal of an update about a member it does not hold: it holds its bound of
/// members, this one included ([`Membership::capacity`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// A member's liveness in the SWIM sense. The override order **at equal incarnation** is
/// `Alive < Suspect < Dead`: a suspicion overrides a same-incarnation alive belief, a death overrides
/// both, and an alive belief never overrides a same-incarnation suspicion (only a higher incarnation,
/// which the member itself asserts, refutes a suspicion).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Believed healthy.
    Alive,
    /// Suspected failed (a ping went unanswered); still a member until confirmed.
    Suspect,
    /// Confirmed failed; to be removed from the neighbourhood.
    Dead,
}

impl Liveness {
    /// The override rank at equal incarnation (higher wins): Alive 0, Suspect 1, Dead 2.
    fn rank(self) -> u8 {
        match self {
            Liveness::Alive => 0,
            Liveness::Suspect => 1,
            Liveness::Dead => 2,
        }
    }
}

/// A member's state: its liveness and the **incarnation** it was asserted under. Incarnation numbers
/// are the tie-breaker SWIM uses so a member can refute a stale suspicion — a member raises its own
/// incarnation and re-asserts `Alive`, which outranks any suspicion carried at a lower incarnation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemberState {
    /// The believed liveness.
    pub liveness: Liveness,
    /// The incarnation this belief is under.
    pub incarnation: u64,
}

/// What applying a gossiped update changed in the local view — for the caller to gossip onward or act
/// on (a `Dead` prompts removal from the neighbourhood; a `Refuted` must be gossiped so the fleet
/// learns the member is alive again).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// A member's state was adopted as this.
    Adopted {
        /// The member whose state changed.
        member: HostId,
        /// The state now believed.
        state: MemberState,
    },
    /// The local node refuted a suspicion or death about itself, raising to this incarnation and
    /// re-asserting that it is alive.
    Refuted {
        /// The local node's new incarnation.
        incarnation: u64,
    },
}

/// A node's SWIM membership view: itself and every other member it knows, each with a liveness and an
/// incarnation. The view converges by applying gossiped updates under the incarnation/override merge.
pub struct Membership {
    local: HostId,
    local_incarnation: u64,
    members: BTreeMap<HostId, MemberState>,
    /// The members `members` holds suspected, so the detector ages them each period without
    /// scanning the membership.
    suspected: BTreeSet<HostId>,
    /// The most members the view holds, this one included: the owner's placement.
    capacity: NonZeroUsize,
}

impl Membership {
    /// A view of a lone node — itself, alive, incarnation zero (the laptop degenerate) — that holds at
    /// most `capacity` members, itself included.
    pub fn new(local: HostId, capacity: NonZeroUsize) -> Membership {
        let mut members = BTreeMap::new();
        members.insert(
            local,
            MemberState {
                liveness: Liveness::Alive,
                incarnation: 0,
            },
        );
        Membership {
            local,
            local_incarnation: 0,
            members,
            suspected: BTreeSet::new(),
            capacity,
        }
    }

    /// Applies a gossiped `update` about `subject`, returning the change it made or `None` if the update
    /// was stale (a lower incarnation, or an equal incarnation that does not override). An update about
    /// the **local** node that would suspect or declare it dead — at an incarnation the local node has
    /// reached — is refuted: the local node raises its incarnation past the suspicion and re-asserts
    /// `Alive` (SWIM self-refutation), so a false suspicion cannot persist. An update about a member
    /// the view does not hold, while it holds its bound, is refused.
    pub fn apply(&mut self, subject: HostId, update: MemberState) -> Result<Option<Change>, Full> {
        if subject == self.local {
            return Ok(self.refute(update));
        }
        let overrides = match self.members.get(&subject) {
            None if self.members.len() >= self.capacity.get() => return Err(Full),
            None => true,
            Some(current) => {
                update.incarnation > current.incarnation
                    || (update.incarnation == current.incarnation
                        && update.liveness.rank() > current.liveness.rank())
            }
        };
        if !overrides {
            return Ok(None);
        }
        self.members.insert(subject, update);
        if update.liveness == Liveness::Suspect {
            self.suspected.insert(subject);
        } else {
            self.suspected.remove(&subject);
        }
        Ok(Some(Change::Adopted {
            member: subject,
            state: update,
        }))
    }

    /// Forgets `member`, held dead: the view holds it no more, and an update about it is a
    /// newcomer's. A member alive or suspected, and this one, are never forgotten. Whether it was.
    pub fn forget(&mut self, member: HostId) -> bool {
        let dead = member != self.local
            && self
                .members
                .get(&member)
                .is_some_and(|state| state.liveness == Liveness::Dead);
        if dead {
            self.members.remove(&member);
        }
        dead
    }

    /// The most members the view holds, this one included.
    pub fn capacity(&self) -> usize {
        self.capacity.get()
    }

    /// Refutes a suspicion or death about the local node: if the `update` is not `Alive` and reaches the
    /// local incarnation, raise the incarnation past it and re-assert alive. An alive update about
    /// ourselves, or a stale one below our incarnation, changes nothing.
    fn refute(&mut self, update: MemberState) -> Option<Change> {
        if update.liveness == Liveness::Alive || update.incarnation < self.local_incarnation {
            return None;
        }
        self.local_incarnation = update.incarnation.saturating_add(1);
        self.members.insert(
            self.local,
            MemberState {
                liveness: Liveness::Alive,
                incarnation: self.local_incarnation,
            },
        );
        Some(Change::Refuted {
            incarnation: self.local_incarnation,
        })
    }

    /// The members currently believed alive (the local node included), in id order — the neighbourhood
    /// the configuration group and placement draw from.
    pub fn alive(&self) -> impl Iterator<Item = HostId> + '_ {
        self.members
            .iter()
            .filter(|(_, state)| state.liveness == Liveness::Alive)
            .map(|(&host, _)| host)
    }

    /// The members believed dead, in id order: a scan of the membership, for a member that has
    /// nobody else left to probe.
    pub fn dead(&self) -> impl Iterator<Item = HostId> + '_ {
        self.members
            .iter()
            .filter(|(_, state)| state.liveness == Liveness::Dead)
            .map(|(&host, _)| host)
    }

    /// The members known, the local node included, whatever their liveness.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether no member is known: never, as the local node always is.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The known state of `member`, if any.
    pub fn state(&self, member: HostId) -> Option<MemberState> {
        self.members.get(&member).copied()
    }

    /// The members currently suspected, with their incarnations — the set the failure detector ages
    /// toward death (each still a member until confirmed dead or refuted).
    pub fn suspects(&self) -> impl Iterator<Item = (HostId, u64)> + '_ {
        self.suspected.iter().filter_map(|&host| {
            self.members
                .get(&host)
                .map(|state| (host, state.incarnation))
        })
    }

    /// The local node's current incarnation.
    pub fn local_incarnation(&self) -> u64 {
        self.local_incarnation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: HostId = HostId(1);
    const PEER: HostId = HostId(2);

    fn state(liveness: Liveness, incarnation: u64) -> MemberState {
        MemberState {
            liveness,
            incarnation,
        }
    }

    /// A view with room for every member a test names.
    fn view() -> Membership {
        Membership::new(LOCAL, NonZeroUsize::new(16).unwrap())
    }

    /// A never-seen member is adopted; a higher incarnation wins and a lower one is ignored.
    #[test]
    fn a_higher_incarnation_wins_and_a_lower_is_ignored() {
        let mut view = view();
        assert_eq!(
            view.apply(PEER, state(Liveness::Alive, 5)),
            Ok(Some(Change::Adopted {
                member: PEER,
                state: state(Liveness::Alive, 5)
            })),
            "a new member is adopted"
        );
        // A higher incarnation is adopted even if it is only alive-vs-alive.
        assert!(
            view.apply(PEER, state(Liveness::Alive, 6))
                .unwrap()
                .is_some()
        );
        assert_eq!(view.state(PEER).unwrap().incarnation, 6);
        // A lower incarnation is stale — ignored.
        assert_eq!(view.apply(PEER, state(Liveness::Dead, 5)), Ok(None));
        assert_eq!(view.state(PEER).unwrap().liveness, Liveness::Alive);
    }

    /// At equal incarnation the override order holds: Suspect over Alive, Dead over Suspect, and Alive
    /// never over a same-incarnation Suspect.
    #[test]
    fn the_override_order_holds_at_equal_incarnation() {
        let mut view = view();
        view.apply(PEER, state(Liveness::Alive, 3)).unwrap();
        assert!(
            view.apply(PEER, state(Liveness::Suspect, 3))
                .unwrap()
                .is_some(),
            "suspect overrides alive at the same incarnation"
        );
        assert_eq!(
            view.apply(PEER, state(Liveness::Alive, 3)),
            Ok(None),
            "alive does not override a same-incarnation suspicion"
        );
        assert!(
            view.apply(PEER, state(Liveness::Dead, 3))
                .unwrap()
                .is_some(),
            "dead overrides suspect at the same incarnation"
        );
        assert_eq!(view.state(PEER).unwrap().liveness, Liveness::Dead);
    }

    /// The local node refutes a suspicion about itself: it raises its incarnation past the suspicion and
    /// re-asserts alive, and a suspicion below its incarnation is ignored.
    #[test]
    fn the_local_node_refutes_a_suspicion_about_itself() {
        let mut view = view();
        assert_eq!(view.local_incarnation(), 0);
        assert_eq!(
            view.apply(LOCAL, state(Liveness::Suspect, 0)),
            Ok(Some(Change::Refuted { incarnation: 1 })),
            "a suspicion at the local incarnation is refuted with a higher one"
        );
        assert_eq!(view.state(LOCAL).unwrap(), state(Liveness::Alive, 1));
        // A stale suspicion (below the refuted incarnation) changes nothing.
        assert_eq!(view.apply(LOCAL, state(Liveness::Suspect, 0)), Ok(None));
        assert_eq!(view.local_incarnation(), 1);
    }

    /// The alive set reflects the merge — the local node plus alive peers, and not the dead.
    #[test]
    fn the_alive_set_reflects_the_view() {
        let mut view = view();
        view.apply(PEER, state(Liveness::Alive, 1)).unwrap();
        view.apply(HostId(3), state(Liveness::Dead, 1)).unwrap();
        assert_eq!(
            view.alive().collect::<Vec<_>>(),
            vec![LOCAL, PEER],
            "the dead member is excluded"
        );
    }

    /// The view holds its bound and no more: an update about one more member is refused, whoever
    /// names it, while updates about the members it holds, itself included, still apply.
    #[test]
    fn a_member_past_the_bound_is_refused_and_one_held_is_not() {
        let mut view = Membership::new(LOCAL, NonZeroUsize::new(3).unwrap());
        assert!(view.apply(PEER, state(Liveness::Alive, 0)).is_ok());
        assert!(view.apply(HostId(3), state(Liveness::Dead, 0)).is_ok());
        assert_eq!(view.apply(HostId(4), state(Liveness::Alive, 0)), Err(Full));
        assert_eq!(view.apply(HostId(4), state(Liveness::Dead, 0)), Err(Full));
        assert_eq!((view.len(), view.capacity()), (3, 3));
        assert!(
            view.apply(PEER, state(Liveness::Suspect, 0))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            view.apply(LOCAL, state(Liveness::Dead, 0)),
            Ok(Some(Change::Refuted { incarnation: 1 }))
        );
    }

    /// Only a dead member is forgotten, never this one, and its place takes a newcomer.
    #[test]
    fn only_a_dead_member_is_forgotten() {
        let mut view = Membership::new(LOCAL, NonZeroUsize::new(3).unwrap());
        view.apply(PEER, state(Liveness::Alive, 0)).unwrap();
        view.apply(HostId(3), state(Liveness::Dead, 2)).unwrap();
        assert!(!view.forget(PEER), "alive");
        assert!(!view.forget(LOCAL), "this one");
        assert!(!view.forget(HostId(9)), "never held");
        assert!(view.forget(HostId(3)));
        assert_eq!(view.state(HostId(3)), None);
        assert!(view.dead().next().is_none());
        assert!(view.apply(HostId(4), state(Liveness::Alive, 0)).is_ok());
    }
}
