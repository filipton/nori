//! Tuning the sync check (sync.rs) on a real library: three ignored tests, off unless run by hand, that read
//! the server named in `~/.music.pass` (line 1 its address, line 3 the user, line 4 the password; never
//! printed) and keep what they measure under `NORI_TUNE_DIR`.
//!
//! - `sync_gather` takes a sample of songs by genre (never a provider's `ext-` song), downloads each, measures
//!   its vocal curve as the AutoMix analysis does (the audio decoded by the host's `ffmpeg`),
//!   asks every lyrics service that is on by default, one after the other, and keeps each answer's **timings
//!   only**: line and word start and end times, and each line's count of letters (what sync.rs reads of the
//!   text). The words are dropped as soon as the answers have been compared with each other (whether two
//!   are the same words, and the trust score, which need them). No text is written anywhere.
//! - `sync_real` reads that back and measures the check against agreement between the services: answers
//!   that are the same words with the same line starts are taken as good, one the same words at a clear
//!   offset from them as shifted by it, other words or another length as wrong; plus the good ones shifted
//!   and half-shifted by hand, or laid on another song's curve. It prints per-genre tables and sweeps the
//!   thresholds (`NORI_TUNE_T` sets them, `NORI_TUNE_GRID` searches a grid, `NORI_TUNE_CURVE` reads a
//!   variant's curves in place of the kept ones, `NORI_TUNE_DETAIL` prints every answer's numbers).
//! - `sync_variants` measures variants of the curve (vocal.rs's band and peak test) from the kept audio, on
//!   its downmix: the middle of the stereo image that vocal.rs measures a stereo song on is not in its copy.
//!   `sync_gather` measures with the analyser itself, so the curves it keeps are the middle's; `sync_real`
//!   reads what was kept, so a directory gathered before the middle was measured compares the old curve.
//!
//! The audio is kept in the directory for the run only: delete the directory when done.
//!
//! `NORI_TUNE_DIR=<dir> cargo test --release -p nori-lyrics sync_gather -- --ignored --nocapture`, then
//! `sync_real` the same way.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nori_model::{LyricLine, LyricWord, Lyrics, Song};
use nori_net::api::{Auth, Server};
use nori_net::transport::{Exchange, FailureKind, Transport, TransportError, TransportResponse};
use nori_player::automix::analysis::Analyzer;
use nori_player::automix::vocal::VocalCurve;
use nori_settings::lyrics_sources::{lyrics_lookup, LyricsService};
use serde::{Deserialize, Serialize};

use crate::credits::strip_edges;
use crate::fit::{agree, plausible};
use crate::services::{self, tests::block, Ask, Lookup, Shared};
use crate::sync::{measure, Measure, SyncCheck, SyncKind, Thresholds};
use crate::trust::{name_alike, score, with_sync, Named};

// ---- what is kept -----------------------------------------------------------------------------------------

/// A line's timing: start, end, letters, backing only, and its words' (start, end).
#[derive(Serialize, Deserialize, Clone)]
struct Line(i64, i64, u32, bool, Vec<(i64, i64)>);

#[derive(Serialize, Deserialize, Clone)]
struct Answer {
    service: String,
    synced: bool,
    word_timed: bool,
    /// The length the service named, seconds.
    named_s: Option<f64>,
    /// How alike the title it named is to the song's.
    title_alike: Option<f64>,
    /// The trust score before the sync check, among the other answers.
    trust: f64,
    lines: Vec<Line>,
}

#[derive(Serialize, Deserialize)]
struct Kept {
    genre: String,
    duration: u32,
    fps: f32,
    t0: f32,
    level: Vec<u8>,
    answers: Vec<Answer>,
    /// `same[i][j]`: answers i and j are the same words (fit.rs `agree`).
    same: Vec<Vec<bool>>,
}

/// The letters placeholder sync.rs counts: as many as the line had, none of its words.
fn placeholder(n: u32) -> String {
    "x".repeat(n as usize)
}

impl Answer {
    fn lyrics(&self) -> Lyrics {
        let lines = self
            .lines
            .iter()
            .map(|l| {
                let text = placeholder(l.2.max(1));
                LyricLine { start_ms: l.0, end_ms: l.1, text, words: l.4.iter().map(|w| LyricWord { start_ms: w.0, end_ms: w.1, start: 0, end: 1 }).collect(), background: l.3, ..Default::default() }
            })
            .collect();
        Lyrics { synced: self.synced, word_timed: self.word_timed, lines, ..Default::default() }
    }
}

fn timings(l: &Lyrics) -> Vec<Line> {
    l.lines
        .iter()
        .filter(|x| !x.text.trim().is_empty())
        .map(|x| Line(x.start_ms, x.end_ms, x.text.chars().filter(|c| c.is_alphanumeric()).count() as u32, x.background, x.words.iter().map(|w| (w.start_ms, w.end_ms)).collect()))
        .collect()
}

// ---- the network --------------------------------------------------------------------------------------------

struct Web {
    agent: ureq::Agent,
}

impl Web {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent("nori-music/sync-tune")
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build();
        Web { agent: ureq::Agent::new_with_config(config) }
    }
}

fn failed() -> TransportError {
    // Never the error's own words: they can hold the address, and the server's has the credentials in it.
    TransportError::Failed { kind: FailureKind::Other, detail: None }
}

#[async_trait::async_trait]
impl Transport for Web {
    async fn get(&self, url: String, timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        self.send(Exchange { url, timeout_ms, ..Default::default() }).await
    }

    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
        let timeout = (request.timeout_ms > 0).then(|| Duration::from_millis(request.timeout_ms as u64));
        let r = match &request.json {
            Some(json) => {
                let mut req = self.agent.post(&request.url).header("Content-Type", "application/json");
                for (k, v) in &request.headers {
                    req = req.header(k, v);
                }
                if timeout.is_some() {
                    req = req.config().timeout_global(timeout).build();
                }
                req.send(json.as_bytes())
            }
            None => {
                let mut req = self.agent.get(&request.url);
                for (k, v) in &request.headers {
                    req = req.header(k, v);
                }
                if timeout.is_some() {
                    req = req.config().timeout_global(timeout).build();
                }
                req.call()
            }
        }
        .map_err(|_| failed())?;
        let status = r.status().as_u16();
        let body = r.into_body().with_config().limit(1 << 30).read_to_vec().map_err(|_| failed())?;
        Ok(TransportResponse { status, body })
    }

    fn address_changed(&self) {}
}

fn server() -> Server {
    let pass = std::fs::read_to_string(std::env::var("HOME").unwrap() + "/.music.pass").expect("~/.music.pass");
    let l: Vec<&str> = pass.lines().collect();
    Server::with(l[0].trim(), Auth::Token { user: l[2].trim(), password: l[3].trim() })
}

fn api(web: &Web, s: &Server, endpoint: &str, params: &[(&str, &str)]) -> serde_json::Value {
    let params: Vec<(String, String)> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let r = block(web.get(s.url(endpoint, &params), 60_000)).unwrap_or_else(|_| panic!("{endpoint} failed"));
    let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or_else(|_| panic!("{endpoint}: status {}", r.status));
    v["subsonic-response"].clone()
}

fn songs_of(v: &serde_json::Value) -> Vec<Song> {
    v.as_array().map(|a| a.iter().filter_map(|x| serde_json::from_value::<Song>(x.clone()).ok()).collect()).unwrap_or_default()
}

/// A stable pseudo-random order, so the same sample comes back on another run.
fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn usable(s: &Song) -> bool {
    !s.id.starts_with("ext-") && !s.is_external && !s.suffix.eq_ignore_ascii_case("remote") && (90..=720).contains(&s.duration)
}

