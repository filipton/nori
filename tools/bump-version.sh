#!/usr/bin/env bash
# Moves every place that names the release to a new version, in one go:
#   tools/bump-version.sh 0.3.2
#
#   app/build.gradle.kts   versionName, and versionCode as major*10000 + minor*100 + patch (0.3.2 -> 302)
#   Cargo.toml             the workspace version, and Cargo.lock's entries for the workspace crates
#   docs/features.md       "Living inventory for nori x.y.z"
#
# Left alone on purpose: the README's version badge reads the latest GitHub release by itself, and the
# benchmark tables (README, BENCHMARKS.md) name the version the numbers were measured on, which only a
# new measurement should change. Prints what changed; commits nothing.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
new="${1:-}"
[[ "$new" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || { echo "usage: tools/bump-version.sh <major.minor.patch>" >&2; exit 2; }
code=$(( BASH_REMATCH[1] * 10000 + BASH_REMATCH[2] * 100 + BASH_REMATCH[3] ))
old=$(grep -oE 'versionName = "[^"]+"' "$root/app/build.gradle.kts" | head -1 | cut -d'"' -f2)
[ "$old" != "$new" ] || { echo "already at $new" >&2; exit 1; }
echo "$old -> $new (versionCode $code)"

python3 - "$root" "$old" "$new" "$code" <<'EOF'
import re, sys, pathlib
root, old, new, code = sys.argv[1:]
root = pathlib.Path(root)
esc = re.escape(old)

def edit(path, subs):
    p = root / path
    if not p.exists():
        print(f"  skip {path} (missing)"); return
    text = p.read_text(); before = text
    for pattern, repl in subs:
        text = re.sub(pattern, repl, text)
    if text != before:
        p.write_text(text); print(f"  {path}")
    else:
        print(f"  {path}: nothing to change")

edit("app/build.gradle.kts", [
    (r'versionName = "[^"]+"', f'versionName = "{new}"'),
    (r'versionCode = \d+', f'versionCode = {code}'),
])
# The workspace version can lag behind the app's (it did at 0.3.1), so it is set outright.
edit("Cargo.toml", [(r'(\[workspace\.package\][^\[]*?\nversion = )"[^"]+"', rf'\g<1>"{new}"')])
# Every workspace crate takes the workspace version, so each has an entry to move.
members = [re.search(r'^name = "([^"]+)"', (p / "Cargo.toml").read_text(), re.M).group(1) for p in sorted((root / "crates").iterdir()) if (p / "Cargo.toml").exists()]
edit("Cargo.lock", [(rf'(\[\[package\]\]\nname = "{re.escape(m)}"\nversion = )"[^"]+"', rf'\g<1>"{new}"') for m in members])
edit("docs/features.md", [(rf'nori {esc}\*\*', f'nori {new}**')])
EOF
