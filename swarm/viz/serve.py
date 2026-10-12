#!/usr/bin/env python3
"""Live visualizer for the sliver swarm. Stdlib only.

    swarm viz [--port 7777] [--host 127.0.0.1]

GET /                 the page
GET /api/snapshot     peers (with live agent status, cost, context, current activity), board, stats
GET /api/stream       SSE: one `history` event, then every new channel/inbox event as it happens
GET /snap/<name>.png  Touch Bar renders committed on origin/main
"""
import argparse, json, re, subprocess, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent


def git(*args, cwd=None):
    r = subprocess.run(["git", *args], cwd=cwd or HERE, capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else ""


COMMON = Path(git("rev-parse", "--path-format=absolute", "--git-common-dir").strip())
MAIN = COMMON.parent
SWARM = COMMON / "swarm"
FOOTER_RE = re.compile(r"\$(\d+(?:\.\d+)?)\s+(\d+(?:\.\d+)?)%/")
# A thought or a reply: starts like an English sentence, not like code.
PROSE_RE = re.compile(r"^(?!(use|pub|fn|let|impl|mod|struct|enum|const|match|if|for|return|import|def|from)\b)[A-Z][A-Za-z'’]*[ ,:]")


def herdr_json(*args):
    try:
        r = subprocess.run(["herdr", *args], capture_output=True, text=True, timeout=8)
        return json.loads(r.stdout) if r.stdout.strip().startswith("{") else None
    except Exception:
        return None


def read_jsonl(p: Path):
    if not p.exists():
        return []
    out = []
    for line in p.read_text().splitlines():
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return out


def load_state():
    try:
        return json.loads((SWARM / "state.json").read_text())
    except Exception:
        return {"peers": {}, "tasks": {}, "humans": {}}


# ------------------------------------------------------------- live peer info (cached)

class Live:
    """Polls herdr in the background so page requests never block on it."""

    def __init__(self):
        self.data, self.repo = {}, {}
        threading.Thread(target=self.loop, daemon=True).start()

    def peer(self, name):
        info = {"agent": None, "cost": None, "ctx": None, "activity": ""}
        g = herdr_json("agent", "get", name)
        if g:
            a = g.get("result", {}).get("agent", {})
            info["agent"] = a.get("agent_status")
        if info["agent"] is None:
            return info
        try:
            text = subprocess.run(["herdr", "agent", "read", name, "--source", "visible"],
                                  capture_output=True, text=True, timeout=8).stdout
        except Exception:
            text = ""
        lines = text.splitlines()
        for l in lines[::-1]:
            m = FOOTER_RE.search(l)
            if m:
                info["cost"], info["ctx"] = float(m.group(1)), float(m.group(2))
                break
        # The agent's latest output: lines above the "── Working ──" / separator footer.
        body = []
        for l in lines:
            if l.strip().startswith("──") or l.strip().startswith("~/"):
                break
            body.append(l)
        paras, cur = [], []
        for l in body:
            if not l.strip():
                if cur:
                    paras.append(cur); cur = []
            else:
                cur.append(l.strip())
        if cur:
            paras.append(cur)
        info["cmd"] = ""
        for p in paras:  # chronological: remember the last thought and any command after it
            first = p[0]
            if re.match(r"^(Took \d|Elapsed \d|\.\.\. \(|\d+[-:]\s)", first):
                continue
            if first.startswith("$ "):
                info["cmd"] = re.sub(r"\s*\(timeout \d+s\)\s*$", "", " ".join(p[:2])[2:])[:110]
            elif PROSE_RE.match(first) and len(" ".join(p).split()) >= 5:
                info["activity"], info["cmd"] = " ".join(p)[:300], ""
        return info

    def repo_stats(self):
        loc = 0
        for line in git("grep", "-c", "", "origin/main", "--", "*.rs", cwd=MAIN).splitlines():
            try:
                loc += int(line.rsplit(":", 1)[1])
            except (ValueError, IndexError):
                pass
        commits = git("rev-list", "--count", "origin/main", cwd=MAIN).strip()
        crates = len([l for l in git("ls-tree", "-d", "--name-only", "origin/main:crates", cwd=MAIN).splitlines() if l])
        snaps = sorted(p for p in git("ls-tree", "-r", "--name-only", "origin/main", cwd=MAIN).splitlines()
                       if re.search(r"snapshots/[^/]+\.png$", p))
        return {"loc": loc, "commits": int(commits or 0), "crates": crates, "snapshots": snaps}

    def loop(self):
        n = 0
        while True:
            try:
                st = load_state()
                names = [k for k, v in st.get("peers", {}).items() if v.get("status") != "gone"]
                fresh = {k: self.peer(k) for k in names}
                for k, v in fresh.items():  # keep the last thought while the screen shows only tool output
                    if not v.get("activity") and self.data.get(k, {}).get("activity"):
                        v["activity"] = self.data[k]["activity"]
                self.data = fresh
                if n % 5 == 0:
                    subprocess.run(["git", "fetch", "-q", "origin"], cwd=MAIN, capture_output=True, timeout=30)
                    self.repo = self.repo_stats()
            except Exception as e:
                print("viz: live poll error:", repr(e))
            n += 1
            time.sleep(3)


LIVE = None


def snapshot():
    st = load_state()
    peers = []
    for name, p in st.get("peers", {}).items():
        if p.get("status") == "gone":
            continue
        live = LIVE.data.get(name, {})
        owned = [t for t in st["tasks"].values() if t.get("owner") == name and t["status"] == "claimed"]
        peers.append({
            "name": name, "status": p.get("status", "active"), "model": p.get("model", ""),
            "knows": p.get("knows", ""), "joined": p.get("joined", 0), "spawned_by": p.get("spawned_by"),
            "tasks": [{"id": t["id"], "title": t["title"]} for t in owned],
            **live,
        })
    peers.sort(key=lambda p: p["joined"])
    tasks = [{k: t.get(k) for k in ("id", "title", "status", "owner", "area")} for t in st["tasks"].values()]
    humans = [{"id": h["id"], "by": h["by"], "text": h["text"]} for h in st.get("humans", {}).values()
              if h["status"] == "open"]
    cost = sum(p.get("cost") or 0 for p in peers)
    return {"now": time.time(), "paused": st.get("paused", False), "peers": peers, "tasks": tasks,
            "humans": humans, "repo": LIVE.repo, "cost": round(cost, 2)}


# ------------------------------------------------------------- events

class Tail:
    """Incrementally reads the channel log and every inbox, returning normalized events."""

    def __init__(self):
        self.offsets = {}

    def poll(self):
        files = [("channel", SWARM / "channel.log")] + \
                [("inbox", p) for p in sorted((SWARM / "inbox").glob("*.jsonl"))]
        out = []
        for src, path in files:
            recs = read_jsonl(path)
            start = self.offsets.get(path, 0)
            for i, r in enumerate(recs[start:], start):
                if src == "channel":
                    out.append({"id": f"c{i}", "at": r.get("at", 0), "by": r.get("by", "?"), "kind": r.get("kind"),
                                "text": r.get("text", ""), "task": r.get("task")})
                else:
                    out.append({"id": f"i{path.stem}{i}", "at": r.get("at", 0), "by": r.get("from", "?"),
                                "to": path.stem, "kind": "msg", "text": r.get("text", ""), "sub": r.get("kind")})
            self.offsets[path] = len(recs)
        out.sort(key=lambda e: (e["at"], e["id"]))
        return out


# ------------------------------------------------------------- http

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, code, body, ctype):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/index.html"):
            return self.send(200, (HERE / "index.html").read_bytes(), "text/html; charset=utf-8")
        if path == "/api/snapshot":
            return self.send(200, json.dumps(snapshot()).encode(), "application/json")
        if path.startswith("/snap/"):
            name = path[len("/snap/"):]
            match = [p for p in LIVE.repo.get("snapshots", []) if p.endswith("/" + name) or p == name]
            if match:
                data = subprocess.run(["git", "show", f"origin/main:{match[0]}"], cwd=MAIN, capture_output=True).stdout
                return self.send(200, data, "image/png")
            return self.send(404, b"no such snapshot", "text/plain")
        if path == "/api/stream":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            tail = Tail()
            try:
                self.wfile.write(f"event: history\ndata: {json.dumps(tail.poll())}\n\n".encode())
                self.wfile.flush()
                last_ping = time.time()
                while True:
                    time.sleep(0.6)
                    for ev in tail.poll():
                        self.wfile.write(f"data: {json.dumps(ev)}\n\n".encode())
                    if time.time() - last_ping > 15:
                        self.wfile.write(b": ping\n\n"); last_ping = time.time()
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                return
        self.send(404, b"not found", "text/plain")


def main():
    global LIVE
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=7777)
    ap.add_argument("--host", default="127.0.0.1", help="0.0.0.0 to show it on your LAN")
    a = ap.parse_args()
    LIVE = Live()
    srv = ThreadingHTTPServer((a.host, a.port), H)
    srv.daemon_threads = True
    print(f"sliver swarm viz on http://{'localhost' if a.host == '127.0.0.1' else a.host}:{a.port}  (?replay for the time-lapse)")
    srv.serve_forever()


if __name__ == "__main__":
    main()
