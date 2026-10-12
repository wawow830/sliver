#!/usr/bin/env python3
"""Fail (and clean up) when the checks left processes behind (T47 item 4).

    scripts/orphans.py --since PID --root DIR [--grace SECONDS] [--report-only]

A leftover is a process of ours that
  * started after PID did (PID = the scripts/check shell),
  * isn't PID or one of its live descendants (those are still being waited for), and
  * belongs to this checkout: its cwd, executable, PWD/CARGO_MANIFEST_DIR or command line points
    under DIR. That covers test binaries, daemons they spawned (dbus-daemon inherits PWD), and
    scripts run from crates/.

Leftovers get `--grace` seconds to exit by themselves (a bus shutting down after its test). Then
they're listed, sent SIGTERM, then SIGKILL, and the exit status is 1. Linux only (needs /proc).
"""

import argparse
import os
import signal
import sys
import time


def starttime(pid):
    with open(f"/proc/{pid}/stat") as f:
        rest = f.read().rsplit(")", 1)[1].split()
    return int(rest[19]), int(rest[1])  # (starttime in ticks, ppid)


def under(path, root):
    return path == root or path.startswith(root + os.sep)


def belongs(pid, root):
    try:
        if under(os.readlink(f"/proc/{pid}/cwd"), root):
            return True
        if under(os.readlink(f"/proc/{pid}/exe"), root):
            return True
    except OSError:
        pass
    try:
        with open(f"/proc/{pid}/environ", "rb") as f:
            for var in f.read().split(b"\0"):
                k, _, v = var.decode(errors="replace").partition("=")
                if k in ("PWD", "CARGO_MANIFEST_DIR") and under(v, root):
                    return True
    except OSError:
        pass
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as f:
            return (root + os.sep).encode() in f.read()
    except OSError:
        return False


def cmdline(pid):
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as f:
            return f.read().replace(b"\0", b" ").decode(errors="replace").strip()[:200]
    except OSError:
        return "?"


def leftovers(since, root):
    me = os.getuid()
    t0 = starttime(since)[0]
    procs = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        pid = int(d)
        try:
            if os.stat(f"/proc/{pid}").st_uid != me:
                continue
            procs[pid] = starttime(pid)
        except (OSError, IndexError, ValueError):
            continue

    def live_descendant(pid):
        seen = 0
        while pid > 1 and seen < 64:
            if pid == since:
                return True
            pid = procs.get(pid, (0, 0))[1]
            seen += 1
        return False

    # The check's own ancestors (the shell running it, `swarm land`) can start in the same clock
    # tick as `since`, so exclude them explicitly.
    ancestors, p = set(), since
    while p > 1 and len(ancestors) < 64:
        ancestors.add(p)
        p = procs.get(p, (0, 0))[1]
    return [
        pid
        for pid, (start, _) in procs.items()
        if start >= t0
        and pid not in ancestors
        and pid != os.getpid()
        and not live_descendant(pid)
        and belongs(pid, root)
    ]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--since", type=int, required=True)
    ap.add_argument("--root", required=True)
    ap.add_argument("--grace", type=float, default=3.0)
    ap.add_argument("--report-only", action="store_true")
    a = ap.parse_args()
    if not os.path.isdir("/proc"):
        return 0
    root = os.path.realpath(a.root)
    end = time.monotonic() + a.grace
    left = leftovers(a.since, root)
    while left and time.monotonic() < end:
        time.sleep(0.1)
        left = leftovers(a.since, root)
    if not left:
        return 0
    print(f"orphans: {len(left)} process(es) outlived the checks:", file=sys.stderr)
    for pid in sorted(left):
        print(f"  {pid}: {cmdline(pid)}", file=sys.stderr)
    if a.report_only:
        return 1
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for pid in left:
            try:
                os.kill(pid, sig)
            except OSError:
                pass
        time.sleep(0.5)
    print("orphans: killed them. Tie child processes to their test (sliver_proc::spawn_tied, "
          "sliver_testbus) and reap them on drop.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
