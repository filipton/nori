#!/usr/bin/env python3
"""Turn conventional commits into a changelog.

The commit messages in this repo already say what changed and in what way --
`feat:`, `fix:`, `perf:` -- so the changelog is a grouping of them rather than a
second thing to keep in step by hand.

    tools/changelog.py                     # what has landed since the last tag
    tools/changelog.py --since v0.1.0      # since some other point
    tools/changelog.py --update            # write that into CHANGELOG.md's Unreleased
    tools/changelog.py --release 0.2.0     # close Unreleased as a dated version
    tools/changelog.py --notes 0.2.0       # print one version's section, for release notes

What is left out is as deliberate as what is kept: `test`, `chore` and `wip`
describe the work, not the app, and a changelog nobody can skim is one nobody
reads.
"""

from __future__ import annotations

import argparse
import datetime
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHANGELOG = ROOT / "CHANGELOG.md"

# Conventional type -> the heading it belongs under. Order is the order sections
# are printed in: what a reader most wants first.
SECTIONS = [
    ("Breaking", None),  # filled from `!` or a BREAKING CHANGE trailer, not a type
    ("Added", {"feat"}),
    ("Changed", {"refactor"}),
    ("Fixed", {"fix"}),
    ("Performance", {"perf"}),
]
# `build` and `docs` are about the repository here - release tooling, measurements, notes for whoever
# works on it next - not about the app someone installs, so they stay out of the release notes too.
SKIP = {"test", "chore", "wip", "merge", "build", "docs"}

SUBJECT = re.compile(r"^(?P<type>[a-z]+)(?:\((?P<scope>[^)]*)\))?(?P<bang>!)?: (?P<desc>.+)$")
# Our own trailers, which are about how a commit was made rather than what it did.
TRAILER = re.compile(r"^(Co-Authored-By|Claude-Session|Signed-off-by):", re.I)


def run(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, check=True).stdout


def last_tag() -> str | None:
    try:
        return run("describe", "--tags", "--abbrev=0").strip() or None
    except subprocess.CalledProcessError:
        pass
    # No tags yet: the last release commit is where the previous release was cut, so the range starts
    # there rather than at the first commit ever made.
    found = run("log", "-1", "--format=%H", "--extended-regexp", r"--grep=^build: release [0-9]+\.[0-9]+\.[0-9]+$").strip()
    return found or None


def commits(since: str | None):
    rng = f"{since}..HEAD" if since else "HEAD"
    out = run("log", "--no-merges", "--reverse", f"--pretty=format:%H%x1f%s%x1f%b%x1e", rng)
    for raw in out.split("\x1e"):
        raw = raw.strip("\n")
        if not raw:
            continue
        sha, subject, body = raw.split("\x1f")
        yield sha, subject, body


def group(since: str | None) -> tuple[dict[str, list[str]], dict[str, int]]:
    found: dict[str, list[str]] = {}
    dropped: dict[str, int] = {}
    for _sha, subject, body in commits(since):
        m = SUBJECT.match(subject)
        if not m:
            dropped["(not a conventional subject)"] = dropped.get("(not a conventional subject)", 0) + 1
            continue
        kind = m.group("type")
        if kind in SKIP:
            dropped[kind] = dropped.get(kind, 0) + 1
            continue
        desc = m.group("desc").strip()
        if desc:
            desc = desc[0].upper() + desc[1:]
        scope = m.group("scope")
        if scope:
            desc = f"**{scope}:** {desc}"
        body_lines = [l for l in body.splitlines() if not TRAILER.match(l)]
        breaking = bool(m.group("bang")) or any("BREAKING CHANGE" in l for l in body_lines)
        heading = "Breaking"
        if not breaking:
            heading = next((name for name, types in SECTIONS if types and kind in types), None)
        if heading:
            found.setdefault(heading, []).append(desc)
        else:
            dropped[kind] = dropped.get(kind, 0) + 1
    return found, dropped


def render(since: str | None) -> str:
    found, dropped = group(since)
    if dropped:
        # A commit that says nothing about the app is right to leave out, but
        # leaving it out quietly is how a `wip:` holding half the program ends up
        # in no changelog at all. Say what was dropped and let a person judge.
        summary = ", ".join(f"{n}x {kind}" for kind, n in sorted(dropped.items()))
        print(f"note: {summary} left out — check none of them was user-visible", file=sys.stderr)
    if not found:
        return "_Nothing user-visible._\n"
    out: list[str] = []
    for name, _types in SECTIONS:
        if name not in found:
            continue
        out.append(f"### {name}\n\n")
        out.extend(f"- {line}\n" for line in found[name])
        out.append("\n")
    return "".join(out)


