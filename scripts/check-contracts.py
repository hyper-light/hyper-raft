#!/usr/bin/env python3
"""Source contracts the compiler cannot state (CLAUDE.md §1).

- `unsafe` is denied workspace-wide (`Cargo.toml`). A file may opt in with
  `#![allow(unsafe_code)]` only when it is listed in UNSAFE_ALLOWED below with the
  interface it binds; any other file that allows `unsafe_code` or writes `unsafe` fails.
- In a listed file, every `unsafe` block and `unsafe impl` has a `// SAFETY:` comment
  directly above it (or on its line) stating the invariant that makes it sound. Clippy's
  `undocumented_unsafe_blocks` says the same; this holds it for every target at once,
  including the ones a machine's clippy run did not compile.
- Every `const` in a crate's `src/` outside a test module carries a `///` doc comment: its
  derivation or its citation (CLAUDE.md §1, "No arbitrary numbers").
- The simulation crates (`hyper-sim`, and `hyper-check` when it lands) hold no `HashMap`,
  `HashSet` or `RandomState`: their iteration order is seeded by the OS, which would make a run's
  decisions depend on more than its seed (docs/sim.md §3.9).
- No `RefCell` in the crates `NO_REFCELL` lists (the owner's rule; docs/runtime.md §2).
- Every test file that makes a `hyper_sim` world runs its first seed through `twice`, the
  run-twice check (docs/sim.md §3.9): determinism is checked by every simulation, not trusted.
"""
import pathlib
from pathlib import PurePosixPath
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# Each entry: the file, and the interface its `unsafe` binds.
UNSAFE_ALLOWED = {
    "crates/hyper-block/src/resident/macos.rs":
        "mmap, mincore and munmap through libc: which pages of a file the unified buffer cache holds (resident reads)",
    "crates/hyper-block/src/resident/linux.rs":
        "mmap, mincore and munmap through libc: which pages of a file the page cache holds, a probe "
        "that starts no I/O (tests only)",
    "crates/hyper-rt/src/frames.rs":
        "a shard's task frames: a future written into a block of its size class, polled pinned in place and dropped in place by its shard, the block recycled on the shard's free list (docs/runtime.md §3.1)",
    "crates/hyper-rt/src/handoff.rs":
        "a parked thread's handle registered by address in a cell's waiter word and read once by the publisher that claimed it",
    "crates/hyper-rt/src/sync/ring.rs":
        "the bounded lock-free ring's slots: a value written or read only by the end whose stamp claimed the slot",
    "crates/hyper-rt/src/sync/room.rs":
        "a waiting sender's place: its thread handle written and read under the place's state word's claim",
    "crates/hyper-rt/src/afd.rs":
        "IOCTL_AFD_POLL through NtDeviceIoControlFile on \\Device\\Afd, its OVERLAPPED blocks and SIO_BASE_HANDLE (socket readiness on Windows; the wepoll and mio technique)",
    "crates/hyper-rt/src/attribution.rs":
        "getrusage(RUSAGE_THREAD) through libc (a long poll's voluntary context switches)",
    "crates/hyper-rt/src/epoll.rs":
        "epoll and eventfd where rustix's wrappers are unsafe by signature, and epoll_pwait2(2) "
        "through libc::syscall (rustix carries it only behind a feature that fails every wait "
        "before Linux 5.11)",
    "crates/hyper-rt/src/iocp.rs":
        "I/O completion ports: CreateIoCompletionPort, GetQueuedCompletionStatusEx, PostQueuedCompletionStatus; "
        "a timed wait's high-resolution waitable timer (CreateWaitableTimerExW, SetWaitableTimer) and "
        "ntdll's wait completion packet that posts its signal to the port (NtCreateWaitCompletionPacket, "
        "NtAssociateWaitCompletionPacket, NtCancelWaitCompletionPacket, found through GetProcAddress)",
    "crates/hyper-rt/src/kqueue.rs":
        "kevent through rustix (unsafe by signature: the udata word) and the EVFILT_USER trigger, "
        "and a timed wait's kevent through libc (its NOTE_CRITICAL timer, which rustix's timer "
        "filter does not set)",
    "crates/hyper-rt/src/machine/clock.rs":
        "clock_gettime and mach_absolute_time through libc; QueryPerformanceCounter (the monotonic clock); "
        "mach_timebase_info and sysctlbyname(hw.tbfrequency) through libc (macOS's process and kernel tick "
        "rates, docs/runtime.md §5.1)",
    "crates/hyper-rt/src/machine/facts.rs":
        "sysctlbyname, host_statistics and the IOKit power-source query (macOS); GetSystemInfo, GlobalMemoryStatusEx, GetLogicalProcessorInformation, GetSystemPowerStatus (Windows)",
    "crates/hyper-rt/src/machine/probes.rs":
        "thread_policy_set (macOS affinity tag); SetThreadAffinityMask, CreateEventW and SetEvent (Windows)",
    "crates/hyper-rt/src/machine/wake.rs":
        "the wake probe's thread parking and placement calls",
    "crates/hyper-rt/src/netsys.rs":
        "Winsock 2: WSAStartup, socket, bind, sendto, recvfrom, setsockopt, ioctlsocket (the datagram socket on Windows, IPv4 and IPv6); IP_DONTFRAG and IPV6_DONTFRAG (macOS)",
    "crates/hyper-rt/src/localsys.rs":
        "AF_UNIX stream sockets and the peer's identity: getpeereid and LOCAL_PEERPID (macOS); Winsock AF_UNIX, SIO_AF_UNIX_GETPEERPID, OpenProcessToken and GetTokenInformation(TokenUser) (Windows)",
    "crates/hyper-rt/src/signal.rs":
        "sigaction and the async-signal-safe self-pipe write (Unix); SetConsoleCtrlHandler (Windows)",
    "crates/hyper-rt/src/tcpsys.rs":
        "Winsock 2 stream calls (socket, accept, connect, recv, send, WSARecv, WSASend over IoSlice's WSABUF layout, shutdown) and the TCP options rustix lacks (TCP_NOTSENT_LOWAT, TCP_RXT_CONNDROPTIME, SO_NOSIGPIPE)",
    "crates/hyper-rt/src/udp/linux.rs":
        "sendmmsg, recvmmsg, UDP_SEGMENT, UDP_GRO and SCM_TIMESTAMPNS control messages, which rustix 1.1 lacks (batched UDP, docs/runtime.md §5.1); "
        "getsockopt(SO_MEMINFO), the kernel's account of a receive queue (docs/transport.md §4i)",
    "crates/hyper-rt/tests/udp_batched.rs":
        "setsockopt(SO_NO_CHECK) through libc, which the runtime has no call for: a socket whose "
        "segmented sends the kernel refuses (tests only, docs/transport.md §4h)",
    "crates/hyper-rt/benches/rearm.rs":
        "one kevent change through rustix (unsafe by signature: the udata word), the kqueue driver's "
        "re-arm measured alone (measurement only, docs/runtime.md §15 item 7)",
    "crates/hyper-rt/src/udp/macos.rs":
        "recvmsg with SCM_TIMESTAMP_MONOTONIC and mach_absolute_time, which rustix drops (kernel receive stamps, docs/runtime.md §5.1)",
    "crates/hyper-rt/src/retire.rs":
        "the counted-pin protocol's raw entry pointer: published from a Box, read under a pin, freed by the last reader of its retirement (docs/runtime.md §4.3)",
    "crates/hyper-rt/src/registry.rs":
        "the static shard table's entry and context pointers under counted readers, and the thread's current-shard pointer (docs/runtime.md §3.4, §13)",
    "crates/hyper-rt/src/thread_clock.rs":
        "pthread_getcpuclockid and clock_gettime; thread_info (macOS); QueryThreadCycleTime and GetThreadTimes (Windows): per-thread CPU clocks",
    "crates/hyper-rt/src/waker.rs":
        "RawWakerVTable over the Copy task word (docs/runtime.md §3.2)",
    "crates/hyper-rt-transport/tests/connection_footprint.rs":
        "std::alloc::GlobalAlloc over hyper-measure's counting allocator, and aws-lc's "
        "CRYPTO_set_mem_functions with malloc, realloc and free over System, each block carrying "
        "its size (what a connection costs its host; measurement only)",
    "crates/hyper-rt-transport/tests/connection_heap.rs":
        "std::alloc::GlobalAlloc over mimalloc's, aws-lc's CRYPTO_set_mem_functions over mi_malloc "
        "and mi_free, and mimalloc's heap API (mi_heap_new, mi_heap_set_default, "
        "mi_heap_visit_blocks, mi_heap_collect, mi_process_info): a connection's cost read from a "
        "heap of its own (measurement only)",
    "crates/hyper-heap/src/lib.rs":
        "mimalloc's heap API through libmimalloc-sys: mi_heap_new, mi_heap_set_default, "
        "mi_heap_get_default, mi_heap_get_backing, mi_heap_collect, mi_heap_visit_blocks and its "
        "area visitor, mi_heap_delete, mi_is_in_heap_region (a thread's work on a heap of its own)",
    "crates/hyper-measure/src/alloc.rs":
        "std::alloc::GlobalAlloc over System (the counting allocator; measurement only)",
    "crates/hyper-measure/src/faults.rs":
        "getrusage(2) through libc; Mach task_info(3) through libc, with task_events_info declared "
        "here and read only when the kernel's count of written words matches it; Win32 "
        "K32GetProcessMemoryInfo through windows-sys (page-fault counts)",
    "crates/hyper-measure/src/usage.rs":
        "proc_pid_rusage(RUSAGE_INFO_V6) through a declaration of <libproc.h>, with "
        "rusage_info_v6 declared here from <sys/resource.h>, and sysctlbyname(hw.tbfrequency), the "
        "kernel's tick rate its times count, through libc (macOS); sysconf(_SC_CLK_TCK) (Linux), "
        "getrusage(2) (other Unix) and getloadavg(3) "
        "through libc; Win32 OpenProcess, CloseHandle, GetProcessTimes, QueryProcessCycleTime and "
        "K32GetProcessMemoryInfo through windows-sys: a process's account; measurement only",
    "crates/hyper-measure/src/wake.rs":
        "std::task::RawWaker over a leaked slot (counting wakers; tests and benchmarks only)",
    "crates/hyper-seal/src/memory.rs":
        "std::alloc::alloc_zeroed for the process's key region, mlock(2) and madvise(2) "
        "MADV_DONTDUMP through rustix (Unix), VirtualLock through windows-sys (Windows), the "
        "region's key slots read and written by their one claimer, and std::ptr::write_volatile "
        "for the wipe a dropped key gets",
    "crates/hyper-seal/src/file_windows.rs":
        "GetSecurityInfo, GetAclInformation, GetAce, EqualSid, IsWellKnownSid and LocalFree through "
        "windows-sys (a key file's DACL, checked owner-only)",
    "crates/hyper-block/src/aio/linux.rs":
        "io_setup(2), io_submit(2), io_getevents(2) and io_destroy(2) through libc::syscall, with <linux/aio_abi.h>'s iocb and io_event records (native AIO reads of a direct file)",
    "crates/hyper-block/src/node/macos.rs":
        "the disk ioctls of <sys/disk.h> (a device node's capacity and cache flush)",
    "crates/hyper-raft-e2e/src/fault_windows.rs":
        "AddVectoredExceptionHandler and the EXCEPTION_POINTERS it is called with, the CONTEXT "
        "union's registers, VirtualQuery before code bytes are read, GetCurrentThreadStackLimits "
        "(an E2E member's account of a fatal fault; test harness only)",
    "crates/hyper-measure/src/wait_windows.rs":
        "WSAPoll (the wait for a datagram every real-socket test and E2E member makes, on Windows, "
        "with no operation in flight past its return; tests and harnesses only)",
    "crates/hyper-block/src/gather.rs":
        "WriteFileGather on a handle opened past the cache for overlapped I/O, its page-aligned "
        "FILE_SEGMENT_ELEMENT array and OVERLAPPED block, GetOverlappedResult, and GetSystemInfo's "
        "page size, through windows-sys (a segment written whole on Windows)",
    "crates/hyper-block/src/node/windows.rs":
        "IOCTL_DISK_GET_LENGTH_INFO (a device's capacity) and GetFileInformationByHandleEx's "
        "FileStorageInfo (the sector sizes of a file's volume)",
    "crates/hyper-block/src/threads/macos.rs":
        "proc_pidinfo and sysctlbyname (the process's threads and the workqueue's thread ceiling); "
        "pthread_mach_thread_np and thread_info (a test's account of a thread's CPU time)",
    "crates/hyper-tokio/src/sys/linux.rs":
        "sendmmsg(2), recvmmsg(2) and the UDP_SEGMENT and UDP_GRO options and control messages (udp(7)); "
        "SO_TIMESTAMPNS and its SCM_TIMESTAMPNS control message (socket(7)); clock_gettime(2); "
        "FIONREAD (the bytes a receive queue holds); getsockopt(SO_MEMINFO) (its charge)",
    "crates/hyper-tokio/src/sys/macos.rs":
        "recvmsg(2), SO_TIMESTAMP_MONOTONIC and its SCM_TIMESTAMP_MONOTONIC control message (the "
        "kernel's receive stamp, which rustix's recvmsg drops); mach_absolute_time and "
        "mach_timebase_info (the clock it stamps on) and sysctlbyname(hw.tbfrequency) (the kernel's "
        "tick rate its stamps count); FIONREAD (the bytes a receive queue holds)",
    "crates/hyper-tokio/src/sys/windows.rs":
        "QueryPerformanceCounter and QueryPerformanceFrequency through windows-sys (the host's "
        "monotonic clock); ioctlsocket(FIONREAD) (the bytes a receive queue holds)",
    "crates/hyper-block/src/threads/windows.rs":
        "ToolHelp snapshots and GetThreadTimes (the process's threads and a thread's CPU time)",
    "crates/hyper-timing-trace/src/sys.rs":
        "libc's setsockopt(2) of SO_TIMESTAMPNS / SO_TIMESTAMP_MONOTONIC, recvmsg(2) and the "
        "CMSG_* walk of its receive-timestamp control message (rustix's recvmsg drops it); on "
        "macOS libc's mach_absolute_time, mach_timebase_info, sysctlbyname(hw.tbfrequency) and "
        "getloadavg(3), and rustix's "
        "select(2), unsafe for I/O safety only (the trace recorder; measurement only)",
}

