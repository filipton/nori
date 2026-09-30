//! Differential harness: JSON answers and variants of them, outputs compared with goldens from the old code.

use super::*;
use serde_json::Value;

fn corpus() -> Vec<String> {
    let mut docs: Vec<String> = [
        include_str!("../testdata/lyricsplus.json"),
        include_str!("../testdata/paxsenix-apple.json"),
        include_str!("../testdata/spotify.json"),
        include_str!("../testdata/musixmatch-richsync.json"),
        include_str!("../testdata/musixmatch-subtitle.json"),
        include_str!("../testdata/youtube-search.json"),
        r#"{"type":"Syllable","lyrics":[{"time":0,"duration":900,"text":"","syllabus":[{"time":0,"duration":300,"text":"Ri","part":true},{"time":300,"duration":300,"text":"ver","part":false},{"time":600,"duration":300,"text":"song","part":false}]}]}"#,
        r#"{"type":"None","content":[{"timestamp":0,"endtime":0,"text":[{"text":"just words","timestamp":0,"endtime":0}]}]}"#,
        r#"{"ttml":"<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div><p begin=\"1.0\" end=\"2.0\"><span begin=\"1.0\" end=\"1.5\">Hi</span> <span begin=\"1.5\" end=\"2.0\">there</span></p></div></body></tt>","score":0.93}"#,
        r#"{"lyrics":"[ti:Song]\n[1000,2000]Hi (1000,500)there(1500,1500)","provider":"qq"}"#,
        r#"{"data":"{\"lrc\":\"[00:01.00]one\\n[00:02.00]two\"}"}"#,
        r#"{"lyrics":"&lt;tt&gt;&lt;body&gt;&lt;p begin=\"1\" end=\"2\"&gt;Hello&lt;/p&gt;&lt;/body&gt;&lt;/tt&gt;"}"#,
        r#"{"plainLyrics":"one\ntwo","syncedLyrics":"[00:01.00]one\n[00:02.00]two"}"#,
        r#"{"error":"No lyrics found","lyrics":"[00:01.00]x"}"#,
        r#"{"isError":true}"#,
        r#"{"error":false,"syncType":"LINE_SYNCED","lines":[{"timeTag":"00:00.96","words":"One"},{"timeTag":"00:04.02","words":"Two"}]}"#,
        r#"{"lyrics_body":"First\nSecond\n\n******* This Lyrics is NOT for Commercial use *******\n(1409623253212)"}"#,
        r#"{"results":{"songs":{"data":[{"id":"1000000001","type":"songs","attributes":{"name":"Glass Harbour","artistName":"The Lanterns","durationInMillis":238640,"albumName":"Low Tide"}}]}}}"#,
        r#"{"ok":true,"tracks":[{"id":"0aBcDeFgHiJkLmNoPqRsTu","name":"Glass Harbour","artists":[{"id":"4Z8W","name":"The Lanterns"}],"album":{"id":"6AZv","name":"Low Tide"},"duration":238}]}"#,
        r#"{"contents":{"singleColumnMusicWatchNextResultsRenderer":{"tabbedRenderer":{"watchNextTabbedResultsRenderer":{"tabs":[{"tabRenderer":{"title":"Up next","content":{}}},{"tabRenderer":{"title":"Lyrics","endpoint":{"browseEndpoint":{"browseId":"MPLYt_abc123","params":"p"}}}},{"tabRenderer":{"title":"Related","endpoint":{"browseEndpoint":{"browseId":"MPTRt_x"}}}}]}}}}}"#,
        r#"{"tabs":[{"tabRenderer":{"title":"Lyrics","unselectable":true}},{"x":{"browseEndpoint":{"browseId":"MPLYt_q"}}}]}"#,
        r#"{"contents":{"sectionListRenderer":{"contents":[{"musicDescriptionShelfRenderer":{"description":{"runs":[{"text":"Paper boats\nLa la\n\nCounting"}]}}}]}}}"#,
        r#"{"actions":[{"transcriptRenderer":{"cueGroups":[{"cues":[{"transcriptCueRenderer":{"cue":{"simpleText":"♪ Paper boats on a quiet river ♪"},"startOffsetMs":"16210","durationMs":"3460"}}]},{"cues":[{"transcriptCueRenderer":{"cue":{"simpleText":"[Music]"},"startOffsetMs":"19670","durationMs":"2000"}}]},{"cues":[{"transcriptCueRenderer":{"cue":{"runs":[{"text":"counting lamps\nalong the (pier (hey)"}]},"startOffsetMs":"22000","durationMs":"3000"}}]}]}}]}"#,
        r#"{"transcriptSegmentRenderer":{"startMs":"1000","endMs":"2500","snippet":{"runs":[{"text":"Hello (Applause) [ooh]"}]}}}"#,
        r#"{"wireMagic":"pb3","events":[{"tStartMs":0,"dDurationMs":1000},{"tStartMs":1200,"dDurationMs":2000,"segs":[{"utf8":"hel"},{"utf8":"lo","tOffsetMs":400}]}]}"#,
        r#"{"lyrics":{"syncType":"UNSYNCED","lines":[{"startTimeMs":"0","words":"a"},{"startTimeMs":"0","words":"b"}]}}"#,
        r#"[{"ts":1.5,"te":3,"l":[],"x":"plain line"},{"ts":"4","l":[{"c":"a","o":0},{"c":" ","o":0.2},{"c":"b"}]}]"#,
        r#"[{"text":"","time":{"total":1}},{"text":"x","time":{"total":"2.5"}},{"text":"♪","time":{"total":4}},{"text":"y","time":{"total":5}}]"#,
        r#"{"data":{"lyrics":[{"time":"100","endTime":900,"text":"hey","element":{"singer":"v2"}},{"time":1000,"duration":500,"text":"you","element":{"singer":"v1"}}]},"metadata":{"agents":{"v1":{"type":"person"},"v2":{"type":"person"}}}}"#,
        r#"{"lyrics":[{"time":0,"duration":1000,"text":"one","element":{"singer":"v1"}},{"time":1000,"duration":1000,"text":"two","element":{"singer":"v2"}},{"time":2000,"duration":1000,"text":"both","element":{"singer":"v1000"}}],"metadata":{"agents":{"v1":{"type":"person"},"v2":{"type":"person"},"v1000":{"type":"group"}}}}"#,
        r#"[{"lyrics":"[00:01.00]first match"},{"lyrics":"[00:01.00]second"}]"#,
        r#"{"message":{"body":{"subtitle":{"subtitle_body":"[{\"text\":\"a\",\"time\":{\"total\":1}}]"}}}}"#,
        r#"{"success":false,"lyrics":"[00:01.00]x"}"#,
        r#"{"ok":false}"#,
        r#"{"error":{"code":1}}"#,
        r#""[00:01.00]just a string""#,
        "[1000,1000](1000,500,0)Hi (1500,500,0)there",
        "<tt><body><p begin=\"1\" end=\"2\">x</p></body></tt>",
        "",
        "not json",
        "{}",
        "[]",
        r#"{"content":[{"timestamp":"1000","endtime":"2000","text":[{"text":"a","part":true,"timestamp":1000,"endtime":1500},{"text":"b","timestamp":1500,"endtime":2000}],"backgroundText":[{"text":"c","timestamp":1600,"endtime":1900}],"oppositeTurn":true}]}"#,
    ]
    .map(String::from)
    .to_vec();
    let base = docs.clone();
    for d in &base {
        let Ok(v) = serde_json::from_str::<Value>(d) else { continue };
        docs.push(stringify_numbers(&v).to_string());
        docs.push(serde_json::json!({ "data": v }).to_string());
        docs.push(serde_json::json!({ "lyrics": d }).to_string());
        docs.push(serde_json::json!([v]).to_string());
        let mut paths = Vec::new();
        keys(&v, &mut Vec::new(), &mut paths);
        for p in paths.iter().take(60) {
            let mut w = v.clone();
            drop_key(&mut w, p);
            docs.push(w.to_string());
        }
    }
    docs
}

