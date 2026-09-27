//! Drawing: the tabs, the screen, the player bar and whatever is on top, from the app's state. Every
//! clickable place is recorded as it is drawn (`App::hits`), so a click lands on what the eye sees.
//! Lists draw only the rows that show.

use std::borrow::Cow;
use std::time::Instant;

use nori_core::Song;
use nori_engine::State;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Button, Hit, HomeRow, ListRef, Load, Overlay, Page, Screen, Sel, LIB_TABS, LOGIN_FIELDS, SCREENS};
use crate::art::{Art, Theme};
use crate::keys::{Scope, BINDINGS};
use crate::settings_view::{self, EqRow, Line as SLine, SettingsView};

/// `s` cut to `w` columns, with an ellipsis when it did not fit.
pub fn fit(s: &str, w: usize) -> Cow<'_, str> {
    if s.width() <= w {
        return Cow::Borrowed(s);
    }
    if w == 0 {
        return Cow::Borrowed("");
    }
    let mut out = String::with_capacity(w + 3);
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        used += cw;
        out.push(c);
    }
    out.push('…');
    Cow::Owned(out)
}

/// `left` and `right` on one line of `w` columns, the left cut to make room.
fn spread<'a>(left: Vec<Span<'a>>, right: Span<'a>, w: usize) -> Line<'a> {
    let rw = right.content.width();
    let room = w.saturating_sub(rw + 1);
    let mut used = 0;
    let mut spans = Vec::with_capacity(left.len() + 2);
    for s in left {
        let sw = s.content.width();
        if used + sw <= room {
            used += sw;
            spans.push(s);
        } else {
            let cut = fit(&s.content, room - used).into_owned();
            used += cut.width();
            spans.push(Span::styled(cut, s.style));
            break;
        }
    }
    spans.push(Span::raw(" ".repeat(w.saturating_sub(used + rw))));
    spans.push(right);
    Line::from(spans)
}

pub fn clock(ms: i64) -> String {
    crate::text::duration(ms.max(0) / 1000)
}

/// Readable text on `bg`.
fn on(bg: Color) -> Color {
    match bg {
        Color::Rgb(r, g, b) if (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 140 => Color::Black,
        _ => Color::White,
    }
}

fn selected(t: &Theme, focused: bool) -> Style {
    if focused {
        Style::default().bg(t.accent).fg(on(t.accent)).add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED)
    }
}

/// A list: the rows that show, the selection kept in view, each row recorded for the mouse.
#[allow(clippy::too_many_arguments)]
fn list<'a>(f: &mut Frame, area: Rect, sel: &mut Sel, len: usize, lref: ListRef, hits: &mut Vec<(Rect, Hit)>, t: &Theme, focused: bool, row: &dyn Fn(usize, usize) -> Line<'a>) {
    hits.push((area, Hit::List(lref)));
    let h = area.height as usize;
    sel.fit(h, len);
    // A list longer than its area keeps its last column for where the view is.
    let width = if len > h { area.width.saturating_sub(1) } else { area.width };
    for (n, i) in (sel.top..len.min(sel.top + h)).enumerate() {
        let r = Rect { x: area.x, y: area.y + n as u16, width, height: 1 };
        let mut line = row(i, width as usize);
        if i == sel.at {
            // The selection's colours over every span's own, so a coloured span stays readable on it.
            let st = selected(t, focused);
            line.spans.iter_mut().for_each(|s| s.style = s.style.patch(st));
            line = line.style(st);
        }
        f.render_widget(Paragraph::new(line), r);
        hits.push((r, Hit::Row(lref, i)));
    }
    if len > h && area.width > 2 {
        // Where the view is in the list, one cell on the right.
        let y = area.y + ((sel.top * h) / len.max(1)).min(h - 1) as u16;
        f.render_widget(Paragraph::new(Span::styled("▐", Style::default().fg(t.dim))), Rect { x: area.x + area.width - 1, y, width: 1, height: 1 });
    }
}

fn centred(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

/// A box in the accent colour over whatever is under `r`, titled `title`: the area inside it.
fn popup(f: &mut Frame, r: Rect, title: Span, t: &Theme) -> Rect {
    let block = Block::default().borders(Borders::ALL).title(title).border_style(Style::default().fg(t.accent));
    let inner = block.inner(r);
    f.render_widget(Clear, r);
    f.render_widget(block, r);
    inner
}

fn status_line(f: &mut Frame, area: Rect, text: &str, style: Style) {
    f.render_widget(Paragraph::new(Span::styled(fit(text, area.width as usize).into_owned(), style)), area);
}

fn song_row<'a>(s: &'a Song, w: usize, t: &Theme, number: Option<usize>) -> Line<'a> {
    let mut left = Vec::with_capacity(5);
    if let Some(n) = number {
        left.push(Span::styled(format!("{n:>3}  "), Style::default().fg(t.dim)));
    }
    left.push(Span::raw(s.title.as_str()));
    if !s.artist.is_empty() {
        left.push(Span::styled("  ", Style::default()));
        left.push(Span::styled(s.artist.as_str(), Style::default().fg(t.dim)));
    }
    if crate::backend::is_provider(s) {
        left.push(Span::styled("  ☁ not in library", Style::default().fg(t.dim)));
    }
    spread(left, Span::styled(clock(s.duration as i64 * 1000), Style::default().fg(t.dim)), w)
}

// ---- the frame ----

#[allow(clippy::needless_option_as_deref)]
pub fn draw(f: &mut Frame, app: &mut App, mut art: Option<&mut Art>) {
    app.hits.clear();
    let area = f.area();
    if let Some(page) = app.theme.page.filter(|_| matches!(app.screen, Screen::Playing | Screen::Lyrics)) {
        // The page's own text colour too, so text drawn in the terminal's colour reads on the page.
        f.render_widget(Block::default().style(Style::default().bg(page).fg(app.theme.text)), area);
    }
    if app.screen == Screen::Login {
        login(f, area, app);
        overlay(f, area, app);
        return;
    }
    let [tabs_area, body, bar, status] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)]).areas(area);
    tabs(f, tabs_area, app);
    match app.screen {
        _ if app.page_shown() => page(f, body, app, art.as_deref_mut()),
        Screen::Home => home(f, body, app),
        Screen::Library => library(f, body, app),
        Screen::Search => search(f, body, app),
        Screen::Queue => queue(f, body, app),
        Screen::Playing => playing(f, body, app, art.as_deref_mut()),
        Screen::Lyrics => lyrics(f, body, app),
        Screen::Downloads => downloads(f, body, app),
        Screen::Equalizer => equalizer(f, body, app),
        Screen::Settings => settings(f, body, app),
        Screen::Login => {}
    }
    player_bar(f, bar, app);
    status_bar(f, status, app);
    help_corner(f, status, app);
    overlay(f, area, app);
}

