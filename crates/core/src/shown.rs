//! Display facts derived from settings and the queue; clients word them.

/// Whether a heart press shows a confirmation (the favourite-notice setting).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn favourite_notice() -> bool {
    crate::settings_store::with_prefs(|p| p.favourite_notice).unwrap_or(true)
}

/// The media session's extra buttons: a heart and a shuffle toggle.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SessionButtons {
    /// A library song is playing (not nothing or radio).
    pub heart: bool,
    /// The heart is filled.
    pub starred: bool,
    pub shuffling: bool,
}

pub fn session_buttons(song: bool, starred: bool, shuffle: bool) -> SessionButtons {
    SessionButtons { heart: song, starred: song && starred, shuffling: shuffle }
}

/// [`session_buttons`] for the queue's current song.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn session_buttons_now(starred: bool, shuffle: bool) -> SessionButtons {
    let song = crate::playlist::with(|p| p.current_id().is_some_and(|id| !id.starts_with(crate::queue::RADIO_PREFIX)));
    session_buttons(song, starred, shuffle)
}

/// A radio stream's title: the announced ICY title, else the station name.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn radio_title(announced: Option<String>, station: Option<String>) -> Option<String> {
    announced.filter(|a| !a.trim().is_empty()).or(station)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_buttons_need_a_song() {
        assert_eq!(session_buttons(true, true, false), SessionButtons { heart: true, starred: true, shuffling: false });
        assert_eq!(session_buttons(true, false, true), SessionButtons { heart: true, starred: false, shuffling: true });
        let b = session_buttons(false, true, false);
        assert_eq!((b.heart, b.starred), (false, false));
    }

    #[test]
    fn radio_title_prefers_announcement() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(radio_title(s("Artist - Song"), s("FM 4")), s("Artist - Song"));
        assert_eq!(radio_title(s("  "), s("FM 4")), s("FM 4"));
        assert_eq!(radio_title(None, s("FM 4")), s("FM 4"));
        assert_eq!(radio_title(None, None), None);
    }
}