def read_changelog() -> str:
    if CHANGELOG.exists():
        return CHANGELOG.read_text()
    return (
        "# Changelog\n\n"
        "Grouped from the commit log by `tools/changelog.py`. Versions follow "
        "[semantic versioning](https://semver.org).\n\n"
        "## [Unreleased]\n\n"
    )


def split_sections(text: str) -> list[tuple[str, str]]:
    """[(heading line, body)] for every `## ` section, the preamble first with an
    empty heading. Rewriting one section then leaves every other byte alone."""
    parts: list[tuple[str, str]] = []
    heading: str | None = None
    buf: list[str] = []
    for line in text.splitlines(keepends=True):
        if line.startswith("## "):
            parts.append((heading or "", "".join(buf)))
            heading, buf = line, []
        else:
            buf.append(line)
    parts.append((heading or "", "".join(buf)))
    return parts


def write_unreleased(body: str) -> None:
    text = read_changelog()
    parts = split_sections(text)
    out = []
    seen = False
    for heading, content in parts:
        if heading.startswith("## [Unreleased]"):
            out.append((heading, "\n" + body))
            seen = True
        else:
            out.append((heading, content))
    if not seen:
        # No Unreleased section: put one directly after the preamble.
        idx = 1 if parts and parts[0][0] == "" else 0
        out.insert(idx, ("## [Unreleased]\n", "\n" + body))
    CHANGELOG.write_text("".join(h + c for h, c in out))


def release(version: str) -> None:
    """Turn Unreleased into a dated version and leave a fresh Unreleased above it."""
    text = read_changelog()
    parts = split_sections(text)
    today = datetime.date.today().isoformat()
    out = []
    for heading, content in parts:
        if heading.startswith("## [Unreleased]"):
            body = content.strip("\n")
            if not body:
                body = render(last_tag()).strip("\n")
            out.append(("## [Unreleased]\n", "\n"))
            out.append((f"## [{version}] - {today}\n", "\n" + body + "\n\n"))
        else:
            out.append((heading, content))
    CHANGELOG.write_text("".join(h + c for h, c in out))


def retitle(old: str, new: str) -> bool:
    """Give a version that never went out a new number and today's date, keeping what it says."""
    today = datetime.date.today().isoformat()
    out, found = [], False
    for heading, content in split_sections(read_changelog()):
        if heading.startswith(f"## [{old}]"):
            heading, found = f"## [{new}] - {today}\n", True
        out.append((heading, content))
    if found:
        CHANGELOG.write_text("".join(h + c for h, c in out))
    return found


def notes(version: str) -> str:
    """One version's section, without its heading — what a release page shows."""
    for heading, content in split_sections(read_changelog()):
        if heading.startswith(f"## [{version}]"):
            return content.strip("\n") + "\n"
    return ""


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--since", help="rev to start from (default: the last tag, else everything)")
    p.add_argument("--update", action="store_true", help="write into CHANGELOG.md's Unreleased section")
    p.add_argument("--release", metavar="VERSION", help="close Unreleased as this dated version")
    p.add_argument("--notes", metavar="VERSION", help="print one version's section")
    p.add_argument("--retitle", nargs=2, metavar=("OLD", "NEW"), help="renumber an unreleased version's section")
    a = p.parse_args()

    if a.retitle:
        if not retitle(*a.retitle):
            print(f"no section for {a.retitle[0]} in CHANGELOG.md", file=sys.stderr)
            return 1
        print(f"CHANGELOG.md: [{a.retitle[0]}] is now [{a.retitle[1]}]")
        return 0
    if a.notes:
        text = notes(a.notes)
        if not text:
            print(f"no section for {a.notes} in CHANGELOG.md", file=sys.stderr)
            return 1
        print(text, end="")
        return 0
    if a.release:
        release(a.release)
        print(f"CHANGELOG.md: [Unreleased] is now [{a.release}]")
        return 0

    body = render(a.since if a.since else last_tag())
    if a.update:
        write_unreleased(body)
        print("CHANGELOG.md: [Unreleased] rewritten")
        return 0
    print(body, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
