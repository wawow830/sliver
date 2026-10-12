#!/usr/bin/env python3
"""Regenerate data/emoji.tsv from Unicode's emoji-test.txt.

    curl -O https://unicode.org/Public/emoji/latest/emoji-test.txt
    python3 tools/gen.py emoji-test.txt > data/emoji.tsv

Keeps emoji up to Emoji 15.1 (MAX_VERSION): the newest additions are missing from most installed
fonts and would draw as empty boxes. Raise it when Noto Color Emoji in the target distros catches up.
Keeps fully-qualified emoji only, in CLDR order, without the skin-tone variants (a picker
applies the tone itself) and without the "Component" group. Columns: emoji, name, group.
Unicode data: https://www.unicode.org/license.txt (permissive).
"""
import re
import sys

MAX_VERSION = (15, 1)

group = None
for line in open(sys.argv[1], encoding="utf-8"):
    line = line.rstrip("\n")
    m = re.match(r"# group: (.*)", line)
    if m:
        group = m.group(1)
        continue
    if not line or line.startswith("#") or group in (None, "Component"):
        continue
    m = re.match(r"([0-9A-F ]+?)\s*;\s*fully-qualified\s*# (\S+) E[\d.]+ (.*)", line)
    if not m:
        continue
    _, emoji, name = m.groups()
    ver = re.search(r"E(\d+)\.(\d+)", line)
    if ver and (int(ver.group(1)), int(ver.group(2))) > MAX_VERSION:
        continue
    if "skin tone" in name:
        continue
    print(f"{emoji}\t{name}\t{group}")
