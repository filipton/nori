//! A page's data laid out once when read: an album's discs, an artist's releases by kind, a list's
//! filter and index letters, the big buttons, and the detail pages.

use nori_model::model::{Album, Artist, DiscTitle, OriginKind, PageOrigin, Playlist, Song};

/// One disc of an album and the positions of its songs.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DiscGroup {
    pub disc: u32,
    /// False for a single-disc album.
    pub headed: bool,
    /// The disc's own name (OpenSubsonic `discTitles`); empty when it has none.
    pub title: String,
    pub songs: Vec<u32>,
    /// Each song's second line, without the album's own artist ([`nori_model::lines::song_line`]).
    pub lines: Vec<String>,
}

/// An album's songs by disc, in disc order (no disc number is disc 1).
pub fn album_discs(album: &Album, songs: &[Song], disc_titles: &[DiscTitle]) -> Vec<DiscGroup> {
    let mut discs: Vec<DiscGroup> = Vec::new();
    for (i, s) in songs.iter().enumerate() {
        let disc = s.disc_number.max(1);
        let line = nori_model::lines::song_line(&s.explicit_status, &s.artist, Some(&album.artist));
        match discs.iter_mut().find(|d| d.disc == disc) {
            Some(d) => {
                d.songs.push(i as u32);
                d.lines.push(line);
            }
            None => discs.push(DiscGroup { disc, headed: false, title: String::new(), songs: vec![i as u32], lines: vec![line] }),
        }
    }
    discs.sort_by_key(|d| d.disc);
    if discs.len() > 1 {
        for d in &mut discs {
            let title = disc_titles.iter().find(|t| t.disc == d.disc).map(|t| t.title.as_str());
            d.headed = true;
            if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
                d.title = t.to_string();
            }
        }
    }
    discs
}

/// A shelf of an artist's page, in page order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[repr(u8)]
pub enum ReleaseKind {
    Album,
    Ep,
    Single,
    Live,
    Compilation,
    Soundtrack,
    Remix,
    Other,
    /// Another type, its shelf named by the server's tag.
    Tagged,
}

/// The known kinds by their capitalised type name.
const RELEASE_NAMES: [(&str, ReleaseKind); 8] = [
    ("Album", ReleaseKind::Album),
    ("EP", ReleaseKind::Ep),
    ("Single", ReleaseKind::Single),
    ("Live", ReleaseKind::Live),
    ("Compilation", ReleaseKind::Compilation),
    ("Soundtrack", ReleaseKind::Soundtrack),
    ("Remix", ReleaseKind::Remix),
    ("Other", ReleaseKind::Other),
];

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// An album's kind: its first known type other than "album", else its first type; without types, a
/// compilation or an album. The capitalised type must read exactly as a kind ("ep" reads "Ep": Tagged).
fn release_kind(a: &Album) -> (ReleaseKind, String) {
    if a.release_types.is_empty() {
        return (if a.is_compilation { ReleaseKind::Compilation } else { ReleaseKind::Album }, String::new());
    }
    let known = a.release_types.iter().find(|t| RELEASE_NAMES.iter().any(|(k, _)| k.eq_ignore_ascii_case(t)) && !t.eq_ignore_ascii_case("Album"));
    let name = capitalised(known.unwrap_or(&a.release_types[0]));
    match RELEASE_NAMES.iter().find(|(k, _)| *k == name) {
        Some((_, kind)) => (*kind, String::new()),
        None => (ReleaseKind::Tagged, name),
    }
}

/// One shelf of an artist's page and the positions of its releases.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ReleaseGroup {
    pub kind: ReleaseKind,
    /// The server's tag for a [`ReleaseKind::Tagged`] shelf; empty for the others.
    pub tag: String,
    pub albums: Vec<u32>,
}

