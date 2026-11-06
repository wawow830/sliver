#!/usr/bin/env python3
"""Prints sliver.spec: sliver.spec.in with what it states filled in from where it's true.

@VERSION@ and @LICENSE@ are Cargo.toml's [workspace.package] version and license.
@BUNDLED_LICENSES@ is what the shipped binaries' dependencies are under (`cargo tree`, no dev or
build dependencies), plus the embedded assets': Noto Sans (OFL-1.1) and Material icons
(Apache-2.0).
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
ASSETS = {frozenset({"OFL-1.1"}), frozenset({"Apache-2.0"})}


def terms(expr: str) -> set[frozenset[str]]:
    """An SPDX expression's top-level AND terms, each a set of alternatives (`/` is the old OR)."""
    out, depth, cur = [], 0, ""
    for tok in re.findall(r"\(|\)|[^()]+", expr):
        depth += (tok == "(") - (tok == ")")
        cur += tok
        if depth == 0:
            out.extend(re.split(r"\s+AND\s+", cur) if "(" not in cur else [cur])
            cur = ""
    return {frozenset(a.strip() for a in re.split(r"\s+OR\s+|/", t.strip().strip("()").strip()))
            for t in out if t.strip().strip("()").strip()}


def spdx(ts: set[frozenset[str]]) -> str:
    alts = sorted(sorted(t) for t in ts)
    return " AND ".join(a[0] if len(a) == 1 else f"({' OR '.join(a)})" for a in alts)


def bundled() -> str:
    tree = subprocess.run(
        ["cargo", "tree", "--workspace", "--edges=no-build,no-dev,no-proc-macro", "--target=all",
         "--prefix=none", "--format", "{p}\t{l}"],
        cwd=ROOT, capture_output=True, text=True, check=True).stdout
    found = set(ASSETS)
    for line in tree.splitlines():
        pkg, _, lic = line.partition("\t")
        if str(ROOT) not in pkg:  # ours are @LICENSE@
            found |= terms(lic.removesuffix(" (*)"))
    return spdx(found)


def workspace(key: str) -> str:
    cargo = (ROOT / "Cargo.toml").read_text()
    return re.search(rf'^\[workspace\.package\][^\[]*?^{key} = "([^"]+)"', cargo, re.M | re.S).group(1)


def render() -> str:
    text = (ROOT / "packaging/rpm/sliver.spec.in").read_text()
    for k, v in {"@VERSION@": workspace("version"), "@LICENSE@": workspace("license"),
                 "@BUNDLED_LICENSES@": bundled()}.items():
        text = text.replace(k, v)
    return text


if __name__ == "__main__":
    sys.stdout.write(render())
