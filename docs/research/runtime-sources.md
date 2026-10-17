# Runtime sources read at named revisions (2026-10-17)

`docs/runtime.md` §15 items 1, 2 and 5: three sources the runtime's design cited unread, or read at no
named revision. Each is read here at a fixed revision, quoted where the design rests on it, and what
the design had wrong is named.

## 1. `TCP_NOTSENT_LOWAT`, Linux against XNU

Read at Linux `v6.12` (`net/ipv4/tcp_ipv4.c`, `include/net/sock.h`, `net/core/stream.c`,
`net/ipv4/tcp.c`) and XNU `xnu-12377.121.6` (`bsd/netinet/tcp_usrreq.c`, `bsd/netinet/tcp_subr.c`,
`bsd/kern/uipc_socket.c`, `bsd/kern/uipc_socket2.c`), both from their public repositories.

**What counts as unsent.**
- Linux: `notsent_bytes = tp->write_seq - tp->snd_nxt`, the bytes queued and never sent
  (`tcp_stream_memory_free`, `tcp_ipv4.c`).
- XNU: `notsent = so->so_snd.sb_cc - (tp->snd_nxt - tp->snd_una)`, the send buffer less what is in
  flight (`tcp_notsent_lowat_check`, `tcp_subr.c`). A FIN or SYN can make it negative, which counts as
  writable.

Both count the same bytes: what the application gave and the connection has not yet put on the wire.

**When the socket is writable.**
- Linux: `tcp_poll` reports `EPOLLOUT` when `__sk_stream_is_writeable(sk, 1)`: free send space of at
  least half of what is queued (`sk_stream_wspace >= sk_stream_min_wspace`, `sk_wmem_queued >> 1`), the
  queue under `sk_sndbuf`, and `(notsent_bytes << 1) < tcp_notsent_lowat(tp)`. The wake on an acknowledgement
  (`sk_stream_write_space`) takes the same test. Writability therefore comes once the unsent bytes are
  under **half** the mark, strictly.
