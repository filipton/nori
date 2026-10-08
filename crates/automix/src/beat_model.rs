//! The Beat This! small0 weights behind "Better beat detection": where the file lives and how its download
//! stands. The core fetches the authors' checkpoint, checks it, converts it to the weights file the graph
//! (`nori_player::automix::weights::GRAPH`) reads, and deletes it when the switch goes off.

use std::path::{Path, PathBuf};

use parking_lot::Mutex;

/// The authors' published checkpoint, its SHA-256 and size.
pub const CHECKPOINT_URL: &str = "https://cloud.cp.jku.at/public.php/dav/files/7ik4RrBKTS273gp/small0.ckpt";
pub const CHECKPOINT_SHA256: &str = "6074be2c4d490c5f6101fcc374a1ec72ae93456e23bb6019783b849f5dc7d47b";
pub const CHECKPOINT_BYTES: u64 = 8_451_101;
/// The weights file made from it (the same bytes on every platform, and from tools/beat-this/export.py): name,
/// SHA-256, size. A new graph or checkpoint gets a new name.
pub const FILE_NAME: &str = "beat-this-small0.weights";
pub const SHA256: &str = "e9349da04b9da4ad41c5e416c71a9471af3a416249e7addef0101b3d569df5a7";
pub const BYTES: u64 = 4_229_216;
/// Download size shown in settings, MB.
pub const SIZE_MB: u32 = 8;

/// A model: its authors' checkpoint, the weights file made from it, and the directory beside the database it is
/// kept in (its own).
#[derive(Debug)]
pub struct Model {
    pub checkpoint_url: &'static str,
    pub checkpoint_sha256: &'static str,
    pub checkpoint_bytes: u64,
    pub dir: &'static str,
    pub file_name: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    /// Download size shown in settings, MB.
    pub size_mb: u32,
}

pub const BEAT_THIS: Model = Model {
    checkpoint_url: CHECKPOINT_URL,
    checkpoint_sha256: CHECKPOINT_SHA256,
    checkpoint_bytes: CHECKPOINT_BYTES,
    dir: "models",
    file_name: FILE_NAME,
    sha256: SHA256,
    bytes: BYTES,
    size_mb: SIZE_MB,
};

/// Where the model's download stands.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Not on the device; fetched the next time AutoMix measures a song.
    Absent,
    /// Wanted, but on mobile data, which the user has not allowed for it.
    WaitingForWifi,
    /// Downloading or converting.
    Downloading,
    Ready,
    Failed(BeatFailure),
}

/// Why the model could not be made (details go to the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[repr(u8)]
pub enum BeatFailure {
    Network,
    /// The checkpoint or the weights made from it did not match their pins.
    WrongFile,
    Storage,
}

struct Kept {
    /// The directory the model is kept in; none before a core is opened on a file.
    dir: Option<PathBuf>,
    state: State,
    /// The switch as last seen, so only turning it off deletes the file.
    on: bool,
}

/// A model's file and its download, for one app: placed by the core at open, followed by the settings'
/// switch, read by the measurer.
pub struct ModelFile {
    pub model: &'static Model,
    kept: Mutex<Kept>,
}

impl ModelFile {
    pub fn new(model: &'static Model) -> ModelFile {
        ModelFile { model, kept: Mutex::new(Kept { dir: None, state: State::Absent, on: false }) }
    }

    /// The model is kept in its directory beside the database at `db_path` (nowhere for an in-memory database).
    pub fn set_home(&self, db_path: &str) {
        let dir = Path::new(db_path).parent().filter(|_| !db_path.is_empty()).map(|p| p.join(self.model.dir));
        let mut k = self.kept.lock();
        // Only a checked file is ever renamed into place, so a file there is ready.
        if k.state != State::Downloading && dir.as_ref().is_some_and(|d| d.join(self.model.file_name).is_file()) {
            k.state = State::Ready;
        }
        k.dir = dir;
    }

    /// The model's directory, when placed.
    pub fn dir(&self) -> Option<PathBuf> {
        self.kept.lock().dir.clone()
    }

    /// Where the weights file is or goes.
    pub fn file(&self) -> Option<PathBuf> {
        self.dir().map(|d| d.join(self.model.file_name))
    }

    /// The weights file, when it is on the device and checked.
    pub fn ready(&self) -> Option<PathBuf> {
        let k = self.kept.lock();
        let f = k.dir.as_ref()?.join(self.model.file_name);
        (k.state == State::Ready && f.is_file()).then_some(f)
    }

    pub fn state(&self) -> State {
        self.kept.lock().state.clone()
    }

    pub fn set_state(&self, s: State) {
        self.kept.lock().state = s;
    }

    /// Claims the download: false while one runs, and after a wrong file until the switch is turned off
    /// and on again (the same address would serve it again).
    pub fn begin_download(&self) -> bool {
        let mut k = self.kept.lock();
        if matches!(k.state, State::Downloading | State::Failed(BeatFailure::WrongFile)) {
            return false;
        }
        k.state = State::Downloading;
        true
    }

    /// Whether the model may be fetched over this network: not over a `metered` one unless `mobile` data is
    /// allowed for it; else it waits for Wi-Fi, still saying why the last download failed if one did.
    pub fn may_fetch(&self, metered: bool, mobile: bool) -> bool {
        let mut k = self.kept.lock();
        if metered && !mobile {
            if !matches!(k.state, State::Failed(_)) {
                k.state = State::WaitingForWifi;
            }
            return false;
        }
        true
    }

    /// The switch changed. Turning it off deletes the model's directory.
    pub fn switched(&self, on: bool) {
        let mut k = self.kept.lock();
        let was = std::mem::replace(&mut k.on, on);
        if was && !on {
            k.state = State::Absent;
            if let Some(dir) = k.dir.clone() {
                drop(k);
                // Off the caller's thread: settings changes come from the UI.
                nori_db::background::run(move || {
                    let _ = std::fs::remove_dir_all(dir);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_downloads_once_and_goes() {
        let dir = nori_testdir::TempDir::new("model");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("models").join(FILE_NAME), b"model").unwrap();
        let m = ModelFile::new(&BEAT_THIS);
        m.set_home(&dir.join("nori.db").to_string_lossy());
        assert_eq!(m.ready(), Some(dir.join("models").join(FILE_NAME)));
        assert_eq!(m.state(), State::Ready);
        m.switched(true);
        m.switched(true);
        assert!(dir.join("models").join(FILE_NAME).is_file());
        m.switched(false);
        nori_db::background::flush();
        assert!(!dir.join("models").exists());
        assert_eq!((m.state(), m.ready()), (State::Absent, None));

        assert!(m.begin_download());
        assert!(!m.begin_download(), "one download at a time");
        m.set_state(State::Failed(BeatFailure::Network));
        assert!(m.begin_download(), "a network failure is tried again");
        m.set_state(State::Failed(BeatFailure::WrongFile));
        assert!(!m.begin_download(), "a wrong file is not fetched again");
        m.switched(true);
        m.switched(false);
        assert!(m.begin_download());
    }

    #[test]
    fn mobile_data_waits_for_wifi() {
        let m = ModelFile::new(&BEAT_THIS);
        assert!(m.may_fetch(false, false) && m.may_fetch(true, true));
        assert!(!m.may_fetch(true, false));
        assert_eq!(m.state(), State::WaitingForWifi);
        m.set_state(State::Failed(BeatFailure::Network));
        assert!(!m.may_fetch(true, false));
        assert_eq!(m.state(), State::Failed(BeatFailure::Network), "the failure still shows");
    }
}
