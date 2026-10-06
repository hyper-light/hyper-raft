#!/usr/bin/env python3
"""The gate's progress, kept where a lost runner leaves it.

Reads the test run's output on stdin and passes every line through unchanged. Each line that marks
progress (a test binary starting, a test ending, an end-to-end scenario's report, libtest's warning
that a test has run long) is written, with the time, the machine's available memory and the gate
process tree's working set and peak, to the progress file (`$GATE_PROGRESS`, or
`target/gate-progress.log`) and, for all but the per-test lines, to `$GITHUB_STEP_SUMMARY` when CI
sets it. A run that fails or times out then shows what was running and how much memory was left.
A runner that loses its connection mid-step runs no later step to upload the file, and whether
GitHub keeps the summary of a step cut off that way is unverified: there the record may be lost.

Memory is read in-process (Linux `/proc`, Windows `GlobalMemoryStatusEx` and
`K32GetProcessMemoryInfo`, macOS `host_statistics64` and `proc_pid_rusage`), never by starting a
process, so a sample costs microseconds against tests that take milliseconds and more.
"""

import ctypes
import datetime
import os
import re
import sys

# libtest's lines, and the end-to-end suites' reports (`e2e <scenario>: ...`)
BINARY = re.compile(r"^\s*Running (.*)$")
TEST_END = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)")
SLOW = re.compile(r"^test (\S+) has been running for over")
E2E = re.compile(r"^e2e ")
DOCTESTS = re.compile(r"^\s*Doc-tests (.*)$")


def linux_memory(root):
    available = None
    with open("/proc/meminfo") as meminfo:
        for line in meminfo:
            if line.startswith("MemAvailable:"):
                available = int(line.split()[1]) * 1024
    children = {}
    status = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/status") as f:
                fields = dict(
                    line.rstrip("\n").split(":\t", 1) for line in f if ":\t" in line
                )
        except OSError:
            continue
        pid = int(entry)
        children.setdefault(int(fields.get("PPid", "0")), []).append(pid)
        status[pid] = fields
    working, peak = 0, 0
    for pid in tree(root, children):
        fields = status.get(pid, {})
        rss = int(fields.get("VmRSS", "0 kB").split()[0]) * 1024
        hwm = int(fields.get("VmHWM", "0 kB").split()[0]) * 1024
        working += rss
        peak = max(peak, hwm)
    return available, working, peak


def tree(root, children):
    out, stack = [], [root]
    while stack:
        pid = stack.pop()
        out.append(pid)
        stack.extend(children.get(pid, []))
    return out


class MemoryStatusEx(ctypes.Structure):
    _fields_ = [
        ("dwLength", ctypes.c_ulong),
        ("dwMemoryLoad", ctypes.c_ulong),
        ("ullTotalPhys", ctypes.c_ulonglong),
        ("ullAvailPhys", ctypes.c_ulonglong),
        ("ullTotalPageFile", ctypes.c_ulonglong),
        ("ullAvailPageFile", ctypes.c_ulonglong),
        ("ullTotalVirtual", ctypes.c_ulonglong),
        ("ullAvailVirtual", ctypes.c_ulonglong),
        ("ullAvailExtendedVirtual", ctypes.c_ulonglong),
    ]


class ProcessEntry32(ctypes.Structure):
    _fields_ = [
        ("dwSize", ctypes.c_ulong),
        ("cntUsage", ctypes.c_ulong),
        ("th32ProcessID", ctypes.c_ulong),
        ("th32DefaultHeapID", ctypes.c_size_t),
        ("th32ModuleID", ctypes.c_ulong),
        ("cntThreads", ctypes.c_ulong),
        ("th32ParentProcessID", ctypes.c_ulong),
        ("pcPriClassBase", ctypes.c_long),
        ("dwFlags", ctypes.c_ulong),
        ("szExeFile", ctypes.c_char * 260),
    ]


class ProcessMemoryCounters(ctypes.Structure):
    _fields_ = [
        ("cb", ctypes.c_ulong),
        ("PageFaultCount", ctypes.c_ulong),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
    ]


def windows_memory(root):
    kernel32 = ctypes.windll.kernel32
    status = MemoryStatusEx()
    status.dwLength = ctypes.sizeof(MemoryStatusEx)
    kernel32.GlobalMemoryStatusEx(ctypes.byref(status))
    snapshot = kernel32.CreateToolhelp32Snapshot(0x2, 0)  # TH32CS_SNAPPROCESS
    children = {}
    entry = ProcessEntry32()
    entry.dwSize = ctypes.sizeof(ProcessEntry32)
    more = kernel32.Process32First(snapshot, ctypes.byref(entry))
    while more:
        children.setdefault(entry.th32ParentProcessID, []).append(entry.th32ProcessID)
        more = kernel32.Process32Next(snapshot, ctypes.byref(entry))
    kernel32.CloseHandle(snapshot)
    working, peak = 0, 0
    for pid in tree(root, children):
        # PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ
        handle = kernel32.OpenProcess(0x1000 | 0x0010, False, pid)
        if not handle:
            continue
        counters = ProcessMemoryCounters()
        counters.cb = ctypes.sizeof(ProcessMemoryCounters)
        if kernel32.K32GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
            working += counters.WorkingSetSize
            peak = max(peak, counters.PeakWorkingSetSize)
        kernel32.CloseHandle(handle)
    return status.ullAvailPhys, working, peak


