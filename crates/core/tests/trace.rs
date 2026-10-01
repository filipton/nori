//! Differential harness for moving the process-wide state into owners: a fixed-seed run of the queue,
//! rules, refill, scrobbling, settings and planner through their platform entry points, written as a
//! trace. With `NORI_TRACE_GOLDEN=<dir>` the trace is compared with `<dir>/trace.txt` (written when
//! missing).

use nori_core::autofill::{autofill_landed, autofill_next};
use nori_core::playlist::*;
use nori_core::queue::{queue_flags, queue_register, queue_songs};
use nori_core::rules::{queue_bridge_failed, queue_error, queue_last_error, queue_measure, queue_playing, queue_precache, queue_previous_restarts, sleep_set, sleep_song_changed};
use nori_core::scrobble::{scrobble_playing, scrobble_track, TrackChange};
use nori_core::settings::StoredPrefs;
use nori_core::settings_store::{settings_open, settings_put};
use nori_core::{Core, PlaybackError, ReplayGain, Song};

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n.max(1)
    }
}

fn song(k: u64) -> Song {
    Song {
        id: format!("s{k}"),
        title: format!("t{k}"),
        duration: 60 + k as u32 * 7,
        album_id: Some(format!("al{}", k / 3)),
        track: (k % 3) as u32 + 1,
        explicit_status: if k % 5 == 0 { "explicit".into() } else { String::new() },
        replay_gain: (k % 2 == 0).then(|| ReplayGain { track_gain: Some(-(k as f32) / 2.0), album_gain: Some(-3.0), track_peak: Some(0.9), album_peak: Some(0.95), ..Default::default() }),
        ..Default::default()
    }
}

fn ids(r: &mut Rng, n: u64) -> Vec<String> {
    (0..n).map(|_| format!("s{}", r.below(16))).collect()
}

#[test]
fn trace() {
    let dir = nori_testdir::TempDir::new("trace");
    settings_open(dir.join("app.db").display().to_string()).unwrap();
    let core = Core::new(dir.join("p.db").display().to_string(), "p".into()).unwrap();
    queue_register((0..16).map(song).collect());
    let mut r = Rng(7);
    let mut out = Vec::new();
    let mut now = 0i64;
    for step in 0..3_000 {
        now += 1_000 + r.below(60_000) as i64;
        let line = match r.below(26) {
            0 => { let n = 1 + r.below(10); format!("set {:?}", playlist_set(ids(&mut r, n), Some(r.below(3) as u32), false, None)) }
            1 => { let (at, n) = (r.below(6) as u32, 1 + r.below(3)); format!("take {:?}", playlist_take(at, ids(&mut r, n), vec![[Hand::Next, Hand::Last, Hand::No][r.below(3) as usize]], None)) }
            2 => format!("remove {:?}", { let a = r.below(5) as u32; playlist_remove(a, a + 1 + r.below(2) as u32) }),
            3 => format!("restore {:?}", playlist_restore(format!("s{}", r.below(16)))),
            4 => format!("move {:?}", playlist_move(r.below(4) as u32, 1 + r.below(4) as u32, r.below(6) as u32)),
            5 => { playlist_repeat(r.below(3) as u8); "repeat".into() }
            6 => { playlist_moved_to(r.below(8) as i32 - 1); "moved".into() }
            7 => format!("skips {}", playlist_skips(r.below(6) as usize)),
            8 => format!("upcoming {:?}", playlist_upcoming(4)),
            9 => format!("window {}", playlist_window()),
            10 => format!("gain {:.4} {:.4}", playlist_gain(false), playlist_gain_of(r.below(5) as usize, r.below(2) == 0)),
            11 => { let v = playlist_view(r.below(3)); format!("view {} {:?} {:?} {:?} {} {}", v.len, v.order, v.queued, v.songs.iter().map(|s| &s.id).collect::<Vec<_>>(), v.index, v.repeat) }
            12 => format!("push {:?} {:?}", playlist_to_push(), snapshot()),
            13 => format!("error {:?} {:?}", queue_error([PlaybackError::Network, PlaybackError::Other, PlaybackError::Output][r.below(3) as usize], r.below(2) == 0, r.below(2) == 0), queue_last_error()),
            14 => { queue_playing(); format!("bridge failed {}", queue_bridge_failed()) }
            15 => format!("precache {:?} measure {:?}", queue_precache(r.below(2) == 0), queue_measure()),
            16 => format!("previous {}", queue_previous_restarts(r.below(8_000) as i64, r.below(2) == 0)),
            17 => format!("sleep {} {}", sleep_set(r.below(3) as u32, r.below(2) == 0), sleep_song_changed()),
            18 => format!("fill {:?} {}", autofill_next(), autofill_landed()),
            19 => { scrobble_playing(r.below(2) == 0, now); "edge".into() }
            20 => format!("track {:?}", scrobble_track(Some(format!("s{}", r.below(16))), [TrackChange::Moved, TrackChange::Looped, TrackChange::Ended][r.below(3) as usize], r.below(2) == 0, now, now + 1_000_000, 0)),
            21 => {
                let p = StoredPrefs { skip_explicit: r.below(2) == 0, scrobble: r.below(2) == 0, auto_fill: r.below(2) == 0, taste_model: r.below(2) == 0, crossfade_sec: r.below(3) as i32 * 4, auto_mix: r.below(2) == 0, skip_on_error: r.below(2) == 0, ..StoredPrefs::default() };
                format!("prefs {}", settings_put(p))
            }
            22 => format!("flags {}", queue_flags(format!("s{}", r.below(16)))),
            23 => format!("songs {:?}", queue_songs(ids(&mut r, 2)).iter().map(|s| (s.id.clone(), s.duration)).collect::<Vec<_>>()),
            24 => format!("plan {:?}", playlist_upcoming(1).first().and_then(|id| nori_core::automix::planner::plan_for(id)).map(|p| format!("{p:?}"))),
            _ => format!("unbridge {:?} {:?}", playlist_unbridge().map(|e| e.seek), playlist_bridge_state().bridging),
        };
        out.push(format!("{step} {line}"));
    }
    nori_core::background::flush();
    out.push(format!("history {}", core.history_recent(1_000, None, true).unwrap().0.len()));
    let trace = out.join("\n");
    let Ok(golden) = std::env::var("NORI_TRACE_GOLDEN") else { return };
    let path = std::path::Path::new(&golden).join("trace.txt");
    match std::fs::read_to_string(&path) {
        Ok(want) => {
            let first = want.lines().zip(trace.lines()).position(|(a, b)| a != b);
            assert!(first.is_none() && want.lines().count() == trace.lines().count(), "differs at line {first:?}: {:?} vs {:?}", first.map(|k| want.lines().nth(k)), first.map(|k| trace.lines().nth(k)));
        }
        Err(_) => std::fs::write(&path, trace).unwrap(),
    }
}
