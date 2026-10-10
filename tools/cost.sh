#!/usr/bin/env bash
# tools/cost.sh LABEL [SECONDS]: what the app costs on screen as it is now, from a debug build over adb:
# CPU % of a core, wakeups/s (context switches of all its threads), frames/s and GCs/min over the window,
# and the six busiest threads. Set ANDROID_SERIAL for the device. See docs/site/developers/building.md.

label=$1; secs=${2:-30}
cd "$(dirname "$0")/.."; m=$(mktemp -d)
pid=$(adb shell pidof dev.nori.music)
snap() { adb shell "cd /proc/$pid/task && for t in *; do echo \$t \$(cat \$t/comm | tr ' ' _) \$(cut -d' ' -f14,15 \$t/stat) \$(cat \$t/status | grep -E '^(voluntary|nonvoluntary)_ctxt' | awk '{print \$2}' | tr '\n' ' '); done"; }
gc() { adb logcat -c; adb shell "am broadcast -n dev.nori.music/dev.nori.music.app.TestBridge -a dev.nori.music.TEST --es cmd gc" >/dev/null; sleep 0.5; adb logcat -d -s noritest:I | grep -o 'gc [0-9]*' | tail -1 | cut -d' ' -f2; }
g0=$(gc)
adb shell dumpsys gfxinfo dev.nori.music reset >/dev/null
snap 2>/dev/null > $m/a
sleep "$secs"
snap 2>/dev/null > $m/b
f=$(adb shell dumpsys gfxinfo dev.nori.music | grep -m1 "Total frames rendered" | awk '{print $4}')
g1=$(gc)
python3 - "$label" "$secs" "$f" "$g0" "$g1" "$m" <<'PY'
import sys
label,secs,f,g0,g1=sys.argv[1],float(sys.argv[2]),int(sys.argv[3] or 0),int(sys.argv[4] or 0),int(sys.argv[5] or 0)
def rd(p):
    d={}
    for l in open(p):
        x=l.split()
        if len(x)>=6: d[x[0]]=(x[1],int(x[2])+int(x[3]),int(x[4])+int(x[5]))
    return d
a,b=rd(sys.argv[6]+'/a'),rd(sys.argv[6]+'/b')
rows=[]
for t,(n,c,w) in b.items():
    c0,w0=a.get(t,(n,0,0))[1:]
    rows.append((n,(c-c0)/100.0,(w-w0)))
cpu=sum(r[1] for r in rows)/secs*100; wk=sum(r[2] for r in rows)/secs
print(f"{label}: CPU {cpu:.1f} %, wakeups {wk:.0f}/s, {f/secs:.1f} fps, GCs {max(0,g1-g0)*60/secs:.1f}/min")
for n,c,w in sorted(rows,key=lambda r:-r[1])[:6]: print(f"    {n:18s} {c/secs*100:5.1f} % {w/secs:6.0f}/s")
PY
