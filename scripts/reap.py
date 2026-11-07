#!/usr/bin/env python3
"""Run a command and own everything it starts.

    scripts/reap.py CMD [ARGS...]

This process becomes a child subreaper (PR_SET_CHILD_SUBREAPER): whatever CMD starts that outlives
its own parent is reparented here, not to init. Like an init, it reaps those as they exit, so a
killed process tree is gone at once. When CMD ends, anything of it still running is a leftover: it
gets a moment to exit by itself (a bus shutting down after its test), then is listed, sent
SIGTERM, then SIGKILL, and the exit status is 1. Otherwise it's CMD's. Linux only.
"""

import ctypes
import os
import signal
import sys
import time

PR_SET_CHILD_SUBREAPER = 36
GRACE = 2.0


def children():
    """Includes CMD's orphans: the subreaper inherits them."""
    me, out = os.getpid(), []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/stat") as f:
                rest = f.read().rsplit(")", 1)[1].split()
        except OSError:
            continue
        if int(rest[1]) == me and rest[0] != "Z":
            out.append(int(d))
    return out


def reap():
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return


def cmdline(pid):
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as f:
            return f.read().replace(b"\0", b" ").decode(errors="replace").strip()
    except OSError:
        return "?"


def settle(seconds):
    end = time.monotonic() + seconds
    while True:
        reap()
        left = children()
        if not left or time.monotonic() >= end:
            return left
        time.sleep(0.05)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        sys.exit(f"reap: prctl: {os.strerror(ctypes.get_errno())}")
    # Ctrl-C reaches the whole foreground group; the command decides what it means.
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    cmd = os.fork()
    if cmd == 0:
        signal.signal(signal.SIGINT, signal.SIG_DFL)
        os.execvp(sys.argv[1], sys.argv[1:])
    status = None
    while status is None:
        pid, st = os.waitpid(-1, 0)
        if pid == cmd:
            status = os.waitstatus_to_exitcode(st)
    left = settle(GRACE)
    if not left:
        return status
    print("== leftover processes (stopped)")
    for pid in left:
        print(f"  {pid}: {cmdline(pid)}")
    for sig, wait in [(signal.SIGTERM, GRACE), (signal.SIGKILL, GRACE)]:
        for pid in left:
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass
        left = settle(wait)
        if not left:
            break
    return 1


if __name__ == "__main__":
    sys.exit(main())
