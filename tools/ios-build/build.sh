#!/usr/bin/env bash
# Builds the iPod app inside the container of ./Dockerfile (tools/ipod.sh starts it, the checkout as working
# directory): libnori_ios.a, then nori.app around it with one swiftc call (no Xcode project), fake-signed with
# ldid for AppSync, and build/ios/nori-ipod-<version>.ipa.
# Env: NORI_IOS_CACHE, where the SDK and Swift's iOS parts stay between runs; NORI_VERSION, NORI_BUILD.
set -euo pipefail
cache="$NORI_IOS_CACHE"
target_os=12.2
out=build/ios
app="$out/nori.app"
obj="$out/obj"
lib="$CARGO_TARGET_DIR/aarch64-apple-ios/release/libnori_ios.a"

# theos' copy of the iPhoneOS SDK: headers, .tbd stubs and Swift interfaces of iOS 16.4.
sdk_name=iPhoneOS16.5.sdk
sdk_sha256=5e0fd3f01266cce4ce012d4a99b38eb56578fca40d09edc81cd83dee958202fb
sdk="$cache/$sdk_name"
# The image's Swift (the Dockerfile's SWIFT): its macOS toolchain carries the iOS parts below.
swift_pkg_sha256=ee82e57774d6650f94aa06302435d6f44a055b9411698db8ecb85d9a3bcc91d0
swift_ios="$cache/swift-$SWIFT_VERSION-ios"

# Downloads $1 into $cache/$2, checked against the sha256 $3.
fetch() {
  mkdir -p "$cache"
  curl -sSfL "$1" -o "$cache/$2.part"
  echo "$3  $cache/$2.part" | sha256sum -c --quiet - || { rm -f "$cache/$2.part"; exit 1; }
  mv "$cache/$2.part" "$cache/$2"
}

if [ ! -d "$sdk" ]; then
  echo "sdk: $sdk_name from theos/sdks …"
  fetch "https://github.com/theos/sdks/releases/download/master-146e41f/$sdk_name.tar.xz" sdk.tar.xz "$sdk_sha256"
  # Unpacked aside and moved in whole: an unpacking that failed leaves no SDK that later runs would trust.
  rm -rf "$sdk.part" && mkdir -p "$sdk.part"
  tar -xJf "$cache/sdk.tar.xz" -C "$sdk.part"
  mv "$sdk.part/$sdk_name" "$sdk" && rmdir "$sdk.part"
  rm "$cache/sdk.tar.xz"
fi

# What Linux's toolchain lacks for iOS: Swift's compatibility libraries that a deployment target below iOS 13
# links, the API notes of Darwin's Dispatch and os, and clang's iOS runtime library.
if [ ! -d "$swift_ios" ]; then
  echo "swift: iOS parts of swift.org's $SWIFT_VERSION macOS toolchain (1.5 GB, once) …"
  name="swift-$SWIFT_VERSION-RELEASE"
  fetch "https://download.swift.org/swift-$SWIFT_VERSION-release/xcode/$name/$name-osx.pkg" swift.pkg "$swift_pkg_sha256"
  mkdir -p "$swift_ios.part"
  python3 "$(dirname "$0")/pkg-payload.py" "$cache/swift.pkg" "$name-osx-package.pkg" |
    bsdtar -xf - -C "$swift_ios.part" -s ',^\./usr/lib/swift/,,' -s ',^\./usr/lib/clang/[^/]*/lib/,,' \
      './usr/lib/swift/iphoneos/libswiftCompatibility*.a' './usr/lib/swift/apinotes/*.apinotes' \
      './usr/lib/clang/*/lib/darwin/libclang_rt.ios.a'
  rm "$cache/swift.pkg"
  mv "$swift_ios.part" "$swift_ios"
fi

echo "rust: libnori_ios.a for iOS $target_os …"
SDKROOT="$sdk" IPHONEOS_DEPLOYMENT_TARGET=$target_os cargo build -j4 --release --target aarch64-apple-ios -p nori-ios
ls -lh "$lib" | awk '{print "  " $5 "  libnori_ios.a"}'

