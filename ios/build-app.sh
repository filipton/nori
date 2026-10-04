#!/usr/bin/env bash
# Runs on the Mac with Xcode (tools/ipod.sh `app` step copies it there): compiles the Swift sources,
# links libnori_ios.a and lays out nori.app. No Xcode project: one swiftc call is the whole build, and
# it runs the same from a shell on either Mac. The launch screen is a plain image (`UILaunchImages`), so
# nothing of Interface Builder's tooling is needed.
# Env: NORI_VERSION, NORI_BUILD. Cwd: a directory holding ios/, licences/ and libnori_ios.a.
set -euo pipefail
target_os=12.2
sdk=$(xcrun --sdk iphoneos --show-sdk-path)
app=nori.app
rm -rf "$app" obj && mkdir -p "$app" obj

xcrun --sdk iphoneos clang \
  -target arm64-apple-ios$target_os -isysroot "$sdk" -fobjc-arc -O2 \
  -I ios/Sound \
  -c ios/Sound/NoriAudio.m -o obj/NoriAudio.o

xcrun --sdk iphoneos swiftc \
  -target arm64-apple-ios$target_os -sdk "$sdk" -O -wmo \
  -module-name nori \
  -import-objc-header ios/Sources/nori_ios.h \
  ios/Sources/*.swift \
  -L . -lnori_ios obj/NoriAudio.o -lc++ \
  -framework UIKit -framework Foundation -framework Security \
  -framework AVFoundation -framework AudioToolbox -framework CoreAudio \
  -Xlinker -ld_classic -Xlinker -dead_strip \
  -o "$app/nori"

cp ios/Launch-568h@2x.png ios/AppIcon*.png "$app/"
cp -R licences "$app/licences"
sed -e "s/NORI_VERSION/${NORI_VERSION:-0.0.0}/" -e "s/NORI_BUILD/${NORI_BUILD:-1}/" ios/Info.plist > "$app/Info.plist"
plutil -convert binary1 "$app/Info.plist"
cp ios/entitlements.plist "$app/entitlements.plist"
printf 'APPL????' > "$app/PkgInfo"

ls -lh "$app/nori" | awk '{print "  " $5 "  nori (executable)"}'
vtool -show-build "$app/nori" | grep -E "platform|minos" | tr -s ' ' | sed 's/^/  /'
