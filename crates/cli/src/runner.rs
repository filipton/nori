//! The event loop: terminal input, engine events and worker results arrive on one channel; the screen
//! is drawn only after something arrived or a timer (`App::next_wake`) fired. Idle, it blocks.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Instant;

use nori_core::settings::SavedServer;
use nori_core::settings_store;
use nori_engine::Event;
use nori_http::Http;
use ratatui::crossterm::event::{self, Event as TermEvent};
use ratatui_image::picker::Picker;

use crate::app::{App, Cmd, View};
use crate::art::{protocol_name, Art, COVER_PX};
use crate::backend::{own, Msg, Open, Session};
use crate::Options;

/// Forwards terminal input from a dedicated thread.
fn read_input(tx: Sender<Msg>) {
    let _ = std::thread::Builder::new().name("nori-input".into()).spawn(move || {
        let mut replies = Replies::default();
        loop {
        let msg = match event::read() {
            Ok(TermEvent::Key(k)) if replies.swallows(&k) => continue,
            Ok(TermEvent::Key(k)) => Msg::Key(k),
            Ok(TermEvent::Mouse(m)) => Msg::Mouse(m),
            Ok(TermEvent::Paste(p)) => Msg::Paste(p),
            Ok(TermEvent::Resize(..)) => Msg::Resize,
            Ok(TermEvent::FocusGained) => Msg::Focus(true),
            Ok(TermEvent::FocusLost) => Msg::Focus(false),
            Err(_) => return,
        };
        if tx.send(msg).is_err() {
            return;
        }
        }
    });
}

/// Drops terminal query replies that arrive after the query timed out (common over ssh). crossterm reads
/// them (APC `ESC _ … ESC \`, DCS `ESC P … ESC \`, OSC `ESC ] … BEL`) as alt+_ followed by keys that
/// would trigger bindings. A reply is dropped from its opener to its terminator, 300 ms, or 512 keys.
#[derive(Default)]
pub(crate) struct Replies {
    since: Option<(Instant, usize)>,
}

impl Replies {
    pub(crate) fn swallows(&mut self, k: &ratatui::crossterm::event::KeyEvent) -> bool {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        if let Some((at, n)) = &mut self.since {
            *n += 1;
            let over = at.elapsed() > std::time::Duration::from_millis(300) || *n > 512;
            let end = (alt && k.code == KeyCode::Char('\\')) || k.code == KeyCode::Char('\x07') || (k.code == KeyCode::Char('g') && k.modifiers.contains(KeyModifiers::CONTROL));
            if end || over {
                self.since = None;
            }
            return !over || end;
        }
        if alt && matches!(k.code, KeyCode::Char('_' | 'P' | ']')) {
            self.since = Some((Instant::now(), 0));
            return true;
        }
        false
    }
}

/// Graphics query timeout; longer over ssh.
fn query_ms() -> u64 {
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        2500
    } else {
        1000
    }
}