fn stringify_numbers(v: &Value) -> Value {
    match v {
        Value::Number(n) => Value::String(n.to_string()),
        Value::Array(a) => Value::Array(a.iter().map(stringify_numbers).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), stringify_numbers(x))).collect()),
        x => x.clone(),
    }
}

fn keys(v: &Value, at: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                at.push(k.clone());
                out.push(at.clone());
                keys(x, at, out);
                at.pop();
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                at.push(i.to_string());
                keys(x, at, out);
                at.pop();
            }
        }
        _ => {}
    }
}

fn drop_key(v: &mut Value, path: &[String]) {
    let (last, head) = path.split_last().unwrap();
    let mut cur = v;
    for p in head {
        cur = match cur {
            Value::Object(o) => o.get_mut(p).unwrap(),
            Value::Array(a) => &mut a[p.parse::<usize>().unwrap()],
            _ => return,
        };
    }
    if let Value::Object(o) = cur {
        o.remove(last);
    }
}

#[test]
fn json_matches_goldens() {
    let mut got = String::new();
    for d in corpus() {
        got.push_str(&format!("## {d}\n"));
        got.push_str(&format!("plus {:?}\n", from_lyricsplus(&d)));
        got.push_str(&format!("provider {:?}\n", from_provider(&d, "Song")));
        got.push_str(&format!("tracks {:?}\n", found_tracks(&d)));
        got.push_str(&format!("yt {:?}\n", youtube_songs(&d)));
        got.push_str(&format!("page {:?}\n", youtube_lyrics_page(&d)));
        got.push_str(&format!("ytm {:?}\n", from_youtube_music(&d)));
        got.push_str(&format!("captions {:?}\n", from_youtube_captions(&d)));
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/golden_json.txt");
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
