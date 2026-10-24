#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""List license expressions of every non-workspace package in the build graph
(cargo metadata, all targets) and flag any that are not clearly GPL-3-compatible."""
import json
import subprocess
import sys
from collections import defaultdict

meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--locked"],
    capture_output=True, text=True, check=True).stdout)
ws = set(meta["workspace_members"])
resolved = {n["id"] for n in meta["resolve"]["nodes"]}

# Licenses known to be compatible with GPL-3.0 (FSF list). An expression is OK
# if every OR-alternative set contains at least one OK option per AND term.
OK = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unlicense",
      "MPL-2.0", "CC0-1.0", "BSL-1.0", "0BSD", "Unicode-3.0", "Unicode-DFS-2016",
      "LGPL-2.1-or-later", "LGPL-3.0", "LGPL-3.0-or-later", "GPL-3.0", "GPL-3.0-or-later",
      "Apache-2.0 WITH LLVM-exception", "MIT-0", "OFL-1.1", "CDLA-Permissive-2.0", "bzip2-1.0.6"}

def ok(expr):
    expr = expr.replace("/", " OR ").replace("(", " ").replace(")", " ")
    # Evaluate "A OR B AND C" loosely: OK if any OR branch has all AND terms OK.
    for branch in expr.split(" OR "):
        terms = [t.strip() for t in branch.split(" AND ") if t.strip()]
        if terms and all(t in OK for t in terms):
            return True
    return False

by = defaultdict(list)
bad = []
for p in meta["packages"]:
    if p["id"] in ws or p["id"] not in resolved:
        continue
    lic = p.get("license") or ("FILE:" + str(p.get("license_file")))
    by[lic].append(f'{p["name"]} {p["version"]}')
    if not ok(lic):
        bad.append((p["name"], p["version"], lic))

total = sum(len(v) for v in by.values())
print(f"{total} third-party packages in Cargo.lock graph")
for lic, pk in sorted(by.items(), key=lambda kv: -len(kv[1])):
    print(f"{len(pk):4d}  {lic}")
print()
if bad:
    print("NEEDS A LOOK (not in the simple GPL-3-compatible list):")
    for b in bad:
        print("  ", *b)
else:
    print("All packages have at least one GPL-3-compatible license option.")
