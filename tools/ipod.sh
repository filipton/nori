#!/usr/bin/env bash
# Builds the iPod app (docs/site/developers/ipod-internals.md) and puts it on the device:
#   build    build/ios/nori.app and nori-ipod-<version>.ipa, fake-signed for AppSync: tools/ios-build/build.sh in
#            the Docker image of tools/ios-build/Dockerfile, the only thing a machine needs
#   install  puts it in /Applications on the iPod and registers it (uicache)
#   run      launches it and tails its log
#   tools/ipod.sh                build, then install run when an iPod is on USB
#   tools/ipod.sh install run    just those steps
# The SDK and the toolchains' caches stay in Docker's volume nori-ios-cache. install and run need
# sshpass and libimobiledevice (iproxy).
#
# The iPod is reached over USB: iproxy forwards port 2244 to checkra1n's dropbear (port 44), password
# auth (`NORI_IPOD_PASSWORD`, default alpine, through sshpass): this dropbear accepts a public key and then
# stalls, so keys are not used.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"; root="$(cd "$here/.." && pwd)"
port="${NORI_IPOD_PORT:-2244}"
version=$(grep -oE '^version = "[^"]+"' "$root/Cargo.toml" | head -1 | cut -d'"' -f2)
build_no=$(date +%Y%m%d%H%M)
out="$root/build/ios"
app="$out/nori.app"

ipod_ssh=(sshpass -p "${NORI_IPOD_PASSWORD:-alpine}" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o PubkeyAuthentication=no -p "$port" root@127.0.0.1)

# Starts the USB forward if nothing listens on the port yet.
forward() {
  if ! nc -z 127.0.0.1 "$port" 2>/dev/null; then
    # Detached: it stays up for the next step and the next run.
    nohup iproxy "$port" 44 >/dev/null 2>&1 < /dev/null &
    disown
    sleep 1
  fi
}

# As this user, with the checkout at its own path. The cache and cargo's target are in a volume, not a folder of this machine's:
# the SDK is an archive of symlinks, which cannot be unpacked onto a folder Docker shares from a Mac, and rustc
# dies with SIGBUS on mapped files there.
step_build() {
  docker build -q -t nori-ios-build "$here/ios-build" >/dev/null
  docker run --rm -u 0 -v nori-ios-cache:/cache nori-ios-build sh -c \
    'mkdir -p /cache/home && chown "$0" /cache /cache/home' "$(id -u):$(id -g)"
  docker run --rm -u "$(id -u):$(id -g)" -v "$root:$root" -v nori-ios-cache:/cache -w "$root" \
    -e HOME=/cache/home -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target \
    -e NORI_IOS_CACHE=/cache -e NORI_VERSION="$version" -e NORI_BUILD="$build_no" \
    nori-ios-build tools/ios-build/build.sh
}

step_install() {
  forward
  echo "install: /Applications/nori.app …"
  [ -d "$app" ] || { echo "no $app: run the build step first" >&2; exit 1; }
  # COPYFILE_DISABLE: no AppleDouble ._ files from macOS's tar inside the app.
  COPYFILE_DISABLE=1 tar -C "$out" -cf - nori.app | "${ipod_ssh[@]}" "rm -rf /Applications/nori.app && tar -C /Applications -xf - && chown -R root:wheel /Applications/nori.app && chmod 755 /Applications/nori.app/nori && uicache --path /Applications/nori.app --respring"
  echo "  installed (version $version, build $build_no)"
}

step_run() {
  forward
  echo "run: …"
  # The device has no awk or pgrep: ps, grep and cut only.
  # A respring from install is still coming back; uiopen before SpringBoard is up does not start the app.
  local _
  for _ in 1 2 3 4 5 6; do
    "${ipod_ssh[@]}" "uiopen -b dev.nori.music; ps aux | grep -q '[A]pplications/nori.app/nori'" && break
    sleep 2
  done
  "${ipod_ssh[@]}" "ps aux | grep '[A]pplications/nori.app/nori' | cut -c1-90 | sed 's/^/  /'; ls -la '/var/mobile/Library/Application Support/nori/' | sed 's/^/  /'"
}

steps=("$@")
if [ ${#steps[@]} -eq 0 ]; then
  steps=(build)
  if command -v idevice_id >/dev/null && [ -n "$(idevice_id -l 2>/dev/null)" ]; then steps+=(install run); fi
fi
for s in "${steps[@]}"; do
  case "$s" in
    build|install|run) "step_$s" ;;
    *) echo "unknown step: $s" >&2; exit 2 ;;
  esac
done
