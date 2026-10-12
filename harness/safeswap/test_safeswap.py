"""Unit tests for sliver-safeswap. No root, no systemd, no hardware. Run by scripts/check."""

import contextlib
import importlib.machinery
import io
import importlib.util
import os
import struct
import sys
import tempfile
import unittest
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
_loader = importlib.machinery.SourceFileLoader("sliver_safeswap", str(HERE / "sliver-safeswap"))
_spec = importlib.util.spec_from_loader("sliver_safeswap", _loader)
ss = importlib.util.module_from_spec(_spec)
sys.modules["sliver_safeswap"] = ss  # dataclasses needs the module registered
_loader.exec_module(ss)


def props_dict(props):
    d = {}
    for p in props:
        k, _, v = p.partition("=")
        d.setdefault(k, []).append(v)
    return d


class Props(unittest.TestCase):
    def setUp(self):
        self.mode = ss.Mode(user=False, helper="/usr/local/libexec/sliver-safeswap")
        self.props = props_dict(
            ss.run_unit_props(
                self.mode, 30, 1000, Path("/run/sliver-safeswap/out"), Path("/dev/dri/card1"),
                Path("/sys/class/backlight/228600000.dsi.0/brightness"),
            )
        )

    def test_restores_tiny_dfr(self):
        self.assertEqual(self.props["Conflicts"], ["tiny-dfr.service"])
        self.assertEqual(self.props["After"], ["tiny-dfr.service"])
        (post,) = self.props["ExecStopPost"]
        self.assertTrue(post.startswith("+"), "restore must run as root")
        self.assertIn("/usr/local/libexec/sliver-safeswap restore", post)

    def test_bounded(self):
        self.assertEqual(self.props["RuntimeMaxSec"], ["30"])
        self.assertEqual(self.props["TimeoutStopSec"], [str(ss.STOP_TIMEOUT)])
        self.assertEqual(self.props["SendSIGKILL"], ["yes"])
        self.assertEqual(self.props["Restart"], ["no"])

    def test_unprivileged_payload(self):
        self.assertEqual(self.props["User"], ["1000"])
        self.assertEqual(self.props["NoNewPrivileges"], ["yes"])
        self.assertEqual(self.props["DevicePolicy"], ["closed"])
        self.assertIn("/dev/dri/card1 rw", self.props["DeviceAllow"])
        self.assertEqual(self.props["OpenFile"], ["/sys/class/backlight/228600000.dsi.0/brightness:backlight"])

    def test_timer_after_deadline(self):
        cmd = ss.restore_timer_cmd(self.mode, 30)
        self.assertIn(f"--on-active={30 + ss.STOP_TIMEOUT + ss.GRACE}s", cmd)
        self.assertEqual(cmd[-3:], ["restore", "--reason", "timer"])

    def test_user_mode_never_uses_system_manager(self):
        m = ss.Mode(user=True, target="fake.service")
        props = props_dict(ss.run_unit_props(m, 5, 1000, Path("/tmp/o"), None, None))
        self.assertIn("--user-mode", props["ExecStopPost"][0])
        self.assertNotIn("User", props)
        self.assertEqual(m.systemctl()[:2], ["systemctl", "--user"])
        self.assertEqual(m.systemd_run()[:2], ["systemd-run", "--user"])


class Validation(unittest.TestCase):
    def test_seconds(self):
        self.assertEqual(ss.validate_seconds(1), 1)
        self.assertEqual(ss.validate_seconds(ss.MAX_SECONDS), ss.MAX_SECONDS)
        for bad in (0, -1, ss.MAX_SECONDS + 1):
            with self.assertRaises(ss.SwapError):
                ss.validate_seconds(bad)

    def test_binary(self):
        with tempfile.TemporaryDirectory() as d:
            elf = Path(d, "sliverd")
            elf.write_bytes(b"\x7fELF" + b"\0" * 60)
            script = Path(d, "s.sh")
            script.write_text("#!/bin/sh\n")
            self.assertEqual(ss.validate_binary(str(elf)), elf.resolve())
            with self.assertRaises(ss.SwapError):
                ss.validate_binary("relative/sliverd")
            with self.assertRaises(ss.SwapError):
                ss.validate_binary(str(script))
            self.assertEqual(ss.validate_binary(str(script), allow_script=True), script.resolve())
            with self.assertRaises(ss.SwapError):
                ss.validate_binary(d)

    def test_args(self):
        self.assertEqual(ss.validate_args(["--fake", "x"]), ["--fake", "x"])
        with self.assertRaises(ss.SwapError):
            ss.validate_args(["a\nb"])
        with self.assertRaises(ss.SwapError):
            ss.validate_args(["x"] * (ss.MAX_EXTRA_ARGS + 1))

    def test_invoking_uid(self):
        m = ss.Mode(user=False)
        self.assertEqual(ss.invoking_uid(m, {"PKEXEC_UID": "1000"}), 1000)
        self.assertEqual(ss.invoking_uid(m, {"SUDO_UID": "1001"}), 1001)
        for env in ({}, {"PKEXEC_UID": "0"}, {"PKEXEC_UID": "abc"}):
            with self.assertRaises(ss.SwapError):
                ss.invoking_uid(m, env)

    def test_test_overrides_rejected_in_real_mode(self):
        for argv in (["--target", "sshd.service", "restore"], ["--allow-script", "status"]):
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(ss.main(argv), 2)


