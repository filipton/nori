//! The jam this computer hosts or is a guest in, as app.slint's `Jam` global shows it: who joined, what
//! was asked for, the player's strip and the invite's QR code; and for a guest, the host's queue. The core
//! keeps the jam; this only words and draws it.

use std::collections::HashSet;

use nori_core::remote::wire::{DeviceState, Entry, Role};
use nori_core::remote::JamView;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::{words, JamAsk, JamPerson};

/// What the panels show of a jam.
pub struct Shown {
    pub people: Vec<JamPerson>,
    /// The host's: every request waiting; a guest's: its own.
    pub asks: Vec<JamAsk>,
    pub strip: String,
    pub listening: String,
    /// The host's name.
    pub host: String,
    /// The songs a guest asked for that wait for the host.
    pub asked: HashSet<String>,
}

pub fn shown(v: &JamView) -> Shown {
    let host = v.host().to_string();
    let people: Vec<JamPerson> = v
        .listeners()
        .map(|m| JamPerson {
            id: m.id.as_str().into(),
            name: m.name.as_str().into(),
            initial: m.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
            role: words::jam_role(m.role).into(),
            admin: m.role == Role::Admin,
        })
        .collect();
    let asks = v
        .asks()
        .map(|p| JamAsk {
            request: p.request.to_string().into(),
            title: p.song.title.as_str().into(),
            line: if v.hosting { words::jam_asked(&p.from_name) } else { words::jam_waiting(&host) }.into(),
            note: if p.provider && v.hosting { words::JAM_DOWNLOADS.into() } else { Default::default() },
            art: p.song.cover_art.clone().unwrap_or_default().into(),
        })
        .collect();
    let asked = if v.hosting { HashSet::new() } else { v.asks().map(|p| p.song.id.clone()).collect() };
    let strip = if v.hosting { words::jam_strip(people.len()) } else { words::jam_guest_strip(&host, people.len()) };
    Shown { strip, listening: words::jam_listening(people.len()), people, asks, host, asked }
}

/// The song the host plays, in its published state.
pub fn playing(st: &DeviceState) -> Option<&Entry> {
    st.entries.iter().find(|e| Some(e.index) == st.index)
}

/// The songs the host plays after the current one, in play order, as far as its state lists them.
pub fn upcoming(st: &DeviceState) -> impl Iterator<Item = &Entry> {
    let after = playing(st).map(|e| e.turn);
    st.entries.iter().filter(move |e| after.is_none_or(|t| e.turn > t))
}

/// The invite link as a QR code, a pixel a module (app.slint scales it up unsmoothed).
pub fn qr(link: &str) -> Image {
    let Some(code) = nori_core::remote::qr_code(link.to_string()) else { return Image::default() };
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(code.size, code.size);
    for (px, dark) in buf.make_mut_slice().iter_mut().zip(&code.dark) {
        let v = if *dark { 0 } else { 255 };
        *px = Rgba8Pixel { r: v, g: v, b: v, a: 255 };
    }
    Image::from_rgba8(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_core::remote::wire::{JamMember, Pending};

    fn member(id: &str, name: &str, role: Role) -> JamMember {
        JamMember { id: id.into(), name: name.into(), role }
    }

    #[test]
    fn the_host_sees_who_joined_and_what_they_asked_for() {
        let ask = |request, from: &str, title: &str, provider| Pending {
            request,
            from: from.to_lowercase(),
            from_name: from.into(),
            song: Entry { title: title.into(), cover_art: Some(format!("al-{title}")), ..Default::default() },
            provider,
        };
        let v = JamView {
            hosting: true,
            link: Some("nori://jam?s=x&k=y".into()),
            you: "me".into(),
            members: vec![member("me", "Desk", Role::Host), member("gus", "gus", Role::Guest), member("dee", "Dee", Role::Admin)],
            pending: vec![ask(7, "Gus", "Wish", true), ask(12_000_000_000, "Dee", "Blue", false)],
            queue: None,
            age_ms: 0,
            refused: None,
            along: true,
            listening: nori_core::remote::Listening::Watching,
        };
        let s = shown(&v);
        let people: Vec<(&str, &str, bool)> = s.people.iter().map(|p| (p.name.as_str(), p.initial.as_str(), p.admin)).collect();
        assert_eq!(people, [("gus", "G", false), ("Dee", "D", true)], "the host is not among the listeners");
        assert_eq!((s.strip.as_str(), s.listening.as_str()), ("Jam · 2 listening", "2 listening"));
        let asks: Vec<(&str, &str, &str, bool)> = s.asks.iter().map(|a| (a.request.as_str(), a.title.as_str(), a.line.as_str(), !a.note.is_empty())).collect();
        assert_eq!(asks, [("7", "Wish", "Asked by Gus", true), ("12000000000", "Blue", "Asked by Dee", false)], "only the provider's song says it is downloaded");
        assert_eq!(s.asks[0].art, "al-Wish");
    }

    #[test]
    fn a_guest_sees_the_host_its_own_requests_and_what_plays_next() {
        let ask = |request, from: &str, id: &str| Pending {
            request,
            from: from.to_lowercase(),
            from_name: from.into(),
            song: Entry { id: id.into(), title: id.to_uppercase(), ..Default::default() },
            provider: true,
        };
        let entry = |index, turn, id: &str, by: Option<&str>| Entry { index, turn, id: id.into(), by: by.map(Into::into), ..Default::default() };
        let queue = DeviceState { index: Some(4), entries: vec![entry(3, 0, "a", None), entry(4, 1, "b", None), entry(0, 2, "c", Some("Gus")), entry(1, 3, "d", None)], ..Default::default() };
        let v = JamView {
            hosting: false,
            link: None,
            you: "gus".into(),
            members: vec![member("desk", "Desk", Role::Host), member("gus", "Gus", Role::Guest), member("dee", "Dee", Role::Guest)],
            pending: vec![ask(1, "Gus", "x"), ask(2, "Dee", "y")],
            queue: Some(queue.clone()),
            age_ms: 0,
            refused: None,
            along: false,
            listening: nori_core::remote::Listening::Watching,
        };
        let s = shown(&v);
        assert_eq!((s.host.as_str(), s.strip.as_str()), ("Desk", "Jam · Desk · 2 listening"));
        let asks: Vec<(&str, &str, bool)> = s.asks.iter().map(|a| (a.title.as_str(), a.line.as_str(), a.note.is_empty())).collect();
        assert_eq!(asks, [("X", "Waiting for Desk", true)], "only its own, with nothing to accept");
        assert_eq!(s.asked, HashSet::from(["x".to_string()]));
        assert_eq!(playing(&queue).map(|e| e.id.as_str()), Some("b"));
        let next: Vec<(&str, Option<&str>)> = upcoming(&queue).map(|e| (e.id.as_str(), e.by.as_deref())).collect();
        assert_eq!(next, [("c", Some("Gus")), ("d", None)]);
    }

    #[test]
    fn the_invite_code_is_drawn_module_for_module() {
        let link = "http://octo:5274/nori/jam#s=http%3A%2F%2Focto%3A5274&k=c1a952e53165572a6349ca04fa2e3b8a";
        let code = nori_core::remote::qr_code(link.into()).unwrap();
        let image = qr(link);
        assert_eq!((image.size().width, image.size().height), (code.size, code.size));
        let buf = image.to_rgba8().unwrap();
        let dark: Vec<bool> = buf.as_slice().iter().map(|p| p.r == 0).collect();
        assert_eq!(dark, code.dark);
    }
}
