#!/usr/bin/env python3
"""Prints sliver.spec: sliver.spec.in with what it states filled in from where it's true.

@CARGO_VERSION@ and @CARGO_LICENSE@ are Cargo.toml's [workspace.package] version and license.
@BUNDLED_LICENSES@ is every license the shipped binaries' dependencies are under (`cargo
metadata`: normal dependencies of our crates, transitively, proc-macros aside), plus the embedded
assets': Noto Sans (OFL-1.1) and Material icons (Apache-2.0). Each crate's expression is kept as
written; only Cargo's old `/` becomes ` OR ` and alternatives are put in order, so equal
expressions merge.
"""

import json
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
ASSETS = {"OFL-1.1", "Apache-2.0"}


def normal(expr: str) -> str:
    """`MIT/Apache-2.0` and `Apache-2.0 OR MIT` are the same: `Apache-2.0 OR MIT`."""
    expr = " ".join(expr.replace("/", " OR ").split())
    if "(" in expr or " AND " in expr:
        return expr
    return " OR ".join(sorted(expr.split(" OR ")))


def bundled() -> str:
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--color", "never"],
        cwd=ROOT, capture_output=True, text=True, check=True).stdout)
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    ours = set(meta["workspace_members"])
    proc_macro = {i for i, p in packages.items()
                  if any("proc-macro" in t["kind"] for t in p["targets"])}
    seen, todo = set(), list(ours)
    while todo:
        for dep in nodes[todo.pop()]["deps"]:
            runtime = any(k["kind"] is None for k in dep["dep_kinds"])
            if runtime and dep["pkg"] not in seen and dep["pkg"] not in proc_macro:
                seen.add(dep["pkg"])
                todo.append(dep["pkg"])
    licenses = {normal(packages[i]["license"]) for i in seen - ours} | ASSETS
    return " AND ".join(f"({x})" if " " in x else x for x in sorted(licenses))


def render() -> str:
    ws = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]
    text = (ROOT / "packaging/rpm/sliver.spec.in").read_text()
    for k, v in {"@CARGO_VERSION@": ws["version"], "@CARGO_LICENSE@": ws["license"],
                 "@BUNDLED_LICENSES@": bundled()}.items():
        text = text.replace(k, v)
    return text


if __name__ == "__main__":
    sys.stdout.write(render())