fn tabs(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let mut x = area.x;
    let brand = " nori ";
    f.render_widget(Paragraph::new(Span::styled(brand, Style::default().fg(on(t.accent)).bg(t.accent).add_modifier(Modifier::BOLD))), Rect { x, y: area.y, width: (brand.len() as u16).min(area.width), height: 1 });
    x += brand.len() as u16 + 1;
    // The server's name on the right keeps a little room; the tabs shorten to fit the rest.
    let room = area.width.saturating_sub(brand.len() as u16 + 1 + 16) as usize;
    let full: usize = SCREENS.iter().map(|(_, n)| n.len() + 4).sum();
    let short: usize = SCREENS.iter().map(|(_, n)| n.len().min(4) + 3).sum();
    for (i, (s, name)) in SCREENS.iter().enumerate() {
        let label = if full <= room {
            format!("{} {}", i + 1, name)
        } else if short <= room {
            format!("{}{}", i + 1, &name[..name.len().min(4)])
        } else {
            (i + 1).to_string()
        };
        let w = label.width() as u16 + 2;
        if x + w > area.x + area.width {
            break;
        }
        let style = if *s == app.screen { Style::default().fg(t.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { Style::default().fg(t.dim) };
        let r = Rect { x, y: area.y, width: w, height: 1 };
        f.render_widget(Paragraph::new(Span::styled(format!(" {label} "), style)), r);
        app.hits.push((r, Hit::Tab(i)));
        x += w;
    }
    // Whose server, and whether it is there.
    let (text, style) = if app.offline {
        (format!("offline · {}", app.server), Style::default().fg(Color::Yellow))
    } else if app.unreachable.is_some() {
        (format!("unreachable · {}", app.server), Style::default().fg(Color::LightRed))
    } else {
        (app.server.clone(), Style::default().fg(t.dim))
    };
    let w = (text.width() as u16 + 1).min(area.width.saturating_sub(x - area.x));
    if w > 4 {
        f.render_widget(Paragraph::new(Span::styled(fit(&text, w as usize - 1).into_owned(), style)).alignment(Alignment::Right), Rect { x: area.x + area.width - w, y: area.y, width: w, height: 1 });
    }
}

fn status_bar(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    if let Some((text, error, _)) = &app.note {
        let style = if *error { Style::default().fg(Color::LightRed) } else { Style::default().fg(t.accent) };
        return status_line(f, area, text, style);
    }
    if let Some(e) = &app.unreachable {
        return status_line(f, area, &format!("{e} · showing what is stored · R to try again"), Style::default().fg(Color::LightRed));
    }
    let hint = match app.screen {
        Screen::Queue => "enter play · d remove · K/J move · s shuffle · r repeat · ? help",
        Screen::Lyrics => "↑↓ pick a line · enter play from it · [ ] timing · ? help",
        Screen::Settings | Screen::Equalizer => "↑↓ move · ←→ change · enter switch or open · tab pane · ? help",
        Screen::Search => "/ type · tab pane · enter play or open · a queue · ? help",
        _ => "space play · n/p next/prev · ←→ seek · / search · a queue · x play all · ? help · q quit",
    };
    status_line(f, area, hint, Style::default().fg(t.dim));
}

/// The corner of the status bar that opens the help, as a click.
fn help_corner(f: &mut Frame, area: Rect, app: &mut App) {
    let label = " ? ";
    if area.width < 20 {
        return;
    }
    let r = Rect { x: area.x + area.width - label.len() as u16, y: area.y, width: label.len() as u16, height: 1 };
    f.render_widget(Paragraph::new(Span::styled(label, Style::default().fg(on(app.theme.accent)).bg(app.theme.accent))), r);
    app.hits.push((r, Hit::Button(Button::Help)));
}

fn player_bar(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let block = Block::default().borders(Borders::TOP).border_style(Style::default().fg(t.dim));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let now = Instant::now();
    let [l1, l2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    let glyph = match (app.now.state, app.now.buffering) {
        (_, true) => "⋯",
        (State::Playing, _) => "▶",
        (State::Paused, _) => "⏸",
        _ => "■",
    };
    match &app.song {
        Some(s) => {
            let mut left = vec![
                Span::styled(format!("{glyph} "), Style::default().fg(t.accent)),
                Span::styled(s.title.as_str(), Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {}", s.artist), Style::default().fg(t.text)),
            ];
            if !s.album.is_empty() {
                left.push(Span::styled(format!(" · {}", s.album), Style::default().fg(t.dim)));
            }
            let right = if app.now.mixing {
                Span::styled("◇ mixing", Style::default().fg(t.accent))
            } else {
                Span::raw("")
            };
            f.render_widget(Paragraph::new(spread(left, right, w)), l1);
        }
        None => f.render_widget(Paragraph::new(Span::styled(format!("{glyph} Nothing playing · pick something and press enter"), Style::default().fg(t.dim))), l1),
    }
    // Buttons, the seek bar with its times, the modes and the volume.
    let mut x = l2.x;
    let buttons: [(&str, Button); 3] = [(" ⏮ ", Button::Previous), (if app.now.state == State::Playing { " ⏸ " } else { " ▶ " }, Button::Toggle), (" ⏭ ", Button::Next)];
    for (label, b) in buttons {
        let r = Rect { x, y: l2.y, width: 3, height: 1 };
        if x + 3 > l2.x + l2.width {
            return;
        }
        f.render_widget(Paragraph::new(Span::styled(label, Style::default().fg(t.accent).add_modifier(Modifier::BOLD))), r);
        app.hits.push((r, Hit::Button(b)));
        x += 3;
    }
    x += 1;
    let len = app.song.as_ref().map_or(0, |s| s.duration as i64 * 1000);
    let pos = match app.scrub {
        Some(share) => (share * len as f32) as i64,
        None => app.now.position(now).clamp(0, len.max(0)),
    };
    let left = clock(pos);
    let right = clock(len);
    let shuffle = if app.queue_shuffled() { "⤮" } else { " " };
    let repeat = match app.repeat() {
        1 => "↻1",
        2 => "↻ ",
        _ => "  ",
    };
    let vol = format!(" {:>3}%", (app.volume * 100.0).round() as i32);
    let tail_w = 2 + 3 + 1 + 1 + 2 + vol.width() as u16 + 2;
    let end = l2.x + l2.width;
    let bar_w = end.saturating_sub(x + left.width() as u16 + right.width() as u16 + 2 + tail_w);
    f.render_widget(Paragraph::new(Span::styled(left.as_str(), Style::default().fg(t.dim))), Rect { x, y: l2.y, width: left.width() as u16, height: 1 });
    x += left.width() as u16 + 1;
    if bar_w >= 4 {
        let share = if len > 0 { pos as f32 / len as f32 } else { 0.0 };
        let filled = ((share * bar_w as f32) as u16).min(bar_w.saturating_sub(1));
        let bar = Line::from(vec![
            Span::styled("━".repeat(filled as usize), Style::default().fg(t.accent)),
            Span::styled("●", Style::default().fg(t.accent)),
            Span::styled("─".repeat((bar_w - filled - 1) as usize), Style::default().fg(t.dim)),
        ]);
        let r = Rect { x, y: l2.y, width: bar_w, height: 1 };
        f.render_widget(Paragraph::new(bar), r);
        app.seek_rect = r;
        app.hits.push((r, Hit::Seek));
        x += bar_w + 1;
    }
    f.render_widget(Paragraph::new(Span::styled(right.as_str(), Style::default().fg(t.dim))), Rect { x, y: l2.y, width: right.width() as u16, height: 1 });
    x += right.width() as u16 + 2;
    let mut put = |x: &mut u16, text: &str, style: Style, hit: Option<Button>, hits: &mut Vec<(Rect, Hit)>| {
        let w = (text.width() as u16).min(end.saturating_sub(*x));
        if w == 0 {
            return;
        }
        let r = Rect { x: *x, y: l2.y, width: w, height: 1 };
        f.render_widget(Paragraph::new(Span::styled(text.to_string(), style)), r);
        if let Some(b) = hit {
            hits.push((r, Hit::Button(b)));
        }
        *x += w + 1;
    };
    let lit = Style::default().fg(t.accent);
    put(&mut x, shuffle, lit, Some(Button::Shuffle), &mut app.hits);
    put(&mut x, repeat, lit, Some(Button::Repeat), &mut app.hits);
    put(&mut x, "−", Style::default().fg(t.dim), Some(Button::VolumeDown), &mut app.hits);
    put(&mut x, &vol, Style::default().fg(t.text), None, &mut app.hits);
    put(&mut x, "+", Style::default().fg(t.dim), Some(Button::VolumeUp), &mut app.hits);
}

// ---- screens ----

fn failed(f: &mut Frame, area: Rect, t: &Theme, e: &str) {
    let text = vec![
        Line::from(Span::styled("Could not load this page", Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(e.to_string(), Style::default().fg(t.text))),
        Line::from(""),
        Line::from(Span::styled("R tries again", Style::default().fg(t.dim))),
    ];
    f.render_widget(Paragraph::new(text).alignment(Alignment::Center).wrap(ratatui::widgets::Wrap { trim: true }), centred(area, area.width.min(70), 6));
}

fn loading(f: &mut Frame, area: Rect, t: &Theme) {
    f.render_widget(Paragraph::new(Span::styled("Loading…", Style::default().fg(t.dim))).alignment(Alignment::Center), centred(area, 20, 1));
}

fn empty(f: &mut Frame, area: Rect, t: &Theme, text: &str) {
    f.render_widget(Paragraph::new(Span::styled(text.to_string(), Style::default().fg(t.dim))).alignment(Alignment::Center), centred(area, area.width, 1));
}

fn album_line<'a>(a: &'a nori_core::Album, w: usize, t: &Theme) -> Line<'a> {
    let year = if a.year > 0 { a.year.to_string() } else { String::new() };
    spread(vec![Span::raw(a.name.as_str()), Span::styled(format!("  {}", a.artist), Style::default().fg(t.dim))], Span::styled(year, Style::default().fg(t.dim)), w)
}

fn home(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let App { home, hits, .. } = app;
    let flat = home.flat();
    if flat.is_empty() {
        match &home.error {
            Some(e) => failed(f, area, &t, e),
            None => loading(f, area, &t),
        }
        return;
    }
    let len = flat.len();
    // The selection rests on albums, never on a shelf's title.
    let mut sel = home.sel;
    if matches!(flat.get(sel.at), Some(HomeRow::Title(_))) {
        sel.at += 1;
    }
    let row = |i: usize, w: usize| match &flat[i] {
        HomeRow::Title(title) => Line::from(Span::styled(title.to_string(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))),
        HomeRow::Album(a) => {
            let mut l = album_line(a, w.saturating_sub(2), &t);
            l.spans.insert(0, Span::raw("  "));
            l
        }
    };
    list(f, area, &mut sel, len, ListRef::Home, hits, &t, true, &row);
    drop(flat);
    home.sel = sel;
}

fn library(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let [top, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    let mut x = top.x;
    for (i, name) in LIB_TABS.iter().enumerate() {
        let w = name.width() as u16 + 4;
        let style = if i == app.library.tab { Style::default().fg(on(t.accent)).bg(t.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(t.dim) };
        let r = Rect { x, y: top.y, width: w.min(top.width.saturating_sub(x - top.x)), height: 1 };
        f.render_widget(Paragraph::new(Span::styled(format!("  {name}  "), style)), r);
        app.hits.push((r, Hit::LibTab(i)));
        x += w + 1;
    }
    let App { library: l, hits, .. } = app;
    let tab = l.tab;
    let len = l.len(tab);
    macro_rules! show {
        ($load:expr, $row:expr) => {
            match &$load {
                Load::Ready(v) if v.is_empty() => empty(f, body, &t, "Nothing here yet"),
                Load::Ready(v) => {
                    let row = |i: usize, w: usize| $row(&v[i], w, &t);
                    list(f, body, &mut l.sels[tab], len, ListRef::Library, hits, &t, true, &row);
                }
                Load::Failed(e) => failed(f, body, &t, e),
                _ => loading(f, body, &t),
            }
        };
    }
    match tab {
        0 => show!(l.albums, album_line),
        1 => show!(l.artists, artist_line),
        2 => show!(l.playlists, playlist_line),
        _ => show!(l.songs, plain_song_row),
    }
}

fn artist_line<'a>(a: &'a nori_core::Artist, w: usize, t: &Theme) -> Line<'a> {
    spread(vec![Span::raw(a.name.as_str())], Span::styled(crate::text::albums(a.album_count), Style::default().fg(t.dim)), w)
}

fn playlist_line<'a>(p: &'a nori_core::Playlist, w: usize, t: &Theme) -> Line<'a> {
    spread(vec![Span::raw(p.name.as_str())], Span::styled(crate::text::playlist_line(p), Style::default().fg(t.dim)), w)
}

fn plain_song_row<'a>(s: &'a Song, w: usize, t: &Theme) -> Line<'a> {
    song_row(s, w, t, None)
}

