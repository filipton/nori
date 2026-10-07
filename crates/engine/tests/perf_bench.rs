// Host perf report, included into tests/engine.rs for its rig: per minute of music, the engine's wakes,
// CPU time and allocations, and the heap's peak (the test thread's own work left out). Not a check; compare revisions with
// tools/perf-host.sh. `NORI_PERF=name,name` runs only those cases, `NORI_PERF_MINUTES` plays each that
// long (for a profiler). The queue repeats.
//
//   cargo test --release -p nori-engine --test engine perf_report -- --ignored --nocapture --test-threads=1

/// CPU time (user and system) of every thread but this one, ms: the test thread moves the clock and
/// records what the card heard, which is not the engine's work.
pub(crate) fn cpu_ms() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct it is handed.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let ms = |t: libc::timeval| t.tv_sec as f64 * 1000.0 + t.tv_usec as f64 / 1000.0;
    let mut own: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: clock_gettime fills the struct it is handed.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut own) };
    ms(u.ru_utime) + ms(u.ru_stime) - (own.tv_sec as f64 * 1000.0 + own.tv_nsec as f64 / 1e6)
}

/// Whether `NORI_PERF` asks for case `name`.
fn perf_wanted(name: &str) -> bool {
    std::env::var("NORI_PERF").map_or(true, |only| only.split(',').any(|n| n == name))
}

/// Prints one "perf:" line for `rig`, playing, measured over `minutes`; `during` runs every 15 s of it.
fn perf_measure(name: &str, rig: &Rig, minutes: u64, mut during: impl FnMut(&Rig, u64)) {
    let minutes = std::env::var("NORI_PERF_MINUTES").ok().and_then(|m| m.parse().ok()).unwrap_or(minutes);
    rig.heard.lock().reserve((minutes + 1) as usize * 60 * RATE as usize * 2);
    rig.engine.set_repeat(nori_player::playlist::REPEAT_ALL);
    rig.engine.play_at(0, 0);
    assert!(rig.wait_for(10, |r| r.heard.lock().len() > RATE as usize * 2 * 2), "{name}: the music started");
    let (wakes, cpu, (allocs, bytes), heard) = (rig.time.clock.sleeps(), cpu_ms(), crate::perf_alloc::counts(), rig.heard.lock().len());
    crate::perf_alloc::restart_peak();
    for k in 0..minutes * 4 {
        during(rig, k);
        rig.run(15_000);
    }
    let played = (rig.heard.lock().len() - heard) as f64 / 2.0 / RATE as f64 / 60.0;
    let (allocs2, bytes2) = crate::perf_alloc::counts();
    let per = |v: f64| v / played.max(1e-9);
    println!(
        "perf: {name:<10} music {played:5.2} min | wakes/min {:7.1} | cpu ms/min {:8.1} | allocs/min {:9.0} | alloc KB/min {:9.0} | peak MB {:6.1}",
        per((rig.time.clock.sleeps() - wakes) as f64),
        per(cpu_ms() - cpu),
        per((allocs2 - allocs) as f64),
        per((bytes2 - bytes) as f64 / 1024.0),
        crate::perf_alloc::peak() as f64 / 1e6,
    );
    rig.engine.stop();
}

/// `pcm` (stereo at [`RATE`]) encoded by ffmpeg with `args` (output options); None without ffmpeg.
fn encoded(pcm: &[i16], args: &[&str]) -> Option<Vec<u8>> {
    let dir = nori_testdir::TempDir::new("perf-encoded");
    let (raw, out) = (dir.join("music.raw"), dir.join("music.out"));
    std::fs::write(&raw, sim::bytes(pcm)).unwrap();
    let input = ["-hide_banner", "-loglevel", "error", "-f", "s16le", "-ar", "44100", "-ac", "2", "-i"];
    let ran = std::process::Command::new("ffmpeg").args(input).arg(&raw).args(args).arg("-y").arg(&out).status();
    ran.is_ok_and(|s| s.success()).then(|| std::fs::read(&out).unwrap())
}

