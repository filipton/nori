#!/usr/bin/env bash
# Drives a debug build of nori directly, instead of tapping screen coordinates:
#   tools/app.sh open settings/sound      navigate to a route
#   tools/app.sh play "search:noise 1"    play a song by id, album, or first search hit
#   tools/app.sh set limiter true         flip one setting
#   tools/app.sh state                    one JSON line: route, playback, key settings
# Needs the app running (tools/app.sh launch). Answers come back from logcat, tag noritest.
set -uo pipefail
# The one emulator, unless told otherwise: a second device attached must never be driven by accident.
export ANDROID_SERIAL=${ANDROID_SERIAL:-emulator-5554}
pkg=${NORI_PKG:-dev.nori.music}
send() {
  # The answer is the first noritest line after the ones already there: the log is never cleared, so the
  # app's own lines stay readable for whoever is debugging a run.
  local before; before=$(adb logcat -d -s noritest:I | grep -c 'noritest')
  # Quote for the shell ON THE DEVICE: adb hands it a command line, so an unquoted | or space there
  # becomes a pipe or an argument break and the extra arrives mangled (or not at all).
  local cmdline="am broadcast -n $pkg/dev.nori.music.app.TestBridge -a dev.nori.music.TEST --es cmd '$1'"
  [ -n "${2:-}" ] && cmdline="$cmdline --es arg '$2'"
  [ -n "${3:-}" ] && cmdline="$cmdline --es value '$3'"
  adb shell "$cmdline" >/dev/null 2>&1
  for _ in $(seq 20); do
    local lines; lines=$(adb logcat -d -s noritest:I | grep 'noritest')
    if [ "$(printf '%s\n' "$lines" | grep -c 'noritest')" -gt "$before" ]; then
      printf '%s\n' "$lines" | tail -1 | sed -E 's/^.*noritest: //'; return 0
    fi
    sleep 0.25
  done
  echo "no answer (is a debug build running?)" >&2; return 1
}
case "${1:-}" in
  # Up as soon as the screen answers (a route other than the service's "background"), 20 s at most.
  # The launcher's own activity, started as a tap on its icon would: monkey refuses to run on images that
  # declare no hardware keys ("SYS_KEYS has no physical keys"), which newer emulators do.
  launch) main=$(adb shell cmd package resolve-activity --brief -c android.intent.category.LAUNCHER $pkg | tr -d '\r' | tail -1)
    adb shell am start -n "$main" >/dev/null 2>&1
    for _ in $(seq 40); do
      s=$(send state 2>/dev/null) && [[ "$s" == *'"route"'* && "$s" != *'"route":"background"'* ]] && { echo "$s"; exit 0; }
      sleep 0.5
    done
    send state ;;
  open|play|state|set|login|do|remote) send "$@" ;;
  wake) adb shell input keyevent 224 >/dev/null; adb shell wm dismiss-keyguard >/dev/null 2>&1; sleep 1 ;;
  *) echo "usage: app.sh launch|open <route>|play <ref>|do <action>|login <url|user|pass>|set <name> <value>|state|wake" >&2; exit 2 ;;
esac
