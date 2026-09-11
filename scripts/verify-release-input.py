#!/usr/bin/env python3
"""Read-only evidence for the two inputs whose permissions Sliver changes.

Never opens an input node or reads events. Match current sysfs/udev identities,
inspect tiny-dfr's existing descriptors, and ask the kernel to check current
access (including ACLs) in a short-lived child with the daemon's credentials.
This does not prove physical Fn handling; that still needs an operator check.
"""
import argparse
import json
import os
from pathlib import Path
import stat
import time

SYS_INPUT = Path("/sys/class/input")
DEV_INPUT = Path("/dev/input")
UDEV_DATA = Path("/run/udev/data")
PROC = Path("/proc")


def properties(text):
    return dict(line[2:].split("=", 1) for line in text.splitlines()
                if line.startswith("E:") and "=" in line)


def discover():
    """Use the package rule identities, never saved event numbers or names."""
    devices = {}
    for entry in sorted(SYS_INPUT.glob("event*")):
        if not entry.name[5:].isdigit():
            continue
        devnum = (entry / "dev").read_text().strip()
        props = properties((UDEV_DATA / f"c{devnum}").read_text())
        role = None
        if (props.get("ID_PATH") == "platform-24eb30000.input"
                and props.get("ID_INPUT_KEYBOARD") == "1"
                and props.get("ID_SEAT", "seat0") == "seat0"):
            role = "keyboard"
        elif (props.get("ID_SEAT") == "seat-touchbar"
                and props.get("ID_INPUT_TOUCHSCREEN") == "1"):
            role = "touchbar"
        if role is None:
            continue
        node = DEV_INPUT / entry.name
        st = node.stat()
        if not stat.S_ISCHR(st.st_mode) or devnum != f"{os.major(st.st_rdev)}:{os.minor(st.st_rdev)}":
            raise ValueError(f"device identity changed: {node}")
        if role in devices:
            raise ValueError(f"ambiguous {role} input identity")
        devices[role] = {
            "node": str(node), "sysfs_device": str((entry / "device").resolve(strict=True)),
            "devnum": devnum, "properties": props,
            "uid": st.st_uid, "gid": st.st_gid, "mode": oct(stat.S_IMODE(st.st_mode)),
            "rdev": st.st_rdev, "inode": st.st_ino, "filesystem": st.st_dev,
        }
    if set(devices) != {"keyboard", "touchbar"}:
        raise ValueError("expected exactly one internal keyboard and one Touch Bar input")
    return devices


def process_identity(pid):
    process = PROC / str(pid)
    if (process / "comm").read_text().strip() != "tiny-dfr":
        raise ValueError("MainPID is not tiny-dfr")
    status = dict(line.split(":", 1) for line in (process / "status").read_text().splitlines() if ":" in line)
    uids = list(map(int, status["Uid"].split()))
    gids = list(map(int, status["Gid"].split()))
    if len(uids) != 4 or len(gids) != 4 or len(set(uids)) != 1 or len(set(gids)) != 1:
        raise ValueError("tiny-dfr credentials are not stable")
    if int(status["CapEff"], 16) != 0:
        raise ValueError("tiny-dfr still has effective capabilities")
    return {"pid": pid, "starttime": (process / "stat").read_text().rsplit(") ", 1)[1].split()[19],
            "uid": uids[3], "gid": gids[3], "groups": sorted(map(int, status["Groups"].split()))}


def accessible(node, identity, mode):
    child = os.fork()
    if child == 0:
        try:
            # Allow an unprivileged same-credential caller (and fixture tests).
            if sorted(os.getgroups()) != identity["groups"]:
                os.setgroups(identity["groups"])
            os.setresgid(identity["gid"], identity["gid"], identity["gid"])
            os.setresuid(identity["uid"], identity["uid"], identity["uid"])
            os._exit(0 if os.access(node, mode, effective_ids=True) else 1)
        except OSError:
            os._exit(2)
    _, status = os.waitpid(child, 0)
    return os.waitstatus_to_exitcode(status) == 0


def inspect(pid):
    identity = process_identity(pid)
    devices = discover()
    fds = list((PROC / str(pid) / "fd").iterdir())
    for device in devices.values():
        device["fds"] = []
        required_access = os.R_OK
        for fd in fds:
            try:
                st = fd.stat()
                if not (stat.S_ISCHR(st.st_mode) and st.st_rdev == device["rdev"]
                        and st.st_ino == device["inode"] and st.st_dev == device["filesystem"]):
                    continue
                info = dict(line.split(":", 1) for line in (PROC / str(pid) / "fdinfo" / fd.name).read_text().splitlines() if ":" in line)
                flags = int(info["flags"], 8)
                if flags & os.O_PATH or flags & os.O_ACCMODE == os.O_WRONLY:
                    continue
                if flags & os.O_ACCMODE == os.O_RDWR:
                    required_access |= os.W_OK
                device["fds"].append(fd.name)
            except FileNotFoundError:
                continue  # Descriptor closed while collecting: never count it.
        device["access_mask"] = required_access
        device["accessible"] = accessible(device["node"], identity, required_access)
    # Reject process replacement, permission changes, or device renumbering
    # during this observation instead of joining unrelated snapshots.
    current = discover()
    stable = process_identity(pid) == identity and all(
        all(current[role][key] == value for key, value in before.items()
            if key not in ("fds", "access_mask", "accessible"))
        for role, before in devices.items()
    )
    return {"ok": stable and all(d["accessible"] and d["fds"] for d in devices.values()),
            "process": identity, "devices": devices, "stable": stable}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("nodes", "check"))
    parser.add_argument("--pid", type=int)
    parser.add_argument("--timeout", type=float, default=5)
    args = parser.parse_args()
    if not 0 <= args.timeout <= 30:
        parser.error("timeout must be between 0 and 30 seconds")
    if args.mode == "nodes":
        for device in discover().values():
            print(device["node"])
        return 0
    if args.pid is None or args.pid <= 0:
        parser.error("check requires a positive --pid")
    deadline = time.monotonic() + args.timeout
    while True:
        try:
            evidence = inspect(args.pid)
        except (OSError, ValueError, KeyError, IndexError) as error:
            evidence = {"ok": False, "error": str(error), "pid": args.pid}
        evidence["monotonic_s"] = time.monotonic()
        print(json.dumps(evidence), flush=True)
        if evidence["ok"]:
            return 0
        if time.monotonic() >= deadline:
            return 1
        time.sleep(0.25)


if __name__ == "__main__":
    raise SystemExit(main())
