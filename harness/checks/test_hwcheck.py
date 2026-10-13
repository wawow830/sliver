"""hwcheck without hardware: a check with no [bar] section, answered through stdin."""
import os
import subprocess
import sys
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent


class Hwcheck(unittest.TestCase):
    def test_checks_parse(self):
        for f in HERE.glob("*.toml"):
            t = tomllib.loads(f.read_text())
            self.assertIn("title", t, f)
            known = {"say", "confirm", "shell", "resume", "within", "wait", "expect", "snap", "ask",
                     "options", "name", "collect"}
            for st in t.get("step", []):
                self.assertLessEqual(set(st), known, f"{f.name}: unknown step field")
                if "expect" in st or "snap" in st:
                    self.assertIn("bar", t, f"{f.name}: expect/snap need a [bar] run")

    def test_walks_steps_and_records_answers(self):
        tmp = ROOT / "target/test-tmp/hwcheck"
        tmp.mkdir(parents=True, exist_ok=True)
        check = tmp / "smoke.toml"
        check.write_text('''title = "smoke"
[[step]]
say = "hello"
[[step]]
name = "colour"
ask = "Which?"
options = ["red", "green"]
[[step]]
name = "free"
ask = "Say something"
[[step]]
name = "env"
collect = "echo collected"
''')
        env = dict(os.environ, PATH=f"{tmp}:{os.environ['PATH']}")
        # No open asks: a fake `swarm` that lists nothing (the real one would touch the swarm state).
        (tmp / "swarm").write_text("#!/bin/sh\nexit 0\n")
        (tmp / "swarm").chmod(0o755)
        r = subprocess.run([sys.executable, HERE / "hwcheck", check], input="\nb\nall good\n",
                           capture_output=True, text=True, env=env, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("smoke: colour = green; free = all good", r.stdout)
        dest = Path(r.stdout.split("(files: ")[1].split(")")[0])
        self.assertIn("collected", (dest / "env.txt").read_text())
        self.assertIn("colour = green", (dest / "result.txt").read_text())
        subprocess.run(["rm", "-rf", dest, tmp])


if __name__ == "__main__":
    unittest.main()
