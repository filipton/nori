//! Differential harness: seeded random gets, puts, removes and clears on the memory and disk caches,
//! each answer and size transcribed to `$NORI_DIFF_OUT/lru.txt`.

use std::fmt::Write as _;

use nori_covers::{DiskCache, Key, MemoryCache, Sized};

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

#[test]
fn transcribe() {
    let Some(dir) = std::env::var_os("NORI_DIFF_OUT") else { return };
    let mut log = String::new();
    for seed in 1..=6u64 {
        let mut r = Rng(seed * 7919);
        let limit = [0, 50, 200, 1000][seed as usize % 4];
        let m: MemoryCache<u32> = MemoryCache::new(limit);
        let d = nori_testdir::TempDir::new(&format!("diff-lru-{seed}"));
        let disk = DiskCache::open(d.path(), limit as u64).unwrap();
        for step in 0..3000 {
            let k = r.below(12);
            let sized = Sized { key: Key::of(&k.to_string()), width: 1 + r.below(2) as u32, height: 1 };
            let key = Key::of(&k.to_string());
            let size = r.below(120) as usize;
            let op = r.below(10);
            let said = match op {
                0..=3 => format!("get {:?} read {}", m.get(&sized), disk.read(key, &mut Vec::new())),
                4..=7 => {
                    m.put(sized, step, size);
                    disk.put(key, &vec![1; size]).unwrap();
                    "put".to_string()
                }
                8 => {
                    disk.remove(key);
                    format!("contains {}", disk.contains(key))
                }
                _ if r.below(20) == 0 => {
                    m.clear();
                    disk.clear();
                    "clear".to_string()
                }
                _ => format!("contains {}", disk.contains(key)),
            };
            let _ = writeln!(log, "{seed} {step} {op} {k} {size} {said} {} {}", m.bytes(), disk.bytes());
        }
    }
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(std::path::Path::new(&dir).join("lru.txt"), log).unwrap();
}
