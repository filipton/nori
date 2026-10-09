#!/usr/bin/env python3
"""Time a development command and sample host compiler/JVM/emulator CPU (macOS)."""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import statistics
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / 'build/development-timings'
ENV = dict(os.environ)
RESULTS = []

def cpu_time(s):
    parts = s.split(':')
    return sum(float(x) * 60 ** i for i, x in enumerate(reversed(parts)))

def snapshot():
    p = subprocess.run(['ps', '-axo', 'pid,ppid,time,rss,comm'], text=True, capture_output=True, check=True)
    rows = {}
    for line in p.stdout.splitlines()[1:]:
        fields = line.strip().split(None, 4)
        if len(fields) != 5:
            continue
        pid, ppid, cpu, rss, command = fields
        rows[int(pid)] = dict(ppid=int(ppid), cpu=cpu_time(cpu), rss=int(rss), command=command)
    return rows

def group(command, descendant):
    name = Path(command).name
    if 'qemu-system' in name or name == 'emulator':
        return 'emulator'
    if 'VirtualMachine' in command or 'Docker' in command or 'com.docker' in command or name == 'virtiofsd':
        return 'docker_vm'
    if name == 'java':
        return 'jvm'
    if name in ('cargo', 'cargo-nextest', 'rustc', 'sccache', 'clang', 'clang++', 'cc', 'ld', 'ld.lld', 'lld') or '/target/' in command:
        return 'rust_and_native'
    if descendant:
        return 'driver'
    return 'other'

def run(name, args, timeout=2400):
    print(f'START {name}', flush=True)
    stop = threading.Event()
    samples = []
    before = snapshot()
    known = set()
    start = time.monotonic()
    top_log = (OUT / f'{name}.top.log').open('w')
    top = subprocess.Popen(['top', '-l', '0', '-s', '1', '-n', '0'], stdout=top_log, stderr=subprocess.STDOUT)
    with (OUT / f'{name}.log').open('w') as log:
        p = subprocess.Popen(['/usr/bin/time', '-l', '-o', str(OUT / f'{name}.resources'), *args],
                             cwd=ROOT, env=ENV, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        known.add(p.pid)

        def sample():
            previous = before
            last = start
            while not stop.wait(1):
                now = time.monotonic()
                current = snapshot()
                for _ in range(8):
                    added = {pid for pid, row in current.items() if row['ppid'] in known}
                    if added <= known:
                        break
                    known.update(added)
                totals = {g: 0.0 for g in ['emulator', 'docker_vm', 'jvm', 'rust_and_native', 'driver', 'other']}
                memory = dict.fromkeys(totals, 0)
                for pid, row in current.items():
                    g = group(row['command'], pid in known)
                    delta = max(0, row['cpu'] - previous.get(pid, dict(cpu=0))['cpu'])
                    totals[g] += delta
                    memory[g] += row['rss']
                samples.append(dict(seconds=now-start, interval=now-last, cpu_seconds=totals, rss_kib=memory))
                previous, last = current, now

        thread = threading.Thread(target=sample)
        thread.start()
        timed_out = False
        try:
            code = p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(p.pid, signal.SIGTERM)
            try:
                code = p.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                code = p.wait()
        except KeyboardInterrupt:
            os.killpg(p.pid, signal.SIGTERM)
            try:
                p.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()
            raise
        finally:
            elapsed = time.monotonic() - start
            stop.set()
            thread.join()
            top.terminate()
            top.wait(timeout=10)
            top_log.close()
    (OUT / f'{name}.samples.json').write_text(json.dumps(samples, indent=2))
    duration = sum(x['interval'] for x in samples)
    groups = {}
    for g in ['emulator', 'docker_vm', 'jvm', 'rust_and_native', 'driver', 'other']:
        percentages = sorted(x['cpu_seconds'][g] / x['interval'] * 100 for x in samples)
        groups[g] = dict(cpu_seconds=sum(x['cpu_seconds'][g] for x in samples),
                         average_percent=sum(x['cpu_seconds'][g] for x in samples) / max(duration, 0.001) * 100,
                         peak_percent=max(percentages, default=0),
                         p95_percent=percentages[min(len(percentages)-1, int(len(percentages)*0.95))] if percentages else 0,
                         peak_rss_mib=max((x['rss_kib'][g] / 1024 for x in samples), default=0))
    host = []
    for line in (OUT / f'{name}.top.log').read_text().splitlines():
        m = re.search(r'CPU usage: ([0-9.]+)% user, ([0-9.]+)% sys, ([0-9.]+)% idle', line)
        if m:
            host.append(float(m[1]) + float(m[2]))
    host = host[1:]
    resources = (OUT / f'{name}.resources').read_text() if (OUT / f'{name}.resources').exists() else ''
    m = re.search(r'([0-9.]+) real\s+([0-9.]+) user\s+([0-9.]+) sys', resources)
    direct_cpu = float(m[2]) + float(m[3]) if m else None
    result = dict(name=name, command=args, wall_seconds=elapsed, exit_code=code, timed_out=timed_out,
                  command_cpu_seconds=direct_cpu, command_cpu_average_percent=direct_cpu/elapsed*100 if direct_cpu is not None else None,
                  sampled_seconds=duration, groups=groups,
                  machine_busy_average_percent=statistics.mean(host) if host else None,
                  machine_busy_peak_percent=max(host, default=None))
    RESULTS.append(result)
    (OUT / 'measurements.json').write_text(json.dumps(RESULTS, indent=2) + '\n')
    print(f'END {name}: {elapsed:.2f}s exit={code} emulator={groups["emulator"]["average_percent"]:.1f}% rust={groups["rust_and_native"]["average_percent"]:.1f}% JVM={groups["jvm"]["average_percent"]:.1f}%', flush=True)
    if code:
        print((OUT / f'{name}.log').read_text()[-3500:], flush=True)
    return code

def main():
    global OUT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--name', required=True, help='directory name under build/development-timings')
    parser.add_argument('--timeout', type=int, default=2400)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9_.-]+', args.name) or args.name in ('.', '..'):
        parser.error('name must be a directory name')
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        parser.error('a command is required')
    OUT /= args.name
    OUT.mkdir(parents=True, exist_ok=True)
    raise SystemExit(run(args.name, command, args.timeout))

if __name__ == '__main__':
    main()
