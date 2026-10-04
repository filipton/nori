#!/usr/bin/env bash
# Builds the iPod app (docs/ipod.md) and puts it on the device, across the machines each step needs:
#   rust     libnori_ios.a here, for aarch64-apple-ios against an iPhoneOS SDK: $NORI_IOS_SDK, else Xcode's
#   app      nori.app with Xcode's swiftc: on this machine, or over SSH on the Mac named by $NORI_IOS_MAC
#   sign     fake-signs it with ldid (AppSync lets it in): this machine's (brew install ldid), else the iPod's
#   install  puts it in /Applications on the iPod and registers it (uicache)
#   run      launches it and tails its log
#   ipa      build/ios/nori-ipod-<version>.ipa: the signed app as Payload/nori.app, for a release
#   tools/ipod.sh            all of the above, in order
#   tools/ipod.sh rust app   just those steps
# Needs: rustup target aarch64-apple-ios, an iPhoneOS SDK (Xcode 15's works for iOS 12; a machine without
# Xcode can build the Rust half against a copy of it), and for the steps that reach the iPod sshpass and
# libimobiledevice (iproxy) from brew.
#
# The iPod is reached over USB: iproxy forwards port 2244 to checkra1n's dropbear (port 44), password
# auth (`NORI_IPOD_PASSWORD`, default alpine, through sshpass): this dropbear accepts a public key and then
# stalls, so keys are not used.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"; root="$(cd "$here/.." && pwd)"
sdk="${NORI_IOS_SDK:-$(xcrun --sdk iphoneos --show-sdk-path 2>/dev/null || true)}"
# Empty: this machine has Xcode and links the app itself.
mac="${NORI_IOS_MAC:-}"
remote_dir="${NORI_IOS_REMOTE:-nori-ios}"
port="${NORI_IPOD_PORT:-2244}"
target_os=12.2
version=$(grep -oE '^version = "[^"]+"' "$root/Cargo.toml" | head -1 | cut -d'"' -f2)
build_no=$(date +%Y%m%d%H%M)
out="$root/build/ios"
lib="$root/target/aarch64-apple-ios/release/libnori_ios.a"
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

step_rust() {
  [ -d "$sdk" ] || { echo "no iPhoneOS SDK: install Xcode, or set NORI_IOS_SDK to a copy of Xcode's" >&2; exit 1; }
  echo "rust: libnori_ios.a for iOS $target_os …"
  # SDKROOT points the C compilers at the iPhoneOS SDK, and clang reads it when linking too: build
  # scripts, which run on this Mac, link through a wrapper that clears it.
  local host linker
  host=$(rustc -vV | sed -n 's/^host: //p' | tr 'a-z-' 'A-Z_')
  mkdir -p "$out"
  linker="$out/host-cc"
  printf '#!/bin/sh\nunset SDKROOT\nexec cc "$@"\n' > "$linker"
  chmod +x "$linker"
  (cd "$root" && env "CARGO_TARGET_${host}_LINKER=$linker" SDKROOT="$sdk" IPHONEOS_DEPLOYMENT_TARGET=$target_os \
    cargo build -j4 --release --target aarch64-apple-ios -p nori-ios)
  ls -lh "$lib" | awk '{print "  " $5 "  " $9}'
}

# nori.app, built where Xcode is (this machine, or $NORI_IOS_MAC over SSH) and brought back.
step_app() {
  [ -f "$lib" ] || { echo "no $lib: run the rust step first" >&2; exit 1; }
  local dest work=""
  if [ -n "$mac" ]; then
    echo "app: on $mac …"
    ssh "$mac" "mkdir -p $remote_dir"
    dest="$mac:$remote_dir"
  else
    echo "app: here …"
    work="$root/build/ios-work"
    mkdir -p "$work"
    dest="$work"
  fi
  rsync -a --delete "$root/ios/" "$dest/ios/"
  rsync -a "$lib" "$dest/libnori_ios.a"
  # The licence texts the credits page shows, kept once, with Android's.
  rsync -a --delete "$root/app/src/main/assets/licences/" "$dest/licences/"
  local build="NORI_VERSION='$version' NORI_BUILD='$build_no' bash ios/build-app.sh"
  if [ -n "$mac" ]; then ssh "$mac" "cd $remote_dir && $build"; else (cd "$work" && eval "$build"); fi
  mkdir -p "$out"
  rsync -a --delete "$dest/nori.app/" "$app/"
  du -sh "$app" | awk '{print "  " $1 "  nori.app"}'
}

# Signs build/ios/nori.app in place: with this machine's ldid (brew install ldid) if there is one, else on
# the iPod, whose signed copy comes back.
step_sign() {
  [ -d "$app" ] || { echo "no $app: run the app step first" >&2; exit 1; }
  if command -v ldid >/dev/null; then
    echo "sign: with this machine's ldid …"
    ldid -S"$app/entitlements.plist" "$app/nori"
    ldid -e "$app/nori" | grep -q application-identifier
  else
    forward
    echo "sign: with the iPod's ldid …"
    "${ipod_ssh[@]}" "rm -rf /tmp/nori-sign && mkdir -p /tmp/nori-sign"
    # COPYFILE_DISABLE: no AppleDouble ._ files from macOS's tar inside the app.
    COPYFILE_DISABLE=1 tar -C "$out" -cf - nori.app | "${ipod_ssh[@]}" "tar -C /tmp/nori-sign -xf -"
    "${ipod_ssh[@]}" "cd /tmp/nori-sign && ldid -S/tmp/nori-sign/nori.app/entitlements.plist nori.app/nori && ldid -e nori.app/nori | grep -q application-identifier"
    rm -rf "$app"
    "${ipod_ssh[@]}" "tar -C /tmp/nori-sign -cf - nori.app" | tar -C "$out" -xf -
  fi
  echo "  signed"
}

step_install() {
  forward
  echo "install: /Applications/nori.app …"
  [ -d "$app" ] || { echo "no $app: run the app and sign steps first" >&2; exit 1; }
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

# The signed app, zipped the way an .ipa is.
step_ipa() {
  [ -d "$app" ] || { echo "no $app: run the app and sign steps first" >&2; exit 1; }
  echo "ipa: …"
  rm -rf "$out/Payload" && mkdir -p "$out/Payload"
  cp -R "$app" "$out/Payload/"
  rm -f "$out/nori-ipod-$version.ipa"
  (cd "$out" && zip -qry "nori-ipod-$version.ipa" Payload)
  rm -rf "$out/Payload"
  echo "  $out/nori-ipod-$version.ipa"
}

steps=("$@")
[ ${#steps[@]} -eq 0 ] && steps=(rust app sign install run)
for s in "${steps[@]}"; do
  case "$s" in
    rust|app|sign|install|run|ipa) "step_$s" ;;
    *) echo "unknown step: $s" >&2; exit 2 ;;
  esac
done