ALLOW_UNSAFE = re.compile(r"(allow|expect)\s*\(\s*unsafe_code\b")
UNSAFE_USE = re.compile(r"\bunsafe\s*(\{|fn\b|extern\b|impl\b)")
NEEDS_SAFETY = re.compile(r"\bunsafe\s*(\{|impl\b)")
CONST = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+[A-Z_][A-Z0-9_]*\s*:")
TEST_MODULE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+")
TEST_MODULE_FILE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")


def code_of(line):
    """The line with any `//` comment removed (strings holding `//` are not used here)."""
    at = line.find("//")
    return line if at < 0 else line[:at]


def sources():
    for path in sorted(ROOT.glob("crates/**/*.rs")):
        rel = path.relative_to(ROOT).as_posix()
        if "/target/" in rel:
            continue
        yield rel, path.read_text(encoding="utf-8").splitlines()


def has_safety_comment(lines, at):
    """Whether the comment block directly above line `at`, or the line itself, says SAFETY."""
    if "SAFETY:" in lines[at]:
        return True
    i = at - 1
    while i >= 0:
        stripped = lines[i].strip()
        if stripped.startswith("//"):
            if "SAFETY:" in stripped:
                return True
            i -= 1
            continue
        # A statement may begin on the line above the block (`let x =` then `unsafe {`).
        if stripped.endswith("=") or stripped.endswith("(") or stripped.endswith(","):
            i -= 1
            continue
        return False
    return False


