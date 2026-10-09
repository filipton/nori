#!/usr/bin/env bash
# Run an existing arm64 AVD with host graphics; choose a distinct even port for each worker.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
if [ "${1:-}" = --leased ]; then shift; else
  exec python3 "$here/with-resource.py" "device:emulator-${2:-5554}" bash "$0" --leased "$@"
fi
avd="${1:-a16}"; port="${2:-5554}"
[[ "$port" =~ ^[0-9]+$ ]] && [ $((port % 2)) -eq 0 ] || { echo "use an even emulator port" >&2; exit 2; }
serial="emulator-$port"
if adb -s "$serial" get-state >/dev/null 2>&1; then
  echo "$serial is already running"
  exit 0
fi
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
mkdir -p "$here/../build/emulators"
nohup "$sdk/emulator/emulator" \
  -avd "$avd" -port "$port" -gpu host -cores 4 -no-window -no-audio -no-boot-anim \
  -no-snapshot-save > "$here/../build/emulators/$serial.log" 2>&1 < /dev/null &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT
deadline=$((SECONDS + 120))
until [ "$(adb -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ]; do
  kill -0 "$pid" 2>/dev/null || { cat "$here/../build/emulators/$serial.log" >&2; exit 1; }
  [ "$SECONDS" -lt "$deadline" ] || { echo "$serial did not boot" >&2; exit 1; }
  sleep 0.3
done
trap - EXIT
echo "$serial ready ($avd, host graphics)"
