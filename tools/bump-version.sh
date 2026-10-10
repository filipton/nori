#!/usr/bin/env bash
# Moves every place that names the release to a new version, in one go:
#   tools/bump-version.sh 0.3.2
#   tools/bump-version.sh 0.5.2-beta.1      a beta of 0.5.2 (tools/release.sh publishes it as a prerelease)
#
#   app/build.gradle.kts   versionName, and versionCode as major*1000000 + minor*10000 + patch*100 + beta,
#                          beta being the beta's number, 99 for the release itself (0.5.2-beta.3 -> 50203,
#                          0.5.2 -> 50299): every beta installs over the last, the release over its betas
#   Cargo.toml             the workspace version, and Cargo.lock's entries for the workspace crates
#   docs/features.md       "Living inventory for nori x.y.z"
#
# Left alone on purpose: the README's version badge reads the latest GitHub release by itself, and the
# benchmark tables (docs/performance/raw/) name the version the numbers were measured on, which only a
# new measurement should change. Prints what changed; commits nothing.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
new="${1:-}"
[[ "$new" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-beta\.([1-9][0-9]?))?$ ]] || { echo "usage: tools/bump-version.sh <major.minor.patch>[-beta.<1-98>]" >&2; exit 2; }
beta=${BASH_REMATCH[5]:-99}
[ "$beta" -le 98 ] || [ -z "${BASH_REMATCH[4]}" ] || { echo "a beta number goes up to 98" >&2; exit 2; }
code=$(( BASH_REMATCH[1] * 1000000 + BASH_REMATCH[2] * 10000 + BASH_REMATCH[3] * 100 + beta ))
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
# Every crate that takes the workspace version has an entry to move. Not all do: uniffi-jni-runtime is
# upstream's crate and keeps upstream's own version, which a lock file saying otherwise only makes cargo
# write back at the next build.
manifests = [(p / "Cargo.toml").read_text() for p in sorted((root / "crates").iterdir()) if (p / "Cargo.toml").exists()]
members = [re.search(r'^name = "([^"]+)"', m, re.M).group(1) for m in manifests if re.search(r'^version\.workspace\s*=\s*true', m, re.M)]
edit("Cargo.lock", [(rf'(\[\[package\]\]\nname = "{re.escape(m)}"\nversion = )"[^"]+"', rf'\g<1>"{new}"') for m in members])
edit("docs/features.md", [(rf'nori {esc}\*\*', f'nori {new}**')])
EOF
