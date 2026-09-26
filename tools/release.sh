#!/usr/bin/env bash
# Makes a release: a signed APK, checksums and notes, put on GitHub.
#
#   tools/release.sh                    the guided release (below)
#   tools/release.sh --build            only build, into build/release-<version>/, nothing leaves the machine
#   tools/release.sh --publish          build this version and publish it, no questions (for scripts)
#   tools/release.sh --live             with --publish: published rather than a draft
#   tools/release.sh --abi arm64-v8a    phones only: about half the size
#   tools/release.sh --no-test          skip cargo test first
#
# The guided release shows the latest version on GitHub and the one in the code, asks for the new
# version, then does every step itself: tools/bump-version.sh, tools/changelog.py --update and
# --release, shows the notes (and opens $EDITOR on CHANGELOG.md if you want to reword them), commits
# "build: release <version>", builds, tags, pushes and creates the GitHub release - a draft or live,
# as you answer. Saying no to any question puts every file back as it was. If the build fails after
# the commit, run it again: a version that is in the code but not yet on GitHub is offered first, and
# its changelog section is kept.
#
# By default the APK carries the Rust core for both 64-bit ABIs, so nobody has to choose: Android
# installs the slice that matches. arm64-v8a is every phone of the last decade, x86_64 is emulators
# and Chromebooks.
#
# Leaves one directory holding everything a release page needs:
#
#   nori-music-<version>.apk    (nori-music-<version>-<abi>.apk with --abi)
#   SHA256SUMS                  one line per file, as `sha256sum -c` wants it
#   RELEASE.txt                 version, commit, ABIs, size, signing certificate
#
# The APK is signed with nori-release.jks, through keystore.properties (both gitignored). The first
# run adopts this machine's Android debug key as that key (what every earlier build was signed with,
# so installed copies update in place), or creates a new one if there is none. That key is what lets
# a phone update rather than reinstall: lose it and every install has to be removed before the next
# release will go on. Keep a copy somewhere safe. The certificate is printed rather than assumed, so
# compare it with the last release before uploading.
#
# Publishing is the only part that leaves this machine. It refuses to run from a dirty tree, takes the
# release notes from the CHANGELOG section for this version, tags the commit, pushes it, and attaches
# the files to a GitHub release - a draft, so nothing is public until you press publish. --live skips
# the draft.
set -euo pipefail
cd "$(dirname "$0")/.."

ABI=arm64-v8a,x86_64
RUN_TESTS=1
PUBLISH=0
GUIDED=1
DRAFT=--draft
while [ $# -gt 0 ]; do
  case "$1" in
    --abi) ABI="$2"; shift ;;
    --no-test) RUN_TESTS=0 ;;
    --build) GUIDED=0 ;;
    --publish) PUBLISH=1; GUIDED=0 ;;
    --live) DRAFT="" ;;
    -h|--help) sed -n 2,39p "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

die() { echo "$@" >&2; exit 1; }
code_version() { sed -n 's/.*versionName = "\([^"]*\)".*/\1/p' app/build.gradle.kts | head -1; }
# a > b, as versions (0.3.10 is after 0.3.9)
newer() { [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]; }

