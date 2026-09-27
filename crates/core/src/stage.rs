//! How the app draws its pages and times them, as numbers and small rules every front end shares: how
//! long the waits and fades are, which glyph the transport shows, where the seek bar's time comes from and
//! every gradient's stops (`nori_look::sleeve`). Where things sit on a phone's screen is the phone's. The platform reads [`stage`] once,
//! at start, and asks the rules at the moment something happens (a song changes) - never per frame. How a
//! touch gesture feels (flick speeds, how far a drag turns a record, where the sheet settles) is the
//! platform's own: a desktop or terminal client has other input.

use nori_look::sleeve;

/// One stop of a gradient: where along it (0..1) and how opaque.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct GradientStop {
    pub at: f32,
    pub alpha: f32,
}

fn stops(s: &[sleeve::Stop]) -> Vec<GradientStop> {
    s.iter().map(|s| GradientStop { at: s.at, alpha: s.alpha }).collect()
}

/// Everything the platform lays out and times by, read once.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Stage {
    /// How much of the sleeve's height goes soft at its bottom; the same share its colour is averaged
    /// from (`nori_look::cover`).
    pub melt: f32,
    pub rub_out: Vec<GradientStop>,
    pub soft: Vec<GradientStop>,
    pub soft_from: f32,
    pub soft_to: f32,
    pub status_shade: f32,
    pub status_shade_to: f32,
    pub hero_stops: Vec<f32>,
    pub floor_stops: Vec<f32>,
    pub lyrics_mask: Vec<GradientStop>,

    /// The spinner in Play only comes in after this long buffering: most skips start inside it, and a
    /// spinner flicking into the pause button for a frame made skipping feel rough.
    pub spinner_after_ms: i64,
    /// How long the artwork, the lyrics and the queue take to dissolve into one another.
    pub panel_ms: i32,
    /// How long a song whose cover is not at hand yet keeps the last picture on the sleeve before it fades
    /// to the placeholder: long enough for a cover read from the disk to arrive without the placeholder
    /// blinking in first, short enough that the last song's cover never reads as the new song's.
    pub sleeve_hold_ms: i64,
    /// The same grace for the page's colours: a song whose colours are not worked out yet keeps the last
    /// song's this long, then the page fades to the plain page until its own come (or for good, when it has
    /// no artwork).
    pub colour_wait_ms: i64,
    /// How long the page's colours take to cross-fade to a song's (as long as a record takes to slide).
    pub colour_fade_ms: i32,
    /// How long the lyrics stay where a finger left them.
    pub lyrics_reading_ms: i64,
    /// A sung word's rise takes at least this long, its settling back this long once it is done, and a
    /// note held `lyrics_held_ms` or more glows, fading over `lyrics_glow_fade_ms` after it ends. The
    /// clock draws every frame while any of it moves (`nori_look::lyrics::MOTION_TAIL_MS`).
    pub lyrics_rise_min_ms: i64,
    pub lyrics_settle_ms: i64,
    pub lyrics_held_ms: i64,
    pub lyrics_glow_fade_ms: i64,
    /// How lit the unsung words of the line being filled are (`nori_look::lyrics::UNSUNG`).
    pub lyrics_unsung: f32,
    /// Data that arrives within this long of a page opening was never waited for: it snaps in.
    pub quick_load_ms: i64,
    /// How often the limiter's meter is read while the equalizer is on screen: quick enough to follow a
    /// peak, slow enough that the screen is not redrawn for nothing between them.
    pub meter_ms: i64,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn stage() -> Stage {
    Stage {
        melt: nori_look::cover::MELT,
        rub_out: stops(&sleeve::RUB_OUT),
        soft: stops(&sleeve::SOFT),
        soft_from: sleeve::SOFT_FROM,
        soft_to: sleeve::SOFT_TO,
        status_shade: sleeve::STATUS_SHADE,
        status_shade_to: sleeve::STATUS_SHADE_TO,
        hero_stops: sleeve::HERO_STOPS.to_vec(),
        floor_stops: sleeve::FLOOR_STOPS.to_vec(),
        lyrics_mask: stops(&sleeve::LYRICS_MASK),
        spinner_after_ms: 300,
        panel_ms: 360,
        sleeve_hold_ms: 180,
        colour_wait_ms: 180,
        colour_fade_ms: 420,
        lyrics_reading_ms: nori_look::lyrics::READING_MS,
        lyrics_rise_min_ms: nori_look::lyrics::RISE_MIN_MS,
        lyrics_settle_ms: nori_look::lyrics::SETTLE_MS,
        lyrics_held_ms: nori_look::lyrics::HELD_MS,
        lyrics_glow_fade_ms: nori_look::lyrics::GLOW_FADE_MS,
        lyrics_unsung: nori_look::lyrics::UNSUNG,
        quick_load_ms: 300,
        meter_ms: 120,
    }
}