/// An artist's releases by kind in [`ReleaseKind`] order (unknown tags last, as first seen), newest
/// first within each, equal years in the server's order.
pub fn release_groups(albums: &[Album]) -> Vec<ReleaseGroup> {
    let mut order: Vec<usize> = (0..albums.len()).collect();
    order.sort_by(|&a, &b| albums[b].year.cmp(&albums[a].year));
    let mut groups: Vec<ReleaseGroup> = Vec::new();
    for i in order {
        let (kind, tag) = release_kind(&albums[i]);
        match groups.iter_mut().find(|g| g.kind == kind && g.tag == tag) {
            Some(g) => g.albums.push(i as u32),
            None => groups.push(ReleaseGroup { kind, tag, albums: vec![i as u32] }),
        }
    }
    groups.sort_by_key(|g| g.kind);
    groups
}

/// Kotlin's `contains(needle, ignoreCase = true)`, `needle` as chars; allocates nothing.
fn contains_ignoring_case(hay: &str, needle: &[char]) -> bool {
    let same = |a: char, b: char| a == b || upper(a) == upper(b) || lower(a) == lower(b);
    let mut rest = hay;
    loop {
        let mut h = rest.chars();
        if needle.iter().all(|&b| h.next().is_some_and(|a| same(a, b))) {
            return true;
        }
        let mut next = rest.chars();
        if next.next().is_none() {
            return false;
        }
        rest = next.as_str();
    }
}

/// Java's `Character.toUpperCase`: single-character case mapping only ('ß' stays).
fn upper(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

fn lower(c: char) -> char {
    let mut u = c.to_lowercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// A letter down the side of a long list and the first row that starts with it.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct IndexLetter {
    pub letter: String,
    pub row: u32,
}

/// What a filter keeps of a list (positions in the whole list), and its letters.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct IndexView {
    pub rows: Vec<u32>,
    /// Each initial's first kept row, by letter; '#' for a non-letter.
    pub letters: Vec<IndexLetter>,
    /// More than [`INDEX_FROM`] letters.
    pub show_letters: bool,
}

/// A list with this many initials or fewer gets no index.
const INDEX_FROM: usize = 3;

/// A list's text held for the page's life, so a filter sends only what was typed. A row (one or more
/// fields) is kept when any field contains the filter, ignoring case.
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct TextIndex {
    rows: Vec<Vec<String>>,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl TextIndex {
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new(rows: Vec<Vec<String>>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { rows })
    }

    /// A search: rows whose first field contains `query`, then those where only a later one does.
    pub fn ranked(&self, query: String) -> Vec<u32> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let query: Vec<char> = query.chars().collect();
        let hit = |i: usize, first: bool| {
            let f = &self.rows[i];
            let head = f.first().is_some_and(|h| contains_ignoring_case(h, &query));
            if first { head } else { !head && f.iter().skip(1).any(|x| contains_ignoring_case(x, &query)) }
        };
        let n = self.rows.len();
        (0..n).filter(|&i| hit(i, true)).chain((0..n).filter(|&i| hit(i, false))).map(|i| i as u32).collect()
    }

    /// The rows `filter` keeps (all of them for a blank one), and the letters of the first field.
    pub fn view(&self, filter: String) -> IndexView {
        let blank = filter.trim().is_empty();
        let filter: Vec<char> = filter.chars().collect();
        let rows: Vec<u32> = (0..self.rows.len())
            .filter(|&i| blank || self.rows[i].iter().any(|f| contains_ignoring_case(f, &filter)))
            .map(|i| i as u32)
            .collect();
        let mut letters: Vec<(char, u32)> = Vec::new();
        for (at, &i) in rows.iter().enumerate() {
            let first = self.rows[i as usize].first().and_then(|f| f.chars().next());
            let c = first.map(upper).filter(|c| c.is_alphabetic()).unwrap_or('#');
            if !letters.iter().any(|l| l.0 == c) {
                letters.push((c, at as u32));
            }
        }
        letters.sort_by_key(|l| l.0);
        let show_letters = letters.len() > INDEX_FROM;
        IndexView { rows, letters: letters.into_iter().map(|(c, row)| IndexLetter { letter: c.to_string(), row }).collect(), show_letters }
    }
}

/// The origin a queue started from this page carries, held for the page's life. A page is "playing"
/// only when the queue was started from it, not when the song playing merely is one of its songs.
#[derive(Debug)]
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct PageQueue {
    origin: PageOrigin,
}