fn search(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let [field, note, body] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let s = &app.search;
    let cursor = if s.editing { "▌" } else { "" };
    let style = if s.editing { Style::default().fg(t.text) } else { Style::default().fg(t.dim) };
    let prompt = Line::from(vec![Span::styled(" / ", Style::default().fg(on(t.accent)).bg(t.accent)), Span::styled(format!(" {}{cursor}", s.text), style)]);
    f.render_widget(Paragraph::new(prompt), field);
    app.hits.push((field, Hit::SearchField));
    let view = s.view.as_ref();
    let line = match view {
        Some(v) if v.error.is_some() => Span::styled(crate::text::search_fallback(v.error.as_ref().and_then(|e| e.reason.as_deref())), Style::default().fg(Color::LightRed)),
        Some(v) if v.searching => Span::styled("Asking the server…", Style::default().fg(t.dim)),
        Some(v) if v.nothing_found => Span::styled("Nothing found", Style::default().fg(t.dim)),
        Some(v) if !v.query.is_empty() => Span::styled(if v.from_server { "From the server" } else { "From the offline index · the server is asked when you pause" }, Style::default().fg(t.dim)),
        _ => Span::styled("Type to search the library", Style::default().fg(t.dim)),
    };
    f.render_widget(Paragraph::new(line), note);
    let App { search: s, hits, .. } = app;
    let Some(r) = s.view.as_ref().and_then(|v| v.shown.as_ref()) else { return };
    let r = r.clone();
    let wide = body.width >= 110;
    let panes: [Rect; 3] = if wide {
        Layout::horizontal([Constraint::Percentage(25), Constraint::Percentage(30), Constraint::Percentage(45)]).areas(body)
    } else {
        Layout::vertical([Constraint::Percentage(25), Constraint::Percentage(30), Constraint::Percentage(45)]).areas(body)
    };
    let titles = ["Artists", "Albums", "Songs"];
    for (p, area) in panes.into_iter().enumerate() {
        let focused = s.pane == p && !s.editing;
        let block = Block::default()
            .borders(if wide { Borders::TOP | Borders::RIGHT } else { Borders::TOP })
            .title(Span::styled(format!(" {} ({}) ", titles[p], s.len(p)), if focused { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(t.dim) }))
            .border_style(Style::default().fg(t.dim));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let len = s.len(p);
        match p {
            0 => list(f, inner, &mut s.sels[0], len, ListRef::Search(0), hits, &t, focused, &|i, w| spread(vec![Span::raw(r.artists[i].name.as_str())], Span::styled(crate::text::albums(r.artists[i].album_count), Style::default().fg(t.dim)), w)),
            1 => list(f, inner, &mut s.sels[1], len, ListRef::Search(1), hits, &t, focused, &|i, w| album_line(&r.albums[i], w, &t)),
            _ => list(f, inner, &mut s.sels[2], len, ListRef::Search(2), hits, &t, focused, &|i, w| song_row(&r.songs[i], w, &t, None)),
        }
    }
}