// ---- the transport -----------------------------------------------------------------------------------------

/// What the play button shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TransportGlyph {
    Play,
    Pause,
    /// Buffering long enough to notice.
    Spinner,
}

/// The play button: a spinner once buffering has lasted `spinner_after_ms` (`waited`); before that the
/// wait is "playing" - the player is going to play, that is what buffering means - and showing Play
/// meanwhile said "paused" for a fraction of a second after every skip.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn transport_glyph(playing: bool, buffering: bool, waited: bool) -> TransportGlyph {
    if waited {
        TransportGlyph::Spinner
    } else if playing || buffering {
        TransportGlyph::Pause
    } else {
        TransportGlyph::Play
    }
}

/// Whether movement is kept to a minimum: the app's own switch, or the system's animations turned off
/// (Developer options, or the accessibility setting some people rely on) unless the listener asked the
/// app to animate regardless.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn motion_reduced(reduce: bool, ignore_system: bool, system_off: bool) -> bool {
    reduce || (system_off && !ignore_system)
}

/// Whether a page that can wear its cover goes black (see `nori_look::sleeve::page_black`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn page_black(amoled: bool, cover_colours: bool) -> bool {
    sleeve::page_black(amoled, cover_colours)
}

/// The sleeve band's colour matrix for the look's band tint (see `nori_look::sleeve::band_matrix`).
/// Asked once per tint and kept.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn band_matrix(strength: f32, kr: f32, kg: f32, kb: f32) -> Vec<f32> {
    sleeve::band_matrix(strength, kr, kg, kb).to_vec()
}

/// Whether the screen stays on for the lyrics.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn lyrics_keep_screen_on(asked: bool, shown: bool, playing: bool) -> bool {
    nori_look::lyrics::keeps_screen_on(asked, shown, playing)
}

// ---- the seek bar ------------------------------------------------------------------------------------------

/// The times either side of the seek bar, in whole seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SeekTimes {
    pub at_s: i64,
    pub left_s: i64,
}

/// The seek bar shows one of three places: the finger's, while it is down (`drag`, a share of the bar);
/// the place a released scrub asked for (`held_ms`, -1 for none) until the player is really there;
/// otherwise the music's. The time left is counted from the same place.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn seek_times(dragging: bool, drag: f32, held_ms: i64, position_ms: i64, duration_ms: i64) -> SeekTimes {
    let d = duration_ms.max(1) as f32;
    let shown = if dragging {
        (drag * d) as i64
    } else if held_ms >= 0 {
        held_ms
    } else {
        position_ms
    };
    SeekTimes { at_s: shown / 1000, left_s: (duration_ms - shown).max(0) / 1000 }
}

// ---- the queue panel ---------------------------------------------------------------------------------------

/// The queue as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QueueRows {
    /// Positions in the queue in the order they will play (under shuffle not the list's own order).
    pub order: Vec<u32>,
    /// A drag moves a song within the list, so reordering is offered only when the two orders are one.
    pub reorderable: bool,
    /// The rows a sideways swipe does not take out (list indexes): the song playing, as the page shows it
    /// and as the queue has it (the two differ while a mix hands over). A swipe is quick and easy to make
    /// by accident, and taking the song playing out cuts the music; the row's × still does it on purpose.
    pub kept: Vec<u32>,
    /// The row (a place in `order`) of the song the page shows playing, -1 for none. The panel opens with
    /// it at the top; the rows before it have played (earlier in the play order, which under shuffle is
    /// not the list's), are drawn dimmed above it, and a drag neither lifts them nor drops a song among them.
    pub now: i32,
}

