#!/usr/bin/env bash
# Reserve one configured emulator for install, setup and the entire suite.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/.."
if [ "${1:-}" != --leased ]; then
  exec python3 "$here/with-resource.py" device bash "$0" --leased "$@"
fi
shift
if [ "${1:-}" = --install ]; then
  adb install -r "$2"
  shift 2
fi
case "${1:-}" in
  smoke|audio|feature) suite="$1"; shift ;;
  *) echo "usage: device-check.sh [--install APK] smoke|audio|feature [--only SECTIONS]" >&2; exit 2 ;;
esac
case "$suite" in
  smoke) exec "$here/smoke.sh" "$@" ;;
  audio) exec "$here/audio-e2e.sh" "$@" ;;
  feature) exec "$here/feature-e2e.sh" "$@" ;;
esac
