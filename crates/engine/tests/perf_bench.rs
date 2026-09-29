// Host perf report, included into tests/engine.rs for its rig: per minute of music, the engine's wakes,
// the process's CPU time, and allocations. Not a check; compare revisions with tools/perf-host.sh.
//
//   cargo test --release -p nori-engine --test engine perf_report -- --ignored --nocapture --test-threads=1

/// Process CPU time (user and system), ms.
fn cpu_ms() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct it is handed.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let ms = |t: libc::timeval| t.tv_sec as f64 * 1000.0 + t.tv_usec as f64 / 1000.0;
    ms(u.ru_utime) + ms(u.ru_stime)
}

/// Plays `songs` and prints one "perf:" line measured over `minutes`.
fn perf_case(name: &str, songs: &[(&str, &[i16])], prefs: TransitionPrefs, settings: Settings, minutes: u64) {
    let rig = Rig::new(songs, prefs, settings);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2), "{name}: the music started");
    let (wakes, cpu, (allocs, bytes), heard) = (rig.time.clock.sleeps(), cpu_ms(), crate::perf_alloc::counts(), rig.heard.lock().len());
    rig.run(minutes * 60_000);
    let played = (rig.heard.lock().len() - heard) as f64 / 2.0 / RATE as f64 / 60.0;
    let (allocs2, bytes2) = crate::perf_alloc::counts();
    let per = |v: f64| v / played.max(1e-9);
    println!(
        "perf: {name:<10} music {played:5.2} min | wakes/min {:7.1} | cpu ms/min {:8.1} | allocs/min {:9.0} | alloc KB/min {:9.0}",
        per((rig.time.clock.sleeps() - wakes) as f64),
        per(cpu_ms() - cpu),
        per((allocs2 - allocs) as f64),
        per((bytes2 - bytes) as f64 / 1024.0),
    );
    rig.engine.stop();
}

#[test]
#[ignore]
fn perf_report() {
    let long = music(300.0, 91);
    perf_case("plain", &[("a", &long)], prefs_off(), Settings::default(), 4);
    perf_case("equalizer", &[("a", &long)], prefs_off(), loud_eq(), 4);
    let (a, b, c) = (music(90.0, 92), music(90.0, 93), music(90.0, 94));
    perf_case("crossfade", &[("a", &a), ("b", &b), ("c", &c)], crossfade(6), Settings { crossfade_s: 6, ..Settings::default() }, 4);
}