/// A picture in `area`, or a plate with a note where there is none (yet).
fn cover(f: &mut Frame, area: Rect, art: Option<&mut Art>, key: Option<&str>, t: &Theme) {
    let protocol: Option<&mut StatefulProtocol> = match (art, key) {
        (Some(a), Some(k)) => a.get(k),
        _ => None,
    };
    match protocol {
        Some(p) => {
            let resize = Resize::Scale(Some(image::imageops::FilterType::Triangle));
            let encode = ratatui_image::ResizeEncodeRender::needs_resize(p, &resize, area);
            f.render_stateful_widget(StatefulImage::<StatefulProtocol>::default().resize(resize), area, p);
            forced_width(f, area);
            // Made for this area now: the next frame written sends the picture to the terminal.
            if let Some(rect) = encode {
                match p.last_encoding_result() {
                    Some(Err(e)) => eprintln!("nori: cover {} would not encode for {}x{} cells: {e}", key.unwrap_or_default(), rect.width, rect.height),
                    _ => crate::term::debug!("cover {} encoded for {}x{} cells at {},{}: sent with this frame", key.unwrap_or_default(), rect.width, rect.height, area.x, area.y),
                }
            }
        }
        None => {
            let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(t.dim));
            let inner = block.inner(area);
            f.render_widget(block, area);
            f.render_widget(Paragraph::new(Span::styled("♪", Style::default().fg(t.dim))).alignment(Alignment::Center), centred(inner, 1, 1));
        }
    }
}

/// A graphics protocol's picture is one escape sequence written into the first cell of its area. ratatui
/// 0.30 takes a cell's text width as the columns it covers and skips that many cells when it works out
/// what changed, so a sixel or kitty picture (thousands of characters of escape codes) hid the rest of
/// the frame after it. The cell is marked as one column wide, which is what it is on screen.
fn forced_width(f: &mut Frame, area: Rect) {
    let one = ratatui::buffer::CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN);
    let buf = f.buffer_mut();
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if cell.symbol().starts_with('\x1b') {
                cell.set_diff_option(one);
            }
        }
    }
}

/// A square cover in cells: twice as wide as tall.
fn cover_box(area: Rect, rows: u16) -> Rect {
    let h = rows.min(area.height);
    let w = (h * 2).min(area.width);
    Rect { x: area.x, y: area.y, width: w, height: h }
}