/// The panel's rows for a queue of `len` songs as the page holds it, in the core's play order, with the
/// song the page shows playing at `shown` (-1 none); when the core's order does not cover the page's queue
/// (the change has not reached it yet) the list's own order stands in.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_rows(len: u32, shuffle: bool, shown: i32) -> QueueRows {
    let (order, current) = crate::playlist::with(|p| {
        let here = p.len() == len as usize;
        (here.then(|| p.play_order().map(|i| i as u32).collect()), p.current().filter(|_| here))
    });
    rows(order, len, shuffle, shown, current)
}

fn rows(order: Option<Vec<u32>>, len: u32, shuffle: bool, shown: i32, current: Option<usize>) -> QueueRows {
    let order = order.filter(|o| o.len() == len as usize).unwrap_or_else(|| (0..len).collect());
    let mut kept: Vec<u32> = u32::try_from(shown).ok().into_iter().chain(current.map(|c| c as u32)).filter(|&i| i < len).collect();
    kept.dedup();
    let now = u32::try_from(shown).ok().and_then(|s| order.iter().position(|&i| i == s)).map_or(-1, |p| p as i32);
    QueueRows { order, reorderable: !shuffle, kept, now }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_shows_pause_while_it_waits_and_spins_only_late() {
        assert_eq!(transport_glyph(false, true, false), TransportGlyph::Pause);
        assert_eq!(transport_glyph(false, true, true), TransportGlyph::Spinner);
        assert_eq!(transport_glyph(true, false, false), TransportGlyph::Pause);
        assert_eq!(transport_glyph(false, false, false), TransportGlyph::Play);
        assert!(motion_reduced(true, true, false));
        assert!(motion_reduced(false, false, true));
        assert!(!motion_reduced(false, true, true));
        assert!(!motion_reduced(false, false, false));
    }

    #[test]
    fn the_seek_bar_reads_finger_then_hold_then_music() {
        assert_eq!(seek_times(true, 0.5, 9_000, 1_000, 200_000), SeekTimes { at_s: 100, left_s: 100 });
        assert_eq!(seek_times(false, 0.5, 9_000, 1_000, 200_000), SeekTimes { at_s: 9, left_s: 191 });
        assert_eq!(seek_times(false, 0.5, -1, 61_500, 200_000), SeekTimes { at_s: 61, left_s: 138 });
        assert_eq!(seek_times(false, 0.0, -1, 5_000, 0), SeekTimes { at_s: 5, left_s: 0 });
    }

    #[test]
    fn the_queue_lists_in_play_order_and_reorders_only_unshuffled() {
        assert_eq!(rows(Some(vec![2, 0, 1]), 3, true, 2, Some(2)), QueueRows { order: vec![2, 0, 1], reorderable: false, kept: vec![2], now: 0 });
        assert_eq!(rows(None, 3, false, -1, None), QueueRows { order: vec![0, 1, 2], reorderable: true, kept: vec![], now: -1 });
    }

    #[test]
    fn what_has_played_is_what_comes_before_the_song_playing_in_play_order() {
        // Unshuffled the list's own order: the two before the third have played.
        assert_eq!(rows(None, 5, false, 2, Some(2)).now, 2);
        // Shuffled, list index 1 plays fourth: the three rows before it have played, list index 4 among them.
        assert_eq!(rows(Some(vec![3, 4, 0, 1, 2]), 5, true, 1, Some(1)).now, 3);
        // Nothing shown playing, or a place past the end: nothing has played.
        assert_eq!(rows(None, 3, false, -1, None).now, -1);
        assert_eq!(rows(None, 3, false, 7, None).now, -1);
    }

    #[test]
    fn a_swipe_leaves_the_song_playing() {
        // The page still on the song a mix is leaving, the queue already on the next: both stay.
        assert_eq!(rows(None, 4, false, 1, Some(2)).kept, [1, 2]);
        assert_eq!(rows(None, 4, false, 3, None).kept, [3], "the page's own, before the core has the queue");
        assert_eq!(rows(None, 2, false, 5, Some(7)).kept, Vec::<u32>::new(), "nothing past the end");
    }
}