echo "app: …"
rm -rf "$app" "$obj" && mkdir -p "$app" "$obj"
# Swift's resource directory is Linux's (its Dispatch module clashes with the SDK's): a new one keeps the
# platform-free parts and adds the iOS ones. clang links its runtime library from <its resource dir>/lib/darwin.
res=$(swiftc -print-target-info | python3 -c 'import json, sys; print(json.load(sys.stdin)["paths"]["runtimeResourcePath"])')
mkdir -p "$obj/swift" "$obj/clang/lib"
ln -s "$res/shims" "$obj/swift/shims"
ln -s "$res/clang" "$obj/swift/clang"
ln -s "$swift_ios/iphoneos" "$obj/swift/iphoneos"
ln -s "$swift_ios/apinotes" "$obj/swift/apinotes"
ln -s "$swift_ios/darwin" "$obj/clang/lib/darwin"

clang -target arm64-apple-ios$target_os -isysroot "$sdk" -fobjc-arc -O2 -I ios/Sound \
  -c ios/Sound/NoriAudio.m -o "$obj/NoriAudio.o"

# lld, not Apple's new linker, whose binaries crash on iOS 12.5 (docs/ipod.md): dyld info, no chained fixups.
# No retain sinking or release hoisting: a compiler that is not Apple's moves the releases of the empty array
# singleton ahead of its retains, which only an immortal one survives, and iOS 12's is not (abort in malloc).
swiftc \
  -target arm64-apple-ios$target_os -sdk "$sdk" -resource-dir "$obj/swift" -O -wmo \
  -Xllvm -sil-disable-pass=retain-sinking -Xllvm -sil-disable-pass=release-hoisting \
  -module-name nori \
  -import-objc-header ios/Sources/nori_ios.h \
  ios/Sources/*.swift \
  -L "$(dirname "$lib")" -lnori_ios "$obj/NoriAudio.o" -lc++ \
  -framework UIKit -framework Foundation -framework Security \
  -framework AVFoundation -framework AudioToolbox -framework CoreAudio \
  -use-ld=lld -Xclang-linker -isysroot -Xclang-linker "$sdk" \
  -Xclang-linker -resource-dir -Xclang-linker "$obj/clang" -Xlinker -dead_strip \
  -o "$app/nori"

# The launch screen is a plain image (UILaunchImages): nothing of Interface Builder's tooling is needed.
cp ios/Launch-568h@2x.png ios/AppIcon*.png "$app/"
# The licence texts the credits page shows, kept once, with Android's.
cp -R app/src/main/assets/licences "$app/licences"
sed -e "s/NORI_VERSION/$NORI_VERSION/" -e "s/NORI_BUILD/$NORI_BUILD/" ios/Info.plist |
  python3 -c 'import plistlib, sys; sys.stdout.buffer.write(plistlib.dumps(plistlib.loads(sys.stdin.buffer.read()), fmt=plistlib.FMT_BINARY))' \
  > "$app/Info.plist"
cp ios/entitlements.plist "$app/entitlements.plist"
printf 'APPL????' > "$app/PkgInfo"
ls -lh "$app/nori" | awk '{print "  " $5 "  nori (executable)"}'
llvm-objdump --macho --private-headers "$app/nori" | grep -A4 LC_BUILD_VERSION | grep -E "minos|sdk" | tr -s ' ' | sed 's/^/  /'

echo "sign: …"
ldid -S"$app/entitlements.plist" "$app/nori"
ldid -e "$app/nori" | grep -q application-identifier

echo "ipa: …"
ipa="$out/nori-ipod-$NORI_VERSION.ipa"
rm -rf "$out/Payload" "$ipa" && mkdir -p "$out/Payload"
cp -R "$app" "$out/Payload/"
(cd "$out" && zip -qry "$(basename "$ipa")" Payload)
rm -rf "$out/Payload" "$obj"
echo "  $ipa"
