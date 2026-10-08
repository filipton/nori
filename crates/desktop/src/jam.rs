//! The jam this computer hosts, as app.slint's `Jam` global shows it: who joined, what they asked for,
//! the player's strip and the invite's QR code. The core keeps the jam; this only words and draws it.

use nori_core::remote::wire::Role;
use nori_core::remote::JamView;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::{words, JamAsk, JamPerson};

/// What the panels show of a hosted jam.
pub struct Shown {
    pub people: Vec<JamPerson>,
    pub asks: Vec<JamAsk>,
    pub strip: String,
    pub listening: String,
}

pub fn shown(v: &JamView) -> Shown {
    let people: Vec<JamPerson> = v
        .members
        .iter()
        .filter(|m| m.role != Role::Host)
        .map(|m| JamPerson {
            id: m.id.as_str().into(),
            name: m.name.as_str().into(),
            initial: m.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
            role: words::jam_role(m.role).into(),
            admin: m.role == Role::Admin,
        })
        .collect();
    let asks = v
        .pending
        .iter()
        .map(|p| JamAsk {
            request: p.request.to_string().into(),
            title: p.song.title.as_str().into(),
            line: words::jam_asked(&p.from_name).into(),
            note: if p.provider { words::JAM_DOWNLOADS.into() } else { Default::default() },
            art: p.song.cover_art.clone().unwrap_or_default().into(),
        })
        .collect();
    Shown { strip: words::jam_strip(people.len()), listening: words::jam_listening(people.len()), people, asks }
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
    use nori_core::remote::wire::{Entry, JamMember, Pending};

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
    fn the_invite_code_is_drawn_module_for_module() {
        let link = "nori://jam?s=http%3A%2F%2Focto%3A5274&k=c1a952e53165572a6349ca04fa2e3b8a";
        let code = nori_core::remote::qr_code(link.into()).unwrap();
        let image = qr(link);
        assert_eq!((image.size().width, image.size().height), (code.size, code.size));
        let buf = image.to_rgba8().unwrap();
        let dark: Vec<bool> = buf.as_slice().iter().map(|p| p.r == 0).collect();
        assert_eq!(dark, code.dark);
    }
}