# --- the guided release ------------------------------------------------------------
if [ "$GUIDED" = 1 ]; then
  [ -t 0 ] || die "the guided release asks questions: run it in a terminal, or use --build / --publish"
  command -v gh >/dev/null || die "releasing needs the GitHub CLI (gh) on PATH"
  gh auth status >/dev/null 2>&1 || die "gh is not logged in: run 'gh auth login'"
  [ -z "$(git status --porcelain)" ] || die "commit or stash your changes first: the release commit holds only the release"

  current=$(code_version)
  latest=$(gh release list --exclude-drafts --limit 1 --json tagName --jq '.[0].tagName // ""' 2>/dev/null || true)
  latest=${latest#v}
  echo "latest release on GitHub:  ${latest:-none yet}"
  echo "version in the code:       $current"
  # A version already in the code but not yet released is what a failed or first run left behind:
  # offer that. Otherwise the next patch after whichever of the two is further on.
  if ! gh release view "v$current" >/dev/null 2>&1 && { [ -z "$latest" ] || newer "$current" "$latest"; }; then
    suggest=$current
  else
    base=$current; [ -n "$latest" ] && newer "$latest" "$base" && base=$latest
    IFS=. read -r ma mi pa <<< "$base"; suggest="$ma.$mi.$((pa + 1))"
  fi
  echo "(the GitHub tag gets its v by itself: 0.3.3 is released as v0.3.3)"
  # -e: line editing, so an arrow key moves the cursor instead of typing an escape sequence.
  read -erp "new version [$suggest]: " new
  new=$(printf '%s' "$new" | tr -d '[:space:]'); new=${new#[vV]}
  new=${new:-$suggest}
  [[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "not a version: $new (want major.minor.patch, e.g. 0.3.3)"
  [ -z "$latest" ] || newer "$new" "$latest" || die "$new is not after the latest release, $latest"
  ! gh release view "v$new" >/dev/null 2>&1 || die "a release v$new already exists on GitHub"

  # From here on files change. Until the commit, any way out puts them back.
  committed=0
  undo() { if [ "$committed" = 0 ]; then git checkout -q -- . && echo "stopped: every file is as it was."; fi; }
  trap undo EXIT
  [ "$new" = "$current" ] || tools/bump-version.sh "$new"
  # The version in the code never went out, and this one replaces it: its notes are this release's.
  if [ "$new" != "$current" ] && ! gh release view "v$current" >/dev/null 2>&1 &&
     tools/changelog.py --notes "$current" >/dev/null 2>&1; then
    tools/changelog.py --retitle "$current" "$new"
    extra=$(tools/changelog.py 2>/dev/null)
    if [ "$extra" != "_Nothing user-visible._" ]; then
      echo "also since then, not yet in the notes - add what matters when you edit:"
      echo "$extra"
    fi
  fi
  if ! tools/changelog.py --notes "$new" >/dev/null 2>&1; then
    tools/changelog.py --update
    tools/changelog.py --release "$new"
  fi
  echo
  echo "---- release notes for $new ----"
  tools/changelog.py --notes "$new"
  echo "--------------------------------"
  read -erp "edit them first? [y/N] " a
  if [[ "$a" =~ ^[Yy] ]]; then
    "${EDITOR:-nano}" CHANGELOG.md
    tools/changelog.py --notes "$new" >/dev/null || die "CHANGELOG.md lost its [$new] section"
  fi
  read -erp "publish $new as a (d)raft, (l)ive, or (s)top? [d/l/s] " a
  case "$a" in
    l|L) DRAFT="" ;;
    d|D|"") DRAFT=--draft ;;
    *) exit 0 ;;
  esac
  if [ -n "$(git status --porcelain)" ]; then
    git commit -qam "build: release $new"
    echo "committed: build: release $new"
  fi
  committed=1
  trap - EXIT
  PUBLISH=1
fi

version=$(code_version)
commit=$(git rev-parse --short=10 HEAD 2>/dev/null || echo unknown)
if [ -n "$(git status --porcelain 2>/dev/null)" ]; then dirty=" (dirty)"; else dirty=""; fi

# Everything that would stop a publish is checked before the build rather than after it.
if [ "$PUBLISH" = 1 ]; then
  command -v gh >/dev/null || die "publishing needs the GitHub CLI (gh) on PATH"
  gh auth status >/dev/null 2>&1 || die "gh is not logged in: run 'gh auth login'"
  git remote get-url origin >/dev/null 2>&1 || die "this repository has no 'origin' remote to publish to"
  [ -z "$dirty" ] || die "refusing to publish from a dirty tree: commit or stash first"
  [ -f keystore.properties ] || die "no keystore.properties: run tools/release.sh --build once to create the key, and back it up"
  ./tools/changelog.py --notes "$version" >/dev/null ||
    die "CHANGELOG.md has no section for $version.
write one, or generate it:  tools/changelog.py --update && tools/changelog.py --release $version"
  ! gh release view "v$version" >/dev/null 2>&1 ||
    die "a release v$version already exists. move to a new version first: tools/bump-version.sh <x.y.z>"
fi

# --- signing key ----------------------------------------------------------------
# Every build before 0.3.2 was signed with this machine's Android debug key, and a phone only updates
# in place from the same key. So the first run adopts that key as the release key - a copy, kept
# here - and nothing installed has to be removed. Only a machine without one gets a fresh key.
if [ ! -f keystore.properties ]; then
  debug_ks="$HOME/.android/debug.keystore"
  if [ -f "$debug_ks" ]; then
    cp "$debug_ks" nori-release.jks
    cat > keystore.properties <<PROPS
storeFile=nori-release.jks
storePassword=android
keyAlias=androiddebugkey
keyPassword=android
PROPS
    echo "adopted this machine's debug key as nori-release.jks, so installed builds update in place."
  else
    KEYTOOL=$(command -v keytool || ls "${JAVA_HOME:-/usr/lib/jvm/default}"/bin/keytool 2>/dev/null | head -1)
    [ -n "$KEYTOOL" ] || die "keytool not found; set JAVA_HOME"
    PASS=$(head -c 24 /dev/urandom | base64 | tr -d '/+=')
    "$KEYTOOL" -genkeypair -keystore nori-release.jks -alias nori -keyalg RSA -keysize 4096 \
      -validity 10000 -storepass "$PASS" -keypass "$PASS" -dname "CN=Nori" >/dev/null 2>&1
    cat > keystore.properties <<PROPS
storeFile=nori-release.jks
storePassword=$PASS
keyAlias=nori
keyPassword=$PASS
PROPS
    echo "created a new key, nori-release.jks."
  fi
  chmod 600 keystore.properties nori-release.jks
  echo "keystore.properties + nori-release.jks are gitignored. BACK THEM UP: without them no phone can update to a later release."
fi

if [ "$RUN_TESTS" = 1 ]; then
  echo "==> cargo test"
  cargo test -q
fi

out="build/release-$version"
rm -rf "$out"
mkdir -p "$out"

echo "==> building $version for $ABI"
./gradlew :app:assembleRelease -PrustTargets="$ABI" -q
case "$ABI" in
  *,*) name="nori-music-$version.apk" ;;
  *) name="nori-music-$version-$ABI.apk" ;;
