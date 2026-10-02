use super::*;

const SEARCH: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","openSubsonic":true,"searchResult3":{
 "artist":[{"id":"ar1","name":"Pink Floyd","albumCount":2,"starred":"2024-01-01T00:00:00Z"}],
 "album":[{"id":"al1","name":"Animals","artist":"Pink Floyd","songCount":5},{"id":"pl-deezer-9","name":"Mix","isExternal":true}],
 "song":[{"id":"s1","title":"Dogs","artist":"Pink Floyd","album":"Animals","duration":1024,"replayGain":{"trackGain":-6.5},"discNumber":1},
         {"id":"ext-deezer-song-7","title":"Pigs","artist":"Pink Floyd","isExternal":true},
         {"id":42,"title":"Numeric"}]}}}"#;

#[test]
fn search_indexes_skip_external() {
    let core = Core::new(String::new(), "t".into()).unwrap();
    let r = core.parse_search(SEARCH.into()).unwrap();
    core.ingest_search(SEARCH.into()).unwrap();
    assert_eq!(r.songs.len(), 3);
    assert!(r.artists[0].starred);
    assert!(r.songs[1].is_external);
    assert_eq!(r.songs[2].id, "42");
    assert_eq!(r.songs[0].replay_gain.as_ref().unwrap().track_gain, Some(-6.5));

    let size = core.index_size().unwrap();
    assert_eq!((size.artists, size.albums, size.songs), (1, 1, 2));

    let l = core.local_search("pin flo do".into(), 10).unwrap();
    assert_eq!(l.songs.len(), 1);
    assert_eq!(l.songs[0], r.songs[0]);
    assert_eq!(core.local_search("pigs".into(), 10).unwrap().songs.len(), 0);
    assert_eq!(core.local_search("\"' OR *".into(), 10).unwrap().songs.len(), 0);

    // same page again changes nothing but still reports what it saw
    assert_eq!(core.ingest_search(SEARCH.into()).unwrap().songs, 3);
    assert_eq!(core.index_size().unwrap().songs, 2);
}

#[test]
fn queue_and_cache_round_trip() {
    let core = Core::new(String::new(), "t".into()).unwrap();
    let songs = core.parse_search(SEARCH.into()).unwrap().songs;
    core.save_queue(PlayQueue { songs: songs.clone(), index: 1, position_ms: 5000, origin: None }).unwrap();
    let q = core.load_queue().unwrap();
    assert_eq!((q.songs, q.index, q.position_ms), (songs.clone(), 1, 5000));
    core.save_queue(PlayQueue { songs: songs.clone(), index: 99, position_ms: 0, origin: None }).unwrap();
    assert_eq!(core.load_queue().unwrap().index as usize, songs.len() - 1, "an index past the end: the last song");

    core.cache_put("getAlbum?id=1".into(), vec![1, 2]).unwrap();
    core.cache_put("getArtist?id=1".into(), vec![3]).unwrap();
    assert!(core.cache_fresh("getAlbum?id=1".into(), 60_000).unwrap());
    assert!(!core.cache_fresh("getAlbum?id=1".into(), 0).unwrap());
    assert!(!core.cache_fresh("missing".into(), 60_000).unwrap());
    core.cache_evict("getAlbum".into()).unwrap();
    assert_eq!(core.cache_get("getAlbum?id=1".into()).unwrap(), None);
    assert_eq!(core.cache_get("getArtist?id=1".into()).unwrap(), Some(vec![3]));
}

#[test]
fn synced_lyrics_preferred() {
    let l = crate::lyrics::build(crate::parse(r#"{"subsonic-response":{"status":"ok","lyricsList":{"structuredLyrics":[
          {"synced":false,"line":[{"value":"plain"}]},{"synced":true,"line":[{"start":1500,"value":"timed"}]}]}}}"#.as_bytes()).unwrap().lyrics_list.unwrap_or_default().structured_lyrics);
    assert!(l.synced);
    assert_eq!((l.lines[0].start_ms, l.lines[0].text.as_str(), l.lines[0].words.len()), (1500, "timed", 1));
}

#[test]
fn a_device_is_bound_to_one_profile() {
    let core = Core::new(String::new(), "t".into()).unwrap();
    let p = |name: &str, outputs: &[&str]| SoundProfile { name: name.into(), json: "{}".into(), outputs: outputs.iter().map(|s| s.to_string()).collect() };
    core.profile_save(p("IEM", &["USB: DAC", "Wired headphones"])).unwrap();
    core.profile_save(p("Flat", &[])).unwrap();
    core.profile_bind("USB: DAC".into(), Some("Flat".into())).unwrap();
    assert_eq!(core.profile_for_output("USB: DAC".into()).unwrap().unwrap().name, "Flat");
    assert_eq!(core.profile_for_output("Wired headphones".into()).unwrap().unwrap().name, "IEM", "other devices keep theirs");
    core.profile_bind("USB: DAC".into(), None).unwrap();
    assert_eq!(core.profile_for_output("USB: DAC".into()).unwrap(), None);
    core.profile_bind("USB: DAC".into(), Some("IEM".into())).unwrap();
    core.profile_bind("USB: DAC".into(), Some("IEM".into())).unwrap();
    let iem = core.profiles().unwrap().into_iter().find(|p| p.name == "IEM").unwrap();
    assert_eq!(iem.outputs, vec!["Wired headphones".to_string(), "USB: DAC".to_string()], "bound once, not twice");
}

#[test]
fn autoeq_preset_is_read() {
    let p = parse_eq_preset("Preamp: -6.2 dB\nFilter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70\nFilter 2: OFF PK Fc 500 Hz Gain 2 dB Q 1\nFilter 3: ON HSC Fc 10000 Hz Gain 4.0 dB Q 0.7\nnonsense".into());
    assert_eq!(p.preamp_db, -6.2);
    assert_eq!(p.bands.len(), 2);
    assert_eq!(p.bands[0], EqBand { kind: EqKind::Peaking, freq: 105.0, gain_db: -3.5, q: 0.70 });
    assert_eq!(p.bands[1].kind, EqKind::HighShelf);

    let extra = parse_eq_preset("Filter 1: ON HP Fc 30 Hz Q 0.70\nFilter 2: ON NO Fc 8000 Hz Q 4\nFilter 3: ON PK Fc 100 Hz Q 1".into());
    assert_eq!(extra.bands.len(), 2, "a peaking line without a gain is not a filter");
    assert_eq!((extra.bands[0].kind, extra.bands[1].kind), (EqKind::HighPass, EqKind::Notch));
}

#[test]
fn browse_sorts_filters_decades() {
    let core = Core::new(String::new(), "t".into()).unwrap();
    core.ingest_search(r#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[
      {"id":"a","title":"beta","year":1994,"starred":"2020-01-01"},{"id":"b","title":"Alpha","year":2003},{"id":"c","title":"gamma","year":1999}]}}}"#.into()).unwrap();
    let by_title: Vec<String> = core.browse_songs("title".into(), false, false, 0, 0, 0, 10).unwrap().into_iter().map(|s| s.title).collect();
    assert_eq!(by_title, ["Alpha", "beta", "gamma"]);
    assert_eq!(core.browse_songs("year".into(), true, false, 1990, 1999, 0, 10).unwrap().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["c", "a"]);
    assert_eq!(core.browse_songs("title".into(), false, true, 0, 0, 0, 10).unwrap().len(), 1);
    let d = core.browse_decades().unwrap();
    assert_eq!((d[0].start, d[0].song_count, d[1].start, d[1].song_count), (2000, 1, 1990, 2));
}