fn page(f: &mut Frame, area: Rect, app: &mut App, art: Option<&mut Art>) {
    let t = app.theme;
    let images = app.images;
    let App { pages, hits, screen, .. } = app;
    let Some((_, p)) = pages.iter_mut().rev().find(|(s, _)| s == screen) else { return };
    let header_h = if images { 10u16.min(area.height / 2) } else { 4 };
    let [head, body] = Layout::vertical([Constraint::Length(header_h), Constraint::Min(1)]).areas(area);
    let (title, sub, caption, art_key): (String, String, String, Option<String>) = match p {
        Page::Album { detail: Load::Ready(d), .. } => (d.album.name.clone(), d.album.artist.clone(), crate::text::album_caption(d), d.album.cover_art.clone()),
        Page::Artist { detail: Load::Ready(d), .. } => (d.artist.name.clone(), crate::text::albums(d.artist.album_count), String::new(), None),
        Page::Playlist { detail: Load::Ready(d), .. } => (d.playlist.name.clone(), d.playlist.owner.clone().unwrap_or_default(), crate::text::playlist_caption(d), None),
        Page::Album { detail: Load::Failed(e), .. } | Page::Artist { detail: Load::Failed(e), .. } | Page::Playlist { detail: Load::Failed(e), .. } => {
            return failed(f, area, &t, e);
        }
        _ => return loading(f, area, &t),
    };
    let mut text_area = head;
    if images && matches!(p, Page::Album { .. }) {
        let c = cover_box(head, header_h.saturating_sub(1));
        cover(f, c, art, art_key.as_deref(), &t);
        text_area = Rect { x: c.x + c.width + 2, width: head.width.saturating_sub(c.width + 2), ..head };
    }
    let w = text_area.width as usize;
    let lines = vec![
        Line::from(Span::styled(fit(&title, w).into_owned(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(fit(&sub, w).into_owned(), Style::default().fg(t.text))),
        Line::from(Span::styled(fit(&caption, w).into_owned(), Style::default().fg(t.dim))),
    ];
    f.render_widget(Paragraph::new(lines), text_area);
    // The page's buttons.
    let y = text_area.y + 3.min(text_area.height.saturating_sub(1));
    let mut x = text_area.x;
    for (label, b) in [("[ ▶ Play ]", Button::PlayAll), ("[ ⤮ Shuffle ]", Button::ShuffleAll), ("[ ‹ Back ]", Button::Back)] {
        let wd = label.width() as u16;
        if x + wd > text_area.x + text_area.width {
            break;
        }
        let r = Rect { x, y, width: wd, height: 1 };
        f.render_widget(Paragraph::new(Span::styled(label, Style::default().fg(t.accent))), r);
        hits.push((r, Hit::Button(b)));
        x += wd + 1;
    }
    match p {
        Page::Album { detail: Load::Ready(d), sel, .. } => {
            let multi = d.discs.len() > 1;
            let songs = &d.songs;
            list(f, body, sel, songs.len(), ListRef::Page, hits, &t, true, &|i, w| {
                let n = if multi { songs[i].disc_number as usize * 100 + songs[i].track as usize } else { songs[i].track as usize };
                song_row(&songs[i], w, &t, Some(if n > 0 { n } else { i + 1 }))
            });
        }
        Page::Playlist { detail: Load::Ready(d), sel, .. } => {
            let songs = &d.songs;
            list(f, body, sel, songs.len(), ListRef::Page, hits, &t, true, &|i, w| song_row(&songs[i], w, &t, Some(i + 1)));
        }
        Page::Artist { detail: Load::Ready(d), sel, .. } => {
            let albums = &d.albums;
            list(f, body, sel, albums.len(), ListRef::Page, hits, &t, true, &|i, w| album_line(&albums[i], w, &t));
        }
        _ => {}
    }
}

fn queue(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let order = app.queue_order();
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let Some(q) = app.queue.as_ref() else { return empty(f, area, &t, "The queue is empty") };
    if q.len == 0 {
        return empty(f, area, &t, "The queue is empty · pick something and press enter, or a to add it");
    }
    let secs: u64 = q.songs.iter().map(|s| s.duration as u64).sum();
    let mut modes = String::new();
    if q.shuffle {
        modes.push_str(" · shuffle");
    }
    match q.repeat {
        1 => modes.push_str(" · repeat one"),
        2 => modes.push_str(" · repeat all"),
        _ => {}
    }
    let head_text = format!("{}{modes}", crate::text::songs_caption(q.len, secs));
    f.render_widget(Paragraph::new(Span::styled(head_text, Style::default().fg(t.dim))), head);
    let App { queue, queue_sel, hits, .. } = app;
    let q = queue.as_ref().expect("checked");
    let current = q.index;
    let by_hand: std::collections::HashSet<u32> = q.queued.iter().copied().collect();
    list(f, body, queue_sel, order.len(), ListRef::Queue, hits, &t, true, &|row, w| {
        let i = order[row];
        let Some(s) = q.songs.get(i) else { return Line::from("") };
        let playing = i as i32 == current;
        let mark = if playing { "▶ " } else if by_hand.contains(&(i as u32)) { "+ " } else { "  " };
        let mut l = song_row(s, w.saturating_sub(2), &t, None);
        l.spans.insert(0, Span::styled(mark, Style::default().fg(t.accent)));
        if playing {
            l = l.style(Style::default().fg(t.accent).add_modifier(Modifier::BOLD));
        }
        l
    });
}

fn playing(f: &mut Frame, area: Rect, app: &mut App, art: Option<&mut Art>) {
    let t = app.theme;
    let Some(song) = app.song.clone() else { return empty(f, area, &t, "Nothing playing") };
    let images = app.images;
    let side = if images { (area.height.saturating_sub(1)).min(area.width / 4) } else { 0 };
    let c = cover_box(Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(1) }, side);
    if images {
        cover(f, c, art, song.cover_art.as_deref(), &t);
    }
    let x = if images { c.x + c.width + 3 } else { area.x + 2 };
    let right = Rect { x, y: area.y + 1, width: (area.x + area.width).saturating_sub(x + 1), height: area.height.saturating_sub(1) };
    let w = right.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(fit(&song.title, w).into_owned(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(fit(&song.artist, w).into_owned(), Style::default().fg(t.text))),
        Line::from(Span::styled(fit(&song.album, w).into_owned(), Style::default().fg(t.dim))),
    ];
    let mut facts = Vec::new();
    if song.year > 0 {
        facts.push(song.year.to_string());
    }
    if let Some(q) = crate::text::quality(&song) {
        facts.push(q);
    }
    lines.push(Line::from(Span::styled(fit(&facts.join(" · "), w).into_owned(), Style::default().fg(t.dim))));
    lines.push(Line::from(""));
    // What the ear hears: the song the engine says, through a mix the one that has become the louder.
    // Through a mix the ear is on the louder song: still the outgoing one, or already the incoming one.
    let from = app.mixed_in.as_ref().and_then(|n| nori_core::queue::queue_song(n.outgoing_id.clone())).map(|s| s.title);
    let heard = match (app.now.state, app.now.mixing, &from) {
        (_, true, Some(from)) => format!("Heard now, mixing in from “{from}”"),
        (_, true, None) => "Heard now, mixing into the next".to_string(),
        (State::Playing, ..) => "Heard now".to_string(),
        (State::Paused, ..) => "Paused".to_string(),
        _ => "Stopped".to_string(),
    };
    lines.push(Line::from(vec![Span::styled("● ", Style::default().fg(t.accent)), Span::styled(fit(&heard, w.saturating_sub(2)).into_owned(), Style::default().fg(t.text))]));
    if let Some(n) = app.mixed_in.as_ref().filter(|_| app.now.mixing) {
        lines.push(Line::from(Span::styled(fit(&format!("  {}", how_mixed(n)), w).into_owned(), Style::default().fg(t.dim))));
    }
    if let Some(n) = &app.transition {
        let next = nori_core::queue::queue_song(n.incoming_id.clone()).map(|s| s.title).unwrap_or_default();
        let how = how_mixed(n);
        lines.push(Line::from(vec![Span::styled("⇢ ", Style::default().fg(t.accent)), Span::styled(fit(&format!("Next: “{next}”"), w.saturating_sub(2)).into_owned(), Style::default().fg(t.text))]));
        lines.push(Line::from(Span::styled(fit(&format!("  {how}"), w).into_owned(), Style::default().fg(t.text))));
        if !n.reason.is_empty() {
            lines.push(Line::from(Span::styled(fit(&format!("  {}", n.reason), w).into_owned(), Style::default().fg(t.dim))));
        }
    } else if app.prefs.auto_mix || app.prefs.crossfade_sec > 0 {
        let what = if app.prefs.auto_mix { "AutoMix" } else { "Crossfade" };
        lines.push(Line::from(Span::styled(format!("⇢ {what} on · the next transition is planned as this song plays"), Style::default().fg(t.dim))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Up next", Style::default().fg(t.accent).add_modifier(Modifier::BOLD))));
    let top = lines.len() as u16;
    f.render_widget(Paragraph::new(lines), right);
    let next_area = Rect { y: right.y + top, height: right.height.saturating_sub(top), ..right };
    let upcoming = app.up_next();
    let App { queue, up_next_sel, hits, .. } = app;
    let Some(q) = queue.as_ref() else { return };
    list(f, next_area, up_next_sel, upcoming.len(), ListRef::UpNext, hits, &t, true, &|row, w| match q.songs.get(upcoming[row]) {
        Some(s) => song_row(s, w, &t, None),
        None => Line::from(""),
    });
}

/// A planned transition in words: its kind, how long, from where, and the incoming song's speed.
fn how_mixed(n: &nori_core::automix::planner::TransitionNote) -> String {
    if n.duration_ms <= 0 {
        return "Gapless".to_string();
    }
    let tempo = if (n.tempo_ratio - 1.0).abs() > 0.001 { format!(", tempo ×{:.3}", n.tempo_ratio) } else { String::new() };
    format!("{}, {:.1} s from {}{tempo}", words_kind(&n.kind), n.duration_ms as f32 / 1000.0, clock(n.start_ms))
}

/// A transition's kind as the planner names it, in words.
fn words_kind(kind: &str) -> &'static str {
    match kind {
        "BeatMatched" => "AutoMix: beat-matched mix",
        "EchoOut" => "AutoMix: echo out",
        "MixRampFade" => "AutoMix: fade",
        "EqualPowerFade" => "Crossfade",
        _ => "Transition",
    }
}

fn lyrics(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let Some(l) = &app.lyrics else {
        let text = match (&app.song, &app.lyrics_for) {
            (None, _) => "Nothing playing",
            (Some(_), Some(_)) => "Looking for lyrics…",
            _ => "No lyrics",
        };
        return empty(f, area, &t, text);
    };
    if l.pick.lyrics.lines.is_empty() {
        return empty(f, area, &t, "No lyrics for this song");
    }
    let frame = l.clock.shown();
    let synced = l.pick.lyrics.synced;
    let sweep = app.prefs.lyrics_sweep && l.clock.timing().sweeps();
    let translate = app.prefs.lyrics_translation;
    let active = frame.active;
    // Once the last line is over (`active` past it) the page stays on it, drawn as sung.
    let focus = app.lyrics_sel.unwrap_or((active.max(0) as usize).min(l.pick.lyrics.lines.len().saturating_sub(1)));
    let credit = l.credit();
    let body = if credit.is_some() { Rect { height: area.height.saturating_sub(1), ..area } } else { area };
    // Each lyric line and what it draws under it: the backing vocals, a translation.
    let extra = |i: usize| -> u16 {
        let line = &l.pick.lyrics.lines[i];
        (!line.backing.is_empty()) as u16 + (translate && line.translation.as_ref().is_some_and(|x| !x.is_empty())) as u16
    };
    // The line in focus sits a third of the way down, as Android's does.
    let anchor = body.y + body.height / 3;
    let mut y = anchor as i32;
    let mut i = focus as i32;
    while i > 0 && y > body.y as i32 {
        i -= 1;
        y -= 1 + extra(i as usize) as i32 + 1;
    }
    let mut rows = Vec::new();
    let w = body.width as usize;
    let mut at = y;
    for (n, line) in l.pick.lyrics.lines.iter().enumerate().skip(i.max(0) as usize) {
        if at >= (body.y + body.height) as i32 {
            break;
        }
        let strength = nori_look::lyrics::line_strength(synced, n as i32, active);
        let lit = t.lit(strength);
        let is_active = synced && n as i32 == active;
        let text = fit(&line.text, w.saturating_sub(4)).into_owned();
        let spans = if is_active && sweep {
            // Word by word: what is sung fully lit, the rest of the line as the clock's unsung words.
            let sung = l.sung_chars(n, frame.sung);
            let cut = text.char_indices().nth(sung).map_or(text.len(), |(b, _)| b);
            vec![
                Span::styled(text[..cut].to_string(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD)),
                Span::styled(text[cut..].to_string(), Style::default().fg(t.lit(nori_look::lyrics::UNSUNG)).add_modifier(Modifier::BOLD)),
            ]
        } else if is_active {
            vec![Span::styled(text, Style::default().fg(t.accent).add_modifier(Modifier::BOLD))]
        } else {
            vec![Span::styled(text, Style::default().fg(lit))]
        };
        let mut row = Line::from(spans).alignment(Alignment::Center);
        if app.lyrics_sel == Some(n) {
            row = row.style(Style::default().add_modifier(Modifier::UNDERLINED));
        }
        rows.push((at, row, n));
        at += 1;
        if !line.backing.is_empty() {
            rows.push((at, Line::from(Span::styled(fit(&format!("({})", line.backing), w).into_owned(), Style::default().fg(t.lit(strength * 0.7)).add_modifier(Modifier::ITALIC))).alignment(Alignment::Center), n));
            at += 1;
        }
        if translate {
            if let Some(tr) = line.translation.as_ref().filter(|x| !x.is_empty()) {
                rows.push((at, Line::from(Span::styled(fit(tr, w).into_owned(), Style::default().fg(t.lit(strength * 0.8)))).alignment(Alignment::Center), n));
                at += 1;
            }
        }
        at += 1;
    }
    app.hits.push((body, Hit::List(ListRef::Lyrics)));
    for (y, row, n) in rows {
        if y < body.y as i32 || y >= (body.y + body.height) as i32 {
            continue;
        }
        let r = Rect { x: body.x, y: y as u16, width: body.width, height: 1 };
        f.render_widget(Paragraph::new(row), r);
        app.hits.push((r, Hit::Row(ListRef::Lyrics, n)));
    }
    if let Some(c) = credit {
        status_line(f, Rect { y: area.y + area.height - 1, height: 1, ..area }, &format!("  {c}"), Style::default().fg(t.dim));
    }
}

fn downloads(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    match &app.downloads {
        Load::Failed(e) => return failed(f, area, &t, &e.clone()),
        Load::Ready(_) => {}
        _ => return loading(f, area, &t),
    }
    let rows = app.download_rows();
    if rows.is_empty() {
        return empty(f, area, &t, "Nothing downloaded · D on a song, an album or a playlist downloads it");
    }
    let lines: Vec<Line> = rows
        .iter()
        .map(|(title, song)| match song {
            None => Line::from(Span::styled(title.clone(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))),
            Some((list, i)) => {
                let s = &list[*i];
                Line::from(vec![Span::raw("  "), Span::raw(s.title.clone()), Span::styled(format!("  {}", s.artist), Style::default().fg(t.dim))])
            }
        })
        .collect();
    let len = lines.len();
    let App { downloads_sel, hits, .. } = app;
    list(f, area, downloads_sel, len, ListRef::Downloads, hits, &t, true, &|i, _| lines[i].clone());
}

fn equalizer(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let p = &app.prefs;
    let bypass = nori_core::settings::eq_bypass(p.hi_res, p.bit_perfect, p.sound_bypass);
    let graph_h = (area.height / 2).clamp(6, 14);
    let [note, graph, rows_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(graph_h), Constraint::Min(3)]).areas(area);
    let head: &str = match bypass {
        Some(why) => crate::text::eq_bypass(why),
        None if p.eq_enabled => "Equalizer on · changes are heard at once while this screen is open",
        None => "Equalizer off · enter on the first row switches it on",
    };
    status_line(f, note, head, Style::default().fg(t.dim));
    let rows = crate::settings_view::eq_rows(p);
    let selected_band = match rows.get(app.eq_sel.at) {
        Some(EqRow::Band(i)) => Some(*i),
        _ => None,
    };
    eq_bars(f, graph, p, selected_band, &t, p.eq_enabled);
    let App { eq_sel, hits, prefs, .. } = app;
    let len = rows.len();
    list(f, rows_area, eq_sel, len, ListRef::Eq, hits, &t, true, &|i, w| {
        let (title, value) = rows[i].words(prefs);
        spread(vec![Span::raw(format!("  {title}"))], Span::styled(value, Style::default().fg(t.accent)), w)
    });
}

/// The bands as bars: up from the 0 dB line for a boost, down for a cut, in eighths of a cell, the one
/// selected in the accent.
fn eq_bars(f: &mut Frame, area: Rect, p: &nori_core::settings::StoredPrefs, selected: Option<usize>, t: &Theme, on_: bool) {
    const UP: [&str; 8] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇"];
    let bands = &p.eq_bands;
    if bands.is_empty() || area.height < 4 || area.width < 12 {
        return;
    }
    // An odd number of rows, so the 0 dB line is in the middle.
    let h = (area.height - 1) | 1;
    let h = if h >= area.height { h - 2 } else { h };
    let mid = (h - 1) / 2;
    let range = nori_core::settings::EQ_RANGES.gain.max;
    let col = ((area.width as usize - 6) / bands.len()).clamp(3, 8) as u16;
    for (y, text) in [(0, format!("+{range:.0}")), (mid, " 0".into()), (h - 1, format!("-{range:.0}"))] {
        f.render_widget(Paragraph::new(Span::styled(text, Style::default().fg(t.dim))), Rect { x: area.x, y: area.y + y, width: 4, height: 1 });
    }
    for (i, b) in bands.iter().enumerate() {
        let x = area.x + 5 + i as u16 * col;
        if x + col > area.x + area.width {
            break;
        }
        let colour = if Some(i) == selected { t.accent } else if on_ { t.text } else { t.dim };
        let cells = (b.gain_db.abs() / range).min(1.0) * mid as f32;
        for y in 0..h {
            let d = (y as i32 - mid as i32).unsigned_abs() as f32;
            let side = (y < mid && b.gain_db > 0.0) || (y > mid && b.gain_db < 0.0);
            let sym = if y == mid {
                "─"
            } else if !side || d - 1.0 >= cells {
                " "
            } else if d <= cells {
                "█"
            } else {
                let frac = cells - (d - 1.0);
                if b.gain_db > 0.0 {
                    UP[((frac * 8.0) as usize).min(7)]
                } else if frac >= 0.5 {
                    "▀"
                } else {
                    "▔"
                }
            };
            let style = if y == mid { Style::default().fg(if Some(i) == selected { t.accent } else { t.dim }) } else { Style::default().fg(colour) };
            f.render_widget(Paragraph::new(Span::styled(sym.repeat((col - 1) as usize), style)), Rect { x, y: area.y + y, width: col - 1, height: 1 });
        }
        let label = crate::text::hz(b.freq);
        f.render_widget(Paragraph::new(Span::styled(fit(&label, col as usize - 1).into_owned(), Style::default().fg(colour))), Rect { x, y: area.y + h, width: col - 1, height: 1 });
    }
}

fn settings(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let prefs = app.prefs.clone();
    let own = &app.settings.own;
    app.settings.own = settings_view::Own { mouse: app.mouse, images: app.images, volume: app.volume, protocol: app.protocol.to_string(), data: own.data.clone(), device: own.device.clone() };
    let [left, right] = Layout::horizontal([Constraint::Length(30.min(area.width / 3)), Constraint::Min(10)]).areas(area);
    let App { settings: v, hits, .. } = app;
    let groups: Vec<&str> = v.groups().iter().map(|g| g.title).collect();
    let pane = v.pane;
    list(f, left, &mut v.group, groups.len(), ListRef::Groups, hits, &t, pane == 0, &|i, w| Line::from(Span::raw(fit(groups[i], w).into_owned())));
    let page = v.page(&prefs).clone();
    let lines = SettingsView::lines(&page);
    let len = lines.len();
    let block = Block::default().borders(Borders::LEFT).border_style(Style::default().fg(t.dim));
    let inner = block.inner(right);
    f.render_widget(block, right);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(1), ..inner };
    if len == 0 {
        return empty(f, inner, &t, "Nothing to set here");
    }
    list(f, inner, &mut v.row, len, ListRef::Rows, hits, &t, pane == 1, &|i, w| setting_line(&lines[i], w, &t));
}

/// One line of a settings page, drawn from its row.
fn setting_line<'a>(line: &SLine<'a>, w: usize, t: &Theme) -> Line<'static> {
    let row = match line {
        SLine::Title(title) => return Line::from(Span::styled(title.to_string(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))),
        SLine::Row(r) => *r,
    };
    let (title, detail) = settings_view::row_words(row);
    let live = settings_view::row_enabled(row);
    let text_style = if live { Style::default().fg(t.text) } else { Style::default().fg(t.dim) };
    if let settings_view::Row::Palette { colours, chosen, .. } = row {
        let mut spans = vec![Span::styled("  Accent colour  ", text_style)];
        for c in colours {
            let colour = crate::art::argb(*c as u32);
            spans.push(Span::styled(if c == chosen { "[●]" } else { " ● " }, Style::default().fg(colour)));
        }
        return Line::from(spans);
    }
    if let Some((share, centred)) = settings_view::slider_share(row) {
        let bar_w = (w / 3).clamp(8, 30);
        let at = (share * (bar_w - 1) as f32).round() as usize;
        let mut bar = String::with_capacity(bar_w * 3);
        for i in 0..bar_w {
            bar.push(if i == at {
                '●'
            } else if centred && i == bar_w / 2 {
                '┼'
            } else if i < at {
                '━'
            } else {
                '─'
            });
        }
        return spread(vec![Span::styled(format!("  {title}"), text_style)], Span::styled(bar, Style::default().fg(t.accent)), w);
    }
    if title.is_empty() {
        // A note: a line of explanation between the rows.
        return Line::from(Span::styled(fit(&format!("  {detail}"), w).into_owned(), Style::default().fg(t.dim).add_modifier(Modifier::ITALIC)));
    }
    let value = settings_view::row_value(row);
    let mut left = vec![Span::styled(format!("  {title}"), text_style)];
    if !detail.is_empty() {
        left.push(Span::styled(format!("  {detail}"), Style::default().fg(t.dim)));
    }
    let value_style = if live { Style::default().fg(t.accent) } else { Style::default().fg(t.dim) };
    let line = spread(left, Span::styled(value, value_style), w);
    Line::from(line.spans.into_iter().map(|s| Span::styled(s.content.into_owned(), s.style)).collect::<Vec<_>>())
}