impl PageQueue {
    pub(crate) fn of(kind: OriginKind, id: &str) -> std::sync::Arc<Self> {
        std::sync::Arc::new(PageQueue { origin: PageOrigin::new(kind, id) })
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl PageQueue {
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new(origin: PageOrigin) -> std::sync::Arc<Self> {
        std::sync::Arc::new(PageQueue { origin })
    }

    pub fn origin(&self) -> PageOrigin {
        self.origin.clone()
    }
}

impl PageQueue {
    pub fn origin_ref(&self) -> &PageOrigin {
        &self.origin
    }
}

/// A page not read yet: an empty id is never playing.
impl Default for PageQueue {
    fn default() -> Self {
        PageQueue { origin: PageOrigin::new(OriginKind::Album, "") }
    }
}

/// What the page's two big buttons press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum HeroPress {
    /// Start this page's songs (played or shuffled).
    Start,
    /// Pause or resume the queue this page started.
    Toggle,
    /// Turn shuffle off for the queue this page started.
    ShuffleOff,
}

/// A page's Shuffle and Play as they stand.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct HeroButtons {
    /// Shuffle lit: this page's queue is shuffling.
    pub shuffle_lit: bool,
    pub shuffle_enabled: bool,
    pub shuffle_press: HeroPress,
    /// This page's queue sounds: Play is Pause.
    pub pausing: bool,
    pub play_enabled: bool,
    pub play_press: HeroPress,
}

/// The big buttons for the page's own queue (`here`): Play toggles it, Shuffle lights while it shuffles
/// and turns shuffle off. Away from it, both start the page's songs when `can_play` / `can_shuffle`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn hero_buttons(here: bool, shuffle: bool, playing: bool, buffering: bool, can_play: bool, can_shuffle: bool) -> HeroButtons {
    let lit = shuffle && here;
    let pausing = here && (playing || buffering);
    HeroButtons {
        shuffle_lit: lit,
        shuffle_enabled: lit || can_shuffle,
        shuffle_press: if lit { HeroPress::ShuffleOff } else { HeroPress::Start },
        pausing,
        play_enabled: here || can_play,
        play_press: if here { HeroPress::Toggle } else { HeroPress::Start },
    }
}

impl HeroPress {
    fn bits(self) -> i32 {
        match self {
            HeroPress::Start => 0,
            HeroPress::Toggle => 1,
            HeroPress::ShuffleOff => 2,
        }
    }
}

impl HeroButtons {
    /// The buttons packed for JNI (`CoverLook.heroButtons`): bit 0 Shuffle lit, 1 Shuffle enabled, 2
    /// pausing, 3 Play enabled, bits 4-5 Shuffle's press and 6-7 Play's (0 start, 1 toggle, 2 shuffle off).
    pub fn pack(&self) -> i32 {
        self.shuffle_lit as i32
            | (self.shuffle_enabled as i32) << 1
            | (self.pausing as i32) << 2
            | (self.play_enabled as i32) << 3
            | self.shuffle_press.bits() << 4
            | self.play_press.bits() << 6
    }
}

/// The "add to library" offer on a provider's album or playlist page (starring it has octo-fiesta fetch it).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct LibraryOffer {
    /// A playlist, else an album.
    pub playlist: bool,
}

/// The offer for the page `id`; none for the library's own.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn library_offer(id: String, is_external: bool) -> Option<LibraryOffer> {
    let playlist = id.starts_with("pl-");
    if !is_external && !nori_model::is_provider_id(&id) {
        return None;
    }
    Some(LibraryOffer { playlist })
}

/// Whether a list offers a filter: when long, or while one is typed.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn filter_offered(songs: u32, filtering: bool) -> bool {
    songs > FILTER_FROM || filtering
}

const FILTER_FROM: u32 = 12;

/// The similar artists the server can open (last.fm names some it does not have).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn similar_artists(similar: Vec<nori_model::Artist>) -> Vec<nori_model::Artist> {
    similar.into_iter().filter(|a| !a.id.is_empty()).collect()
}

