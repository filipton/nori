//! Memory breakdown read at a stretch's ends: Android's PSS summary, native heap, and the app's big
//! holders (Rust heap, engine song buffers, ring, beat model, cover Bitmaps, moving cover player). The
//! engine part comes through a hook ([`install`]) since this crate does not link the engine.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Rust-side memory, KB.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfRust {
    /// Rust heap live KB (`nori_model::heap`); -1 when not counted (sentinel kept for the FFI record).
    #[serde(rename = "h")]
    pub heap_kb: i64,
    /// Live song loaders and their in-memory KB.
    #[serde(rename = "sn")]
    pub songs: i32,
    #[serde(rename = "sk")]
    pub songs_kb: i64,
    /// Loaders reading from their stream cache file (memory released).
    #[serde(rename = "sd", default)]
    pub songs_on_disk: i32,
    /// The engine's sample ring.
    #[serde(rename = "r")]
    pub ring_kb: i64,
    /// Beat model heap growth while loaded (1 when uncounted), 0 when not loaded.
    #[serde(rename = "m", default)]
    pub model_kb: i64,
}

/// Process memory, KB. The first seven fields are Android's `Debug.MemoryInfo.getMemoryStats` (summing to
/// total PSS); the rest break down the app's own share.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfMemory {
    #[serde(rename = "j")]
    pub java_kb: i64,
    #[serde(rename = "n")]
    pub native_kb: i64,
    #[serde(rename = "c")]
    pub code_kb: i64,
    #[serde(rename = "s")]
    pub stack_kb: i64,
    #[serde(rename = "g")]
    pub graphics_kb: i64,
    #[serde(rename = "o")]
    pub other_kb: i64,
    #[serde(rename = "y")]
    pub system_kb: i64,
    /// Native heap allocated (mallinfo), Rust and platform.
    #[serde(rename = "na")]
    pub native_alloc_kb: i64,
    /// Cover Bitmaps in memory: KB and count.
    #[serde(rename = "ck")]
    pub covers_kb: i64,
    #[serde(rename = "cn")]
    pub covers: i32,
    /// Live moving-cover players.
    #[serde(rename = "mv", default)]
    pub motion: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust: Option<PerfRust>,
}

/// The engine's part of [`PerfRust`].
pub struct EngineMemory {
    pub songs: i32,
    pub songs_kb: i64,
    pub songs_on_disk: i32,
    pub ring_kb: i64,
    pub model_kb: i64,
}

/// Global because [`perf_rust_memory`] is an FFI entry point with no handle, and this crate cannot link
/// the engine.
static ENGINE: OnceLock<fn() -> EngineMemory> = OnceLock::new();

/// Installs the engine memory hook (once, by the client that links the engine).
pub fn install(engine: fn() -> EngineMemory) {
    let _ = ENGINE.set(engine);
}

/// Current Rust-side memory (takes one lock per song loader).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_rust_memory() -> PerfRust {
    let e = ENGINE.get().map(|f| f());
    PerfRust {
        heap_kb: nori_model::heap::live_bytes().map_or(-1, |b| b / 1024),
        songs: e.as_ref().map_or(0, |e| e.songs),
        songs_kb: e.as_ref().map_or(0, |e| e.songs_kb),
        songs_on_disk: e.as_ref().map_or(0, |e| e.songs_on_disk),
        ring_kb: e.as_ref().map_or(0, |e| e.ring_kb),
        model_kb: e.as_ref().map_or(0, |e| e.model_kb),
    }
}

fn mb(kb: i64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb < 10.0 { format!("{mb:.1}") } else { format!("{mb:.0}") }
}

/// The report's memory line, in MB.
pub fn memory_line(m: &PerfMemory) -> String {
    let total = m.java_kb + m.native_kb + m.code_kb + m.stack_kb + m.graphics_kb + m.other_kb + m.system_kb;
    let mut out = format!(
        "memory: PSS {} MB = Java {}, native {}, code {}, stack {}, graphics {}, other {}, system {}; native heap allocated {}",
        mb(total),
        mb(m.java_kb),
        mb(m.native_kb),
        mb(m.code_kb),
        mb(m.stack_kb),
        mb(m.graphics_kb),
        mb(m.other_kb),
        mb(m.system_kb),
        mb(m.native_alloc_kb),
    );
    if let Some(r) = &m.rust {
        if r.heap_kb >= 0 {
            out.push_str(&format!(", of it Rust {}", mb(r.heap_kb)));
        }
        out.push_str(&format!("; songs {} in {} loaders", mb(r.songs_kb), r.songs));
        if r.songs_on_disk > 0 {
            out.push_str(&format!(" ({} read from the disk)", r.songs_on_disk));
        }
        out.push_str(&format!(", ring {}", mb(r.ring_kb)));
        if r.model_kb > 0 {
            out.push_str(&format!(", beat model {}", mb(r.model_kb)));
        }
    }
    out.push_str(&format!("; covers {} in {} Bitmaps", mb(m.covers_kb), m.covers));
    if m.motion > 0 {
        out.push_str(&format!(", moving cover playing ({})", m.motion));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_lines() {
        let m = PerfMemory {
            java_kb: 16 * 1024,
            native_kb: 50 * 1024,
            code_kb: 11 * 1024,
            stack_kb: 2 * 1024 + 512,
            graphics_kb: 0,
            other_kb: 8 * 1024,
            system_kb: 10 * 1024,
            native_alloc_kb: 46 * 1024,
            covers_kb: 12 * 1024,
            covers: 30,
            motion: 0,
            rust: Some(PerfRust { heap_kb: 18 * 1024, songs: 2, songs_kb: 21 * 1024, songs_on_disk: 1, ring_kb: 4134, model_kb: 0 }),
        };
        assert_eq!(
            memory_line(&m),
            "memory: PSS 98 MB = Java 16, native 50, code 11, stack 2.5, graphics 0.0, other 8.0, system 10; native heap allocated 46, \
             of it Rust 18; songs 21 in 2 loaders (1 read from the disk), ring 4.0; covers 12 in 30 Bitmaps"
        );

        // Memory line omits unknown parts.
        let m = PerfMemory { rust: Some(PerfRust { heap_kb: -1, model_kb: 60 * 1024, ..Default::default() }), motion: 1, ..Default::default() };
        let line = memory_line(&m);
        assert!(!line.contains("of it Rust"), "{line}");
        assert!(line.contains("beat model 60"), "{line}");
        assert!(line.ends_with("moving cover playing (1)"), "{line}");

        // Memory json uses short keys and round trips.
        let m = PerfMemory { java_kb: 1, rust: Some(PerfRust { heap_kb: 2, ..Default::default() }), ..Default::default() };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"j\":1") && json.contains("\"h\":2"), "{json}");
        assert_eq!(serde_json::from_str::<PerfMemory>(&json).unwrap(), m);
    }

}
