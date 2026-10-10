#!/usr/bin/env bash
# Battery and CPU of a music app on the iPod (docs/site/developers/ipod-internals.md W13): a fixed playlist playing, screen off,
# Wi-Fi on, unplugged. Every 30 s it reads the battery (ioreg AppleARMPMUCharger) and the app's RSS over
# SSH; at the end a table and the averages. Run it once for nori and once for Apple's Music app on the
# same downloaded songs, and compare.
#
#   tools/ipod-bench.sh nori            start the playlist in nori, lock the screen, then run this
#   tools/ipod-bench.sh music           the same in Apple's Music app
#   options: --minutes N (30), --every S (30), --plugged (sample anyway: checks the script, measures nothing)
#
# The iPod is reached over Wi-Fi: the USB cable that carries SSH elsewhere also charges it, and a charging
# battery reports nothing useful. NORI_IPOD_HOST is its Wi-Fi address (dropbear on port 44, or
# NORI_IPOD_WIFI_PORT); the password is NORI_IPOD_PASSWORD (alpine).
# Never tools/bench.sh: that is the owner's Android phone comparison.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
app="${1:-}"
shift || true
minutes=30 every=30 plugged=0
while [ $# -gt 0 ]; do
  case "$1" in
    --minutes) minutes="$2"; shift 2 ;;
    --every) every="$2"; shift 2 ;;
    --plugged) plugged=1; shift ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
case "$app" in
  nori) process='/Applications/nori.app/nori' ;;
  music) process='/Applications/Music.app/Music' ;;
  *) echo "usage: tools/ipod-bench.sh nori|music [--minutes N] [--every S] [--plugged]" >&2; exit 2 ;;
esac
host="${NORI_IPOD_HOST:-}"
port="${NORI_IPOD_WIFI_PORT:-44}"
if [ -z "$host" ] && [ "$plugged" = 1 ]; then host=127.0.0.1 port="${NORI_IPOD_PORT:-2244}"; fi
[ -n "$host" ] || { echo "set NORI_IPOD_HOST to the iPod's Wi-Fi address (USB charges it)" >&2; exit 2; }
ipod=(sshpass -p "${NORI_IPOD_PASSWORD:-alpine}" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
      -o LogLevel=ERROR -o PubkeyAuthentication=no -o ConnectTimeout=10 -p "$port" "root@$host")

# One sample: epoch, external power, raw mAh, mV, mA (negative: drawn; empty when not reported), the app's
# pid, RSS KB and CPU time. Unsigned 64-bit readings of a negative current are turned back into negatives.
sample() {
  # The iPod has grep and sed but no awk.
  "${ipod[@]}" "p=\$(ps -axo pid=,comm= | grep ' $process\$' | head -1 | sed 's/^ *//; s/ .*//')
    r=\$(ioreg -rc AppleARMPMUCharger)
    v() { printf '%s\n' \"\$r\" | sed -n \"s/^ *| *\\\"\$1\\\" = \\(.*\\)\$/\\1/p\" | head -1; }
    amp=\$(v InstantAmperage); [ -n \"\$amp\" ] || amp=\$(v Amperage)
    if [ -n \"\$p\" ]; then set -- \$(ps -o rss=,time= -p \"\$p\"); rss=\$1 cpu=\$2; else rss= cpu=; fi
    echo \"\$(date +%s) \$(v ExternalConnected) \$(v AppleRawCurrentCapacity) \$(v Voltage) \${amp:-} \${p:-} \${rss:-} \${cpu:-}\"" |
  awk '{ a = $5; if (a != "" && a > 9.2e18) a = a - 18446744073709551616; print $1, $2, $3, $4, a, $6, $7, $8 }'
}

# "M:SS.cc" or "H:MM:SS" CPU time in seconds.
cpu_s() { awk -F: '{ s = 0; for (i = 1; i <= NF; i++) s = s * 60 + $i; print s }' <<<"$1"; }

first=$(sample)
read -r t0 power0 mah0 _ _ pid0 _ cpu0 <<<"$first"
[ -n "$pid0" ] || { echo "$app is not running on the iPod: start the playlist first" >&2; exit 1; }
if [ "$power0" = "Yes" ] && [ "$plugged" = 0 ]; then
  echo "the iPod is on external power: unplug it (SSH goes over Wi-Fi), or pass --plugged to check the script" >&2
  exit 1
fi

mkdir -p "$root/build"
out="$root/build/ipod-bench-$app-$(date +%Y%m%d-%H%M).csv"
echo "epoch,external,mah,mv,ma,pid,rss_kb,cpu_time" >"$out"
echo "$first" | tr ' ' ',' >>"$out"
echo "$app (pid $pid0), $minutes min, a sample every $every s → $out"
printf '%8s %8s %7s %7s %9s\n' "min" "mAh" "mV" "mA" "RSS MB"

end=$((t0 + minutes * 60))
last="$first"
while [ "$(date +%s)" -lt "$end" ]; do
  sleep "$every"
  s=$(sample) || { echo "  (no answer)"; continue; }
  read -r t _ mah mv ma pid rss _ <<<"$s"
  if [ "$pid" != "$pid0" ]; then echo "$app stopped or restarted (pid ${pid:-none}): run again" >&2; exit 1; fi
  echo "$s" | tr ' ' ',' >>"$out"
  last="$s"
  printf '%8.1f %8s %7s %7s %9.1f\n' "$(bc -l <<<"($t - $t0) / 60")" "$mah" "$mv" "${ma:-?}" "$(bc -l <<<"${rss:-0} / 1024")"
done

read -r t1 _ mah1 _ _ _ _ cpu1 <<<"$last"
hours=$(bc -l <<<"($t1 - $t0) / 3600")
[ "$(bc -l <<<"$hours > 0")" = 1 ] || { echo "no samples after the first" >&2; exit 1; }
echo
echo "summary ($app, $(bc -l <<<"$hours * 60" | xargs printf '%.1f') min)"
printf '  drawn, from the battery counter  %6.1f mA\n' "$(bc -l <<<"($mah0 - $mah1) / $hours")"
awk -F, 'NR > 1 && $5 != "" { s += $5; n++ } END { if (n) printf "  drawn, mean of the readings      %6.1f mA\n", -s / n }' "$out"
printf '  app CPU, of one core              %6.2f %%\n' "$(bc -l <<<"($(cpu_s "$cpu1") - $(cpu_s "$cpu0")) / ($t1 - $t0) * 100")"
awk -F, 'NR > 1 && $7 != "" { if ($7 > m) m = $7; s += $7; n++ } END { if (n) printf "  app RSS                           %6.1f MB mean, %.1f MB most\n", s / n / 1024, m / 1024 }' "$out"