def check_unsafe(rel, lines):
    failures = []
    text = "\n".join(code_of(line) for line in lines)
    if rel not in UNSAFE_ALLOWED:
        if ALLOW_UNSAFE.search(text):
            failures.append(f"{rel}: allows unsafe_code but is not in UNSAFE_ALLOWED")
        elif UNSAFE_USE.search(text):
            failures.append(f"{rel}: uses unsafe but is not in UNSAFE_ALLOWED")
        return failures
    for n, line in enumerate(lines):
        if NEEDS_SAFETY.search(code_of(line)) and not has_safety_comment(lines, n):
            failures.append(f"{rel}:{n + 1}: an unsafe block or impl without a // SAFETY: comment")
    return failures


def test_module_files(all_sources):
    """Files of modules declared `#[cfg(test)] mod name;`: test code, as an inline test module's
    body is, with every module below them."""
    files, dirs = set(), set()
    for rel, lines in all_sources:
        path = PurePosixPath(rel)
        # A module's children sit beside lib.rs, main.rs and mod.rs, and under `name/` otherwise
        parent = path.parent if path.name in ("lib.rs", "main.rs", "mod.rs") else path.parent / path.stem
        for n, line in enumerate(lines):
            if line.strip() != "#[cfg(test)]":
                continue
            after = next((l for l in lines[n + 1:] if l.strip() and not l.strip().startswith("#[")), "")
            declared = TEST_MODULE_FILE.match(after)
            if declared:
                name = declared.group(1)
                files.add((parent / f"{name}.rs").as_posix())
                dirs.add((parent / name).as_posix() + "/")
    return files, dirs


