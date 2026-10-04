#!/usr/bin/env bash
# Compiles the Swift sources, links libnori_ios.a and lays out nori.app (tools/ipod.sh `app` step runs it:
# here, in its Linux container, or on $NORI_IOS_MAC). No Xcode project: one swiftc call is the whole
# build. The launch screen is a plain image (`UILaunchImages`), so nothing of Interface Builder's
# tooling is needed.
# Env: NORI_VERSION, NORI_BUILD; NORI_IOS_SDK, an iPhoneOS SDK (default Xcode's); NORI_SWIFT_IOS, for a
# toolchain other than Xcode's, the directory of the iOS parts it lacks (iphoneos/, apinotes/, darwin/).
# Cwd: a directory holding ios/, licences/ and libnori_ios.a.
set -euo pipefail
target_os=12.2
sdk="${NORI_IOS_SDK:-$(xcrun --sdk iphoneos --show-sdk-path)}"
app=nori.app
rm -rf "$app" obj && mkdir -p "$app" obj

swift_flags=()
if [ -n "${NORI_SWIFT_IOS:-}" ]; then
  # The toolchain's own resource directory is for its host (Linux's carries a Dispatch module that clashes
  # with the SDK's): a new one keeps only the platform-free parts and adds the iOS ones.
  res=$(swiftc -print-target-info | python3 -c 'import json, sys; print(json.load(sys.stdin)["paths"]["runtimeResourcePath"])')
  mkdir -p obj/swift
  ln -s "$res/shims" obj/swift/shims
  ln -s "$res/clang" obj/swift/clang
  ln -s "$NORI_SWIFT_IOS/iphoneos" obj/swift/iphoneos
  ln -s "$NORI_SWIFT_IOS/apinotes" obj/swift/apinotes
  # clang links its runtime library (__isPlatformVersionAtLeast) from <resource dir>/lib/darwin.
  mkdir -p obj/clang/lib
  ln -s "$NORI_SWIFT_IOS/darwin" obj/clang/lib/darwin
  swift_flags+=(-resource-dir obj/swift -Xclang-linker -resource-dir -Xclang-linker obj/clang)
fi
# Xcode 26's linker makes binaries that crash on iOS 12.5 (docs/ipod.md): Apple's classic one, or lld.
if [ "$(uname)" = Darwin ]; then
  swift_flags+=(-Xlinker -ld_classic)
else
  swift_flags+=(-use-ld=lld)
fi

clang \
  -target arm64-apple-ios$target_os -isysroot "$sdk" -fobjc-arc -O2 \
  -I ios/Sound \
  -c ios/Sound/NoriAudio.m -o obj/NoriAudio.o

swiftc \
  -target arm64-apple-ios$target_os -sdk "$sdk" -O -wmo \
  -module-name nori \
  -import-objc-header ios/Sources/nori_ios.h \
  ios/Sources/*.swift \
  -L . -lnori_ios obj/NoriAudio.o -lc++ \
  -framework UIKit -framework Foundation -framework Security \
  -framework AVFoundation -framework AudioToolbox -framework CoreAudio \
  "${swift_flags[@]}" -Xclang-linker -isysroot -Xclang-linker "$sdk" -Xlinker -dead_strip \
  -o "$app/nori"

cp ios/Launch-568h@2x.png ios/AppIcon*.png "$app/"
cp -R licences "$app/licences"
sed -e "s/NORI_VERSION/${NORI_VERSION:-0.0.0}/" -e "s/NORI_BUILD/${NORI_BUILD:-1}/" ios/Info.plist |
  python3 -c 'import plistlib, sys; sys.stdout.buffer.write(plistlib.dumps(plistlib.loads(sys.stdin.buffer.read()), fmt=plistlib.FMT_BINARY))' \
  > "$app/Info.plist"
cp ios/entitlements.plist "$app/entitlements.plist"
printf 'APPL????' > "$app/PkgInfo"

ls -lh "$app/nori" | awk '{print "  " $5 "  nori (executable)"}'
{ otool -l "$app/nori" 2>/dev/null || llvm-objdump --macho --private-headers "$app/nori"; } |
  grep -A4 LC_BUILD_VERSION | grep -E "platform|minos|sdk" | tr -s ' ' | sed 's/^/  /'
