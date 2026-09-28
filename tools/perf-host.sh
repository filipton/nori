#!/usr/bin/env bash
# The engine's host perf report (crates/engine/tests/perf_bench.rs) for this checkout and for another
# revision, side by side: minutes of music played on the test's clock, and per minute of it the engine's
# wakes, the CPU time, and the allocations. No phone, no emulator: the numbers are this machine's, so only
# a comparison made on it means anything.
#
#   tools/perf-host.sh [rev=the latest release tag] [runs=2]
#
# The other revision is checked out in a scratch worktree, given the bench's files if it predates them,
# and removed again. Each side runs `runs` times, one after the other (never side by side: they would
# take CPU from each other).
set -euo pipefail
cd "$(dirname "$0")/.."
rev=${1:-$(git describe --tags --abbrev=0)}
runs=${2:-2}
here=$(pwd)
tree=$(mktemp -d "${TMPDIR:-/tmp}/nori-perf.XXXXXX")
trap 'git -C "$here" worktree remove --force "$tree" >/dev/null 2>&1 || true' EXIT
git worktree add -q --detach "$tree" "$rev"

# An older revision gets the bench as it is here: its files, the lines that bring them in, and libc for tests.
if [ ! -f "$tree/crates/engine/tests/perf_bench.rs" ]; then
    cp crates/engine/tests/perf_bench.rs crates/engine/tests/perf_alloc.rs "$tree/crates/engine/tests/"
    printf '\ninclude!("perf_bench.rs");\n' >> "$tree/crates/engine/tests/engine.rs"
    python3 - "$tree/crates/engine/tests/main.rs" "$tree/crates/engine/Cargo.toml" <<'EOF'
import sys
main, cargo = sys.argv[1], sys.argv[2]
s = open(main).read()
s = s.replace('#[path = "engine.rs"]\nmod engine;\n', '#[path = "perf_alloc.rs"]\nmod perf_alloc;\n#[path = "engine.rs"]\nmod engine;\n', 1)
open(main, 'w').write(s)
c = open(cargo).read()
if 'dev-dependencies' in c and '# tests/perf_bench.rs' not in c:
    c = c.replace('[dev-dependencies]\n', '[dev-dependencies]\n# tests/perf_bench.rs: the process\'s CPU time.\nlibc = "0.2"\n', 1)
open(cargo, 'w').write(c)
EOF
fi

bench() {
    (cd "$1" && cargo test --release -j4 -p nori-engine --test engine perf_report -- --ignored --nocapture --test-threads=1 2>&1) | grep -o 'perf: .*'
}

for side in "$rev:$tree" "working tree:$here"; do
    name=${side%%:*}; dir=${side#*:}
    echo "== $name"
    for _ in $(seq "$runs"); do bench "$dir"; done
done
