#!/usr/bin/env bash
# Builds the desktop client as a macOS app, ~/Applications/nori.app (Launchpad and Spotlight list it): the
# release binary, the icon from docs/brand/nori-macos.png, an Info.plist, an ad-hoc signature.
#   tools/desktop-app.sh            build and install
#   tools/desktop-app.sh --no-build bundle the binary already in target/release
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
app="${NORI_APP_DIR:-$HOME/Applications}/nori.app"
version=$(grep -oE '^version = "[^"]+"' "$root/Cargo.toml" | head -1 | cut -d'"' -f2)

[ "${1:-}" = --no-build ] || (cd "$root" && cargo build -j4 --release -p nori-desktop)

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$root/target/release/nori-desktop" "$app/Contents/MacOS/nori"

# The icon at every size iconutil wants, from the one 1024 px picture.
set_dir=$(mktemp -d)/nori.iconset
mkdir -p "$set_dir"
for size in 16 32 128 256 512; do
  sips -z $size $size "$root/docs/brand/nori-macos.png" --out "$set_dir/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) "$root/docs/brand/nori-macos.png" --out "$set_dir/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$set_dir" -o "$app/Contents/Resources/nori.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>nori</string>
<key>CFBundleDisplayName</key><string>nori</string>
<key>CFBundleIdentifier</key><string>dev.nori.desktop</string>
<key>CFBundleExecutable</key><string>nori</string>
<key>CFBundleIconFile</key><string>nori</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
<key>LSApplicationCategoryType</key><string>public.app-category.music</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$app" 2>/dev/null
touch "$app"
echo "$app (version $version)"