/// The sample: `(genre label, song)`, a few per genre, and live recordings.
fn sample(web: &Web, s: &Server) -> Vec<(String, Song)> {
    let plan: [(&str, &[&str], usize); 8] = [
        ("pop", &["Pop"], 9),
        ("rock", &["Rock", "Hard Rock", "Punk - New Wave"], 9),
        ("alt", &["Alternatif et Indé"], 8),
        ("metal", &["Metal"], 8),
        ("electronic", &["Électronique", "Dance", "Techno", "Disco"], 9),
        ("classical", &["Classique", "Musique de chambre", "Musique symphonique", "Symphonies", "Bandes originales de films", "Film"], 6),
        ("rap/rnb", &["Rap", "R&B", "Funk", "Soul"], 8),
        ("folk/blues", &["Folk", "Blues", "Country"], 3),
    ];
    let mut out: Vec<(String, Song)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (label, genres, n) in plan {
        let mut pool: Vec<Song> = Vec::new();
        for g in genres {
            let r = api(web, s, "getSongsByGenre", &[("genre", g), ("count", "500")]);
            pool.extend(songs_of(&r["songsByGenre"]["song"]));
            std::thread::sleep(Duration::from_millis(300));
        }
        pool.retain(usable);
        pool.sort_by_key(|x| hash(&x.id));
        // One song per album where there are enough.
        let mut albums = std::collections::HashSet::new();
        let mut picked = 0;
        for x in pool.iter().filter(|x| albums.insert(x.album.clone())).chain(pool.iter()) {
            if picked == n {
                break;
            }
            if seen.insert(x.id.clone()) {
                out.push((label.to_string(), x.clone()));
                picked += 1;
            }
        }
    }
    let r = api(web, s, "search3", &[("query", "live"), ("songCount", "200"), ("albumCount", "0"), ("artistCount", "0")]);
    let mut live: Vec<Song> = songs_of(&r["searchResult3"]["song"]).into_iter().filter(|x| usable(x) && (x.title.to_lowercase().contains("live") || x.album.to_lowercase().contains("live"))).collect();
    live.sort_by_key(|x| hash(&x.id));
    for x in live.into_iter().take(6) {
        if seen.insert(x.id.clone()) {
            out.push(("live".into(), x));
        }
    }
    out
}

/// Where song `i`'s audio is kept for the run (deleted with the directory after it).
fn audio_path(dir: &Path, i: usize) -> PathBuf {
    dir.join("audio").join(format!("{i:03}.bin"))
}

/// Downloads song `i`'s audio into `dir` unless it is there already.
fn fetch(web: &Web, s: &Server, song: &Song, dir: &Path, i: usize) -> bool {
    let file = audio_path(dir, i);
    if file.exists() {
        return true;
    }
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let url = s.url("stream", &[("id".into(), song.id.clone()), ("format".into(), "raw".into())]);
    let Ok(r) = web.agent.get(&url).call() else { return false };
    if r.status().as_u16() != 200 {
        return false;
    }
    let mut body = Vec::new();
    if r.into_body().into_reader().read_to_end(&mut body).is_err() {
        return false;
    }
    std::fs::write(&file, &body).is_ok()
}

