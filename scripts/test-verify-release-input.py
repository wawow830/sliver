#!/usr/bin/env python3
"""Read-only input inspection against private sysfs/udev/proc fixtures."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("input_check", Path(__file__).with_name("verify-release-input.py"))
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class InputAccessTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        for name, suffix in (("SYS_INPUT", "sys"), ("DEV_INPUT", "dev"), ("UDEV_DATA", "udev"), ("PROC", "proc")):
            path = root / suffix
            path.mkdir()
            patcher = patch.object(checker, name, path)
            patcher.start()
            self.addCleanup(patcher.stop)
        for event, target, props in (
            ("event7", "/dev/null", "E:ID_INPUT_KEYBOARD=1\nE:ID_PATH=platform-24eb30000.input\n"),
            ("event12", "/dev/zero", "E:ID_INPUT_TOUCHSCREEN=1\nE:ID_SEAT=seat-touchbar\n"),
        ):
            (checker.DEV_INPUT / event).symlink_to(target)
            entry = checker.SYS_INPUT / event
            (entry / "device").mkdir(parents=True)
            st = os.stat(target)
            devnum = f"{os.major(st.st_rdev)}:{os.minor(st.st_rdev)}"
            (entry / "dev").write_text(devnum)
            (checker.UDEV_DATA / f"c{devnum}").write_text(props)
        self.process = checker.PROC / "1234"
        (self.process / "fd").mkdir(parents=True)
        (self.process / "fdinfo").mkdir()
        (self.process / "comm").write_text("tiny-dfr\n")
        (self.process / "stat").write_text("1234 (tiny-dfr) S " + "0 " * 18 + "76543\n")
        (self.process / "status").write_text(
            f"Uid:\t{os.getuid()}\t{os.getuid()}\t{os.getuid()}\t{os.getuid()}\n"
            f"Gid:\t{os.getgid()}\t{os.getgid()}\t{os.getgid()}\t{os.getgid()}\n"
            "Groups:\t" + " ".join(map(str, os.getgroups())) + "\nCapEff:\t0000000000000000\n"
        )
        for fd, event in (("20", "event7"), ("21", "event12")):
            (self.process / "fd" / fd).symlink_to(checker.DEV_INPUT / event)
            (self.process / "fdinfo" / fd).write_text("flags:\t0104000\n")

    def test_both_current_devices_are_accessible_and_open(self):
        evidence = checker.inspect(1234)
        self.assertTrue(evidence["ok"], evidence)
        self.assertEqual(set(evidence["devices"]), {"keyboard", "touchbar"})
        self.assertEqual(evidence["devices"]["keyboard"]["fds"], ["20"])

    def test_either_required_device_missing_fails(self):
        for fd in ("20", "21"):
            path = self.process / "fd" / fd
            target = path.readlink()
            path.unlink()
            self.assertFalse(checker.inspect(1234)["ok"])
            path.symlink_to(target)

    def test_retained_fd_does_not_prove_current_permissions(self):
        with patch.object(checker.os, "access", return_value=False):
            evidence = checker.inspect(1234)
        self.assertFalse(evidence["ok"])
        self.assertEqual(evidence["devices"]["keyboard"]["fds"], ["20"])

    def test_path_only_descriptor_is_not_input_access(self):
        (self.process / "fdinfo" / "20").write_text(f"flags:\t{os.O_PATH:o}\n")
        self.assertFalse(checker.inspect(1234)["ok"])

    def test_ambiguous_identity_is_rejected(self):
        (checker.DEV_INPUT / "event9").symlink_to("/dev/null")
        (checker.SYS_INPUT / "event9").symlink_to(checker.SYS_INPUT / "event7")
        with self.assertRaises(ValueError):
            checker.discover()


if __name__ == "__main__":
    unittest.main()