/// Queries the terminal's graphics protocol and cell size (ratatui-image). Returns whether tmux itself
/// keeps the pictures.
///
/// After a timeout the query reader still waits on stdin and would take the next key, so a
/// device-attributes query (always answered) is sent to release it; late replies are drained.
///
/// Under tmux, tmux is queried directly when it renders sixel ([`crate::term::tmux_draws_sixel`]):
/// it then keeps and redraws pictures itself. Otherwise pictures pass through and are resent when the
/// pane returns ([`crate::term::tmux_focus_events`]).
fn query_picker() -> (Picker, bool) {
    use std::io::Write;
    let t0 = Instant::now();
    let query = || {
        let mut o = ratatui_image::picker::cap_parser::QueryStdioOptions::default();
        o.timeout = std::time::Duration::from_millis(query_ms());
        Picker::from_query_stdio_with_options(o).unwrap_or_else(|_| Picker::halfblocks())
    };
    let tmux_sixel = (crate::term::in_tmux() && crate::term::tmux_draws_sixel())
        .then(|| crate::term::not_tmux(query))
        .filter(|p| p.protocol_type() == ratatui_image::picker::ProtocolType::Sixel);
    let kept_by_tmux = tmux_sixel.is_some();
    let picker = tmux_sixel.unwrap_or_else(query);
    if t0.elapsed() >= std::time::Duration::from_millis(query_ms()) {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[c");
        let _ = out.flush();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    while event::poll(std::time::Duration::from_millis(30)).unwrap_or(false) {
        let _ = event::read();
    }
    let by = if kept_by_tmux { " (by tmux)" } else { "" };
    eprintln!("nori: pictures drawn with {:?}{by}, cells {:?} px, in {:?}", picker.protocol_type(), picker.font_size(), t0.elapsed());
    (picker, kept_by_tmux)
}

struct Runner {
    o: Options,
    http: Arc<Http>,
    tx: Sender<Msg>,
    session: Option<Session>,
    art: Option<Art>,
    picker: Option<Picker>,
    /// Pending cover requests; dropping a ticket cancels the fetch.
    tickets: Vec<nori_covers::loader::Ticket>,
    thumb_tickets: Vec<nori_covers::loader::Ticket>,
    /// Id of the audible song last shown.
    heard: Option<String>,
    /// Rewrite the whole screen, pictures included, at the next draw.
    repaint: bool,
    /// Terminal (or tmux pane) focus, from focus reports.
    focused: bool,
    /// A cover arrived while unfocused: repaint when focus returns.
    unseen_cover: bool,
    /// State and song from the engine's latest events, until its status agrees.
    said: Said,
}

/// The engine emits an event before its status reflects it (the status is written at the end of the
/// same wake). Read in between, the stale status would stay on screen when nothing else is due (a
/// pause), so the events win until the status agrees, and the status is re-read shortly after.
#[derive(Default)]
pub(crate) struct Said {
    pub(crate) state: Option<nori_engine::State>,
    pub(crate) song: Option<String>,
    /// When to read the status again, and how many times it was read without agreeing.
    again: Option<Instant>,
    tries: u32,
}

impl Said {
    /// None when the status matches the events; else Some, with the events' song if the status has another.
    pub(crate) fn behind(&self, state: nori_engine::State, id: Option<&str>) -> Option<Option<String>> {
        let song = self.song.as_deref().filter(|x| id != Some(*x));
        if self.state.is_none_or(|x| x == state) && song.is_none() {
            return None;
        }
        Some(song.map(String::from))
    }
}

/// Re-read interval and retry cap for a status that disagrees with the events.
const AGAIN_MS: u64 = 20;
const AGAIN_TRIES: u32 = 50;

pub fn run(o: Options) -> Result<(), String> {
    crate::term::stderr_to(&o.data.join("nori.log"));
    crate::term::hook_panics();
    let db = crate::backend::db_path(&o.data);
    let mut prefs = settings_store::settings_open(db).map_err(|e| format!("the settings: {e}"))?;
    // A server given on the command line is added (or found) and made active.
    if let Some((url, user, password)) = o.login.clone() {
        let found = prefs.servers.iter().find(|s| s.url == url && s.user == user).map(|s| s.id.clone());
        let id = found.unwrap_or_else(|| {
            let s = SavedServer { id: nori_core::settings::new_server_id(), url: url.clone(), user: user.clone(), password: password.clone(), ..Default::default() };
            prefs.servers.push(s.clone());
            s.id
        });
        prefs.servers.iter_mut().filter(|s| s.id == id && !password.is_empty()).for_each(|s| s.password = password.clone());
        prefs.active_server_id = id;
        settings_store::settings_put(prefs.clone());
    }
    let mouse = o.mouse.unwrap_or_else(|| own::flag(own::MOUSE, true));
    let images = o.images.unwrap_or_else(|| own::flag(own::IMAGES, true));
    let mut terminal = crate::term::enter(mouse).map_err(|e| e.to_string())?;
    // Before the input thread starts: the query reply arrives on stdin.
    let (picker, kept_by_tmux) = match images {
        true => {
            let (p, kept) = query_picker();
            (Some(p), kept)
        }
        false => (None, false),
    };
    // Pictures passed through tmux are lost while the pane is hidden: have tmux report its return.
    if picker.as_ref().is_some_and(|p| p.protocol_type() != ratatui_image::picker::ProtocolType::Halfblocks) && !kept_by_tmux {
        crate::term::tmux_focus_events();
    }
    let (tx, rx) = channel();
    read_input(tx.clone());
    let mut app = App::new(prefs.clone());
    app.mouse = mouse;
    app.images = images;
    app.card_covers = own::flag(own::CARD_COVERS, true);
    app.offline = o.offline;
    app.volume = own::number(own::VOLUME, 1.0);
    app.protocol = picker.as_ref().map_or("off", |p| protocol_name(p.protocol_type()));
    app.settings.own.data = o.data.display().to_string();
    app.settings.own.device = own::text(own::DEVICE).unwrap_or_default();
    let art = picker.clone().map(Art::new);
    let mut r = Runner { http: Http::new(), tx, session: None, art, picker, tickets: Vec::new(), thumb_tickets: Vec::new(), heard: None, repaint: false, focused: true, unseen_cover: false, said: Said::default(), o };
    match prefs.servers.iter().find(|s| s.id == prefs.active_server_id).cloned() {
        Some(p) => r.open(&mut app, p),
        None => app.view = View::Login,
    }
    if app.view != View::Login {
        app.go(View::Home);
    }
    let result = r.run(&mut app, &mut terminal, rx);
    if let Some(s) = r.session.take() {
        eprintln!("nori: output underruns this run: {}", s.engine.status().underruns);
        s.close();
    }
    crate::term::leave();
    result
}

impl Runner {
    fn open(&mut self, app: &mut App, profile: SavedServer) {
        if let Some(s) = self.session.take() {
            s.close();
        }
        let name = nori_core::settings::label(&profile.name, &profile.url);
        let o = Open {
            data: &self.o.data,
            http: self.http.clone(),
            profile,
            device: self.o.device.clone(),
            images: app.images,
            offline: self.o.offline,
            mpris: self.o.mpris,
            tx: self.tx.clone(),
        };
        match Session::open(o) {
            Ok(s) => {
                s.check();
                app.server = name;
                app.unreachable = None;
                self.session = Some(s);
                self.heard = None;
                self.said = Said::default();
                // Fresh screens for the new server, keeping client-wide state.
                let prefs = app.prefs.clone();
                let keep = (app.mouse, app.images, app.card_covers, app.volume, app.protocol, app.offline, app.server.clone(), app.settings.own.data.clone());
                *app = App::new(prefs);
                (app.mouse, app.images, app.card_covers, app.volume, app.protocol, app.offline, app.server, app.settings.own.data) = keep;
                self.follow(app);
            }
            Err(e) => {
                app.view = View::Login;
                app.login.error = Some(e);
            }
        }
    }

    fn run(&mut self, app: &mut App, terminal: &mut crate::term::Term, rx: Receiver<Msg>) -> Result<(), String> {
        loop {
            self.carry_out(app);
            if app.quit {
                return Ok(());
            }
            if app.dirty {
                self.follow(app);
                // Run what following asked for (e.g. the new song's cover) now.
                self.carry_out(app);
                if std::mem::take(&mut self.repaint) {
                    crate::term::debug!("the whole screen written again, pictures included");
                    if let Some(a) = &mut self.art {
                        a.resend();
                    }
                    crate::term::repaint(terminal).map_err(|e| e.to_string())?;
                }
                let art = self.art.as_mut();
                terminal.draw(|f| crate::ui::draw(f, app, art)).map_err(|e| e.to_string())?;
                // Request card covers drawing found missing.
                self.carry_out(app);
                crate::term::debug!("drew {:?}: {:?} {:?} at {} ms, song {:?}", app.view, app.now.state, self.heard, app.now.position_ms, app.song.as_ref().map(|s| &s.title));
                app.dirty = false;
            }
            let now = Instant::now();
            let wake = match (app.next_wake(now), self.said.again) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let msg = match wake {
                Some(at) => rx.recv_timeout(at.saturating_duration_since(now)),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match msg {
                Ok(m) => {
                    self.take(app, m);
                    // Drain the queue before drawing, running each message's commands before the next
                    // (a held key steps from where the last one left).
                    while let Ok(m) = rx.try_recv() {
                        self.carry_out(app);
                        self.take(app, m);
                    }
                }
                Err(RecvTimeoutError::Timeout) => app.tick(Instant::now()),
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }

    fn take(&mut self, app: &mut App, m: Msg) {
        crate::term::debug!("took {}", m.brief());
        match &m {
            Msg::Engine(e @ (Event::Song { .. } | Event::State(_) | Event::Looped { .. } | Event::Bridge { .. })) => {
                match e {
                    Event::State(st) => self.said.state = Some(*st),
                    Event::Song { id, .. } | Event::Looped { id, .. } => self.said.song = Some(id.clone()),
                    _ => {}
                }
                if let Some(s) = &self.session {
                    s.desktop_changed();
                    s.followed(e);
                }
            }
            Msg::Cover { art, image, .. } => {
                if let Some(a) = &mut self.art {
                    a.put(art.clone(), image);
                }
                self.unseen_cover |= !self.focused;
            }
            Msg::Focus(on) => {
                self.focused = *on;
                // Under tmux, pictures sent while the pane was hidden were lost.
                if *on && (std::mem::take(&mut self.unseen_cover) || crate::term::in_tmux()) {
                    self.repaint = true;
                }
            }
            Msg::LoggedIn(Ok(p)) => {
                let mut prefs = settings_store::settings_current().unwrap_or_default();
                prefs.servers.retain(|s| !(s.url == p.url && s.user == p.user));
                prefs.servers.push(p.clone());
                prefs.active_server_id = p.id.clone();
                settings_store::settings_put(prefs.clone());
                app.prefs_changed(prefs);
                self.open(app, p.clone());
                app.go(View::Home);
                return;
            }
            _ => {}
        }
        app.handle(m);
    }

    /// Reads engine status and the queue into `app`, copying strings only when the song changed.
    fn follow(&mut self, app: &mut App) {
        let Some(s) = &self.session else { return };
        let said = &self.said;
        let (now, id_changed, agrees) = s.engine.status_with(|st| {
            if let Some(song) = said.behind(st.state, st.id.as_deref()) {
                // Status lags the events: keep the events' state (App::engine) and song, from its start.
                let changed = song.is_some() && song != self.heard;
                return (None, changed.then_some(song), false);
            }
            let changed = st.id.as_deref() != self.heard.as_deref();
            (
                Some(crate::app::Now { state: st.state, position_ms: st.position_ms, at: st.at, speed: st.pace, mixing: st.mixing, buffering: app.now.buffering }),
                changed.then(|| st.id.clone()),
                true,
            )
        });
        let at = Instant::now();
        if agrees {
            self.said = Said::default();
        } else if self.said.tries < AGAIN_TRIES {
            self.said.tries += 1;
            self.said.again = Some(at + std::time::Duration::from_millis(AGAIN_MS));
            crate::term::debug!("the engine's status is behind its events: read again in {AGAIN_MS} ms");
        } else {
            // Give up; the events' state stands until the next event.
            self.said = Said::default();
        }
        match now {
            Some(now) => app.follow_now(now),
            None if id_changed.is_some() => {
                app.now.position_ms = 0;
                app.now.at = at;
            }
            None => {}
        }
        if let Some(id) = id_changed {
            self.heard = id.clone();
            app.heard(id.and_then(nori_core::queue::queue_song));
        }
        // Refresh the queue copy on change; the engine advancing does not bump `rev`, so compare the index too.
        let (rev, repeat, index) = nori_core::playlist::with(|p| (p.rev(), p.repeat(), p.current().map_or(-1, |c| c as i32)));
        if app.queue.as_ref().is_none_or(|q| q.rev != rev || q.repeat != repeat || q.index != index) {
            let held = app.queue.as_ref().map_or(u64::MAX, |q| q.list_rev);
            let mut v = nori_core::playlist::playlist_view(held);
            if v.songs.is_empty() && v.len > 0 {
                if let Some(q) = &app.queue {
                    v.songs = q.songs.clone();
                }
            }
            app.queue = Some(v);
        }
        if app.transition_shown() {
            if let Some(id) = &self.heard {
                let note = nori_core::automix::planner::transition_note(id);
                if note != app.transition {
                    app.transition = note;
                }
                app.mixed_in = if app.now.mixing { nori_core::automix::planner::transition_into(id) } else { None };
            }
        }
        if app.lyrics_shown() {
            self.lyrics_step(app);
        }
    }

    /// Advances the lyrics clock and schedules its next wake.
    fn lyrics_step(&self, app: &mut App) {
        let Some(l) = &app.lyrics else {
            app.lyrics_wake = None;
            return;
        };
        let now = Instant::now();
        let force = app.lyrics_wake.is_some_and(|t| t <= now);
        let (_, wait) = l.advance(app.now.position(now), app.prefs.lyrics_sweep, force);
        app.lyrics_wake = wait.map(|ms| now + std::time::Duration::from_millis(ms));
    }

    fn carry_out(&mut self, app: &mut App) {
        let cmds = std::mem::take(&mut app.cmds);
        for c in cmds {
            self.carry(app, c);
        }
    }

    fn carry(&mut self, app: &mut App, c: Cmd) {
        crate::term::debug!("carried out {}", c.brief());
        match c {
            Cmd::Quit => {
                app.quit = true;
                return;
            }
            Cmd::Mouse(on) => {
                crate::term::set_mouse(on);
                own::keep(own::MOUSE, on.to_string());
                app.mouse = on;
                app.settings.invalidate();
                return;
            }
            Cmd::Device(name) => {
                own::keep(own::DEVICE, name.clone());
                app.settings.own.device = name.clone();
                app.settings.invalidate();
                let said = if name.is_empty() { "the system default".to_string() } else { name };
                app.say(format!("Output device: {said}, from the next start"), false);
                return;
            }
            Cmd::CardCovers(on) => {
                own::keep(own::CARD_COVERS, on.to_string());
                app.card_covers = on;
                app.settings.invalidate();
                app.say(if on { "Covers on album cards" } else { "No covers on album cards" }, false);
                return;
            }
            Cmd::Images(on) => {
                app.thumbs_asked.clear();
                own::keep(own::IMAGES, on.to_string());
                app.images = on;
                app.settings.invalidate();
                if on && self.art.is_none() {
                    let picker = self.picker.clone().unwrap_or_else(Picker::halfblocks);
                    app.protocol = protocol_name(picker.protocol_type());
                    self.art = Some(Art::new(picker));
                }
                if !on {
                    self.art = None;
                    self.tickets.clear();
                    self.thumb_tickets.clear();
                }
                app.say(if on { "Covers on (from the next song or page; restart to fetch covers again)" } else { "Covers off" }, false);
                return;
            }
            Cmd::Login(draft) => {
                let (data, http, tx) = (self.o.data.clone(), self.http.clone(), self.tx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::LoggedIn(crate::backend::check_login(&data, http, draft)));
                });
                return;
            }
            Cmd::SwitchServer(id) => {
                let mut prefs = settings_store::settings_current().unwrap_or_default();
                let Some(p) = prefs.servers.iter().find(|s| s.id == id).cloned() else { return };
                prefs.active_server_id = id;
                settings_store::settings_put(prefs.clone());
                app.prefs_changed(prefs);
                self.open(app, p);
                app.go(View::Home);
                return;
            }
            _ => {}
        }
        let Some(s) = &self.session else { return };
        match c {
            Cmd::Load(req) => s.load(req),
            Cmd::Play { songs, start, shuffle, from } => s.play(songs, start, shuffle, from),
            Cmd::PlayFetch(what, shuffle) => s.play_later(what, shuffle),
            Cmd::Enqueue(songs, next) => s.enqueue(songs, next),
            Cmd::EnqueueFetch(what, next) => s.enqueue_later(what, next),
            Cmd::Toggle => {
                // Idle with a restored queue: start it where it was.
                if app.now.state == nori_engine::State::Idle && app.queue.as_ref().is_some_and(|q| q.len > 0) {
                    let at = app.queue.as_ref().map_or(0, |q| q.index.max(0) as usize);
                    s.engine.play_at(at, app.now.position_ms);
                } else {
                    s.engine.toggle();
                }
            }
            Cmd::Next => s.next(),
            Cmd::Previous => {
                s.engine.previous();
            }
            Cmd::Seek(ms) => s.engine.seek(ms),
            Cmd::Volume(v) => {
                s.volume.set(v);
                s.volume_changed(v);
                own::keep(own::VOLUME, v.to_string());
                app.volume = v;
                app.settings.invalidate();
            }
            Cmd::Jump(i) => {
                s.engine.play_at(i, 0);
            }
            Cmd::Remove(i) => s.remove(i),
            Cmd::Restore(id) => s.put_back(&id),
            Cmd::Move(from, to) => s.move_song(from, to),
            Cmd::Shuffle(on) => s.shuffle(on),
            Cmd::Repeat(m) => {
                // Set on the queue first so the screen shows it at once.
                nori_core::playlist::playlist_repeat(m);
                s.repeat(m);
            }
            Cmd::Download(songs) => s.download(songs),
            Cmd::DownloadFetch(what) => s.download_later(what),
            Cmd::DownloadRemove(id) => {
                s.download_remove(&id);
                s.load(crate::backend::Req::Downloads);
            }
            Cmd::Star(kind, id, on) => s.star(kind, id, on),
            Cmd::Setting(name, value) => {
                if s.setting(&name, &value).is_none() {
                    app.say(format!("{name}: not a setting"), true);
                }
                prefs_changed(app);
            }
            Cmd::Level(level, v) => sound_edited(s, app, settings_store::edit_level(level, v).map(|(e, _)| e)),
            Cmd::Graphic(i, gain) => sound_edited(s, app, settings_store::edit_graphic(i, gain).map(|(e, _)| e)),
            Cmd::Band(i, band) => sound_edited(s, app, settings_store::edit_band(i, band).map(|(e, _)| e)),
            Cmd::Sound(tool) => {
                let effect = match tool.tool().map(settings_store::settings_sound_tool) {
                    Some(Ok(change)) => change.map(|c| c.effect),
                    Some(Err(e)) => {
                        app.say(format!("{e:?}"), true);
                        None
                    }
                    None => None,
                };
                sound_edited(s, app, effect);
            }
            Cmd::Action(c) => s.action(c),
            Cmd::Tuning(on) => s.engine.set_tuning(on),
            Cmd::SearchTyped(text) => {
                let v = s.search_typed(&text);
                app.search.view = Some(v);
            }
            Cmd::SearchServer(q) => {
                let _ = s.core.search_remember_recent(q.clone());
                s.search_server(q);
            }
            Cmd::Lyrics(id) => s.lyrics(id),
            Cmd::Cover { art, colours } => {
                if self.art.as_ref().is_some_and(|a| a.has(&art)) && !colours {
                    return;
                }
                if let Some(t) = s.cover(art, COVER_PX, colours) {
                    self.tickets.push(t);
                    if self.tickets.len() > 4 {
                        drop(self.tickets.remove(0));
                    }
                }
            }
            Cmd::Thumb(art) => {
                if let Some(t) = s.thumb(art, crate::art::THUMB_PX) {
                    self.thumb_tickets.push(t);
                    if self.thumb_tickets.len() > 150 {
                        drop(self.thumb_tickets.remove(0));
                    }
                }
            }
            Cmd::Quit | Cmd::Mouse(_) | Cmd::Images(_) | Cmd::CardCovers(_) | Cmd::Device(_) | Cmd::Login(_) | Cmd::SwitchServer(_) => {}
        }
    }

}

/// After a sound edit: applies its effect (None: nothing changed), then may take the low-latency
/// buffer; the engine runs commands in order, so it sees the new sound first.
fn sound_edited(s: &Session, app: &mut App, effect: Option<u32>) {
    if let Some(effect) = effect {
        s.applied(effect);
        if app.sound_edited() {
            s.engine.set_tuning(true);
        }
    }
    prefs_changed(app);
}

fn prefs_changed(app: &mut App) {
    if let Some(p) = settings_store::settings_current() {
        app.prefs_changed(p);
    }
}
