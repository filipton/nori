//! Differential goldens for the engine rewrite: each run's events on the virtual clock, what its card
//! heard and whatever else the test adds, written when the run ends. `NORI_GOLDEN=record` keeps each
//! run unlike those kept before in tests/goldens (racy runs keep several), `NORI_GOLDEN=check` writes a
//! run like none of them next to them as `.new` and lists it in tests/goldens/DIFFERS.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use nori_engine::Event;
use parking_lot::Mutex;

use super::Virtual;

pub struct Trace {
    name: String,
    clock: Virtual,
    lines: Vec<String>,
    extra: Vec<Box<dyn Fn() -> String + Send>>,
}

static RUNS: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

impl Trace {
    pub fn new(clock: &Virtual) -> Trace {
        let test = std::thread::current().name().unwrap_or("unnamed").replace("::", ".");
        let mut runs = RUNS.lock();
        let n = runs.get_or_insert_with(HashMap::new).entry(test.clone()).or_insert(0);
        *n += 1;
        let name = if *n == 1 { test } else { format!("{test}.{n}") };
        Trace { name, clock: clock.clone(), lines: Vec::new(), extra: Vec::new() }
    }

    /// Adds `f`'s words at the end of the run.
    pub fn watch(&mut self, f: impl Fn() -> String + Send + 'static) {
        self.extra.push(Box::new(f));
    }

    pub fn event(&mut self, e: &Event) {
        let ms = self.clock.now_ns() / 1_000_000;
        self.lines.push(format!("{ms} {e:?}"));
    }

    /// `f` wrapped to record every event first.
    pub fn around(mut self, mut f: impl FnMut(Event) + Send + 'static) -> impl FnMut(Event) + Send + 'static {
        move |e| {
            self.event(&e);
            f(e)
        }
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        let Ok(mode) = std::env::var("NORI_GOLDEN") else { return };
        let mut text = self.lines.join("\n");
        for f in &self.extra {
            text.push('\n');
            text.push_str(&f());
        }
        text.push('\n');
        let _ = std::fs::create_dir_all(dir());
        let variant = |k: usize| dir().join(if k == 0 { format!("{}.txt", self.name) } else { format!("{}.v{k}.txt", self.name) });
        let known: Vec<String> = (0..).map_while(|k| std::fs::read_to_string(variant(k)).ok()).collect();
        if known.contains(&text) {
            return;
        }
        if mode == "record" {
            let _ = std::fs::write(variant(known.len()), text);
            return;
        }
        let _ = std::fs::write(dir().join(format!("{}.new", self.name)), &text);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("DIFFERS")).expect("the list");
        let _ = std::io::Write::write_all(&mut f, format!("{}\n", self.name).as_bytes());
    }
}

/// Samples as one hash per `per` of them, with their count.
pub fn hashes<T: Copy>(label: &str, samples: &[T], per: usize, bits: impl Fn(T) -> u64) -> String {
    let mut out = format!("{label}: {} samples", samples.len());
    for c in samples.chunks(per.max(1)) {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &s in c {
            h = (h ^ bits(s)).wrapping_mul(0x100_0000_01b3);
        }
        let _ = write!(out, " {h:016x}");
    }
    out
}

pub fn floats(label: &str, samples: &[f32]) -> String {
    hashes(label, samples, 88_200, |s| s.to_bits() as u64)
}

pub fn shorts(label: &str, samples: &[i16]) -> String {
    hashes(label, samples, 88_200, |s| s as u16 as u64)
}