/// A playlist's description as shown: none when off, or when it is Navidrome's import note and
/// `hide_import_notes` is on.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_description(comment: Option<String>, show: bool, hide_import_notes: bool) -> Option<String> {
    let text = comment.filter(|c| !c.trim().is_empty())?;
    if !show || (hide_import_notes && is_import_note(&text)) {
        return None;
    }
    Some(text)
}

/// Exactly Navidrome's `Auto-imported from '<file>'` note.
fn is_import_note(text: &str) -> bool {
    let t = text.trim();
    t.strip_prefix("Auto-imported from '").is_some_and(|rest| rest.ends_with('\'') && !rest.contains('\n'))
}

/// An artist's biography without the link the server appends.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn biography(text: String) -> String {
    match text.find("<a ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// An artist's MusicBrainz page.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn musicbrainz_artist_url(id: String) -> String {
    format!("https://musicbrainz.org/artist/{id}")
}

// ---- the detail pages, laid out once when read ----

/// The summed length of `songs`, in seconds.
pub fn total_seconds(songs: &[Song]) -> u64 {
    songs.iter().map(|s| s.duration as u64).sum()
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AlbumDetail {
    pub album: Album,
    pub songs: Vec<Song>,
    /// Names of discs that have one (OpenSubsonic `discTitles`).
    pub disc_titles: Vec<DiscTitle>,
    /// [`album_discs`].
    pub discs: Vec<DiscGroup>,
    /// The songs' summed length in seconds.
    pub seconds: u64,
    pub queue: std::sync::Arc<PageQueue>,
}

impl AlbumDetail {
    pub fn new(album: Album, songs: Vec<Song>, disc_titles: Vec<DiscTitle>) -> Self {
        let discs = album_discs(&album, &songs, &disc_titles);
        let seconds = total_seconds(&songs);
        let queue = PageQueue::of(OriginKind::Album, &album.id);
        AlbumDetail { album, songs, disc_titles, discs, seconds, queue }
    }
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ArtistDetail {
    pub artist: Artist,
    pub albums: Vec<Album>,
    /// [`release_groups`].
    pub groups: Vec<ReleaseGroup>,
    pub queue: std::sync::Arc<PageQueue>,
}

impl ArtistDetail {
    pub fn new(artist: Artist, mut albums: Vec<Album>) -> Self {
        // On the artist's own page the cards show only the year.
        for a in &mut albums {
            a.subtitle = nori_model::lines::album_subtitle("", a.year);
        }
        let groups = release_groups(&albums);
        let queue = PageQueue::of(OriginKind::Artist, &artist.id);
        ArtistDetail { artist, albums, groups, queue }
    }
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub songs: Vec<Song>,
    /// The songs' summed length in seconds.
    pub seconds: u64,
    pub queue: std::sync::Arc<PageQueue>,
}

impl PlaylistDetail {
    pub fn new(playlist: Playlist, songs: Vec<Song>) -> Self {
        let seconds = total_seconds(&songs);
        let queue = PageQueue::of(OriginKind::Playlist, &playlist.id);
        PlaylistDetail { playlist, songs, seconds, queue }
    }
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Starred {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub songs: Vec<Song>,
    /// Starred songs in the library (a starred provider song is still being fetched).
    pub library_songs: u32,
}

impl Starred {
    pub fn new(artists: Vec<Artist>, albums: Vec<Album>, songs: Vec<Song>) -> Self {
        let library_songs = songs.iter().filter(|s| !s.is_provider()).count() as u32;
        Starred { artists, albums, songs, library_songs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_model::model::{DiscTitle, Song};

    fn song(disc: u32) -> Song {
        Song { disc_number: disc, ..Default::default() }
    }

    #[test]
    fn discs_in_order_with_their_titles() {
        let album = Album { artist: "Björk".into(), ..Default::default() };
        let songs = vec![song(2), song(0), song(1), song(2)];
        let titles = vec![DiscTitle { disc: 2, title: "Bonus".into() }, DiscTitle { disc: 1, title: " ".into() }];
        let g = album_discs(&album, &songs, &titles);
        assert_eq!(g.iter().map(|x| (x.disc, x.headed, x.title.as_str(), x.songs.clone())).collect::<Vec<_>>(), [(1, true, "", vec![1, 2]), (2, true, "Bonus", vec![0, 3])]);
        let one = album_discs(&album, &[song(1), song(1)], &[]);
        assert_eq!((one.len(), one[0].headed), (1, false));
    }

    #[test]
    fn album_rows_omit_album_artist() {
        let album = Album { artist: "Björk".into(), ..Default::default() };
        let by = |artist: &str, explicit: &str| Song { artist: artist.into(), explicit_status: explicit.into(), ..Default::default() };
        let d = AlbumDetail::new(album, vec![by("BJÖRK", ""), by("Björk", "explicit"), by("Thom Yorke", "")], vec![]);
        assert_eq!(d.discs[0].lines, ["", "🅴 ", "Thom Yorke"]);
        assert_eq!(d.seconds, 0);
    }

    #[test]
    fn artist_page_album_years() {
        let artist = Artist { name: "Björk".into(), ..Default::default() };
        let album = |artist: &str, year: u32| Album { artist: artist.into(), year, ..Default::default() };
        let d = ArtistDetail::new(artist, vec![album("björk", 1997), album("Björk & Thom Yorke", 2001), album("Björk", 0)]);
        assert_eq!(d.albums.iter().map(|a| a.subtitle.as_str()).collect::<Vec<_>>(), ["1997", "2001", ""]);
    }

    #[test]
    fn releases_by_kind_newest_first() {
        let a = |year: u32, types: &[&str], comp: bool| Album { year, release_types: types.iter().map(|t| t.to_string()).collect(), is_compilation: comp, ..Default::default() };
        let albums = vec![a(2001, &[], false), a(2010, &["album", "live"], false), a(2005, &["single"], false), a(2003, &["ep"], false), a(2020, &[], false), a(1999, &[], true)];
        let g = release_groups(&albums);
        let got: Vec<(ReleaseKind, &str, Vec<u32>)> = g.iter().map(|x| (x.kind, x.tag.as_str(), x.albums.clone())).collect();
        assert_eq!(
            got,
            [
                (ReleaseKind::Album, "", vec![4, 0]),
                (ReleaseKind::Single, "", vec![2]),
                (ReleaseKind::Live, "", vec![1]),
                (ReleaseKind::Compilation, "", vec![5]),
                (ReleaseKind::Tagged, "Ep", vec![3]),
            ]
        );
        let upper = release_groups(&[a(2003, &["EP"], false), a(2004, &["mixtape"], false)]);
        assert_eq!((upper[0].kind, upper[1].kind, upper[1].tag.as_str()), (ReleaseKind::Ep, ReleaseKind::Tagged, "Mixtape"));
    }

    #[test]
    fn filters_and_letters() {
        let idx = TextIndex::new(vec![vec!["abba".into()], vec!["Björk".into()], vec!["!!!".into()], vec!["Beck".into(), "x".into()], vec!["ßuper".into()]]);
        let all = idx.view("  ".into());
        assert_eq!(all.rows, [0, 1, 2, 3, 4]);
        let l: Vec<(&str, u32)> = all.letters.iter().map(|x| (x.letter.as_str(), x.row)).collect();
        assert_eq!(l, [("#", 2), ("A", 0), ("B", 1), ("ß", 4)]);
        assert!(all.show_letters);
        assert!(!idx.view("b".into()).show_letters, "three letters or fewer");
        assert_eq!(idx.view("BJÖ".into()).rows, [1]);
        assert_eq!(idx.view("X".into()).rows, [3]);
        assert_eq!(biography("Hello <a href=x>more</a>".into()), "Hello ");
    }

    #[test]
    fn page_queue_origin() {
        let album = AlbumDetail::new(Album { id: "al".into(), ..Default::default() }, vec![], vec![]);
        assert_eq!(album.queue.origin(), PageOrigin::new(OriginKind::Album, "al"));
        let artist = ArtistDetail::new(Artist { id: "ar".into(), ..Default::default() }, vec![Album { id: "al".into(), ..Default::default() }]);
        assert_eq!(artist.queue.origin(), PageOrigin::new(OriginKind::Artist, "ar"), "the artist, not its albums");
        let playlist = PlaylistDetail::new(Playlist { id: "pl".into(), ..Default::default() }, vec![]);
        assert_eq!(playlist.queue.origin_ref(), &PageOrigin::new(OriginKind::Playlist, "pl"));
    }

    #[test]
    fn hero_buttons_follow_own_queue() {
        // Another page's queue playing and shuffling: this page shows Play and Shuffle, and both start its own.
        let away = hero_buttons(false, true, true, false, true, true);
        assert_eq!((away.shuffle_lit, away.pausing, away.play_press, away.shuffle_press), (false, false, HeroPress::Start, HeroPress::Start));
        assert!(away.play_enabled && away.shuffle_enabled);
        assert_eq!(away.pack() & 0b100, 0, "Play, not Pause");
        let here = hero_buttons(true, true, false, true, true, true);
        assert_eq!((here.shuffle_lit, here.pausing, here.play_press, here.shuffle_press), (true, true, HeroPress::Toggle, HeroPress::ShuffleOff));
        assert!(hero_buttons(true, false, true, false, true, true).pausing, "playing pauses too");
        let waiting = hero_buttons(false, false, false, false, false, false);
        assert!(!waiting.play_enabled && !waiting.shuffle_enabled);
        assert!(hero_buttons(true, false, false, false, false, false).play_enabled, "its own queue can always be resumed");
        // Lit, enabled, pausing, Play enabled, Shuffle turns shuffle off (2), Play toggles (1).
        assert_eq!(here.pack(), 0b1 | 0b10 | 0b100 | 0b1000 | 2 << 4 | 1 << 6);
        assert_eq!(waiting.pack(), 0);
    }

    #[test]
    fn playlist_description_hides_import_note() {
        let d = |c: &str, show, hide| playlist_description(Some(c.into()), show, hide);
        assert_eq!(d("Late night driving", true, true).as_deref(), Some("Late night driving"));
        assert_eq!(d("Auto-imported from 'Glitch.m3u8'", true, true), None);
        assert_eq!(d("Auto-imported from 'The New New York.m3u8'", true, false).as_deref(), Some("Auto-imported from 'The New New York.m3u8'"));
        assert_eq!(d("Auto-imported from 'x.m3u' plus my own notes", true, true).as_deref(), Some("Auto-imported from 'x.m3u' plus my own notes"));
        assert_eq!(d("Late night driving", false, true), None);
        assert_eq!((d("  ", true, true), playlist_description(None, true, true)), (None, None));
    }

    #[test]
    fn library_offer_and_filter() {
        let offer = |playlist: bool| Some(LibraryOffer { playlist });
        assert_eq!(library_offer("ext-deezer-album-1".into(), true), offer(false));
        assert_eq!(library_offer("pl-deezer-1".into(), false), offer(true));
        assert_eq!(library_offer("al-1".into(), false), None);
        assert_eq!((filter_offered(12, false), filter_offered(13, false), filter_offered(3, true)), (false, true, true));
        let a = |id: &str| nori_model::Artist { id: id.into(), ..Default::default() };
        assert_eq!(similar_artists(vec![a(""), a("x")]).len(), 1);
    }

    #[test]
    fn a_search_puts_names_before_descriptions() {
        let idx = TextIndex::new(vec![
            vec!["Keep albums gapless".into(), "No mixing".into()],
            vec!["AMOLED black".into(), "Pixels off".into()],
            vec!["Crossfade".into(), "Songs fade, gapless otherwise".into()],
            vec!["Gapless".into(), "".into()],
        ]);
        assert_eq!(idx.ranked("gapless".into()), [0, 3, 2]);
        assert_eq!(idx.ranked("OLED".into()), [1]);
        assert!(idx.ranked(" ".into()).is_empty());
    }
}