class RusageInfoV4(ctypes.Structure):
    # <sys/resource.h>, through the lifetime maximum footprint (a prefix of rusage_info_v4)
    _fields_ = [
        ("ri_uuid", ctypes.c_uint8 * 16),
        ("ri_user_time", ctypes.c_uint64),
        ("ri_system_time", ctypes.c_uint64),
        ("ri_pkg_idle_wkups", ctypes.c_uint64),
        ("ri_interrupt_wkups", ctypes.c_uint64),
        ("ri_pageins", ctypes.c_uint64),
        ("ri_wired_size", ctypes.c_uint64),
        ("ri_resident_size", ctypes.c_uint64),
        ("ri_phys_footprint", ctypes.c_uint64),
        ("ri_proc_start_abstime", ctypes.c_uint64),
        ("ri_proc_exit_abstime", ctypes.c_uint64),
        ("ri_child_user_time", ctypes.c_uint64),
        ("ri_child_system_time", ctypes.c_uint64),
        ("ri_child_pkg_idle_wkups", ctypes.c_uint64),
        ("ri_child_interrupt_wkups", ctypes.c_uint64),
        ("ri_child_pageins", ctypes.c_uint64),
        ("ri_child_elapsed_abstime", ctypes.c_uint64),
        ("ri_diskio_bytesread", ctypes.c_uint64),
        ("ri_diskio_byteswritten", ctypes.c_uint64),
        # RUSAGE_INFO_V3
        ("ri_cpu_time_qos", ctypes.c_uint64 * 7),
        ("ri_billed_system_time", ctypes.c_uint64),
        ("ri_serviced_system_time", ctypes.c_uint64),
        # RUSAGE_INFO_V4
        ("ri_logical_writes", ctypes.c_uint64),
        ("ri_lifetime_max_phys_footprint", ctypes.c_uint64),
    ]


class RusageInfoV4Buffer(ctypes.Structure):
    # rusage_info_v4 is 296 bytes; the call writes all of it
    _fields_ = [("bytes", ctypes.c_uint8 * 296)]


class BsdInfo(ctypes.Structure):
    _fields_ = [
        ("pbi_flags", ctypes.c_uint32),
        ("pbi_status", ctypes.c_uint32),
        ("pbi_xstatus", ctypes.c_uint32),
        ("pbi_pid", ctypes.c_uint32),
        ("pbi_ppid", ctypes.c_uint32),
        ("rest", ctypes.c_uint8 * 120),
    ]


def macos_memory(root):
    libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    # host_statistics64(HOST_VM_INFO64): free and inactive pages are what a process can take
    count = ctypes.c_uint32(38)  # HOST_VM_INFO64_COUNT
    stats = (ctypes.c_uint32 * 38)()
    libc.host_statistics64(libc.mach_host_self(), 4, stats, ctypes.byref(count))
    page = ctypes.c_size_t()
    libc.host_page_size(libc.mach_host_self(), ctypes.byref(page))
    # vm_statistics64: free_count, active_count, inactive_count are the first three fields
    available = (stats[0] + stats[2]) * page.value
    size = libc.proc_listallpids(None, 0)
    pids = (ctypes.c_int * (size + 64))()
    got = libc.proc_listallpids(pids, ctypes.sizeof(pids))
    children = {}
    for pid in pids[:got]:
        info = BsdInfo()
        # PROC_PIDTBSDINFO
        if libc.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) > 0:
            children.setdefault(info.pbi_ppid, []).append(pid)
    working, peak = 0, 0
    for pid in tree(root, children):
        usage = RusageInfoV4Buffer()
        # RUSAGE_INFO_V4, into a buffer the size of the whole structure
        if libc.proc_pid_rusage(pid, 4, ctypes.byref(usage)) == 0:
            info = RusageInfoV4.from_buffer(usage)
            working += info.ri_resident_size
            peak = max(peak, info.ri_lifetime_max_phys_footprint)
    return available, working, peak


def memory(root):
    try:
        if sys.platform.startswith("linux"):
            return linux_memory(root)
        if sys.platform == "win32":
            return windows_memory(root)
        if sys.platform == "darwin":
            return macos_memory(root)
    except (OSError, ValueError, AttributeError):
        pass
    return None, None, None


def mib(value):
    return "?" if value is None else f"{value / (1 << 20):.0f}"


def main():
    # The gate's shell (`GATE_ROOT`, from gates.sh): on Windows this script's parent is a pipeline's
    # own bash, not the one cargo descends from
    root = int(os.environ.get("GATE_ROOT") or os.getppid())
    path = os.environ.get("GATE_PROGRESS") or os.path.join("target", "gate-progress.log")
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    with open(path, "a", buffering=1) as log:
        if summary:
            with open(summary, "a") as out:
                out.write("| time (UTC) | available MiB | gate working set / peak MiB | event |\n")
                out.write("|---|---|---|---|\n")
        for line in sys.stdin:
            sys.stdout.write(line)
            sys.stdout.flush()
            text = line.rstrip("\r\n")
            per_test = TEST_END.match(text)
            if not (per_test or BINARY.match(text) or DOCTESTS.match(text)
                    or SLOW.match(text) or E2E.match(text)):
                continue
            available, working, peak = memory(root)
            now = datetime.datetime.now(datetime.timezone.utc).strftime("%H:%M:%S.%f")[:-3]
            record = f"{now} | {mib(available)} | {mib(working)} / {mib(peak)} | {text}"
            log.write(record + "\n")
            if summary and not (per_test and per_test.group(2) != "FAILED"):
                event = text.strip().replace("|", "\\|")
                with open(summary, "a") as out:
                    out.write(f"| {now} | {mib(available)} | {mib(working)} / {mib(peak)} | {event} |\n")


if __name__ == "__main__":
    main()