/// Plays `songs` and prints one "perf:" line measured over `minutes`.
fn perf_case(name: &str, songs: &[(&str, &[i16])], app: sim::App, settings: Settings, minutes: u64) {
    if perf_wanted(name) {
        perf_measure(name, &Rig::with_app(songs, app, settings), minutes, |_, _| {});
    }
}

fn perf_app(prefs: TransitionPrefs) -> sim::App {
    let mut app = sim::App::new();
    app.prefs = prefs;
    app
}

#[test]
#[ignore]
fn perf_report() {
    // The test thread's own buffers (what the card heard, the songs) are not the engine's heap.
    crate::perf_alloc::exclude_this_thread();
    let long = music(300.0, 91);
    let one = [("a", &long[..])];
    perf_case("plain", &one, perf_app(prefs_off()), Settings::default(), 4);
    perf_case("equalizer", &one, perf_app(prefs_off()), loud_eq(), 4);
    // A ten-band graphic equalizer (an AutoEQ profile is about as many).
    let bands = (0..10).map(|k| nori_player::dsp::Band { kind: nori_player::dsp::PEAKING, freq: 31.25 * 2f64.powi(k), gain_db: if k % 2 == 0 { 3.0 } else { -2.0 }, q: 1.4, channel: 0 }).collect();
    perf_case("eq10", &one, perf_app(prefs_off()), with_sound(nori_engine::Sound { bands, ..Default::default() }), 4);
    let compressor = nori_engine::Sound { effects: Effects { compressor: Some(CompressorPreset::Balanced.settings()), ..Effects::default() }, limiter: true, ..Default::default() };
    perf_case("compressor", &one, perf_app(prefs_off()), with_sound(compressor), 4);
    // Compressed, as a server keeps music: the decoder's cost.
    for (name, args) in [("mp3", ["-c:a", "libmp3lame", "-b:a", "320k", "-f", "mp3"]), ("flac", ["-c:a", "flac", "-compression_level", "5", "-f", "flac"])] {
        if !perf_wanted(name) {
            continue;
        }
        let Some(file) = encoded(&long, &args) else {
            eprintln!("ffmpeg is not installed: no {name} case");
            continue;
        };
        let files = vec![("a".to_string(), file, (long.len() / 2) as i64 * 1000 / RATE as i64)];
        perf_measure(name, &Rig::build(files, perf_app(prefs_off()), Settings::default(), Extra { hint: Some(name), ..Extra::default() }), 4, |_, _| {});
    }
    perf_case("speed", &one, perf_app(prefs_off()), Settings { speed: 1.2, pitch: 0.95, ..Settings::default() }, 4);
    perf_case("silence", &one, perf_app(prefs_off()), Settings { skip_silence: true, ..Settings::default() }, 4);
    let gained = || {
        let mut app = perf_app(prefs_off());
        app.gains.insert("a".into(), 0.7);
        app
    };
    perf_case("gain", &one, gained(), Settings::default(), 4);
    perf_case("gaineq", &one, gained(), loud_eq(), 4);
    if perf_wanted("change") {
        // The equalizer moved every 15 s: each change makes the kept input again.
        let rig = Rig::with_app(&one, perf_app(prefs_off()), loud_eq());
        perf_measure("change", &rig, 4, |r, k| {
            let mut s = loud_eq();
            s.sound.bands[0].gain_db = if k % 2 == 0 { 3.0 } else { 6.0 };
            r.engine.set_settings(s);
        });
    }
    let (a, b, c) = (music(90.0, 92), music(90.0, 93), music(90.0, 94));
    let three = [("a", &a[..]), ("b", &b[..]), ("c", &c[..])];
    perf_case("crossfade", &three, perf_app(crossfade(6)), Settings { crossfade_s: 6, ..Settings::default() }, 4);
    // Beat-matched mixes 4 % apart (stretched), and the last song analysed as it plays (the app's
    // measurer, not the engine, would analyse it ahead).
    let mut mixing = perf_app(TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() });
    mixing.measure_playing = true;
    mixing.measure_ahead = false;
    mixing.analyses.insert("a".into(), measured("a", 120.0, 90_000));
    mixing.analyses.insert("b".into(), measured("b", 125.0, 90_000));
    perf_case("automix", &three, mixing, Settings { auto_mix: true, ..Settings::default() }, 4);
}