fn login(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let servers = app.prefs.servers.len() as u16;
    let h = 15 + if servers > 0 { servers + 2 } else { 0 };
    let r = centred(area, 64, h);
    let inner = popup(f, r, Span::styled(" nori · connect to a server ", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)), &t);
    let l = &app.login;
    let mut y = inner.y + 1;
    for (i, label) in LOGIN_FIELDS.iter().enumerate() {
        let focused = l.focus == i && !l.on_list;
        let value = if i == 3 { "•".repeat(l.fields[i].chars().count()) } else { l.fields[i].clone() };
        let cursor = if focused { "▌" } else { "" };
        let line = Line::from(vec![
            Span::styled(format!(" {label:<16}"), if focused { Style::default().fg(t.accent) } else { Style::default().fg(t.dim) }),
            Span::styled(format!("{value}{cursor}"), Style::default().fg(t.text)),
        ]);
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        f.render_widget(Paragraph::new(line), row);
        app.hits.push((row, Hit::LoginField(i)));
        y += 2;
    }
    let button = if l.busy { "[ Connecting… ]" } else { "[ Connect ]" };
    let br = Rect { x: inner.x + 17, y, width: button.width() as u16, height: 1 };
    f.render_widget(Paragraph::new(Span::styled(button, Style::default().fg(t.accent).add_modifier(Modifier::BOLD))), br);
    app.hits.push((br, Hit::Button(Button::Connect)));
    y += 2;
    let msg = match &l.error {
        Some(e) => Span::styled(format!(" {e}"), Style::default().fg(Color::LightRed)),
        None => Span::styled(" tab next field · enter connect · esc back", Style::default().fg(t.dim)),
    };
    f.render_widget(Paragraph::new(msg).wrap(ratatui::widgets::Wrap { trim: false }), Rect { x: inner.x, y, width: inner.width, height: 2.min(inner.y + inner.height - y) });
    y += 3;
    if servers > 0 {
        f.render_widget(Paragraph::new(Span::styled(" Or use a saved server", Style::default().fg(t.accent))), Rect { x: inner.x, y, width: inner.width, height: 1 });
        y += 1;
        let area = Rect { x: inner.x + 1, y, width: inner.width.saturating_sub(2), height: servers.min(inner.y + inner.height - y) };
        let App { login, prefs, hits, .. } = app;
        let names: Vec<String> = prefs.servers.iter().map(|s| format!("{}  {}", nori_core::settings::label(&s.name, &s.url), s.user)).collect();
        let focused = login.on_list;
        list(f, area, &mut login.sel, names.len(), ListRef::Profiles, hits, &t, focused, &|i, w| Line::from(fit(&names[i], w).into_owned()));
    }
}

