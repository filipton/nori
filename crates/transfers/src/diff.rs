//! Differential harness: random tracker scripts, outputs compared with goldens recorded from the old code.

use super::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const IDS: [&str; 6] = ["a", "b", "c", "ext-d", "e", "f"];
const STATES: [i32; 7] = [QUEUED, STOPPED, DOWNLOADING, COMPLETED, FAILED, RESTARTING, 5];
const WORKS: [Work; 3] = [Work::Lyrics, Work::Analysis, Work::Beats];

fn script(seed: u64) -> String {
    let mut r = Rng(seed);
    let mut t = Tracker::default();
    let mut out = String::new();
    for (i, id) in IDS.iter().enumerate() {
        let size = [0u64, 1_000_000, 4_000_000][i % 3];
        let song = Song { id: id.to_string(), title: format!("T{i}"), album: if i % 2 == 0 { "Blue".into() } else { format!("A{}", i % 4) }, artist: "X".into(), size, duration: 200, ..Song::default() };
        t.keep_info(id, Some(&song));
    }
    let mut now = 0i64;
    t.test_clock = Some(0);
    let mut slots: Vec<i32> = Vec::new();
    let mut bytes = [0i64; 8];
    for _ in 0..400 {
        now += r.below(3000) as i64;
        t.test_clock = Some(now);
        let id = IDS[r.below(6) as usize];
        let w = WORKS[r.below(3) as usize];
        let line = match r.below(20) {
            0 | 1 => format!("followed {id} {:?}", {
                let s = STATES[r.below(7) as usize];
                (s, t.followed(id, s, now))
            }),
            2 => format!("open {id} {}", {
                let s = t.open(id, now);
                slots.push(s);
                s
            }),
            3..=5 => {
                let slot = if slots.is_empty() || r.below(8) == 0 { r.below(4) as i32 } else { slots[r.below(slots.len() as u64) as usize] };
                let b = &mut bytes[slot as usize % 8];
                *b += r.below(600_000) as i64;
                let length = if r.below(3) == 0 { 0 } else { 3_000_000 };
                format!("note {slot} {:?}", t.note(slot, length, *b, now))
            }
            6 => {
                let listed = r.below(4) as i32;
                let waiting = r.below(6) == 0;
                let c = t.notice(listed, waiting, now);
                format!("notice {c} {:?}", t.notice)
            }
            7 => format!("speed_eta {:?}", t.speed_eta_at(now)),
            8 => {
                let n = Needs { analysis: r.below(2) == 0, beats: r.below(2) == 0 };
                let saved = [None, Some(true), Some(false)][r.below(3) as usize];
                format!("plan {id} {n:?} {saved:?} {}", t.plan(id, n, saved))
            }
            9 => {
                t.working(id, w);
                format!("working {id} {w:?}")
            }
            10 => format!("work_done {id} {w:?} {}", t.work_done(id, w)),
            11 => {
                if r.below(2) == 0 {
                    t.analysing_began(id);
                    format!("analysing_began {id}")
                } else {
                    let stored = r.below(2) == 0;
                    t.analysing_ended(id, stored);
                    format!("analysing_ended {id} {stored}")
                }
            }
            12 => format!("expire {}", t.expire()),
            13 => format!("removed {id} {}", t.removed(id)),
            14 => {
                let on = r.below(2) == 0;
                if on {
                    t.beats_wanted.insert(id.to_string());
                } else {
                    t.beats_wanted.remove(id);
                }
                format!("want_beats {id} {on}")
            }
            15 => format!("summary {:?} processing {:?}", t.summary(), t.processing_at(now)),
            16 => {
                let mut m = t.marks_changed();
                let mut rows: Vec<String> = (0..m.ids.len()).map(|i| format!("{}={:?}@{}", m.ids[i], m.phases[i], m.at[i])).collect();
                rows.sort();
                m.ids.clear();
                format!("marks {rows:?}")
            }
            17 => format!("row {id} {:?} waits {:?}", row_facts(&t.slots, id), WORKS.map(|w| t.waits(id, w))),
            18 => {
                t.failed_before(id, 0, 0);
                format!("failed_before {id}")
            }
            _ => {
                let u = if t.unmark(id) { MARKS } else { 0 };
                t.close(id);
                format!("unmark {id} {u}")
            }
        };
        out.push_str(&format!("{now} {line}\n"));
    }
    let pending: Vec<String> = IDS.iter().map(|s| s.to_string()).collect();
    out.push_str(&format!("sections {:?}\n", sections(&pending, &pending[..2], &t.marks, |s: &String| s.as_str())));
    out
}

#[test]
fn tracker_matches_goldens() {
    let got: String = (1..=60u64).map(|seed| format!("# seed {seed}\n{}", script(seed * 7919))).collect();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden_tracker.txt");
    if std::env::var("NORI_BLESS").is_ok() {
        std::fs::write(path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(path).unwrap();
    for (i, (g, w)) in got.lines().zip(want.lines()).enumerate() {
        assert_eq!(g, w, "line {}", i + 1);
    }
    assert_eq!(got.lines().count(), want.lines().count());
}