- XNU: `sowriteable` (select and poll) and `filt_sowrite` (kqueue) take the send space against
  `SO_SNDLOWAT` first, then, with `SOF_NOTSENT_LOWAT` set, `tcp_notsent_lowat_check`: writable when
  `notsent <= tp->t_notsent_lowat`, or when Nagle is on (`TF_NODELAY` clear) and
  `0 < notsent < t_maxseg` ("it is better to wakeup the client until there is atleast one maxseg of data
  to write"). Writability comes once the unsent bytes are **at or under** the mark.

**Whether a write is held to it.** This is where the two differ in kind.
- Linux: `tcp_sendmsg_locked` checks `sk_stream_memory_free(sk)`, the same test without the halving
  (`notsent_bytes < lowat`), before it starts each new segment, and waits for space (or returns
  `EAGAIN` on a non-blocking socket) when it fails. So the kernel holds at most about the mark of
  unsent data, past it by at most the segment it was filling.
- XNU: `sosendcheck` tests only `sbspace(&so->so_snd)` against the write and `sb_lowat`; nothing on the
  send path reads `t_notsent_lowat`. A non-blocking write past the mark is taken until the send buffer is
  full. The mark only delays the wake that follows a full buffer.

**Setting it.** Linux takes any `u32` and a value of 0 means the system's `net.ipv4.tcp_notsent_lowat`
(by default no mark). XNU refuses a negative value (`EINVAL`), and 0 turns the mark off
(`SOF_NOTSENT_LOWAT` cleared). On XNU a listener's mark is inherited by the connections it accepts:
`SOF_NOTSENT_LOWAT` is among the flags a new socket takes from its listener (`uipc_socket2.c`), and
`tcp_input.c` then copies `t_notsent_lowat`.

**What follows for hyper-rt** (`docs/runtime.md` §5.2). hyper-rt writes until the kernel refuses and
then awaits writability. On Linux the kernel refuses at the mark, so the unsent backlog stays within
about the mark and the rest stays with the application, where its connection budget sees it. On macOS
the kernel takes writes up to the whole send buffer, and the mark only governs when the wake comes. The
backlog the application keeps is then the send buffer's, not the mark's. Bounding macOS's unsent bytes to
the mark would take the runtime asking before it writes (the check XNU's `sowriteable` makes), which it
does not do. Until a consumer needs that bound on macOS, the design states the difference.

## 2. `sbitmap`, read at Linux `v6.12`

Read at `v6.12`: `include/linux/sbitmap.h` and `lib/sbitmap.c`.

`struct sbitmap` is one level: `map_nr` words (`struct sbitmap_word`), each
`____cacheline_aligned_in_smp`, holding the free bits in `word` and the freed-but-not-yet-returned bits
in `cleared`, itself on a cache line of its own; `shift` sets how many bits each word holds, so that a
small depth is still spread over several lines. A free sets its bit in `cleared`
(`sbitmap_deferred_clear_bit`), and an allocation that finds its word exhausted folds `cleared` back
into `word` under `swap_lock` (`sbitmap_deferred_clear`). Allocations start from a per-CPU hint
(`alloc_hint`). There is **no summary word**: a search walks the words from the hint.

So the runtime's citation was wrong in part. `wakes.rs` and §3.2 called the wake bitmap "the two-level
shape of the Linux block layer's scalable bitmap" and its summary bit marked "as `sbitmap` does". What
hyper-rt shares with `sbitmap` is the bit per slot in words, and a writer from another CPU touching one
word. The summary level, marked only when a word goes from empty to non-empty so that a drain visits only
marked words, is hyper-rt's own. And `sbitmap` spreads its words one to a cache line, where hyper-rt packs
them (fourteen words on the head line). §15 item 7's measurement of packed words against one word a line is
the measurement of exactly that difference.

## 3. `AF_UNIX` on Windows

- **Since when.** Microsoft's announcement ("AF_UNIX comes to Windows", Windows Command Line blog,
  2017-12-19) ships it in Insider build 17063; the first general release after it is Windows 10 1803
  (build 17134). The post: "Currently, the support only exists for the stream (SOCK_STREAM) socket
  type"; "socketpair socket API is not supported"; it "currently does not support the autobind
  feature"; "There is no support for ancillary data"; abstract addresses are accepted; a socket file's
  creation needs write permission on its directory, and connecting needs write permission on the file.
- **The peer's process.** The post names no peer-credential call. `SIO_AF_UNIX_GETPEERPID` is defined in
  the SDK's `afunix.h` as `_WSAIOR(IOC_VENDOR, 256)`, returning the connected peer's process ID as a
  `ULONG`; read at windows-sys 0.61.2 (generated from the SDK's metadata), `Win32::Networking::WinSock`:
  `SIO_AF_UNIX_GETPEERPID = 0x5800_0100` (`IOC_OUT | IOC_VENDOR | 256`). Microsoft's Winsock IOCTL
  reference (learn.microsoft.com, "Winsock IOCTLs", commit `b584d62`) does not list it.
- **What the runtime relies on, and where it is measured.** `localsys.rs` uses stream sockets only, a
  path in a directory its owner alone may enter, and `SIO_AF_UNIX_GETPEERPID` for the peer. That
  `IOCTL_AFD_POLL` reports accept, receive and send readiness on these sockets is measured, not read:
  `tests/local.rs` (`a_local_streams_accept_reads_and_writes_each_wait_on_readiness`) awaits each of the
  three on the shard's driver, and on Windows the shard would park forever on a readiness AFD never
  reported.

## 4. The wake flag's store against loom 0.7.2

`docs/runtime.md` §15 item 6: on 2026-10-06 a plain `Release` store of the wake bitmap's pending flag
failed loom 0.7.2's parking model, and the flag has been published with a swap since. The execution is
reduced here.

**Reproduced at `060f0f6`**, the tree that introduced the swap, with the swap put back to
`pending.store(true, Release)`: the two parking models fail at their first interleaving, loom reporting
the shard blocked in its wait and the sender done (`deadlock; threads = [(Id(0), Blocked), (Id(1),
Terminated)]`).

**Reduced** to two threads and two flags, loom 0.7.2, `loom::model`:

```rust
// sender                                  // parker
f.store(true, Release);                    let _ = f.swap(false, AcqRel); // its drain
fence(SeqCst);                             p.store(true, SeqCst);         // announce
let saw_parked = p.load(SeqCst);           fence(SeqCst);
                                           let saw_flag = f.load(Acquire);
assert!(saw_flag || saw_parked);
```

loom explores an execution in which both loads read the initial values: the parker's swap reads
`false`, the sender runs whole (store, fence, load of `p` before the announcement), then the parker
announces, fences and its load of `f` returns `false`, the value its own swap wrote. The C++ memory
model forbids it ([atomics.fences]: for seq_cst fences X and Y, an atomic write A sequenced before X
and a read B sequenced after Y, if X precedes Y in the single total order S then B observes A or a
later modification). The sender's fence ran before the parker's, so the parker's load must observe the
sender's store, which is later in the flag's modification order than the swap's write. With the
parker's swap removed, loom finds no such execution; with the sender's store made `SeqCst`, it still
does (loom models `SeqCst` accesses as `AcqRel`, its README's first listed limitation, but its fences
as `SeqCst`, and here the fences alone forbid the outcome). So this is a false alarm of loom 0.7.2's
fence modelling when the reading thread's own read-modify-write precedes the other thread's store in
modification order, not an error of the protocol.

**The current tree.** At `2f7cadc` the drain loads the flag before it swaps it (a drain that finds
nothing pending writes nothing), so the parker's read-modify-write of the reduction no longer precedes
its park, and the four parking models pass with a plain `Release` store (checked 2026-10-17). The swap
the bitmap publishes with is therefore not needed for correctness, by the C++ model or by loom. It stays
until a measurement says a plain store is cheaper on the wake path, which is the runtime's rule for its
hot paths.
