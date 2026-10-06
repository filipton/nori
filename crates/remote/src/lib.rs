//! Remote control and jams. One account's devices control each other, and a jam's members ask its host
//! for songs, through one protocol (wire.rs): octo-fiesta's relay hub with rooms (the account's own
//! room, or a jam's), or straight over the LAN through a device's door (lan.rs). What a controlled device
//! admits is device.rs; a jam's members, roles and requests are jam.rs. No I/O but the door's sockets.

pub mod device;
pub mod jam;
pub mod lan;
pub mod wire;

/// A QR code's modules, row by row.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QrCode {
    /// Modules per side.
    pub size: u32,
    pub dark: Vec<bool>,
}

/// `text` as a QR code (medium error correction); None when it is too long for one.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn qr_code(text: String) -> Option<QrCode> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let dark = code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
    Some(QrCode { size: code.width() as u32, dark })
}

/// Where `state`'s position is now, `elapsed_ms` after it was received, within the current song.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn position_now(state: &wire::DeviceState, elapsed_ms: i64) -> i64 {
    if !state.playing {
        return state.position_ms;
    }
    let length = state.entries.iter().find(|e| Some(e.index) == state.index).map_or(i64::MAX, |e| e.duration as i64 * 1000);
    (state.position_ms + elapsed_ms.max(0)).min(length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::{DeviceState, Entry};

    #[test]
    fn invite_fits_a_qr_code() {
        let link = "nori://jam?s=https%3A%2F%2Fmusic.example.com&k=0123456789abcdef0123456789abcdef";
        let q = qr_code(link.into()).unwrap();
        assert_eq!(q.dark.len(), (q.size * q.size) as usize);
        // The three finder patterns' corners are dark.
        let at = |x: u32, y: u32| q.dark[(y * q.size + x) as usize];
        assert!(at(0, 0) && at(q.size - 1, 0) && at(0, q.size - 1));
    }

    #[test]
    fn position_runs_on_while_playing() {
        let entries = vec![Entry { index: 4, duration: 10, ..Default::default() }];
        let st = DeviceState { playing: true, position_ms: 2_000, index: Some(4), entries, ..Default::default() };
        assert_eq!(position_now(&st, 1_500), 3_500);
        assert_eq!(position_now(&st, 60_000), 10_000, "not past the song's end");
        assert_eq!(position_now(&DeviceState { playing: false, ..st }, 1_500), 2_000);
    }
}