esac
cp app/build/outputs/apk/release/app-release.apk "$out/$name"

# --- what is in the directory ---------------------------------------------------
apksigner=$(ls -d "${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"/build-tools/*/apksigner 2>/dev/null | sort -V | tail -1 || true)
signer() {
  [ -x "${apksigner:-}" ] || { echo "unknown (apksigner not found)"; return; }
  "$apksigner" verify --print-certs "$1" 2>/dev/null |
    sed -n 's/.*certificate SHA-256 digest: \(.*\)/\1/p' | head -1
}

( cd "$out" && sha256sum ./*.apk | sed 's# \./# #' > SHA256SUMS )

{
  echo "nori $version"
  echo "commit $commit$dirty"
  echo "built $(date -u '+%Y-%m-%d %H:%M UTC')"
  echo "minSdk 26 (Android 8.0)"
  echo
  f="$out/$name"
  echo "$name"
  echo "  carries ${ABI//,/ + }"
  echo "  $(du -h "$f" | cut -f1)  ·  sha256 $(sha256sum "$f" | cut -c1-16)…"
  echo "  signing certificate SHA-256: $(signer "$f")"
} > "$out/RELEASE.txt"

echo
cat "$out/RELEASE.txt"
echo
echo "everything to upload is in $out/"

if [ "$PUBLISH" != 1 ]; then
  echo
  echo "to release it:  tools/release.sh"
  exit 0
fi

# --- publish --------------------------------------------------------------------
notes=$(mktemp)
trap 'rm -f "$notes"' EXIT
./tools/changelog.py --notes "$version" > "$notes"

if git rev-parse "v$version" >/dev/null 2>&1; then
  echo "==> tag v$version already exists, reusing it"
else
  echo "==> tagging v$version"
  git tag -a "v$version" -m "nori $version"
fi
# The branch goes first: a tag whose commit is on no branch is one nobody can reach from the repo page.
git push origin "$(git branch --show-current)"
git push origin "v$version"

echo "==> creating the release${DRAFT:+ (draft)}"
# shellcheck disable=SC2086
gh release create "v$version" "$out/$name" "$out/SHA256SUMS" \
  --title "nori $version" --notes-file "$notes" $DRAFT

echo
echo "done: $(gh release view "v$version" --json url --jq .url 2>/dev/null || echo "v$version")"
if [ -n "$DRAFT" ]; then
  echo "it is a draft - nothing is public until you press publish."
fi
