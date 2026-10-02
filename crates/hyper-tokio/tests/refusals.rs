//! The adapter's typed refusals: a misuse is an error returned, never a panic that escapes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_macros,
    missing_docs
)]

#[path = "../../hyper-transport/tests/common/mod.rs"]
mod common;

use std::net::SocketAddr;

use common::*;
use hyper_tokio::{Driver, Error, Io, MAX_BATCH, PlaneSocket};

fn any() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn node() -> Node<Mantle> {
    let pair = Pair::new();
    pair.node::<Mantle>(
        1,
        Role::Node,
        pair.book(Role::Node, Role::Node),
        limits(),
        1 << 20,
        hyper_sim::Anchor::new().instant(0).unwrap(),
    )
}

#[test]
fn outside_a_runtime_the_driver_is_refused() {
    let refused = Driver::bind(node(), any(), Io { batch: 8 }).err();
    assert_eq!(refused, Some(Error::Runtime));
    assert_eq!(
        PlaneSocket::bind(any(), Io { batch: 8 }).err(),
        Some(Error::Runtime)
    );
}

#[test]
fn a_runtime_without_its_timer_is_refused() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    let refused = runtime.block_on(async { Driver::bind(node(), any(), Io { batch: 8 }).err() });
    assert_eq!(refused, Some(Error::Runtime));
}

#[test]
fn a_batch_out_of_range_is_refused() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for batch in [0, MAX_BATCH + 1] {
            assert_eq!(
                Driver::bind(node(), any(), Io { batch }).err(),
                Some(Error::Configuration)
            );
            assert_eq!(
                PlaneSocket::bind(any(), Io { batch }).err(),
                Some(Error::Configuration)
            );
        }
        assert!(Driver::bind(node(), any(), Io { batch: MAX_BATCH }).is_ok());
    });
}
