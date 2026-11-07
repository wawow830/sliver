"""sliverd --fake-hardware and sliver-agent in a sandbox, for scripts/soak and scripts/idle-wakeups.

Private session and system buses (integration/bus.conf: nothing can be activated), private
runtime, config and state dirs under target/test-tmp, no compositor or audio sockets. The dir and
its logs are kept unless `ok` is set.

    with Sandbox("soak", CONFIG, bins) as sb:
        sliverd = sb.sliverd("--touch-socket", str(sb.dir / "touch.sock"))
        agent = sb.agent()
        ...
        sb.ok = True
"""

import os
import shutil
import signal
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class Sandbox:
    def __init__(self, name: str, config: str, bins: Path):
        self.dir = ROOT / "target" / "test-tmp" / f"{name}.{os.getpid()}"
        shutil.rmtree(self.dir, ignore_errors=True)
        (self.dir / "config" / "sliver").mkdir(parents=True)
        (self.dir / "run").mkdir(mode=0o700)
        (self.dir / "config" / "sliver" / "config.toml").write_text(config)
        self.bins = bins
        self.sock = self.dir / "sliverd.sock"
        self.procs: list[subprocess.Popen] = []
        self.ok = False
        # The agent and its helpers also get `env`.
        self.base = {"PATH": os.environ["PATH"], "HOME": os.environ.get("HOME", "/"), "LANG": "C.UTF-8"}
        self.bus = {b: f"unix:path={self.dir / b}" for b in ("session", "system")}

    @property
    def env(self) -> dict[str, str]:
        return self.base | {
            "XDG_CONFIG_HOME": str(self.dir / "config"),
            "XDG_RUNTIME_DIR": str(self.dir / "run"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "DBUS_SESSION_BUS_ADDRESS": self.bus["session"],
            "DBUS_SYSTEM_BUS_ADDRESS": self.bus["system"],
        }

    def start(self, cmd: list[str], log: str, env: dict[str, str] | None = None) -> subprocess.Popen:
        p = subprocess.Popen(cmd, env=env or self.base, stdout=open(self.dir / log, "w"),
                             stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
        self.procs.append(p)
        return p

    def wait(self, ready, seconds: float = 5) -> bool:
        end = time.time() + seconds
        while not ready() and time.time() < end:
            time.sleep(0.05)
        return ready()

    def sliverd(self, *args: str) -> subprocess.Popen:
        conf = str(ROOT / "integration" / "bus.conf")
        for name, addr in self.bus.items():
            self.start(["dbus-daemon", f"--config-file={conf}", "--nofork", f"--address={addr}"],
                       f"{name}-bus.log")
        p = self.start([str(self.bins / "sliverd"), "--fake-hardware", "--socket", str(self.sock), *args],
                       "sliverd.log")
        self.wait(lambda: all(f.exists() for f in (self.sock, self.dir / "session", self.dir / "system")))
        return p

    def agent(self) -> subprocess.Popen:
        return self.start([str(self.bins / "sliver-agent"), "--socket", str(self.sock)], "agent.log", self.env)

    def __enter__(self) -> "Sandbox":
        return self

    def __exit__(self, *_exc) -> None:
        for p in reversed(self.procs):
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for p in self.procs:
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()
        if self.ok:
            shutil.rmtree(self.dir, ignore_errors=True)