class Discovery(unittest.TestCase):
    def test_backlight_names(self):
        with tempfile.TemporaryDirectory() as d:
            for n in ("apple-panel-bl", "228600000.dsi.0"):
                Path(d, n).mkdir()
            self.assertEqual(ss.find_bar_backlight(Path(d)), Path(d, "228600000.dsi.0", "brightness"))
        with tempfile.TemporaryDirectory() as d:
            Path(d, "appletb_backlight").mkdir()  # T2
            self.assertEqual(ss.find_bar_backlight(Path(d)).parent.name, "appletb_backlight")
        with tempfile.TemporaryDirectory() as d:
            Path(d, "intel_backlight").mkdir()
            with self.assertRaises(ss.SwapError):
                ss.find_bar_backlight(Path(d))

    def test_card_by_driver(self):
        with tempfile.TemporaryDirectory() as d:
            for card, drv in (("card2", "asahi"), ("card1", "adp"), ("card3", "apple-drm")):
                dev = Path(d, "devices", card)
                (Path(d, "drivers", drv)).mkdir(parents=True, exist_ok=True)
                dev.mkdir(parents=True)
                os.symlink(Path(d, "drivers", drv), dev / "driver")
                Path(d, "class").mkdir(exist_ok=True)
                Path(d, "class", card).mkdir()
                os.symlink(dev, Path(d, "class", card, "device"))
            Path(d, "class", "card1-DSI-1").mkdir()
            self.assertEqual(ss.find_bar_card(Path(d, "class")), Path("/dev/dri/card1"))


class Snap(unittest.TestCase):
    def test_struct_sizes(self):
        self.assertEqual(struct.calcsize(ss.CRTC_FMT), 104)
        self.assertEqual(struct.calcsize(ss.FB2_FMT), 104)

    def test_rotation_matches_tiny_dfr(self):
        # Portrait fb 4 wide (pitch 5 px) x 3 tall. Landscape pixel (lx, ly) lives at
        # fb (x = W-1-ly, y = lx). Encode (lx, ly) into the red/green channels.
        w, h, pitch = 4, 3, 5 * 4
        buf = bytearray(pitch * h)
        for lx in range(h):
            for ly in range(w):
                o = lx * pitch + (w - 1 - ly) * 4
                buf[o : o + 4] = bytes((0, ly, lx, 0))  # B, G, R, X
        rows = ss.portrait_to_landscape(bytes(buf), pitch, w, h)
        self.assertEqual(len(rows), w)
        for ly, row in enumerate(rows):
            self.assertEqual(len(row), h * 3)
            for lx in range(h):
                self.assertEqual(tuple(row[lx * 3 : lx * 3 + 3]), (lx, ly, 0))

    def test_png(self):
        png = ss.png_rgb([b"\xff\x00\x00" * 2, b"\x00\xff\x00" * 2], 2)
        self.assertTrue(png.startswith(b"\x89PNG\r\n\x1a\n"))
        w, h = struct.unpack(">II", png[16:24])
        self.assertEqual((w, h), (2, 2))
        idat_len = struct.unpack(">I", png[33:37])[0]
        raw = zlib.decompress(png[41 : 41 + idat_len])
        self.assertEqual(raw, b"\x00" + b"\xff\x00\x00" * 2 + b"\x00" + b"\x00\xff\x00" * 2)


if __name__ == "__main__":
    unittest.main()
