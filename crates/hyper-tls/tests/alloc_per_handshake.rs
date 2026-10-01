//! The allocations one handshake makes, client and server together, counted by a counting
//! allocator. Each count is printed for comparison with the same test run against 526c2cc, where
//! configurations were shared by `Arc` (the commit message records both).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io;

use hyper_tls::pki_types::pem::PemObject;
use hyper_tls::pki_types::{CertificateDer, PrivateKeyDer};
use hyper_tls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static REALLOCS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

fn note(counter: &'static std::thread::LocalKey<Cell<u64>>, by: u64) {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = counter.try_with(|c| c.set(c.get() + by));
    }
}

// SAFETY: every method forwards to the system allocator unchanged; the counters are thread-local
// cells that never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(&ALLOCS, 1);
        note(&BYTES, layout.size() as u64);
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(&ALLOCS, 1);
        note(&BYTES, layout.size() as u64);
        // SAFETY: as `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as `alloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(&REALLOCS, 1);
        // SAFETY: as `alloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    allocs: u64,
    reallocs: u64,
    bytes: u64,
}

fn measure<R>(f: impl FnOnce() -> R) -> (R, Counts) {
    for c in [&ALLOCS, &REALLOCS, &BYTES] {
        c.with(|c| c.set(0));
    }
    COUNTING.with(|c| c.set(true));
    let r = f();
    COUNTING.with(|c| c.set(false));
    let counts = Counts {
        allocs: ALLOCS.with(Cell::get),
        reallocs: REALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
    };
    (r, counts)
}

const CHAIN: &[u8] = include_bytes!("../test-ca/ecdsa-p256/end.fullchain");
const KEY: &[u8] = include_bytes!("../test-ca/ecdsa-p256/end.key");
const CA: &[u8] = include_bytes!("../test-ca/ecdsa-p256/ca.cert");

fn server_config(versions: &[&'static hyper_tls::SupportedProtocolVersion]) -> ServerConfig {
    let chain = CertificateDer::pem_slice_iter(CHAIN)
        .map(Result::unwrap)
        .collect();
    let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
    ServerConfig::builder_with_protocol_versions(versions)
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap()
}

fn client_config(versions: &[&'static hyper_tls::SupportedProtocolVersion]) -> ClientConfig {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(CA).unwrap())
        .unwrap();
    ClientConfig::builder_with_protocol_versions(versions)
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Moves everything `from` has to send into `to`, through a stack buffer.
fn transfer(
    from: &mut impl std::ops::DerefMut<Target = hyper_tls::ConnectionCommon<impl hyper_tls::SideData>>,
    to: &mut impl std::ops::DerefMut<Target = hyper_tls::ConnectionCommon<impl hyper_tls::SideData>>,
) {
    let mut buf = [0u8; 65536];
    while from.wants_write() {
        let n = from.write_tls(&mut &mut buf[..]).unwrap();
        let mut offs = 0;
        while offs < n {
            offs += to.read_tls(&mut io::Cursor::new(&buf[offs..n])).unwrap();
        }
    }
}

fn handshake(client_config: &mut ClientConfig, server_config: &mut ServerConfig) {
    let mut client =
        ClientConnection::new(client_config, "testserver.com".try_into().unwrap()).unwrap();
    let mut server = ServerConnection::new(server_config).unwrap();
    while client.is_handshaking()
        || server.is_handshaking()
        || client.wants_write()
        || server.wants_write()
    {
        transfer(&mut client, &mut server);
        server.process_new_packets(server_config).unwrap();
        transfer(&mut server, &mut client);
        client.process_new_packets(client_config).unwrap();
    }
}

fn report(name: &str, versions: &[&'static hyper_tls::SupportedProtocolVersion]) {
    // One-time initialisation (aws-lc, the process-default provider) is paid outside the count.
    handshake(&mut client_config(versions), &mut server_config(versions));

    let mut client = client_config(versions);
    let mut server = server_config(versions);
    let ((), full) = measure(|| handshake(&mut client, &mut server));
    let ((), resumed) = measure(|| handshake(&mut client, &mut server));
    eprintln!("hyper-tls {name} full handshake, client and server: {full:?}");
    eprintln!("hyper-tls {name} resumed handshake, client and server: {resumed:?}");
}

#[test]
fn allocations_per_handshake() {
    report("TLS 1.3", &[&hyper_tls::version::TLS13]);
    report("TLS 1.2", &[&hyper_tls::version::TLS12]);
}
