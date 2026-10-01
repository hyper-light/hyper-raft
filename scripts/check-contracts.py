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
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# Each entry: the file, and the interface its `unsafe` binds.
UNSAFE_ALLOWED = {
    "crates/hyper-measure/src/alloc.rs":
        "std::alloc::GlobalAlloc over System (the counting allocator; measurement only)",
    "crates/hyper-measure/src/faults.rs":
        "getrusage(2), Mach task_info(3) and Win32 GetProcessMemoryInfo (page-fault counts)",
    "crates/hyper-measure/src/wake.rs":
        "std::task::RawWaker over a leaked slot (counting wakers; tests and benchmarks only)",
    "crates/hyper-block/src/node/macos.rs":
        "the disk ioctls of <sys/disk.h> (a device node's capacity and cache flush)",
    "crates/hyper-block/src/node/windows.rs":
        "IOCTL_DISK_GET_LENGTH_INFO (a device's capacity)",
    "crates/hyper-block/src/threads/macos.rs":
        "proc_pidinfo and sysctlbyname (the process's threads and the workqueue's thread ceiling)",
    "crates/hyper-block/src/threads/windows.rs":
        "ToolHelp snapshots and GetThreadTimes (the process's threads and a thread's CPU time)",
}

ALLOW_UNSAFE = re.compile(r"(allow|expect)\s*\(\s*unsafe_code\b")
UNSAFE_USE = re.compile(r"\bunsafe\s*(\{|fn\b|extern\b|impl\b)")
NEEDS_SAFETY = re.compile(r"\bunsafe\s*(\{|impl\b)")
CONST = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+[A-Z_][A-Z0-9_]*\s*:")
TEST_MODULE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+")


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


def check_constants(rel, lines):
    if not re.match(r"crates/[^/]+/src/", rel):
        return []
    failures = []
    for n, line in enumerate(lines):
        # A test module ends what ships: the rest of the file is its body.
        if line.strip() == "#[cfg(test)]":
            after = next((l for l in lines[n + 1:] if l.strip() and not l.strip().startswith("#[")), "")
            if TEST_MODULE.match(after):
                break
        if not CONST.match(line):
            continue
        i = n - 1
        while i >= 0 and lines[i].strip().startswith("#["):
            i -= 1
        if i < 0 or not lines[i].strip().startswith("///"):
            failures.append(f"{rel}:{n + 1}: a const without a /// stating its derivation or citation")
    return failures


def main():
    failures = []
    for rel, lines in sources():
        failures.extend(check_unsafe(rel, lines))
        failures.extend(check_constants(rel, lines))
    for rel in UNSAFE_ALLOWED:
        if not (ROOT / rel).exists():
            failures.append(f"{rel}: listed in UNSAFE_ALLOWED but missing")
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
