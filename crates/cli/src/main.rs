//! nori's terminal client: a full-screen player (ratatui over crossterm), or with `--script` (or
//! `--search`, `--play`, `--wav`, `--devices`) a non-interactive player for scripts and renders.

mod app;
mod art;
mod backend;
mod keys;
mod lyrics;
mod runner;
mod script;
mod settings_view;
mod term;
mod text;
mod ui;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

/// Command-line options of the full-screen client.
pub struct Options {
    /// Database, covers, downloads and stream cache directory.
    pub data: PathBuf,
    /// Output device by name, as `--devices` lists them.
    pub device: Option<String>,
    /// Covers on or off; None uses the stored setting.
    pub images: Option<bool>,
    /// Mouse capture on or off; None uses the stored setting.
    pub mouse: Option<bool>,
    /// (url, user, password) of a server to add and use.
    pub login: Option<(String, String, String)>,
    /// Plays downloads only; no network requests.
    pub offline: bool,
    /// Serve MPRIS media controls.
    pub mpris: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: nori-cli [--data DIR] [--device NAME] [--no-images] [--no-mouse] [--no-mpris] [--offline] [--debug]\n\
         \x20               [--url URL --user USER --password PASSWORD]\n\
         \x20      nori-cli --script ... (the non-interactive player; nori-cli --script --help)\n\
         \x20      nori-cli --devices\n\
         The url, user and password may also come from NORI_URL, NORI_USER and NORI_PASSWORD.\n\
         --debug (or NORI_DEBUG=1) writes every event, frame and picture sent to nori.log in the data directory."
    );
    std::process::exit(2)
}

/// $XDG_DATA_HOME/nori, else ~/.local/share/nori.
fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("nori");
    }
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h).join(".local/share/nori"),
        None => std::env::temp_dir().join("nori"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let script = args.iter().any(|a| matches!(a.as_str(), "--script" | "--search" | "--play" | "--wav" | "--devices" | "--songs" | "--download"));
    if script {
        let rest = args.into_iter().filter(|a| a != "--script").collect();
        script::main(rest);
        return;
    }
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut o = Options { data: data_dir(), device: None, images: None, mouse: None, login: None, offline: false, mpris: true };
    let (mut url, mut user, mut password) = (env("NORI_URL"), env("NORI_USER"), env("NORI_PASSWORD"));
    if env("NORI_DEBUG").is_some_and(|v| v != "0") {
        term::set_debug(true);
    }
    let mut it = args.into_iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().unwrap_or_else(|| usage());
        match k.as_str() {
            "--data" => o.data = v().into(),
            "--device" => o.device = Some(v()),
            "--no-images" => o.images = Some(false),
            "--images" => o.images = Some(true),
            "--no-mouse" => o.mouse = Some(false),
            "--mouse" => o.mouse = Some(true),
            "--no-mpris" => o.mpris = false,
            "--offline" => o.offline = true,
            "--debug" => term::set_debug(true),
            "--url" => url = Some(v()),
            "--user" => user = Some(v()),
            "--password" => password = Some(v()),
            _ => usage(),
        }
    }
    if let (Some(u), Some(n)) = (url, user) {
        o.login = Some((u, n, password.unwrap_or_default()));
    }
    if let Err(e) = std::fs::create_dir_all(&o.data) {
        eprintln!("nori: {}: {e}", o.data.display());
        std::process::exit(1);
    }
    let ran = runner::run(o);
    nori_core::background::flush();
    if let Err(e) = ran {
        eprintln!("nori: {e}");
        std::process::exit(1);
    }
}
