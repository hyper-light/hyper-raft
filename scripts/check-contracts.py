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
    "crates/hyper-measure/src/alloc.rs":
        "std::alloc::GlobalAlloc over System (the counting allocator; measurement only)",
    "crates/hyper-measure/src/faults.rs":
        "getrusage(2) through libc; Mach task_info(3) through libc, with task_events_info declared "
        "here and read only when the kernel's count of written words matches it; Win32 "
        "K32GetProcessMemoryInfo through windows-sys (page-fault counts)",
    "crates/hyper-measure/src/usage.rs":
        "proc_pid_rusage(RUSAGE_INFO_V6) through a declaration of <libproc.h>, with "
        "rusage_info_v6 declared here from <sys/resource.h>, and mach_timebase_info through libc "
        "(macOS); sysconf(_SC_CLK_TCK) (Linux), getrusage(2) (other Unix) and getloadavg(3) "
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
    "crates/hyper-block/src/node/macos.rs":
        "the disk ioctls of <sys/disk.h> (a device node's capacity and cache flush)",
    "crates/hyper-raft-e2e/src/fault_windows.rs":
        "AddVectoredExceptionHandler and the EXCEPTION_POINTERS it is called with, the CONTEXT "
        "union's registers, VirtualQuery before code bytes are read, GetCurrentThreadStackLimits "
        "(an E2E member's account of a fatal fault; test harness only)",
    "crates/hyper-measure/src/wait_windows.rs":
        "WSAPoll (the wait for a datagram every real-socket test and E2E member makes, on Windows, "
        "with no operation in flight past its return; tests and harnesses only)",
    "crates/hyper-block/src/node/windows.rs":
        "IOCTL_DISK_GET_LENGTH_INFO (a device's capacity) and GetFileInformationByHandleEx's "
        "FileStorageInfo (the sector sizes of a file's volume)",
    "crates/hyper-block/src/threads/macos.rs":
        "proc_pidinfo and sysctlbyname (the process's threads and the workqueue's thread ceiling)",
    "crates/hyper-tokio/src/sys/linux.rs":
        "sendmmsg(2), recvmmsg(2) and the UDP_SEGMENT and UDP_GRO options and control messages (udp(7)); "
        "SO_TIMESTAMPNS and its SCM_TIMESTAMPNS control message (socket(7)); clock_gettime(2)",
    "crates/hyper-tokio/src/sys/macos.rs":
        "recvmsg(2), SO_TIMESTAMP_MONOTONIC and its SCM_TIMESTAMP_MONOTONIC control message (the "
        "kernel's receive stamp, which rustix's recvmsg drops); mach_absolute_time and "
        "mach_timebase_info (the clock it stamps on)",
    "crates/hyper-tokio/src/sys/windows.rs":
        "QueryPerformanceCounter and QueryPerformanceFrequency through windows-sys (the host's "
        "monotonic clock)",
    "crates/hyper-block/src/threads/windows.rs":
        "ToolHelp snapshots and GetThreadTimes (the process's threads and a thread's CPU time)",
    "crates/hyper-timing-trace/src/sys.rs":
        "libc's setsockopt(2) of SO_TIMESTAMPNS / SO_TIMESTAMP_MONOTONIC, recvmsg(2) and the "
        "CMSG_* walk of its receive-timestamp control message (rustix's recvmsg drops it); on "
        "macOS libc's mach_absolute_time, mach_timebase_info and getloadavg(3), and rustix's "
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
