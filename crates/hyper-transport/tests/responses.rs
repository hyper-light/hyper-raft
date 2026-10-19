//! The endpoint's own responses (version negotiation, retries, refusals, stateless resets): a burst
//! of dials past a listener's handshake places is answered whole in the turn its datagrams arrive,
//! and the room for the responses is taken as they come, never laid out at the bound.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    missing_docs
)]

mod common;

use common::*;
use hyper_measure::alloc;
use hyper_transport::tls::Tls;
use hyper_transport::{AdmissionLimits, Config, Endpoint, Event, Fixed, Limits, PeerId};

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting;

/// The listener the dialers dial.
const LISTENER: PeerId = 1;
/// The dialers, each its own node: as many as focal's enrollment test sends a sponsor at once.
const DIALERS: PeerId = 8;

/// The listener's bounds: one handshake at a time among four connections, as focal's enrollment
/// sponsor in its joiners' test (a quarter of its connections), and `max_responses`.
fn listener_limits(max_responses: usize) -> Limits {
    Limits {
        admission: AdmissionLimits {
            pending: 1,
            identities: 4,
            connections: 4,
            per_identity: 1,
        },
        max_responses,
        ..limits()
    }
}

/// The listener and its dialers on hyper-sim's network, each knowing every other.
fn burst(max_responses: usize) -> Mesh<Node<Mantle>> {
    let issued = Issued::new(DIALERS + 1);
    let now = hyper_sim::Anchor::new().instant(0).unwrap();
    let nodes: Vec<Node<Mantle>> = issued
        .issued
        .iter()
        .enumerate()
        .map(|(at, (certificate, key))| {
            let peer = at as PeerId + 1;
            let config = Config {
                tls: Tls::mutual(
                    &common::credentials(&issued.pki.root, certificate, key),
                    true,
                )
                .unwrap(),
                role: Role::Node,
                limits: if peer == LISTENER {
                    listener_limits(max_responses)
                } else {
                    limits()
                },
                congestion: Default::default(),
            };
            Endpoint::new_seeded(
                config,
                Fixed::new(256 << 20, 64),
                issued.book(),
                now,
                seed_of(peer),
            )
            .unwrap()
        })
        .collect();
    Mesh::new(nodes, now, hyper_sim::Source::Seed(0))
}

/// The dialers that heard how their dial ended: up, or refused.
fn answered(mesh: &mut Mesh<Node<Mantle>>) -> PeerId {
    let mut answered = 0;
    for dialer in LISTENER + 1..=LISTENER + DIALERS {
        let node = mesh.node(dialer);
        let mut heard = false;
        while let Some(event) = node.poll_event() {
            heard |= matches!(
                event,
                Event::Connected { peer: LISTENER, .. }
                    | Event::Unreachable { peer: LISTENER, .. }
                    | Event::Closed { peer: LISTENER, .. }
            );
        }
        answered += PeerId::from(heard);
    }
    answered
}

/// Do: eight dialers dial a listener with one handshake place at once, the network moving every
/// datagram with no time passing, so no probe timeout fires. Expect: every dialer hears how its
/// dial ended, and the listener dropped no response. Its responses each answer one datagram it
/// took since it last sent, so a listener whose bound is what one receive takes answers a whole
/// burst: with a bound of one, a dialer whose retry was dropped heard nothing until its probe
/// timeout, as focal's joiners did, which failed their joins.
#[test]
fn a_burst_of_dials_past_the_handshake_places_is_answered_before_any_probe_timeout() {
    // At most two datagrams a dialer's first flight: a hybrid key share's ClientHello.
    let mut mesh = burst(2 * DIALERS as usize);
    let now = mesh.now;
    for dialer in LISTENER + 1..=LISTENER + DIALERS {
        mesh.node(dialer)
            .connect(now, LISTENER, mesh_address(LISTENER))
            .unwrap();
    }
    mesh.exchange();
    assert_eq!(
        answered(&mut mesh),
        DIALERS,
        "a dialer heard nothing of its dial before any probe timeout"
    );
    assert_eq!(mesh.node(LISTENER).stats().responses_dropped, 0);
}

/// Do: make the same listener with a bound of one response and of 4,096, counting what each
/// allocates, after one made first. Expect: the same bytes: the room for responses is taken as
/// they come.
#[test]
fn a_listener_lays_out_no_room_for_responses_it_has_not_made() {
    let made = |max_responses: usize| {
        let issued = Issued::new(1);
        let (certificate, key) = &issued.issued[0];
        let now = hyper_sim::Anchor::new().instant(0).unwrap();
        let config = Config {
            tls: Tls::mutual(
                &common::credentials(&issued.pki.root, certificate, key),
                true,
            )
            .unwrap(),
            role: Role::Node,
            limits: listener_limits(max_responses),
            congestion: Default::default(),
        };
        let book = issued.book();
        alloc::begin();
        let node: Node<Mantle> = Endpoint::new_seeded(
            config,
            Fixed::new(256 << 20, 64),
            book,
            now,
            seed_of(LISTENER),
        )
        .unwrap();
        let counts = alloc::end();
        drop(node);
        counts.bytes
    };
    assert!(
        alloc::installed(),
        "the counting allocator is not installed"
    );
    // The first makes what a process makes once (the TLS provider's tables among them).
    made(1);
    assert_eq!(made(1), made(1 << 12));
}