fn overlay(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let App { overlay, hits, .. } = app;
    let Some(o) = overlay else { return };
    match o {
        Overlay::Help { scroll } => {
            let r = centred(area, 84, area.height.saturating_sub(4));
            let inner = popup(f, r, Span::styled(" Keys · any key closes ", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)), &t);
            let mut lines = Vec::new();
            for scope in [Scope::Global, Scope::List, Scope::Queue, Scope::Lyrics, Scope::Values] {
                lines.push(Line::from(Span::styled(scope.title(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))));
                for b in BINDINGS.iter().filter(|b| b.scope == scope && !b.label.is_empty()) {
                    lines.push(Line::from(vec![Span::styled(format!("  {:<22}", b.label), Style::default().fg(t.text)), Span::styled(b.help, Style::default().fg(t.dim))]));
                }
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled("The mouse: click tabs, rows and buttons; click or drag the seek bar; the wheel scrolls.", Style::default().fg(t.dim))));
            *scroll = (*scroll).min(lines.len().saturating_sub(inner.height as usize));
            f.render_widget(Paragraph::new(lines).scroll((*scroll as u16, 0)), inner);
            hits.push((r, Hit::List(ListRef::Help)));
        }
        Overlay::Picker { title, options, sel, .. } => {
            let h = (options.len() as u16 + 2).min(area.height.saturating_sub(4));
            let w = options.iter().map(|o| o.0.width()).max().unwrap_or(10).max(title.width()) as u16 + 8;
            let r = centred(area, w, h);
            let inner = popup(f, r, Span::styled(format!(" {title} "), Style::default().fg(t.accent)), &t);
            hits.push((r, Hit::List(ListRef::Picker)));
            let opts = options.clone();
            list(f, inner, sel, opts.len(), ListRef::Picker, hits, &t, true, &|i, w| Line::from(fit(&format!(" {}", opts[i].0), w).into_owned()));
        }
        Overlay::Input { title, text, secret, .. } => {
            let r = centred(area, 60, 3);
            let inner = popup(f, r, Span::styled(format!(" {title} · enter keeps, esc drops "), Style::default().fg(t.accent)), &t);
            let shown = if *secret { "•".repeat(text.chars().count()) } else { text.clone() };
            f.render_widget(Paragraph::new(format!("{shown}▌")), inner);
        }
    }
}
