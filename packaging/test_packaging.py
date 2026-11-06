"""What the packaging promises, checked by what the files do (scripts/check runs this).

Units are parsed as systemd reads them, and the commands that hand the bar between sliverd and
tiny-dfr are run against a fake systemctl, so a rewrite that keeps the behaviour passes and one
that changes it fails, whatever it's spelled like.
"""

import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SYSTEMD = ROOT / "packaging/systemd"


def unit(path):
    """A unit file as {section: {key: [values]}}: keys may repeat, `\\` continues a line."""
    sections, cur, pending = {}, None, ""
    for raw in Path(path).read_text().splitlines():
        line = pending + raw.strip()
        pending = ""
        if line.endswith("\\"):
            pending = line[:-1] + " "
            continue
        if not line or line[0] in "#;":
            continue
        if line.startswith("["):
            cur = sections.setdefault(line.strip("[]"), {})
        elif "=" in line:
            k, v = line.split("=", 1)
            cur.setdefault(k.strip(), []).append(v.strip())
    return sections


def run_exec(command, systemctl_exit):
    """Runs an Exec*= command line the way systemd would (prefixes `+-` dropped, `$$` = `$`) with
    /usr/bin/systemctl replaced by a fake that exits `systemctl_exit` and records its arguments.
    Returns (whether the command line succeeded or its failure is ignored, the calls)."""
    ignore_failure = False
    while command[0] in "+-!@:|":
        ignore_failure |= command[0] == "-"
        command = command[1:]
    with tempfile.TemporaryDirectory() as d:
        log = Path(d, "calls")
        fake = Path(d, "systemctl")
        fake.write_text(f'#!/bin/sh\necho "$*" >> {log}\nexit {systemctl_exit}\n')
        fake.chmod(0o755)
        command = command.replace("/usr/bin/systemctl", str(fake)).replace("$$", "$")
        ok = subprocess.run(["/bin/sh", "-c", command]).returncode == 0
        calls = log.read_text().splitlines() if log.exists() else []
    return ok or ignore_failure, calls


class Sliverd(unittest.TestCase):
    u = unit(SYSTEMD / "sliverd.service")

    def test_only_the_system_unit_may_open_the_bar(self):
        env = " ".join(self.u["Service"].get("Environment", []))
        self.assertIn("SLIVER_SYSTEM_UNIT=1", env.split())
        self.assertNotIn("SLIVER_SAFESWAP", env)

    def test_tiny_dfr_is_not_a_conflict(self):
        # Conflicts= stops tiny-dfr even for a start the start limit refuses, leaving no daemon.
        self.assertFalse(any("tiny-dfr" in c for c in self.u["Unit"].get("Conflicts", [])))
        self.assertIn("tiny-dfr.service", self.u["Unit"]["OnFailure"])

    def test_starting_stops_tiny_dfr_or_fails(self):
        (pre,) = self.u["Service"]["ExecStartPre"]
        ok, calls = run_exec(pre, 0)
        self.assertTrue(ok)
        self.assertEqual(calls, ["stop tiny-dfr.service"])
        self.assertFalse(run_exec(pre, 1)[0], "a failure to stop it fails the start")


class TinyDfr(unittest.TestCase):
    def test_starting_tiny_dfr_stops_sliverd_and_never_fails(self):
        u = unit(SYSTEMD / "tiny-dfr.service.d/50-sliver.conf")
        (pre,) = u["Service"]["ExecStartPre"]
        ok, calls = run_exec(pre, 0)
        self.assertTrue(ok)
        self.assertEqual(calls, ["stop sliverd.socket sliverd.service"])
        self.assertTrue(run_exec(pre, 1)[0], "the fallback starts whatever happens")


class Agent(unittest.TestCase):
    def test_comes_back_from_a_stray_sigterm(self):
        # systemd counts SIGTERM as a clean exit: only Restart=always restarts it.
        u = unit(SYSTEMD / "user/sliver-agent.service")
        self.assertEqual(u["Service"]["Restart"], ["always"])


class Rpm(unittest.TestCase):
    spec = (ROOT / "packaging/rpm/sliver.spec.in").read_text()

    def sections(self):
        """The spec's %sections, by name, as lists of lines."""
        out, cur = {}, "preamble"
        for line in self.spec.splitlines():
            m = re.match(r"^%(\w+)\b", line)
            if m and m.group(1) in {"description", "prep", "build", "install", "check",
                                    "files", "post", "preun", "postun", "pre", "changelog",
                                    "package"}:
                cur = m.group(1)
            out.setdefault(cur, []).append(line)
        return out

    def test_install_has_no_file_lists(self):
        # A %files line in %install runs as a command, and breaks the build only after compiling.
        bad = [l for l in self.sections().get("install", []) if re.match(r"^%\{_[a-z]+\}/", l)]
        self.assertEqual(bad, [])

    def test_sliverd_requires_its_fallback(self):
        # OnFailure=tiny-dfr.service only keeps the F-keys reachable if tiny-dfr is installed.
        sliverd = self.spec.split("%package -n sliverd", 1)[1].split("%description", 1)[0]
        self.assertRegex(sliverd, r"(?m)^Requires:\s*tiny-dfr\b")

    def test_installing_sends_no_events_to_the_digitizers(self):
        # SYSTEMD_WANTS=sliverd.service fires on 'change' too: that would take the bar over at
        # every install and upgrade.
        bad = [l for l in self.spec.splitlines()
               if re.search(r"udevadm\s+trigger", l) and "subsystem-match=input" in l]
        self.assertEqual(bad, [])


class Spec(unittest.TestCase):
    def test_renders_every_placeholder(self):
        if not shutil.which("cargo"):
            self.skipTest("no cargo")
        out = subprocess.run([sys.executable, str(ROOT / "packaging/rpm/spec.py")],
                             capture_output=True, text=True, check=True).stdout
        self.assertNotRegex(out, r"@CARGO_[A-Z]+@|@BUNDLED_LICENSES@")
        # The metainfo's own placeholder is filled in %install, not here.
        self.assertIn("s|@LICENSE@|%{sliver_license}|", out)
        self.assertRegex(out, r"(?m)^%global bundled_licenses .*\bOFL-1\.1\b")


class Browser(unittest.TestCase):
    def test_native_host_allows_the_extension_its_key_names(self):
        # A Chromium extension's id comes from the public key in its manifest.
        ext = ROOT / "integration/browser/extension/manifest.chromium.json"
        key = json.loads(ext.read_text())["key"]
        h = hashlib.sha256(base64.b64decode(key)).hexdigest()[:32]
        ext_id = "".join(chr(ord("a") + int(c, 16)) for c in h)
        native = ROOT / "integration/browser/native/io.github.wawow830.sliver_browser.chromium.json"
        self.assertIn(f"chrome-extension://{ext_id}/", json.loads(native.read_text())["allowed_origins"])


if __name__ == "__main__":
    unittest.main()
