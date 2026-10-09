#!/usr/bin/env bash
# A fresh accessibility tree without waiting for an animating player to go idle.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
out="$here/../build/ui-dump"
if [ ! -f "$out/classes.dex" ] || [ "$here/UiDump.java" -nt "$out/classes.dex" ]; then
  if [ "${1:-}" != --build ]; then
    python3 "$here/with-resource.py" build bash "$0" --build
  else
    mkdir -p "$out/classes"
    android_jar=$(rg --files "$sdk/platforms" | rg '/android.jar$' | sort | tail -1)
    javac --release 8 -Xlint:-options -cp "$android_jar" -d "$out/classes" "$here/UiDump.java"
    d8=$(rg --files "$sdk/build-tools" | rg '/d8$' | sort | tail -1)
    "$d8" --min-api 26 --output "$out" "$out/classes/dev/nori/tools/UiDump.class"
  fi
fi
[ "${1:-}" != --build ] || exit 0
adb push "$out/classes.dex" /data/local/tmp/nori-ui-dump.dex >/dev/null 2>&1
tree=$(adb exec-out 'CLASSPATH=/data/local/tmp/nori-ui-dump.dex app_process /system/bin dev.nori.tools.UiDump')
[[ "$tree" == '<?xml'* ]] || { echo "no accessibility snapshot: $tree" >&2; exit 1; }
printf '%s\n' "$tree"