/// Kept song `i`'s audio, decoded by ffmpeg: interleaved stereo at 44.1 kHz.
fn pcm(dir: &Path, i: usize) -> Option<Vec<f32>> {
    let out = std::process::Command::new("ffmpeg").args(["-v", "quiet", "-i"]).arg(audio_path(dir, i)).args(["-f", "f32le", "-ac", "2", "-ar", "44100", "pipe:1"]).output().ok()?;
    let pcm = out.stdout;
    (pcm.len() >= 44100 * 8 * 20).then(|| pcm.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

/// The vocal curve as the AutoMix analysis measures it.
fn curve_of(x: &[f32], duration_s: u32) -> VocalCurve {
    let mut a = Analyzer::new(44100, duration_s as u64 * 1000);
    a.feed_interleaved(x, 2, |v| v);
    a.take_features().voice_curve()
}

fn dir() -> PathBuf {
    let d = PathBuf::from(std::env::var("NORI_TUNE_DIR").expect("NORI_TUNE_DIR"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
#[ignore]
fn sync_gather() {
    let (web, s, dir) = (Web::new(), server(), dir());
    let songs = sample(&web, &s);
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (g, _) in &songs {
        *counts.entry(g.clone()).or_default() += 1;
    }
    println!("sample: {counts:?}");
    let lookup = lyrics_lookup(&nori_settings::settings::StoredPrefs::default());
    println!("services: {}", lookup.services.iter().map(|x| x.name()).collect::<Vec<_>>().join(" "));
    for (i, (genre, song)) in songs.iter().enumerate() {
        let path = dir.join(format!("{i:03}.json"));
        let had = path.exists();
        if !fetch(&web, &s, song, &dir, i) {
            println!("{i:03} {genre}: no audio");
            continue;
        }
        if had {
            continue;
        }
        let Some(x) = pcm(&dir, i) else {
            println!("{i:03} {genre}: no audio");
            continue;
        };
        let c = curve_of(&x, song.duration);
        drop(x);
        let shared = Shared::default();
        let mut found: Vec<(LyricsService, Lyrics, Named)> = Vec::new();
        for service in &lookup.services {
            let a = Ask::new(&web, &lookup, &shared, *service);
            if let Lookup::Found(mut l, named) = block(services::ask(*service, &a, song)) {
                strip_edges(&mut l, &song.title, &song.artist);
                if !l.lines.is_empty() && plausible(&l, song) {
                    found.push((*service, l, named));
                }
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        let answers: Vec<Answer> = found
            .iter()
            .enumerate()
            .map(|(k, (service, l, named))| {
                let others: Vec<(&Lyrics, &Named)> = found.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, o)| (&o.1, &o.2)).collect();
                let trust = score(song, l, named, service.prior(), &others, lookup.prefer_words).score;
                Answer {
                    service: service.name().to_string(),
                    synced: l.synced,
                    word_timed: l.word_timed,
                    named_s: named.duration_s.map(Named::seconds),
                    title_alike: named.title.as_deref().map(|t| name_alike(t, &song.title)),
                    trust,
                    lines: timings(l),
                }
            })
            .collect();
        let same: Vec<Vec<bool>> = found.iter().map(|a| found.iter().map(|b| agree(&a.1, &b.1)).collect()).collect();
        drop(found);
        let kept = Kept { genre: genre.clone(), duration: song.duration, fps: c.fps, t0: c.t0, level: c.level, answers, same };
        std::fs::write(&path, serde_json::to_vec(&kept).unwrap()).unwrap();
        println!("{i:03} {genre}: {} answers, {} timed", kept.answers.len(), kept.answers.iter().filter(|a| a.synced).count());
    }
}

// ---- the evaluation ------------------------------------------------------------------------------------------

/// How an answer's line starts sit against another's: the offset (ms, `b` later than `a`) under which the
/// most of `a`'s starts have one of `b`'s within 150 ms, and that share.
fn line_offset(a: &[i64], b: &[i64]) -> (i64, f64) {
    if a.is_empty() || b.is_empty() {
        return (0, 0.0);
    }
    let share = |d: i64| a.iter().filter(|x| b.iter().any(|y| (y - *x - d).abs() <= 150)).count() as f64 / a.len() as f64;
    // Candidate offsets: every nearby pair's difference, rounded to 50 ms.
    let mut cands: Vec<i64> = vec![0];
    for x in a {
        for y in b {
            let d = y - x;
            if d.abs() <= 4_000 {
                cands.push((d as f64 / 50.0).round() as i64 * 50);
            }
        }
    }
    cands.sort();
    cands.dedup();
    let mut best: (i64, f64) = (0, share(0));
    for d in cands {
        let s = share(d);
        if s > best.1 + 1e-9 || (s > best.1 - 1e-9 && d.abs() < best.0.abs()) {
            best = (d, s);
        }
    }
    // The offset refined: the median difference of the matched starts.
    let mut diffs: Vec<i64> = a.iter().filter_map(|x| b.iter().map(|y| y - x).filter(|d| (d - best.0).abs() <= 150).min_by_key(|d| (d - best.0).abs())).collect();
    diffs.sort();
    (diffs.get(diffs.len() / 2).copied().unwrap_or(best.0), best.1)
}

fn starts(a: &Answer) -> Vec<i64> {
    a.lines.iter().filter(|l| !l.3 && l.0 >= 0).map(|l| l.4.first().map_or(l.0, |w| w.0)).collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Truth {
    Good,
    /// Same words, its lines this much later than the good ones'.
    Shifted(i64),
    Wrong,
    Unknown,
}

/// Each answer's class, from agreement among the timed answers.
fn classify(k: &Kept) -> Vec<Truth> {
    let n = k.answers.len();
    let st: Vec<Vec<i64>> = k.answers.iter().map(starts).collect();
    let timed: Vec<bool> = k.answers.iter().map(|a| a.synced && starts(a).len() >= 4).collect();
    // Timing agreement: same words, the same starts within 150 ms for 60 % of each's lines, at no offset.
    let tie = |i: usize, j: usize| -> Option<(i64, f64)> {
        if !(timed[i] && timed[j] && k.same[i][j]) {
            return None;
        }
        let (d1, s1) = line_offset(&st[i], &st[j]);
        let (d2, s2) = line_offset(&st[j], &st[i]);
        (s1 >= 0.6 && s2 >= 0.6 && (d1 + d2).abs() <= 150).then_some((d1, s1.min(s2)))
    };
    // The largest group of answers tied at an offset under 150 ms.
    // ...of at least two timings of their own: copies of one catalogue's are one.
    let distinct = |g: &[usize]| g.iter().enumerate().filter(|(p, i)| !g[..*p].iter().any(|j| copy_of(&k.answers[*j], &k.answers[**i]))).count();
    let mut group: Vec<usize> = Vec::new();
    for i in 0..n {
        let g: Vec<usize> = (0..n).filter(|j| *j == i || tie(i, *j).is_some_and(|(d, _)| d.abs() <= 150)).collect();
        if distinct(&g) >= 2 && distinct(&g) > distinct(&group) {
            group = g;
        }
    }
    let mut out = vec![Truth::Unknown; n];
    if group.is_empty() {
        // No consensus: only lengths far off say anything.
        for (i, a) in k.answers.iter().enumerate() {
            if a.named_s.is_some_and(|d| (d - k.duration as f64).abs() > 8.0) && timed[i] {
                out[i] = Truth::Wrong;
            }
        }
        return out;
    }
    for i in 0..n {
        if !timed[i] {
            continue;
        }
        let other_length = k.answers[i].named_s.is_some_and(|d| (d - k.duration as f64).abs() > 8.0);
        if group.contains(&i) {
            out[i] = if other_length { Truth::Unknown } else { Truth::Good };
            continue;
        }
        let words = group.iter().any(|g| k.same[i][*g]);
        if !words || other_length {
            out[i] = Truth::Wrong;
            continue;
        }
        // Same words, not tied at 0: a clear offset from every member?
        let offs: Vec<i64> = group.iter().filter_map(|g| tie(i, *g).map(|(d, _)| -d)).collect();
        if offs.len() == group.len() {
            let mut o = offs.clone();
            o.sort();
            let m = o[o.len() / 2];
            if m.abs() >= 250 && o.iter().all(|x| (x - m).abs() <= 150) {
                out[i] = Truth::Shifted(m);
            }
        }
    }
    out
}

fn shifted(a: &Answer, by: &dyn Fn(i64) -> i64) -> Answer {
    let mut b = a.clone();
    for l in &mut b.lines {
        l.0 = by(l.0);
        l.1 = by(l.1);
        for w in &mut l.4 {
            *w = (by(w.0), by(w.1));
        }
    }
    b
}

struct Case {
    genre: String,
    song: usize,
    service: String,
    truth: Truth,
    /// What the check should find, beyond the class: "shift" by ms (from the answer's own), "half" drifted.
    made: Option<&'static str>,
    shift_ms: i64,
    m: Option<Measure>,
    /// The same answer unshifted, for shift cases.
    base: Option<Measure>,
    word_timed: bool,
    /// For a shifted answer, the good answers' measures: it should be shown at their offset plus its shift.
    cons: Vec<Measure>,
}

fn load() -> Vec<(usize, Kept)> {
    let dir = dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    files.iter().map(|p| (p.file_stem().unwrap().to_string_lossy().parse().unwrap(), serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())).collect()
}

fn kept_curve(_: usize, k: &Kept) -> Option<VocalCurve> {
    Some(VocalCurve { fps: k.fps, t0: k.t0, level: k.level.clone() })
}

/// Whether `b` is a copy of `a`'s timing (the same catalogue's lyrics through two services).
fn copy_of(a: &Answer, b: &Answer) -> bool {
    let (x, y) = (starts(a), starts(b));
    x.len() == y.len() && x.iter().zip(&y).all(|(p, q)| (p - q).abs() <= 20)
}

fn cases(songs: &[(usize, Kept)], curve_of: &dyn Fn(usize, &Kept) -> Option<VocalCurve>) -> Vec<Case> {
    let mut out = Vec::new();
    let curves: Vec<Option<VocalCurve>> = songs.iter().map(|(i, k)| curve_of(*i, k)).collect();
    for (pos, (idx, k)) in songs.iter().enumerate() {
        let Some(curve) = &curves[pos] else { continue };
        let other = (1..songs.len()).map(|d| (pos + d) % songs.len()).find_map(|j| curves[j].as_ref());
        let truth = classify(k);
        let mut goods: Vec<Measure> = Vec::new();
        let mut shifts: Vec<Case> = Vec::new();
        for (i, (a, t)) in k.answers.iter().zip(&truth).enumerate() {
            if !a.synced || k.answers[..i].iter().any(|b| b.synced && copy_of(b, a)) {
                continue;
            }
            let Some(m) = measure(&a.lyrics(), curve) else { continue };
            let c = |truth, made, shift_ms, m, base| Case { genre: k.genre.clone(), song: *idx, service: a.service.clone(), truth, made, shift_ms, m, base, word_timed: a.word_timed, cons: Vec::new() };
            if let Truth::Shifted(_) = t {
                shifts.push(c(*t, None, 0, m, None));
                continue;
            }
            out.push(c(*t, None, 0, m, None));
            if *t != Truth::Good {
                continue;
            }
            goods.extend(m);
            if let Some(o) = other {
                out.push(c(Truth::Wrong, Some("other"), 0, measure(&a.lyrics(), o).flatten(), None));
            }
            for s in [-2000, -1000, -500, 500, 1000, 2000] {
                let b = shifted(a, &|x| if x >= 0 { x + s } else { x });
                out.push(c(Truth::Good, Some("shift"), s, measure(&b.lyrics(), curve).flatten(), m));
            }
            let st = starts(a);
            if st.len() >= 8 {
                let mid = st[st.len() / 2] - 100;
                for s in [-2000, -1000, 1000, 2000] {
                    let b = shifted(a, &|x| if x >= mid { x + s } else { x });
                    out.push(c(Truth::Good, Some("half"), s, measure(&b.lyrics(), curve).flatten(), m));
                }
            }
        }
        for mut c in shifts {
            c.cons = goods.clone();
            out.push(c);
        }
    }
    out
}

fn auc(a: &[f64], b: &[f64]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return f64::NAN;
    }
    a.iter().map(|x| b.iter().map(|y| if x > y { 1.0 } else if x == y { 0.5 } else { 0.0 }).sum::<f64>()).sum::<f64>() / (a.len() * b.len()) as f64
}

fn q(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[((s.len() - 1) as f64 * p).round() as usize]
}

/// How the thresholds do over the cases.
#[derive(Default, Debug, Clone)]
struct Tally {
    good: usize,
    good_poor: usize,
    good_drifts: usize,
    good_shifted: usize,
    wrong: usize,
    wrong_poor: usize,
    wrong_caught: usize,
    nat_shift: usize,
    nat_fixed: usize,
    synth: usize,
    synth_fixed: usize,
    synth_demoted: usize,
    half: usize,
    half_drifts: usize,
    half_poor: usize,
}

fn tally(cs: &[Case], t: &Thresholds, genre: Option<&str>) -> Tally {
    let mut r = Tally::default();
    for c in cs.iter().filter(|c| genre.is_none_or(|g| c.genre == g)) {
        let Some(m) = c.m else { continue };
        let k = t.read(&m);
        match (c.truth, c.made) {
            (Truth::Good, None) => {
                r.good += 1;
                r.good_poor += usize::from(k.kind == SyncKind::Poor);
                r.good_drifts += usize::from(k.kind == SyncKind::Drifts);
                r.good_shifted += usize::from(k.kind == SyncKind::Shifted);
            }
            (Truth::Wrong, _) => {
                r.wrong += 1;
                r.wrong_poor += usize::from(k.kind == SyncKind::Poor);
                r.wrong_caught += usize::from(matches!(k.kind, SyncKind::Poor | SyncKind::Drifts));
            }
            (Truth::Shifted(s), _) => {
                if c.cons.is_empty() {
                    continue;
                }
                r.nat_shift += 1;
                // Put right: shown at the good answers' offset (as they are shown) plus its own shift, within 250 ms.
                let mut offs: Vec<i64> = c.cons.iter().map(|m| t.read(m).applied_ms()).collect();
                offs.sort();
                r.nat_fixed += usize::from((k.applied_ms() - offs[offs.len() / 2] - s).abs() <= 250);
            }
            (Truth::Good, Some("shift")) => {
                let base = t.read(&c.base.unwrap());
                if matches!(base.kind, SyncKind::Poor | SyncKind::Drifts) {
                    continue;
                }
                r.synth += 1;
                // Shown right: the answer's own applied offset plus the shift, within 250 ms.
                r.synth_fixed += usize::from((k.applied_ms() - base.applied_ms() - c.shift_ms).abs() <= 250);
                r.synth_demoted += usize::from(matches!(k.kind, SyncKind::Poor | SyncKind::Drifts));
            }
            (Truth::Good, Some(_)) => {
                let base = t.read(&c.base.unwrap());
                if matches!(base.kind, SyncKind::Poor | SyncKind::Drifts) {
                    continue;
                }
                r.half += 1;
                r.half_drifts += usize::from(k.kind == SyncKind::Drifts);
                r.half_poor += usize::from(k.kind == SyncKind::Poor);
            }
            _ => {}
        }
    }
    r
}

fn show(name: &str, r: &Tally) {
    let pc = |a: usize, b: usize| if b == 0 { "   -".to_string() } else { format!("{:3.0}%", 100.0 * a as f64 / b as f64) };
    println!(
        "{name:<12} good {:3}: poor {} drifts {} shifted {} | wrong {:3}: poor {} caught {} | natural shift {:2}: fixed {} | shifted by hand {:3}: fixed {} demoted {} | half {:3}: drifts {} poor {}",
        r.good,
        pc(r.good_poor, r.good),
        pc(r.good_drifts, r.good),
        pc(r.good_shifted, r.good),
        r.wrong,
        pc(r.wrong_poor, r.wrong),
        pc(r.wrong_caught, r.wrong),
        r.nat_shift,
        pc(r.nat_fixed, r.nat_shift),
        r.synth,
        pc(r.synth_fixed, r.synth),
        pc(r.synth_demoted, r.synth),
        r.half,
        pc(r.half_drifts, r.half),
        pc(r.half_poor, r.half),
    );
}

/// Lower is better: false demotions of good answers weigh most, then missed wrong ones, missed shifts and
/// missed half-shifts.
fn cost(r: &Tally) -> f64 {
    let f = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
    4.0 * f(r.good_poor + r.good_drifts, r.good) + 2.0 * f(r.synth_demoted, r.synth) + 1.5 * (1.0 - f(r.wrong_caught, r.wrong)) + 1.0 * (1.0 - f(r.synth_fixed, r.synth)) + 1.0 * (1.0 - f(r.half_drifts, r.half)) + 0.5 * f(r.good_shifted, r.good)
}

#[test]
#[ignore]
fn sync_real() {
    let songs = load();
    // NORI_TUNE_CURVE: a variant's curves (from the kept audio, kept once measured) in place of the kept ones.
    let variant = std::env::var("NORI_TUNE_CURVE").ok().map(|n| variants().into_iter().find(|v| v.name == n).expect("no such variant"));
    let curves: HashMap<usize, VocalCurve> = match &variant {
        Some(v) => songs.iter().filter_map(|(i, _)| variant_cached(&dir(), *i, v).map(|c| (*i, c))).collect(),
        None => songs.iter().map(|(i, k)| (*i, kept_curve(*i, k).unwrap())).collect(),
    };
    let cs = cases(&songs, &|i, _| curves.get(&i).cloned());
    let mut genres: Vec<String> = songs.iter().map(|(_, k)| k.genre.clone()).collect();
    genres.sort();
    genres.dedup();
    println!("{} songs, {} with a curve that reads", songs.len(), songs.iter().filter(|(_, k)| crate::sync::check(&Answer { service: String::new(), synced: true, word_timed: false, named_s: None, title_alike: None, trust: 0.0, lines: (0..8).map(|i| Line(10_000 + i * 5_000, 12_000 + i * 5_000, 20, false, vec![])).collect() }.lyrics(), &VocalCurve { fps: k.fps, t0: k.t0, level: k.level.clone() }).is_some()).count());
    for g in &genres {
        let ks: Vec<&Kept> = songs.iter().filter(|(_, k)| &k.genre == g).map(|(_, k)| k).collect();
        let answers: usize = ks.iter().map(|k| k.answers.len()).sum();
        let timed: usize = ks.iter().map(|k| k.answers.iter().filter(|a| a.synced).count()).sum();
        let truths: Vec<Truth> = ks.iter().flat_map(|k| classify(k)).collect();
        let count = |f: &dyn Fn(&Truth) -> bool| truths.iter().filter(|t| f(t)).count();
        println!(
            "{g:<12} {} songs, {answers} answers, {timed} timed: good {}, shifted {}, wrong {}, unknown {}",
            ks.len(),
            count(&|t| *t == Truth::Good),
            count(&|t| matches!(t, Truth::Shifted(_))),
            count(&|t| *t == Truth::Wrong),
            count(&|t| *t == Truth::Unknown) - (answers - timed),
        );
    }
    if std::env::var("NORI_TUNE_DETAIL").is_ok() {
        println!("\n-- per answer: truth, service, word timing, kind, given/best score, offset, confidence, drift parts");
        for c in cs.iter().filter(|c| c.made.is_none()) {
            let Some(m) = c.m else { continue };
            let k = Thresholds::USED.read(&m);
            println!(
                "  {:03} {:<10} {:<14} {:<17} {} {:<7} {:.2}/{:.2} off {:+5} conf {:.2} drift {:?}",
                c.song,
                c.genre,
                format!("{:?}", c.truth),
                c.service,
                if c.word_timed { "w" } else { "l" },
                format!("{:?}", k.kind),
                m.given,
                m.best_score,
                m.offset_ms,
                m.confidence,
                m.drift.map(|d| (d.ms, (d.sure * 100.0).round() / 100.0, (d.gain * 1000.0).round() / 1000.0))
            );
        }
    }
    println!("\n-- scores (at the best offset), good vs wrong, and the offset found for good answers");
    for g in genres.iter().map(|g| Some(g.as_str())).chain([None]) {
        let sel = |c: &&Case| g.is_none_or(|g| c.genre == g) && c.made.is_none() && c.m.is_some();
        let good: Vec<f64> = cs.iter().filter(sel).filter(|c| c.truth == Truth::Good).map(|c| c.m.unwrap().best_score).collect();
        let wrong: Vec<f64> = cs.iter().filter(|c| g.is_none_or(|g| c.genre == g) && c.m.is_some() && c.made != Some("shift") && c.made != Some("half")).filter(|c| c.truth == Truth::Wrong).map(|c| c.m.unwrap().best_score).collect();
        let offs: Vec<f64> = cs.iter().filter(sel).filter(|c| c.truth == Truth::Good).map(|c| c.m.unwrap().offset_ms.abs() as f64).collect();
        let lift_g: Vec<f64> = cs.iter().filter(sel).filter(|c| c.truth == Truth::Good).map(|c| c.m.unwrap().best_score - c.m.unwrap().null).collect();
        let lift_w: Vec<f64> = cs.iter().filter(|c| g.is_none_or(|g| c.genre == g) && c.m.is_some() && c.made != Some("shift") && c.made != Some("half")).filter(|c| c.truth == Truth::Wrong).map(|c| c.m.unwrap().best_score - c.m.unwrap().null).collect();
        println!("{:<12} lift over far offsets: good p10 {:.2} med {:.2} | wrong med {:.2} p90 {:.2} | AUC {:.3}", g.unwrap_or("all"), q(&lift_g, 0.1), q(&lift_g, 0.5), q(&lift_w, 0.5), q(&lift_w, 0.9), auc(&lift_g, &lift_w));
        let conf: Vec<f64> = cs.iter().filter(sel).filter(|c| c.truth == Truth::Good).map(|c| c.m.unwrap().confidence).collect();
        println!(
            "{:<12} good n {:3} p10 {:.2} med {:.2} | wrong n {:3} med {:.2} p90 {:.2} | AUC {:.3} | good |offset| med {:4.0} p90 {:4.0} ms, conf med {:.2}",
            g.unwrap_or("all"),
            good.len(),
            q(&good, 0.1),
            q(&good, 0.5),
            wrong.len(),
            q(&wrong, 0.5),
            q(&wrong, 0.9),
            auc(&good, &wrong),
            q(&offs, 0.5),
            q(&offs, 0.9),
            q(&conf, 0.5)
        );
    }
    // Offsets recovered for the shifts made by hand, whatever the thresholds: error against the answer's own.
    println!("\n-- offset recovery of shifts made by hand (error of the best offset, ms)");
    for g in genres.iter().map(|g| Some(g.as_str())).chain([None]) {
        let errs: Vec<f64> = cs
            .iter()
            .filter(|c| g.is_none_or(|g| c.genre == g) && c.made == Some("shift") && c.m.is_some())
            .map(|c| (c.m.unwrap().offset_ms - c.base.unwrap().offset_ms - c.shift_ms).abs() as f64)
            .collect();
        let within = |ms: f64| errs.iter().filter(|e| **e <= ms).count() as f64 / errs.len().max(1) as f64 * 100.0;
        println!("{:<12} n {:3} median {:4.0} p90 {:5.0}; within 120 ms {:3.0}%, 250 ms {:3.0}%", g.unwrap_or("all"), errs.len(), q(&errs, 0.5), q(&errs, 0.9), within(120.0), within(250.0));
    }
    println!("\n-- drift measured on half-shifts made by hand (the best cut's parts)");
    for s in [-2000i64, -1000, 1000, 2000] {
        let d: Vec<(i64, f64, f64)> = cs.iter().filter(|c| c.made == Some("half") && c.shift_ms == s).filter_map(|c| c.m.and_then(|m| m.drift)).map(|d| (d.ms, d.sure, d.gain)).collect();
        let err: Vec<f64> = d.iter().map(|x| (x.0 - s).abs() as f64).collect();
        println!("half {s:+5}: n {:3} |drift err| med {:4.0} p90 {:5.0}; sure med {:.2} p10 {:.2}; gain med {:.3} p10 {:.3}", d.len(), q(&err, 0.5), q(&err, 0.9), q(&d.iter().map(|x| x.1).collect::<Vec<_>>(), 0.5), q(&d.iter().map(|x| x.1).collect::<Vec<_>>(), 0.1), q(&d.iter().map(|x| x.2).collect::<Vec<_>>(), 0.5), q(&d.iter().map(|x| x.2).collect::<Vec<_>>(), 0.1));
    }
    let good_d: Vec<(i64, f64, f64)> = cs.iter().filter(|c| c.made.is_none() && c.truth == Truth::Good).filter_map(|c| c.m.and_then(|m| m.drift)).map(|d| (d.ms, d.sure, d.gain)).collect();
    println!(
        "good answers: n {} |drift| med {:4.0} p90 {:5.0}; sure med {:.2} p90 {:.2}; gain med {:.3} p90 {:.3}",
        good_d.len(),
        q(&good_d.iter().map(|x| x.0.abs() as f64).collect::<Vec<_>>(), 0.5),
        q(&good_d.iter().map(|x| x.0.abs() as f64).collect::<Vec<_>>(), 0.9),
        q(&good_d.iter().map(|x| x.1).collect::<Vec<_>>(), 0.5),
        q(&good_d.iter().map(|x| x.1).collect::<Vec<_>>(), 0.9),
        q(&good_d.iter().map(|x| x.2).collect::<Vec<_>>(), 0.5),
        q(&good_d.iter().map(|x| x.2).collect::<Vec<_>>(), 0.9)
    );

    println!("\n-- with the thresholds in use");
    for g in genres.iter().map(|g| Some(g.as_str())).chain([None]) {
        show(g.unwrap_or("all"), &tally(&cs, &Thresholds::USED, g));
    }
    let base = cost(&tally(&cs, &Thresholds::USED, None));
    println!("cost {base:.3}");

    // The sweep, one threshold at a time around the ones in use, then a joint grid.
    let mut best = (base, Thresholds::USED);
    let grid = std::env::var("NORI_TUNE_GRID").is_ok();
    for poor in if grid { vec![0.2, 0.3, 0.4, 0.5] } else { vec![] } {
        for offset_sure in [0.3, 0.5, 0.7] {
            for offset_min_ms in [250, 350, 500] {
                for drift_ms in [500, 700, 1000] {
                    for part_sure in [0.2, 0.3, 0.5] {
                        for part_gain in [0.05, 0.1, 0.15, 0.2] {
                            for lift in [0.0, 0.05, 0.08, 0.1, 0.12] {
                                {
                                    let t = Thresholds { poor, offset_sure, offset_min_ms, drift_ms, part_sure, part_gain, lift };
                                    let c = cost(&tally(&cs, &t, None));
                                    if c < best.0 - 1e-9 {
                                        best = (c, t);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("\n-- best on the grid, cost {:.3}: {:?}", best.0, best.1);
    for g in genres.iter().map(|g| Some(g.as_str())).chain([None]) {
        show(g.unwrap_or("all"), &tally(&cs, &best.1, g));
    }
    let custom = std::env::var("NORI_TUNE_T").ok().map(|v| {
        let x: Vec<f64> = v.split(',').map(|s| s.parse().unwrap()).collect();
        Thresholds { poor: x[0], offset_sure: x[1], offset_min_ms: x[2] as i64, drift_ms: x[3] as i64, part_sure: x[4], part_gain: x[5], lift: x[6] }
    });
    if let Some(t) = custom {
        println!("\n-- NORI_TUNE_T {t:?}, cost {:.3}", cost(&tally(&cs, &t, None)));
        for g in genres.iter().map(|g| Some(g.as_str())).chain([None]) {
            show(g.unwrap_or("all"), &tally(&cs, &t, g));
        }
    }
    sweeps(&cs, &custom.unwrap_or(Thresholds::USED));
    trust_weight(&songs, &curves, &custom.unwrap_or(Thresholds::USED));
    detection(&songs, &curves);
}

/// How well the curve tells sung from unsung per genre, the lines of a good answer taken as where the voice
/// is: the AUC of the level (smoothed over half a second) in the lines' sung spans over outside them, and
/// how much of the long stretches with no line (4 s or more, an instrumental break or a solo) reads above
/// the sung spans' median.
fn detection(songs: &[(usize, Kept)], curves: &HashMap<usize, VocalCurve>) {
    println!("\n-- the curve per genre: sung spans (a good answer's lines) against the rest");
    let mut rows: HashMap<String, (Vec<f64>, Vec<f64>)> = HashMap::new();
    for (idx, k) in songs {
        let Some(c) = curves.get(idx) else { continue };
        let Some((a_, above)) = detect(k, c) else { continue };
        let e = rows.entry(k.genre.clone()).or_default();
        e.0.push(a_);
        e.1.extend(above.map(|x| x.0));
        println!("  {idx:03} {:<11} AUC {a_:.2}  long unlined stretches above the sung median {}", k.genre, above.map_or("   -".into(), |(x, secs)| format!("{:3.0}% of {secs:4.0} s", 100.0 * x)));
    }
    detection_rows(rows);
}

fn detection_rows(rows: HashMap<String, (Vec<f64>, Vec<f64>)>) {
    let mut g: Vec<_> = rows.into_iter().collect();
    g.sort_by(|a, b| a.0.cmp(&b.0));
    let (mut all_a, mut all_b) = (Vec::new(), Vec::new());
    for (name, (aucs, above)) in g {
        println!("{name:<12} songs {:2}: AUC median {:.2} min {:.2}; unlined stretches above the sung median, median share {:.2} max {:.2}", aucs.len(), q(&aucs, 0.5), q(&aucs, 0.0), q(&above, 0.5), q(&above, 1.0));
        all_a.extend(aucs);
        all_b.extend(above);
    }
    println!("{:<12} songs {:2}: AUC median {:.2} mean {:.3}; unlined above median, median share {:.2}", "all", all_a.len(), q(&all_a, 0.5), all_a.iter().sum::<f64>() / all_a.len().max(1) as f64, q(&all_b, 0.5));
}

/// A song's sung frames against its unsung ones, the lines of its good answer (word-timed first) taken as
/// where the voice is: the AUC of the level smoothed over half a second, and the share of long unlined
/// stretches (4 s or more: a break or a solo) above the sung frames' median, with their seconds.
fn detect(k: &Kept, c: &VocalCurve) -> Option<(f64, Option<(f64, f64)>)> {
    let truth = classify(k);
    let good: Vec<&Answer> = k.answers.iter().zip(&truth).filter(|(_, t)| **t == Truth::Good).map(|(a, _)| a).collect();
    let a = good.iter().find(|a| a.word_timed).or(good.first())?;
    let fps = c.fps as f64;
    let n = c.level.len();
    let r = (0.25 * fps) as usize;
    let lv: Vec<f64> = (0..n).map(|i| c.level[i.saturating_sub(r)..(i + r + 1).min(n)].iter().map(|v| *v as f64).sum::<f64>() / ((i + r + 1).min(n) - i.saturating_sub(r)) as f64).collect();
    let frame = |ms: i64| (((ms as f64 / 1000.0) - c.t0 as f64) * fps).max(0.0) as usize;
    let mut sung = vec![false; n];
    let lines: Vec<&Line> = a.lines.iter().filter(|l| l.0 >= 0 && !l.3).collect();
    for (i, l) in lines.iter().enumerate() {
        let spans: Vec<(i64, i64)> = if !l.4.is_empty() && a.word_timed {
            l.4.clone()
        } else {
            let next = lines.get(i + 1).map_or(i64::MAX, |x| x.0);
            let end = (l.0 + ((l.2 as f64 * 0.09).clamp(1.5, 6.0) * 1000.0) as i64).min(next).min(if l.1 > l.0 { l.1 } else { i64::MAX });
            vec![(l.0, end)]
        };
        for (s, e) in spans {
            let (fa, fb) = (frame(s).min(n), frame(e).min(n));
            sung[fa..fb.max(fa)].fill(true);
        }
    }
    let (first, last) = (frame(lines.first().map_or(0, |l| l.0)).min(n), frame(lines.last().map_or(0, |l| l.1.max(l.0 + 2000))).min(n));
    let (mut pos, mut neg) = (Vec::new(), Vec::new());
    // Frames near a span's edge say little either way.
    let pad = (0.3 * fps) as usize;
    for i in first..last {
        let w = &sung[i.saturating_sub(pad)..(i + pad + 1).min(n)];
        if w.iter().any(|x| *x) && !w.iter().all(|x| *x) {
            continue;
        }
        if sung[i] {
            pos.push(lv[i])
        } else {
            neg.push(lv[i])
        }
    }
    let med = q(&pos, 0.5);
    let long = (4.0 * fps) as usize;
    let (mut above, mut total, mut run) = (0usize, 0usize, Vec::new());
    for i in first..=last {
        if i < last && !sung[i] {
            run.push(lv[i]);
        } else {
            if run.len() >= long {
                total += run.len();
                above += run.iter().filter(|v| **v > med).count();
            }
            run.clear();
        }
    }
    Some((auc(&pos, &neg), (total > 0).then(|| (above as f64 / total as f64, total as f64 / fps))))
}

/// Whether the sync term makes the race choose better: over songs with good and wrong timed answers, how
/// often the best-scored answer is a good one, with the check weighed at each `W_SYNC`.
fn trust_weight(songs: &[(usize, Kept)], curves: &HashMap<usize, VocalCurve>, t: &Thresholds) {
    println!("\n-- the trust weight: songs where a good answer comes out on top");
    for w in [0.0, 0.1, 0.15, 0.2, 0.25, 0.3, 0.4] {
        let (mut n, mut top_good, mut top_wrong, mut n_all, mut top_good_all) = (0, 0, 0, 0, 0);
        for (i, k) in songs {
            let Some(curve) = curves.get(i).cloned() else { continue };
            let truth = classify(k);
            if !truth.contains(&Truth::Good) {
                continue;
            }
            let scored: Vec<(f64, Truth)> = k
                .answers
                .iter()
                .zip(&truth)
                .map(|(a, tr)| {
                    let check: Option<SyncCheck> = measure(&a.lyrics(), &curve).map(|m| m.map_or(SyncCheck { kind: SyncKind::Unsure, score: 0.0, best_score: 0.0, offset_ms: 0, confidence: 0.0, drift_ms: 0 }, |m| t.read(&m)));
                    let base = crate::trust::Trust { score: a.trust, ..Default::default() };
                    (with_sync_w(base, check.as_ref(), w).score, *tr)
                })
                .collect();
            let top = scored.iter().cloned().fold((f64::MIN, Truth::Unknown), |b, x| if x.0 > b.0 { x } else { b });
            n_all += 1;
            top_good_all += usize::from(top.1 == Truth::Good);
            if truth.contains(&Truth::Wrong) {
                n += 1;
                top_good += usize::from(top.1 == Truth::Good);
                top_wrong += usize::from(top.1 == Truth::Wrong);
            }
        }
        // Another song's good answer put in as a rival trusted `d` more than this song's best good answer: how
        // often the good one still comes out on top.
        let unsure = SyncCheck { kind: SyncKind::Unsure, score: 0.0, best_score: 0.0, offset_ms: 0, confidence: 0.0, drift_ms: 0 };
        let goods: Vec<(usize, &Answer, f64)> = songs
            .iter()
            .filter_map(|(i, k)| {
                let truth = classify(k);
                k.answers.iter().zip(&truth).filter(|(_, t)| **t == Truth::Good).map(|(a, _)| a).max_by(|a, b| a.trust.total_cmp(&b.trust)).map(|a| (*i, a, a.trust))
            })
            .collect();
        let mut beat = [0usize; 3];
        for (j, (i, g, trust)) in goods.iter().enumerate() {
            let Some(curve) = curves.get(i) else { continue };
            let (_, rival, _) = goods[(j + 1) % goods.len()];
            let read = |a: &Answer| measure(&a.lyrics(), curve).map(|m| m.map_or(unsure, |m| t.read(&m)));
            let (cg, cr) = (read(g), read(rival));
            for (k, d) in [0.0, 0.05, 0.1].iter().enumerate() {
                let sg = with_sync_w(crate::trust::Trust { score: *trust, ..Default::default() }, cg.as_ref(), w).score;
                let sr = with_sync_w(crate::trust::Trust { score: trust + d, ..Default::default() }, cr.as_ref(), w).score;
                beat[k] += usize::from(sg > sr);
            }
        }
        println!(
            "W_SYNC {w:.2}: with good and wrong answers {n}: top good {top_good}, top wrong {top_wrong}; all songs with a good answer {n_all}: top good {top_good_all}; another song's answer trusted +0/+0.05/+0.1 beaten {}/{}/{} of {}",
            beat[0],
            beat[1],
            beat[2],
            goods.len()
        );
    }
    let _ = with_sync;
}

/// trust.rs's `with_sync` at another weight.
fn with_sync_w(mut t: crate::trust::Trust, check: Option<&SyncCheck>, w: f64) -> crate::trust::Trust {
    let Some(c) = check.filter(|c| c.kind != SyncKind::Unsure) else { return t };
    let off = match c.kind {
        SyncKind::Drifts | SyncKind::Poor => 0.1,
        _ => 0.0,
    };
    t.score = ((1.0 - w) * t.score + w * c.score - off).clamp(0.0, 1.0);
    t
}

// ---- variants of the curve --------------------------------------------------------------------------------

/// A way of measuring the curve, to try against the one in use (nori-player vocal.rs, copied here with its
/// knobs out): the band, the peak test, and what is added up.
#[derive(Debug, Clone, Copy)]
struct Variant {
    name: &'static str,
    lo_hz: f64,
    hi_hz: f64,
    peak_over: f32,
    /// 0: the movement around peaks (vocal.rs). 1: less `k` times the mean movement of the band's bins that
    /// are no peak's, times the bins read (movement that is not pitched taken off). 2: the share of the
    /// band's movement that is around peaks, times the band's mean log level and `k`.
    mode: u8,
    k: f32,
}

const BASE: Variant = Variant { name: "was", lo_hz: 250.0, hi_hz: 4000.0, peak_over: 10.0, mode: 0, k: 0.0 };

/// The analysis front end (mono at 22.05 kHz, a 1024-point Hann FFT every 256 samples) and the variant's
/// tracker; the curve as vocal.rs keeps it.
fn variant_curve(x: &[f32], v: &Variant) -> VocalCurve {
    use rustfft::num_complex::Complex32;
    let mono: Vec<f32> = x.as_chunks::<4>().0.iter().map(|p| (p[0] + p[1] + p[2] + p[3]) / 4.0).collect();
    let (n, hop, sr) = (1024usize, 256usize, 22050.0f64);
    let fft = rustfft::FftPlanner::<f32>::new().plan_fft_forward(n);
    let win: Vec<f32> = (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect();
    let amp_norm = 2.0 / win.iter().sum::<f32>();
    let bin_hz = sr / n as f64;
    let lo = ((v.lo_hz / bin_hz).ceil() as usize).max(4);
    let hi = ((v.hi_hz.min(sr / 2.0 * 0.9) / bin_hz) as usize).min(n / 2 - 4).max(lo + 8);
    let w = hi - lo + 1;
    let g = 1000.0 * amp_norm;
    let mut buf = vec![Complex32::default(); n];
    let mut prev = vec![0f32; w];
    let mut pow = vec![0f32; w + 6];
    let mut peaky = vec![false; w];
    let (mut acc, mut cnt, mut raw) = (0f32, 0usize, Vec::new());
    let mut k = hop;
    while k <= mono.len() {
        for (i, (o, wv)) in buf.iter_mut().zip(&win).enumerate() {
            let at = (k + i) as i64 - n as i64;
            *o = Complex32::new(if at >= 0 { mono[at as usize] * wv } else { 0.0 }, 0.0);
        }
        fft.process(&mut buf);
        for (p, c) in pow.iter_mut().zip(&buf[lo - 3..=hi + 3]) {
            *p = c.norm_sqr();
        }
        peaky.fill(false);
        for j in 0..w {
            let kk = j + 3;
            let p = pow[kk];
            if p >= pow[kk - 1] && p >= pow[kk + 1] && p > v.peak_over * 0.5 * (pow[kk - 3] + pow[kk + 3]) {
                peaky[j.saturating_sub(1)..(j + 2).min(w)].fill(true);
            }
        }
        let (mut moved, mut read, mut rest, mut rest_n, mut level) = (0f32, 0f32, 0f32, 0f32, 0f32);
        for i in 0..w {
            let now = 1.0 + g * pow[i + 3].sqrt();
            let before = 1.0 + g * prev[i].sqrt();
            let d = (now / before).ln().abs();
            level += now.ln();
            if peaky[i] {
                moved += d;
                read += 1.0;
            } else {
                rest += d;
                rest_n += 1.0;
            }
        }
        prev.copy_from_slice(&pow[3..3 + w]);
        let m = match v.mode {
            0 => moved,
            1 => (moved - v.k * read * rest / rest_n.max(1.0)).max(0.0),
            _ => moved / (moved + rest + 1e-3) * level / w as f32 * v.k,
        };
        acc += m;
        cnt += 1;
        if cnt == 5 {
            raw.push(acc / 5.0);
            (acc, cnt) = (0.0, 0);
        }
        k += hop;
    }
    VocalCurve::from_raw(&raw, sr / hop as f64, hop as f64 * (1.0 - 1.28) / sr)
}

/// Song `i`'s curve by variant `v`, measured once from its kept audio and kept beside it.
fn variant_cached(dir: &Path, i: usize, v: &Variant) -> Option<VocalCurve> {
    let path = dir.join("curves").join(format!("{}-{i:03}.bin", v.name.replace([' ', '/'], "_")));
    if let Some(c) = std::fs::read(&path).ok().and_then(|b| VocalCurve::decode(&b)) {
        return Some(c);
    }
    let c = variant_curve(&pcm(dir, i)?, v);
    std::fs::create_dir_all(path.parent().unwrap()).ok()?;
    std::fs::write(&path, c.encode()).ok()?;
    Some(c)
}

fn variants() -> Vec<Variant> {
    vec![
        BASE,
        Variant { name: "300-3000", lo_hz: 300.0, hi_hz: 3000.0, ..BASE },
        Variant { name: "200-5000", lo_hz: 200.0, hi_hz: 5000.0, ..BASE },
        Variant { name: "peaks x5", peak_over: 5.0, ..BASE },
        Variant { name: "peaks x20", peak_over: 20.0, ..BASE },
        Variant { name: "peaks x3", peak_over: 3.0, ..BASE },
        Variant { name: "peaks x7", peak_over: 7.0, ..BASE },
        Variant { name: "300-3000 x5", lo_hz: 300.0, hi_hz: 3000.0, peak_over: 5.0, ..BASE },
        Variant { name: "less flat 0.5", mode: 1, k: 0.5, ..BASE },
        Variant { name: "less flat 1", mode: 1, k: 1.0, ..BASE },
        Variant { name: "pitched share", mode: 2, k: 10.0, ..BASE },
    ]
}

/// `sync_variants`: each variant's curve for every kept song's audio, then the detection and the check's
/// figures with it. `NORI_TUNE_V` names the variants to run (all by default).
#[test]
#[ignore]
fn sync_variants() {
    let songs = load();
    let dir = dir();
    let pick = std::env::var("NORI_TUNE_V").ok();
    let variants: Vec<Variant> = variants().into_iter().filter(|v| pick.as_deref().is_none_or(|p| p.split(',').any(|x| x == v.name))).collect();
    // Every song's curves, one decode each.
    let mut curves: HashMap<(usize, usize), VocalCurve> = HashMap::new();
    for (idx, k) in &songs {
        let Some(x) = pcm(&dir, *idx) else { continue };
        let got: Vec<VocalCurve> = std::thread::scope(|sc| variants.iter().map(|v| sc.spawn(|| variant_curve(&x, v))).collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap()).collect());
        if variants[0].name == "was" {
            let (a, b) = (&got[0], &k.level);
            let same = a.level.iter().zip(b).filter(|(x, y)| (**x as i32 - **y as i32).abs() <= 1).count();
            if same * 100 < b.len() * 95 || a.level.len().abs_diff(b.len()) > 2 {
                println!("{idx:03}: the copy of the analysis reads differently ({same} of {} frames within 1)", b.len());
            }
        }
        for (vi, c) in got.into_iter().enumerate() {
            curves.insert((vi, *idx), c);
        }
    }
    for (vi, v) in variants.iter().enumerate() {
        println!("\n==== {} {:?}", v.name, v);
        let mut rows: HashMap<String, (Vec<f64>, Vec<f64>)> = HashMap::new();
        for (idx, k) in &songs {
            let Some(c) = curves.get(&(vi, *idx)) else { continue };
            if let Some((a, above)) = detect(k, c) {
                let e = rows.entry(k.genre.clone()).or_default();
                e.0.push(a);
                e.1.extend(above.map(|x| x.0));
            }
        }
        detection_rows(rows);
        let cs = cases(&songs, &|idx, _| curves.get(&(vi, idx)).cloned());
        let good: Vec<f64> = cs.iter().filter(|c| c.made.is_none() && c.truth == Truth::Good).filter_map(|c| c.m.map(|m| m.best_score)).collect();
        let wrong: Vec<f64> = cs.iter().filter(|c| (c.made.is_none() || c.made == Some("other")) && c.truth == Truth::Wrong).filter_map(|c| c.m.map(|m| m.best_score)).collect();
        let errs: Vec<f64> = cs.iter().filter(|c| c.made == Some("shift") && c.m.is_some()).map(|c| (c.m.unwrap().offset_ms - c.base.unwrap().offset_ms - c.shift_ms).abs() as f64).collect();
        println!(
            "scores good med {:.2} p10 {:.2}, wrong med {:.2} p90 {:.2}, AUC {:.3}; shift error med {:.0} p90 {:.0}, within 250 ms {:.0}%",
            q(&good, 0.5),
            q(&good, 0.1),
            q(&wrong, 0.5),
            q(&wrong, 0.9),
            auc(&good, &wrong),
            q(&errs, 0.5),
            q(&errs, 0.9),
            errs.iter().filter(|e| **e <= 250.0).count() as f64 / errs.len().max(1) as f64 * 100.0
        );
        let lg: Vec<f64> = cs.iter().filter(|c| c.made.is_none() && c.truth == Truth::Good).filter_map(|c| c.m.map(|m| m.best_score - m.null)).collect();
        let lw: Vec<f64> = cs.iter().filter(|c| (c.made.is_none() || c.made == Some("other")) && c.truth == Truth::Wrong).filter_map(|c| c.m.map(|m| m.best_score - m.null)).collect();
        println!("lift AUC {:.3}", auc(&lg, &lw));
        for g in ["metal", "rock", "electronic", "rap/rnb", "live"] {
            let lg: Vec<f64> = cs.iter().filter(|c| c.genre == g && c.made.is_none() && c.truth == Truth::Good).filter_map(|c| c.m.map(|m| m.best_score - m.null)).collect();
            let lw: Vec<f64> = cs.iter().filter(|c| c.genre == g && (c.made.is_none() || c.made == Some("other")) && c.truth == Truth::Wrong).filter_map(|c| c.m.map(|m| m.best_score - m.null)).collect();
            print!("  {g} lift AUC {:.3} (n {})", auc(&lg, &lw), lg.len());
        }
        println!();
        show("all", &tally(&cs, &Thresholds::USED, None));
    }
}

/// One knob at a time around `t`: good answers demoted (Poor or Drifts) against what is caught.
fn sweeps(cs: &[Case], t: &Thresholds) {
    let line = |name: String, t: &Thresholds| {
        let r = tally(cs, t, None);
        let f = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
        println!(
            "  {name:<22} good demoted {:4.1}% (poor {:4.1}%, drifts {:4.1}%) shifted {:4.1}% | wrong caught {:4.1}% | by hand fixed {:4.1}% demoted {:4.1}% | half drifts {:4.1}% | cost {:.3}",
            f(r.good_poor + r.good_drifts, r.good),
            f(r.good_poor, r.good),
            f(r.good_drifts, r.good),
            f(r.good_shifted, r.good),
            f(r.wrong_caught, r.wrong),
            f(r.synth_fixed, r.synth),
            f(r.synth_demoted, r.synth),
            f(r.half_drifts, r.half),
            cost(&r)
        );
    };
    println!("\n-- one knob at a time around {t:?}");
    for x in [0.0, 0.02, 0.03, 0.04, 0.05, 0.06, 0.08, 0.1] {
        line(format!("lift {x}"), &Thresholds { lift: x, ..*t });
    }
    for x in [0.2, 0.3, 0.35, 0.4, 0.45, 0.5] {
        line(format!("poor {x}"), &Thresholds { poor: x, ..*t });
    }
    for x in [0.3, 0.4, 0.5, 0.6, 0.7] {
        line(format!("offset_sure {x}"), &Thresholds { offset_sure: x, ..*t });
    }
    for x in [200, 250, 300, 350, 400, 500] {
        line(format!("offset_min_ms {x}"), &Thresholds { offset_min_ms: x, ..*t });
    }
    for x in [500, 700, 900, 1200] {
        line(format!("drift_ms {x}"), &Thresholds { drift_ms: x, ..*t });
    }
    for x in [0.2, 0.3, 0.4, 0.5, 0.7] {
        line(format!("part_sure {x}"), &Thresholds { part_sure: x, ..*t });
    }
    for x in [0.05, 0.08, 0.1, 0.12, 0.15, 0.2] {
        line(format!("part_gain {x}"), &Thresholds { part_gain: x, ..*t });
    }
}