def check_constants(rel, lines, test_files):
    if not re.match(r"crates/[^/]+/src/", rel):
        return []
    files, dirs = test_files
    if rel in files or any(rel.startswith(d) for d in dirs):
        return []
    failures = []
    for n, line in enumerate(lines):
        # A test module ends what ships: the rest of the file is its body. A test module in a file
        # of its own (`mod name;`) is skipped as that file, and the scan goes on.
        if line.strip() == "#[cfg(test)]":
            after = next((l for l in lines[n + 1:] if l.strip() and not l.strip().startswith("#[")), "")
            if TEST_MODULE.match(after) and not TEST_MODULE_FILE.match(after):
                break
        if not CONST.match(line):
            continue
        i = n - 1
        while i >= 0 and lines[i].strip().startswith("#["):
            i -= 1
        if i < 0 or not lines[i].strip().startswith("///"):
            failures.append(f"{rel}:{n + 1}: a const without a /// stating its derivation or citation")
    return failures


SIM_CRATES = ("crates/hyper-sim/", "crates/hyper-check/")
UNORDERED = re.compile(r"\b(HashMap|HashSet|RandomState)\b")
MAKES_WORLD = re.compile(r"\bWorld\s*(::\s*<[^>]*>\s*)?::\s*new\b")


def check_simulation(rel, lines):
    failures = []
    text = "\n".join(code_of(line) for line in lines)
    if rel.startswith(SIM_CRATES):
        for n, line in enumerate(lines):
            if UNORDERED.search(code_of(line)):
                failures.append(
                    f"{rel}:{n + 1}: an unordered map or set in a simulation crate (docs/sim.md §3.9)"
                )
    if re.match(r"crates/[^/]+/tests/", rel) and "hyper_sim" in text and MAKES_WORLD.search(text):
        if not re.search(r"\btwice\s*\(", text):
            failures.append(
                f"{rel}: makes a hyper_sim world but never runs a seed through twice (docs/sim.md §3.9)"
            )
    return failures


# The owner's rule, no RefCell even in tests (docs/runtime.md §2), held for the crates that already meet
# it. A crate joins once its last RefCell is gone; when every crate has, the rule moves to clippy.toml's
# disallowed types and this list goes.
NO_REFCELL = ("crates/hyper-rt/",)
REFCELL = re.compile(r"\bRefCell\b")


def check_refcell(rel, lines):
    if not rel.startswith(NO_REFCELL):
        return []
    return [
        f"{rel}:{n + 1}: RefCell (the owner's rule: exclusive access is structural, docs/runtime.md §3.4)"
        for n, line in enumerate(lines)
        if REFCELL.search(code_of(line))
    ]


def main():
    failures = []
    all_sources = list(sources())
    test_files = test_module_files(all_sources)
    for rel, lines in all_sources:
        failures.extend(check_unsafe(rel, lines))
        failures.extend(check_constants(rel, lines, test_files))
        failures.extend(check_simulation(rel, lines))
        failures.extend(check_refcell(rel, lines))
    for rel in UNSAFE_ALLOWED:
        if not (ROOT / rel).exists():
            failures.append(f"{rel}: listed in UNSAFE_ALLOWED but missing")
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
