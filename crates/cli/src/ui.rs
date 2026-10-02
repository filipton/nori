//! Drawing: sidebar, page, right panel (in the cover's colours) and the player bar. A narrow terminal
//! drops the sidebar and panel and shows the focused one in the page's place. Every clickable area is
//! recorded in `App::hits` as it is drawn. Lists and grids draw only visible rows.

use std::borrow::Cow;
use std::time::Instant;

use nori_core::Song;
use nori_engine::State;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use ratatui::Frame;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Button, Focus, Hit, ListRef, Load, Nav, Overlay, Page, Panel, SearchRow, Sel, View, LOGIN_FIELDS, NAV_BOTTOM, NAV_LIBRARY, NAV_TOP, PANELS};
use crate::art::{Art, Theme};
use crate::keys::{Scope, BINDINGS};
use crate::settings_view::{self, EqRow, Line as SLine, SettingsView};
use crate::text::clock;

/// Sidebar width, and the narrowest page allowed beside the sidebar and panel.
const SIDE_W: u16 = 26;
const MAIN_MIN: u16 = 50;
/// Album card minimum width and height without covers.
const CARD_W: u16 = 22;
const CARD_H: u16 = 4;

/// `s` cut to `w` columns, with an ellipsis if cut.
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

/// `s` cut or space-padded to exactly `w` columns.
fn pad(s: &str, w: usize) -> String {
    let cut = fit(s, w);
    let used = cut.width();
    let mut out = cut.into_owned();
    out.extend(std::iter::repeat_n(' ', w.saturating_sub(used)));
    out
}

/// `left` and right-aligned `right` in `w` columns, cutting `left` to fit.
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

/// Black or white, whichever reads on `bg`.
fn on(bg: Color) -> Color {
    match bg {
        Color::Rgb(r, g, b) if (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 140 => Color::Black,
        _ => Color::White,
    }
}

/// Renders `w` clipped to the frame.
fn put<W: Widget>(f: &mut Frame, w: W, r: Rect) {
    let r = r.intersection(f.area());
    if r.width > 0 && r.height > 0 {
        f.render_widget(w, r);
    }
}

fn text(f: &mut Frame, r: Rect, s: &str, style: Style) {
    put(f, Paragraph::new(Span::styled(fit(s, r.width as usize).into_owned(), style)), Rect { height: r.height.min(1), ..r });
}

fn selected(t: &Theme, focused: bool) -> Style {
    if focused {
        Style::default().bg(t.accent).fg(on(t.accent)).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
    }
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn dim(t: &Theme) -> Style {
    Style::default().fg(t.dim)
}

/// A scrolling list with the selection kept in view and a scrollbar when it overflows.
#[allow(clippy::too_many_arguments)]
fn list<'a>(f: &mut Frame, area: Rect, sel: &mut Sel, len: usize, lref: ListRef, hits: &mut Vec<(Rect, Hit)>, t: &Theme, focused: bool, row: &dyn Fn(usize, usize) -> Line<'a>) {
    hits.push((area, Hit::List(lref)));
    let h = area.height as usize;
    sel.fit(h, len);
    let width = if len > h { area.width.saturating_sub(1) } else { area.width };
    for (n, i) in (sel.top..len.min(sel.top + h)).enumerate() {
        let r = Rect { x: area.x, y: area.y + n as u16, width, height: 1 };
        let mut line = row(i, width as usize);
        if i == sel.at {
            // Patch every span so coloured spans stay readable on the selection.
            let st = selected(t, focused);
            line.spans.iter_mut().for_each(|s| s.style = s.style.patch(st));
            line = line.style(st);
        }
        put(f, Paragraph::new(line), r);
        hits.push((r, Hit::Row(lref, i)));
    }
    if len > h && area.width > 2 && h > 0 {
        let x = area.x + area.width - 1;
        let thumb = (h * h / len).clamp(1, h);
        let y0 = (sel.top * h) / len.max(1);
        for y in 0..h {
            let lit = y >= y0 && y < y0 + thumb;
            put(f, Paragraph::new(Span::styled(if lit { "┃" } else { "│" }, if lit { Style::default().fg(t.accent) } else { dim(t) })), Rect { x, y: area.y + y as u16, width: 1, height: 1 });
        }
    }
}

/// Card cover state for a frame: the covers, whether cards show them, and missing ones to request after.
pub struct Pics<'a> {
    art: Option<&'a mut Art>,
    on: bool,
    want: Vec<String>,
}

impl Pics<'_> {
    /// Card minimum width and height.
    fn card(&self) -> (u16, u16) {
        if self.on {
            (CARD_W + 8, CARD_H + 2)
        } else {
            (CARD_W, CARD_H)
        }
    }
}

/// A grid of cards (cover with text beside it, or a box without covers), selection kept in view;
/// `sel.top` is the first visible row. Returns the number of columns.
#[allow(clippy::too_many_arguments)]
fn cards(f: &mut Frame, area: Rect, sel: &mut Sel, len: usize, lref: ListRef, hits: &mut Vec<(Rect, Hit)>, t: &Theme, focused: bool, pics: &mut Pics, card: &dyn Fn(usize) -> (String, String, Option<String>)) -> usize {
    hits.push((area, Hit::List(lref)));
    let (min_w, card_h) = pics.card();
    let cols = ((area.width + 1) / (min_w + 1)).max(1) as usize;
    let w = ((area.width + 1) / cols as u16).saturating_sub(1).max(1);
    let rows = (area.height / card_h).max(1) as usize;
    sel.at = sel.at.min(len.saturating_sub(1));
    let row_at = sel.at / cols;
    if row_at < sel.top {
        sel.top = row_at;
    } else if row_at >= sel.top + rows {
        sel.top = row_at + 1 - rows;
    }
    for r in 0..rows {
        for c in 0..cols {
            let i = (sel.top + r) * cols + c;
            if i >= len {
                return cols;
            }
            let rect = Rect { x: area.x + c as u16 * (w + 1), y: area.y + r as u16 * card_h, width: w, height: card_h.min(area.height) };
            let (title, sub, art) = card(i);
            one_card(f, rect, &title, &sub, art.as_deref(), pics, t, i == sel.at, focused);
            hits.push((rect.intersection(area), Hit::Row(lref, i)));
        }
    }
    cols
}

#[allow(clippy::too_many_arguments)]
fn one_card(f: &mut Frame, r: Rect, title: &str, sub: &str, art: Option<&str>, pics: &mut Pics, t: &Theme, chosen: bool, focused: bool) {
    if pics.on {
        let h = r.height.saturating_sub(1).min(CARD_H + 1);
        let cr = Rect { width: (h * 2).min(r.width), height: h, ..r };
        let key = art.map(|a| format!("{}{a}", crate::backend::THUMB));
        if let (Some(a), Some(k)) = (art, &key) {
            if !pics.art.as_ref().is_some_and(|x| x.has(k)) {
                pics.want.push(a.to_string());
            }
        }
        cover(f, cr, pics.art.as_deref_mut(), key.as_deref(), t);
        let words = Rect { x: cr.x + cr.width + 1, y: r.y + h.saturating_sub(3) / 2, width: r.width.saturating_sub(cr.width + 1), height: 3.min(h) };
        let w = words.width as usize;
        let title_style = if chosen && focused {
            Style::default().bg(t.accent).fg(on(t.accent)).add_modifier(Modifier::BOLD)
        } else if chosen {
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
        } else {
            bold().fg(t.text)
        };
        let (who, when) = sub.split_once(" · ").unwrap_or((sub, ""));
        let lines = vec![Line::from(Span::styled(pad(title, w), title_style)), Line::from(Span::styled(fit(who, w).into_owned(), dim(t))), Line::from(Span::styled(fit(when, w).into_owned(), dim(t)))];
        put(f, Paragraph::new(lines), words);
        if chosen {
            put(f, Paragraph::new(Span::styled("▔".repeat(cr.width as usize), Style::default().fg(t.accent))), Rect { y: cr.y + cr.height, height: 1, ..cr });
        }
        return;
    }
    let border = if chosen && focused { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else if chosen { Style::default().fg(t.accent) } else { dim(t) };
    let block = Block::default().borders(Borders::ALL).border_type(if chosen { BorderType::Thick } else { BorderType::Rounded }).border_style(border);
    let inner = block.inner(r);
    put(f, block, r);
    let w = inner.width.saturating_sub(1) as usize;
    let title_style = if chosen { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { bold() };
    let lines = vec![Line::from(Span::styled(format!(" {}", fit(title, w)), title_style)), Line::from(Span::styled(format!(" {}", fit(sub, w)), dim(t)))];
    put(f, Paragraph::new(lines), inner);
}

fn centred(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

/// A titled accent box over `r`; returns its inner area.
fn popup(f: &mut Frame, r: Rect, title: Span, t: &Theme) -> Rect {
    let block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(title).border_style(Style::default().fg(t.accent));
    let inner = block.inner(r);
    put(f, Clear, r);
    put(f, block, r);
    inner
}

/// A bold page title with a right-aligned caption; returns the area below plus a blank line.
fn heading(f: &mut Frame, area: Rect, title: &str, caption: &str, t: &Theme) -> Rect {
    if area.height < 3 {
        return area;
    }
    let line = spread(vec![Span::styled(title.to_string(), bold().fg(t.text))], Span::styled(caption.to_string(), dim(t)), area.width as usize);
    put(f, Paragraph::new(line), Rect { height: 1, ..area });
    Rect { y: area.y + 2, height: area.height - 2, ..area }
}

// ---- songs as a table ----

/// Song table column widths in `w`: number, title, artist, album, time. Album only if asked and room;
/// artist dropped first.
fn columns(w: usize, album: bool) -> [usize; 5] {
    let num = 4;
    let time = 6;
    let rest = w.saturating_sub(num + time);
    if album && rest >= 66 {
        let title = rest * 44 / 100;
        let artist = rest * 28 / 100;
        [num, title, artist, rest - title - artist, time]
    } else if rest >= 34 {
        let title = rest * 60 / 100;
        [num, title, rest - title, 0, time]
    } else {
        [num, rest, 0, 0, time]
    }
}

fn table_head(f: &mut Frame, area: Rect, t: &Theme, album: bool) -> Rect {
    if area.height < 4 {
        return area;
    }
    let c = columns(area.width as usize, album);
    let mut s = String::new();
    s.push_str(&pad(" #", c[0]));
    s.push_str(&pad("Title", c[1]));
    if c[2] > 0 {
        s.push_str(&pad("Artist", c[2]));
    }
    if c[3] > 0 {
        s.push_str(&pad("Album", c[3]));
    }
    s.push_str(&format!("{:>w$}", "Time ", w = c[4]));
    put(f, Paragraph::new(Span::styled(s, dim(t).add_modifier(Modifier::BOLD))), Rect { height: 1, ..area });
    put(f, Paragraph::new(Span::styled("─".repeat(area.width as usize), dim(t))), Rect { y: area.y + 1, height: 1, ..area });
    Rect { y: area.y + 2, height: area.height - 2, ..area }
}

/// A song table row; `number` 0 shows none, `playing` shows a marker instead.
fn song_line(s: &Song, number: usize, w: usize, t: &Theme, playing: bool, album: bool) -> Line<'static> {
    let c = columns(w, album);
    let num = if playing {
        Span::styled(pad("  ▶", c[0]), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))
    } else if number == 0 {
        Span::raw(pad("", c[0]))
    } else {
        Span::styled(pad(&format!("{number:>3}"), c[0]), dim(t))
    };
    let mut title = s.title.clone();
    if s.is_provider() {
        title.push_str(" ☁");
    }
    let mark = if s.starred { " ♥" } else { "" };
    let title_style = if playing { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(t.text) };
    let tw = c[1].saturating_sub(mark.width() + 2);
    let mut spans = vec![num, Span::styled(pad(&title, tw), title_style), Span::styled(pad(mark, mark.width() + 2), Style::default().fg(t.accent))];
    if c[2] > 0 {
        spans.push(Span::styled(pad(&s.artist, c[2].saturating_sub(2)), dim(t)));
        spans.push(Span::raw("  "));
    }
    if c[3] > 0 {
        spans.push(Span::styled(pad(&s.album, c[3].saturating_sub(2)), dim(t)));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(format!("{:>w$} ", clock(s.duration as i64 * 1000), w = c[4] - 1), dim(t)));
    Line::from(spans)
}

// ---- the frame ----

#[allow(clippy::needless_option_as_deref)]
pub fn draw(f: &mut Frame, app: &mut App, mut art: Option<&mut Art>) {
    app.hits.clear();
    let area = f.area();
    let ui = Theme::plain(app.prefs.accent);
    if app.view == View::Login {
        login(f, area, app, &ui);
        overlay(f, area, app, &ui);
        return;
    }
    let bar_h = if area.height >= 8 { 3 } else { 0 };
    let [body, bar] = Layout::vertical([Constraint::Min(0), Constraint::Length(bar_h)]).areas(area);
    if app.full {
        full_player(f, body, app, art.as_deref_mut());
    } else {
        let side = area.width >= SIDE_W + MAIN_MIN;
        let panel_w = (area.width / 4).clamp(34, 48);
        let panel = app.panel.is_some() && area.width >= SIDE_W + MAIN_MIN + panel_w;
        app.shown.side = side;
        app.shown.panel = panel;
        let [s, m, p] = Layout::horizontal([Constraint::Length(if side { SIDE_W } else { 0 }), Constraint::Min(0), Constraint::Length(if panel { panel_w } else { 0 })]).areas(body);
        if side {
            sidebar(f, s, app, &ui);
        }
        match app.focus {
            Focus::Side if !side => sidebar(f, m, app, &ui),
            Focus::Panel if !panel && app.panel.is_some() => right_panel(f, m, app, art.as_deref_mut()),
            _ => main(f, m, app, &ui, art.as_deref_mut()),
        }
        if panel {
            right_panel(f, p, app, art.as_deref_mut());
        }
        toast(f, m, app);
    }
    if bar_h > 0 {
        player_bar(f, bar, app, &ui);
    }
    overlay(f, area, app, &ui);
}

/// The status note, bottom right of the page.
fn toast(f: &mut Frame, area: Rect, app: &App) {
    let Some((msg, error, _)) = &app.note else { return };
    let w = (msg.width() as u16 + 4).min(area.width);
    if w < 8 || area.height < 4 {
        return;
    }
    let r = Rect { x: area.x + area.width.saturating_sub(w + 1), y: area.y + area.height - 3, width: w, height: 3 };
    let colour = if *error { Color::LightRed } else { crate::art::argb(app.prefs.accent as u32) };
    let block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(colour));
    let inner = block.inner(r);
    put(f, Clear, r);
    put(f, block, r);
    text(f, Rect { x: inner.x + 1, width: inner.width.saturating_sub(1), ..inner }, msg, Style::default().fg(colour));
}

// ---- the sidebar ----

fn sidebar(f: &mut Frame, area: Rect, app: &mut App, t: &Theme) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let sep = Rect { x: area.x + area.width - 1, width: 1, ..area };
    for y in sep.top()..sep.bottom() {
        put(f, Paragraph::new(Span::styled("│", dim(t))), Rect { y, height: 1, ..sep });
    }
    let area = Rect { width: area.width - 1, ..area };
    app.hits.push((area, Hit::List(ListRef::Side)));
    let focused = app.focus == Focus::Side;
    let w = area.width as usize;
    let mut y = area.y;
    let bottom = area.y + area.height;
    put(f, Paragraph::new(Line::from(vec![Span::styled(" ♫ ", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)), Span::styled("nori", bold())])), Rect { y, height: 1, ..area });
    y += 2;
    let item = |f: &mut Frame, hits: &mut Vec<(Rect, Hit)>, y: u16, i: usize, n: Nav, name: &str| {
        if y >= bottom {
            return;
        }
        let r = Rect { y, height: 1, ..area };
        let open = app.root == n;
        let chosen = focused && app.side.at == i;
        let label = format!("  {}  {}", n.icon(), name);
        let style = if chosen {
            selected(t, true)
        } else if open {
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.text)
        };
        put(f, Paragraph::new(Span::styled(pad(&label, w), style)), r);
        if open {
            put(f, Paragraph::new(Span::styled("▌", Style::default().fg(t.accent))), Rect { width: 1, ..r });
        }
        hits.push((r, Hit::Nav(i)));
    };
    let label = |f: &mut Frame, y: u16, s: &str| {
        if y < bottom {
            put(f, Paragraph::new(Span::styled(format!("  {s}"), dim(t).add_modifier(Modifier::BOLD))), Rect { y, height: 1, ..area });
        }
    };
    let mut i = 0;
    for n in NAV_TOP {
        item(f, &mut app.hits, y, i, n, n.name());
        y += 1;
        i += 1;
    }
    y += 1;
    label(f, y, "LIBRARY");
    y += 1;
    for n in NAV_LIBRARY {
        item(f, &mut app.hits, y, i, n, n.name());
        y += 1;
        i += 1;
    }
    y += 1;
    label(f, y, "PLAYLISTS");
    y += 1;
    // Playlists fill the space above the foot, scrolled to keep the selection visible.
    let foot = 5u16;
    let room = bottom.saturating_sub(y + foot) as usize;
    let names: Vec<String> = app.library.playlists.ready().map_or_else(Vec::new, |v| v.iter().map(|p| p.name.clone()).collect());
    let first = i;
    if names.is_empty() {
        let what = match &app.library.playlists {
            Load::Loading => "Loading…",
            Load::Failed(_) => "Could not load",
            _ => "None yet",
        };
        if room > 0 {
            put(f, Paragraph::new(Span::styled(format!("     {what}"), dim(t))), Rect { y, height: 1, ..area });
        }
    } else if room > 0 {
        let at = app.side.at.checked_sub(first).filter(|a| *a < names.len());
        let top = &mut app.side.top;
        if let Some(at) = at {
            if at < *top {
                *top = at;
            } else if at >= *top + room {
                *top = at + 1 - room;
            }
        }
        *top = (*top).min(names.len().saturating_sub(room));
        let top = *top;
        for (k, name) in names.iter().enumerate().skip(top).take(room) {
            item(f, &mut app.hits, y + (k - top) as u16, first + k, Nav::Playlist(k), name);
        }
        if names.len() > room {
            let more = if top + room < names.len() { "  ⌄" } else { "" };
            if !more.is_empty() {
                put(f, Paragraph::new(Span::styled(more, dim(t))), Rect { x: area.x + area.width.saturating_sub(4), y: y + room as u16 - 1, width: 4, height: 1 });
            }
        }
    }
    i = first + names.len();
    // Foot: equalizer, settings, server status, help hint.
    let fy = bottom.saturating_sub(foot);
    if fy <= y {
        return;
    }
    put(f, Paragraph::new(Span::styled("─".repeat(w), dim(t))), Rect { y: fy, height: 1, ..area });
    for (k, n) in NAV_BOTTOM.iter().enumerate() {
        item(f, &mut app.hits, fy + 1 + k as u16, i + k, *n, n.name());
    }
    let (dot, colour, word) = if app.offline {
        ("●", Color::Yellow, "offline · ")
    } else if app.unreachable.is_some() {
        ("●", Color::LightRed, "unreachable · ")
    } else {
        ("●", Color::Green, "")
    };
    let server = Line::from(vec![Span::styled(format!("  {dot} "), Style::default().fg(colour)), Span::styled(fit(&format!("{word}{}", app.server), w.saturating_sub(4)).into_owned(), dim(t))]);
    put(f, Paragraph::new(server), Rect { y: fy + 3, height: 1, ..area });
    let r = Rect { y: fy + 4, height: 1, ..area };
    put(f, Paragraph::new(Span::styled("  ? keys · q quit", dim(t))), r);
    app.hits.push((Rect { width: 8.min(r.width), ..r }, Hit::Button(Button::Help)));
}

// ---- the page ----

fn main(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, art: Option<&mut Art>) {
    let on = app.images && app.card_covers && art.is_some();
    let mut pics = Pics { art, on, want: Vec::new() };
    page_view(f, area, app, t, &mut pics);
    // Request missing card covers once each.
    for id in pics.want {
        if app.thumbs_asked.len() > 2000 {
            app.thumbs_asked.clear();
        }
        if app.thumbs_asked.insert(id.clone()) {
            app.cmds.push(crate::app::Cmd::Thumb(id));
        }
    }
}

fn page_view(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, pics: &mut Pics) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let mut area = area;
    if let Some(e) = &app.unreachable {
        text(f, Rect { x: area.x + 2, width: area.width.saturating_sub(4), ..area }, &format!("⚠ {e} · showing what is stored · R tries again"), Style::default().fg(Color::LightRed));
        area = Rect { y: area.y + 1, height: area.height - 1, ..area };
    }
    let inner = Rect { x: area.x + 2, y: area.y + 1, width: area.width.saturating_sub(4), height: area.height.saturating_sub(1) };
    let focused = app.focus == Focus::Main;
    if !app.pages.is_empty() {
        return page(f, inner, app, t, focused, pics);
    }
    match app.view {
        View::Home => home(f, inner, app, t, focused, pics),
        View::Search => search(f, inner, app, t, focused),
        View::Albums => albums(f, inner, app, t, focused, pics),
        View::Artists => artists(f, inner, app, t, focused),
        View::Songs => songs(f, inner, app, t, focused),
        View::Downloads => downloads(f, inner, app, t, focused),
        View::Equalizer => equalizer(f, inner, app, t, focused),
        View::Settings => settings(f, inner, app, t, focused),
        View::Login => {}
    }
}

fn failed(f: &mut Frame, area: Rect, t: &Theme, e: &str) {
    let text = vec![
        Line::from(Span::styled("Could not load this page", Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(e.to_string(), Style::default().fg(t.text))),
        Line::from(""),
        Line::from(Span::styled("R tries again", dim(t))),
    ];
    put(f, Paragraph::new(text).alignment(Alignment::Center).wrap(ratatui::widgets::Wrap { trim: true }), centred(area, area.width.min(70), 6));
}

fn loading(f: &mut Frame, area: Rect, t: &Theme) {
    put(f, Paragraph::new(Span::styled("Loading…", dim(t))).alignment(Alignment::Center), centred(area, 20, 1));
}

fn empty(f: &mut Frame, area: Rect, t: &Theme, text: &str) {
    put(f, Paragraph::new(Span::styled(text.to_string(), dim(t))).alignment(Alignment::Center).wrap(ratatui::widgets::Wrap { trim: true }), centred(area, area.width, 2));
}

/// Greeting by local time of day.
fn greeting() -> &'static str {
    let now = nori_core::db::now_ms() / 1000;
    let hour = (now + nori_core::library::local_offset_s(now)).rem_euclid(86_400) / 3600;
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

fn home(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool, pics: &mut Pics) {
    let area = heading(f, area, greeting(), "", t);
    let shelves: Vec<(usize, &'static str, usize)> = app.home.shelves().iter().map(|(i, title, v)| (*i, *title, v.len())).collect();
    if shelves.is_empty() {
        match &app.home.error {
            Some(e) => failed(f, area, t, e),
            None => loading(f, area, t),
        }
        return;
    }
    // Each shelf: title line, a row of cards, a blank line.
    let (min_w, card_h) = pics.card();
    let shelf_h = card_h + 2;
    let fits = (area.height / shelf_h).max(1) as usize;
    let at = shelves.iter().position(|s| s.0 == app.home.shelf).unwrap_or(0);
    let h = &mut app.home;
    if at < h.top {
        h.top = at;
    } else if at >= h.top + fits {
        h.top = at + 1 - fits;
    }
    h.top = h.top.min(shelves.len().saturating_sub(1));
    let cols = ((area.width + 1) / (min_w + 1)).max(1) as usize;
    let w = ((area.width + 1) / cols as u16).saturating_sub(1).max(1);
    for (k, (i, title, len)) in shelves.iter().enumerate().skip(app.home.top).take(fits) {
        let y = area.y + (k - app.home.top) as u16 * shelf_h;
        if y + 1 >= area.y + area.height {
            break;
        }
        let chosen_shelf = *i == app.home.shelf;
        let pos = app.home.pos.get(*i).copied().unwrap_or(0).min(len.saturating_sub(1));
        let left = &mut app.home.left[*i];
        if pos < *left {
            *left = pos;
        } else if pos >= *left + cols {
            *left = pos + 1 - cols;
        }
        let left = *left;
        let more = if *len > cols { format!("{} of {len}  ‹ ›", pos + 1) } else { String::new() };
        let title_style = if chosen_shelf && focused { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { bold() };
        put(f, Paragraph::new(spread(vec![Span::styled(title.to_string(), title_style)], Span::styled(more, dim(t)), area.width as usize)), Rect { y, height: 1, ..area });
        let row = Rect { y: y + 1, height: card_h.min((area.y + area.height).saturating_sub(y + 1)), ..area };
        app.hits.push((row, Hit::List(ListRef::Shelf(*i))));
        let Some(Some((_, albums))) = app.home.rows.get(*i) else { continue };
        for c in 0..cols {
            let n = left + c;
            let Some(a) = albums.get(n) else { break };
            let r = Rect { x: area.x + c as u16 * (w + 1), width: w, ..row };
            let sub = if a.year > 0 { format!("{} · {}", a.artist, a.year) } else { a.artist.clone() };
            one_card(f, r, &a.name, &sub, a.cover_art.as_deref(), pics, t, chosen_shelf && n == pos, focused);
            app.hits.push((r, Hit::Row(ListRef::Shelf(*i), n)));
        }
    }
}

fn albums(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool, pics: &mut Pics) {
    let l = &mut app.library;
    let caption = l.albums.ready().map_or(String::new(), |v| crate::text::count(v.len() as u64, "album", "albums") + if l.albums_more { "+" } else { "" });
    let body = heading(f, area, "Albums", &caption, t);
    match &l.albums {
        Load::Ready(v) if v.is_empty() => empty(f, body, t, "Nothing here yet"),
        Load::Ready(v) => {
            let len = v.len();
            let cols = cards(f, body, &mut l.albums_sel, len, ListRef::Albums, &mut app.hits, t, focused, pics, &|i| {
                let a = &v[i];
                (a.name.clone(), if a.year > 0 { format!("{} · {}", a.artist, a.year) } else { a.artist.clone() }, a.cover_art.clone())
            });
            app.shown.cols = cols;
        }
        Load::Failed(e) => failed(f, body, t, e),
        _ => loading(f, body, t),
    }
}

fn artists(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool) {
    let l = &mut app.library;
    let caption = l.artists.ready().map_or(String::new(), |v| crate::text::count(v.len() as u64, "artist", "artists"));
    let body = heading(f, area, "Artists", &caption, t);
    match &l.artists {
        Load::Ready(v) if v.is_empty() => empty(f, body, t, "Nothing here yet"),
        Load::Ready(v) => {
            let row = |i: usize, w: usize| {
                let a = &v[i];
                let heart = if a.starred { " ♥" } else { "" };
                spread(vec![Span::styled("  ◉  ", Style::default().fg(t.accent)), Span::styled(a.name.as_str(), bold()), Span::styled(heart, Style::default().fg(t.accent))], Span::styled(crate::text::albums(a.album_count) + " ", dim(t)), w)
            };
            list(f, body, &mut l.artists_sel, v.len(), ListRef::Artists, &mut app.hits, t, focused, &row);
        }
        Load::Failed(e) => failed(f, body, t, e),
        _ => loading(f, body, t),
    }
}

fn songs(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool) {
    let playing = app.song.as_ref().map(|s| s.id.clone());
    let l = &mut app.library;
    let caption = l.songs.ready().map_or(String::new(), |v| crate::text::count(v.len() as u64, "song", "songs") + if l.songs_more { "+" } else { "" });
    let body = heading(f, area, "Songs", &caption, t);
    match &l.songs {
        Load::Ready(v) if v.is_empty() => empty(f, body, t, "Nothing here yet"),
        Load::Ready(v) => {
            let body = table_head(f, body, t, true);
            let row = |i: usize, w: usize| song_line(&v[i], i + 1, w, t, playing.as_deref() == Some(v[i].id.as_str()), true);
            list(f, body, &mut l.songs_sel, v.len(), ListRef::Songs, &mut app.hits, t, focused, &row);
        }
        Load::Failed(e) => failed(f, body, t, e),
        _ => loading(f, body, t),
    }
}

fn search(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool) {
    if area.height < 4 {
        return;
    }
    let s = &app.search;
    let field = Rect { height: 3, width: area.width.min(72), ..area };
    let editing = s.editing && focused;
    let block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(if editing { Style::default().fg(t.accent) } else { dim(t) });
    let inner = block.inner(field);
    put(f, block, field);
    let line = if s.text.is_empty() && !editing {
        Line::from(vec![Span::styled(" ⌕  ", dim(t)), Span::styled("What do you want to hear?", dim(t))])
    } else {
        let cursor = if editing { "▏" } else { "" };
        Line::from(vec![Span::styled(" ⌕  ", Style::default().fg(t.accent)), Span::styled(format!("{}{cursor}", s.text), Style::default().fg(t.text))])
    };
    put(f, Paragraph::new(line), inner);
    app.hits.push((field, Hit::SearchField));
    let note = Rect { y: area.y + 3, height: 1, ..area };
    let view = s.view.as_ref();
    let (words, style) = match view {
        Some(v) if v.error.is_some() => (crate::text::search_fallback(v.error.as_ref().and_then(|e| e.reason.as_deref())), Style::default().fg(Color::LightRed)),
        Some(v) if v.searching => ("Asking the server…".to_string(), dim(t)),
        Some(v) if v.nothing_found => ("Nothing found".to_string(), dim(t)),
        Some(v) if !v.query.is_empty() => ((if v.from_server { "From the server" } else { "From the offline index · the server is asked when you pause" }).to_string(), dim(t)),
        _ => ("Songs, albums and artists, from the library".to_string(), dim(t)),
    };
    text(f, Rect { x: note.x + 1, width: note.width.saturating_sub(1), ..note }, &words, style);
    let body = Rect { y: area.y + 5, height: area.height.saturating_sub(5), ..area };
    let playing = app.song.as_ref().map(|s| s.id.clone());
    let App { search: s, hits, .. } = app;
    let lit = focused && !s.editing;
    let mut sel = s.sel;
    let rows = s.rows();
    if rows.is_empty() {
        return;
    }
    let len = rows.len();
    let row = |i: usize, w: usize| match &rows[i] {
        SearchRow::Title(title, n) => spread(vec![Span::styled(title.to_string(), bold().fg(t.text))], Span::styled(format!("{n} "), dim(t)), w),
        SearchRow::Song(song) => song_line(song, 0, w, t, playing.as_deref() == Some(song.id.as_str()), true),
        SearchRow::Album(a) => {
            let year = if a.year > 0 { format!("{} ", a.year) } else { String::new() };
            spread(vec![Span::styled("  ◫  ", Style::default().fg(t.accent)), Span::styled(a.name.clone(), Style::default().fg(t.text)), Span::styled(format!("  {}", a.artist), dim(t))], Span::styled(year, dim(t)), w)
        }
        SearchRow::Artist(a) => spread(vec![Span::styled("  ◉  ", Style::default().fg(t.accent)), Span::styled(a.name.clone(), Style::default().fg(t.text))], Span::styled(crate::text::albums(a.album_count) + " ", dim(t)), w),
    };
    list(f, body, &mut sel, len, ListRef::Search, hits, t, lit, &row);
    drop(rows);
    s.sel = sel;
}

/// A cover in `area`, or a placeholder box if it is not loaded.
fn cover(f: &mut Frame, area: Rect, art: Option<&mut Art>, key: Option<&str>, t: &Theme) {
    let area = area.intersection(f.area());
    if area.width < 2 || area.height < 1 {
        return;
    }
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
            // Encoded for this area now: this frame transmits it.
            if let Some(rect) = encode {
                match p.last_encoding_result() {
                    Some(Err(e)) => eprintln!("nori: cover {} would not encode for {}x{} cells: {e}", key.unwrap_or_default(), rect.width, rect.height),
                    _ => crate::term::debug!("cover {} encoded for {}x{} cells at {},{}: sent with this frame", key.unwrap_or_default(), rect.width, rect.height, area.x, area.y),
                }
            }
        }
        None => {
            let block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(dim(t));
            let inner = block.inner(area);
            put(f, block, area);
            put(f, Paragraph::new(Span::styled("♫", Style::default().fg(t.accent))).alignment(Alignment::Center), centred(inner, 1, 1));
        }
    }
}

/// Marks graphics-protocol cells as one column wide. The picture is one long escape sequence in a single
/// cell, and ratatui 0.30's diff would otherwise skip that many columns, hiding the rest of the frame.
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

/// A square cover `rows` tall (twice as wide in cells) at the top left of `area`.
fn cover_box(area: Rect, rows: u16) -> Rect {
    let h = rows.min(area.height).min(area.width / 2);
    Rect { x: area.x, y: area.y, width: h * 2, height: h }
}

/// A pill button, filled or plain accent; advances `x`.
fn pill(f: &mut Frame, hits: &mut Vec<(Rect, Hit)>, x: &mut u16, y: u16, end: u16, label: &str, filled: bool, b: Button, t: &Theme) {
    let s = format!(" {label} ");
    let w = s.width() as u16;
    if *x + w > end {
        return;
    }
    let style = if filled { Style::default().bg(t.accent).fg(on(t.accent)).add_modifier(Modifier::BOLD) } else { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) };
    let r = Rect { x: *x, y, width: w, height: 1 };
    put(f, Paragraph::new(Span::styled(s, style)), r);
    hits.push((r, Hit::Button(b)));
    *x += w + 2;
}

fn page(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool, pics: &mut Pics) {
    let images = app.images;
    let playing = app.song.as_ref().map(|s| s.id.clone());
    let App { pages, hits, shown, .. } = app;
    let Some(p) = pages.last_mut() else { return };
    // star: None when the page cannot be starred.
    let (kind, title, sub, caption, art_key, star): (&str, String, String, String, Option<String>, Option<bool>) = match p {
        Page::Album { detail: Load::Ready(d), .. } => ("ALBUM", d.album.name.clone(), d.album.artist.clone(), crate::text::album_caption(d), d.album.cover_art.clone(), Some(d.album.starred)),
        Page::Artist { detail: Load::Ready(d), .. } => ("ARTIST", d.artist.name.clone(), crate::text::albums(d.artist.album_count), String::new(), None, Some(d.artist.starred)),
        Page::Playlist { detail: Load::Ready(d), .. } => ("PLAYLIST", d.playlist.name.clone(), d.playlist.owner.clone().unwrap_or_default(), crate::text::playlist_caption(d), None, None),
        Page::Album { detail: Load::Failed(e), .. } | Page::Artist { detail: Load::Failed(e), .. } | Page::Playlist { detail: Load::Failed(e), .. } => {
            return failed(f, area, t, e);
        }
        _ => return loading(f, area, t),
    };
    let with_cover = images && matches!(p, Page::Album { .. }) && area.height >= 16;
    let head_h = if with_cover { 8 } else { 6 }.min(area.height);
    let head = Rect { height: head_h, ..area };
    let mut words = head;
    if with_cover {
        let c = cover_box(head, head_h);
        cover(f, c, pics.art.as_deref_mut(), art_key.as_deref(), t);
        words = Rect { x: c.x + c.width + 3, width: head.width.saturating_sub(c.width + 3), ..head };
    }
    let w = words.width as usize;
    let mut lines = vec![Line::from(Span::styled(kind, dim(t).add_modifier(Modifier::BOLD))), Line::from(Span::styled(fit(&title, w).into_owned(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD)))];
    if !sub.is_empty() {
        lines.push(Line::from(Span::styled(fit(&sub, w).into_owned(), bold().fg(t.text))));
    }
    lines.push(Line::from(Span::styled(fit(&caption, w).into_owned(), dim(t))));
    let top = if with_cover { words.y + words.height.saturating_sub(6) } else { words.y };
    put(f, Paragraph::new(lines), Rect { y: top, height: 4.min(words.height), ..words });
    let y = top + 5;
    if y < area.y + area.height {
        let mut x = words.x;
        let end = words.x + words.width;
        pill(f, hits, &mut x, y, end, "▶ Play", true, Button::PlayAll, t);
        pill(f, hits, &mut x, y, end, "⤮ Shuffle", false, Button::ShuffleAll, t);
        if let Some(on) = star {
            pill(f, hits, &mut x, y, end, if on { "♥ Favorite" } else { "♡ Favorite" }, false, Button::Star, t);
        }
        pill(f, hits, &mut x, y, end, "↓ Download", false, Button::Download, t);
        let back = "‹ Back";
        if x + back.width() as u16 <= end {
            let r = Rect { x, y, width: back.width() as u16, height: 1 };
            put(f, Paragraph::new(Span::styled(back, dim(t))), r);
            hits.push((r, Hit::Button(Button::Back)));
        }
    }
    let body = Rect { y: area.y + head_h + 1, height: area.height.saturating_sub(head_h + 1), ..area };
    match p {
        Page::Album { detail: Load::Ready(d), sel, .. } => {
            let multi = d.discs.len() > 1;
            let songs = &d.songs;
            let body = table_head(f, body, t, false);
            list(f, body, sel, songs.len(), ListRef::Page, hits, t, focused, &|i, w| {
                let n = if multi { songs[i].disc_number as usize * 100 + songs[i].track as usize } else { songs[i].track as usize };
                song_line(&songs[i], if n > 0 { n } else { i + 1 }, w, t, playing.as_deref() == Some(songs[i].id.as_str()), false)
            });
        }
        Page::Playlist { detail: Load::Ready(d), sel, .. } => {
            let songs = &d.songs;
            let body = table_head(f, body, t, true);
            list(f, body, sel, songs.len(), ListRef::Page, hits, t, focused, &|i, w| song_line(&songs[i], i + 1, w, t, playing.as_deref() == Some(songs[i].id.as_str()), true));
        }
        Page::Artist { detail: Load::Ready(d), sel, .. } => {
            let albums = &d.albums;
            let body = heading(f, body, "Albums", "", t);
            shown.cols = cards(f, body, sel, albums.len(), ListRef::Page, hits, t, focused, pics, &|i| {
                let a = &albums[i];
                (a.name.clone(), if a.year > 0 { format!("{} · {}", a.artist, a.year) } else { a.artist.clone() }, a.cover_art.clone())
            });
        }
        _ => {}
    }
}

fn downloads(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool) {
    let body = heading(f, area, "Downloads", "D on a song, an album or a playlist downloads it", t);
    match &app.downloads {
        Load::Failed(e) => return failed(f, body, t, &e.clone()),
        Load::Ready(_) => {}
        _ => return loading(f, body, t),
    }
    let rows = app.download_rows();
    if rows.is_empty() {
        return empty(f, body, t, "Nothing downloaded yet");
    }
    let lines: Vec<Line> = rows
        .iter()
        .map(|(title, song)| match song {
            None => Line::from(Span::styled(title.clone(), bold().fg(t.text))),
            Some((list, i)) => {
                let s = &list[*i];
                Line::from(vec![Span::styled("  ↓  ", Style::default().fg(t.accent)), Span::raw(s.title.clone()), Span::styled(format!("  {}", s.artist), dim(t))])
            }
        })
        .collect();
    let len = lines.len();
    let App { downloads_sel, hits, .. } = app;
    list(f, body, downloads_sel, len, ListRef::Downloads, hits, t, focused, &|i, _| lines[i].clone());
}

/// The equalizer: toolbar chips, band faders, then the rest of the chain as chips. ← → walk them in
/// that order; ↑ ↓ change the selected one.
fn equalizer(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool) {
    let p = app.prefs.clone();
    let bypass = nori_core::settings::eq_bypass(p.bit_perfect, p.sound_bypass);
    let head: &str = match bypass {
        Some(why) => crate::text::eq_bypass(why),
        None if p.eq_enabled => "On · heard at once while this is open",
        None => "Off · switch it on to hear the bands",
    };
    let area = heading(f, area, "Equalizer", head, t);
    if area.height < 6 || area.width < 20 {
        return;
    }
    let rows = crate::settings_view::eq_rows(&p);
    app.eq_sel.at = app.eq_sel.at.min(rows.len().saturating_sub(1));
    let chosen = app.eq_sel.at;
    let top: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].above_bands()).collect();
    let bands: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].band(&p).is_some()).collect();
    let under: Vec<usize> = (0..rows.len()).filter(|&i| !rows[i].above_bands() && rows[i].band(&p).is_none()).collect();
    let boxed = area.height >= 26;
    let chip = |i: usize| rows[i].chip(&p);
    let top_h = chips(f, area, &top, &chip, chosen, focused, boxed, &mut app.hits, t, &|i| lit(&rows[i], &p));
    let hint = "← → choose · ↑ ↓ change · enter switch · d remove a band";
    let under_h = chips_height(area.width, &under, &chip, boxed);
    let bottom = area.y + area.height;
    let under_area = Rect { y: bottom.saturating_sub(under_h + 1), height: under_h, ..area };
    text(f, Rect { y: bottom - 1, height: 1, ..area }, hint, dim(t));
    chips(f, under_area, &under, &chip, chosen, focused, boxed, &mut app.hits, t, &|i| lit(&rows[i], &p));
    let mid = Rect { y: area.y + top_h + 1, height: under_area.y.saturating_sub(area.y + top_h + 2), ..area };
    let on_ = p.eq_enabled && bypass.is_none();
    sliders(f, mid, &rows, &bands, &p, chosen, on_, app, t);
}

/// Whether a chip's switch is on.
fn lit(row: &EqRow, p: &nori_core::settings::StoredPrefs) -> bool {
    match row {
        EqRow::Enabled => p.eq_enabled,
        EqRow::AutoPreamp => p.eq_preamp_db.is_none(),
        EqRow::Mono => p.mono,
        EqRow::Limiter => p.limiter,
        EqRow::CrossfeedPreset | EqRow::Crossfeed => p.crossfeed_db > 0.0,
        _ => false,
    }
}

/// Rows the wrapped chips take in `w` columns.
fn chips_height(w: u16, which: &[usize], label: &dyn Fn(usize) -> String, boxed: bool) -> u16 {
    let (mut x, mut lines) = (0u16, 1u16);
    for &i in which {
        let cw = label(i).width() as u16 + if boxed { 4 } else { 2 };
        if x > 0 && x + cw > w {
            lines += 1;
            x = 0;
        }
        x += cw + 1;
    }
    if which.is_empty() {
        return 0;
    }
    lines * if boxed { 3 } else { 1 }
}

/// Wrapped chips; the selected one filled (bold only when unfocused), switches that are on in the
/// accent. Returns the rows used.
#[allow(clippy::too_many_arguments)]
fn chips(f: &mut Frame, area: Rect, which: &[usize], label: &dyn Fn(usize) -> String, chosen: usize, focused: bool, boxed: bool, hits: &mut Vec<(Rect, Hit)>, t: &Theme, on_: &dyn Fn(usize) -> bool) -> u16 {
    let line_h = if boxed { 3 } else { 1 };
    let (mut x, mut y) = (area.x, area.y);
    for &i in which {
        let text = label(i);
        let cw = text.width() as u16 + if boxed { 4 } else { 2 };
        if x > area.x && x + cw > area.x + area.width {
            x = area.x;
            y += line_h;
        }
        if y + line_h > area.y + area.height.max(line_h) {
            break;
        }
        let r = Rect { x, y, width: cw.min(area.width), height: line_h };
        let is = i == chosen;
        let fg = if is && focused { on(t.accent) } else if is || on_(i) { t.accent } else { t.text };
        let mut style = Style::default().fg(fg);
        if is && focused {
            style = style.bg(t.accent).add_modifier(Modifier::BOLD);
        } else if is {
            style = style.add_modifier(Modifier::BOLD);
        }
        if boxed {
            let border = if is { Style::default().fg(t.accent) } else { dim(t) };
            let block = Block::default().borders(Borders::ALL).border_type(if is { BorderType::Thick } else { BorderType::Rounded }).border_style(border);
            let inner = block.inner(r);
            put(f, block, r);
            put(f, Paragraph::new(Span::styled(format!(" {text} "), style)), inner);
        } else {
            put(f, Paragraph::new(Span::styled(format!(" {text} "), style)), r);
        }
        hits.push((r, Hit::Row(ListRef::Eq, i)));
        x += cw + 1;
    }
    (y + line_h).saturating_sub(area.y)
}

/// Band faders: a track filled from 0 dB to the knob, frequency and gain below.
#[allow(clippy::too_many_arguments)]
fn sliders(f: &mut Frame, area: Rect, rows: &[EqRow], bands: &[usize], p: &nori_core::settings::StoredPrefs, chosen: usize, on_: bool, app: &mut App, t: &Theme) {
    if bands.is_empty() || area.height < 6 || area.width < 16 {
        return;
    }
    let range = nori_core::settings::EQ_RANGES.gain.max;
    // An odd height so 0 dB gets its own middle row; two rows below for labels.
    let h = (area.height - 2).min(25);
    let h = if h % 2 == 0 { h - 1 } else { h };
    let mid = h / 2;
    let top = area.y;
    app.eq_track = (top, h);
    let n = bands.len() as u16;
    let col = ((area.width.saturating_sub(6)) / n).clamp(4, 10);
    let x0 = area.x + 6 + (area.width.saturating_sub(6 + col * n)) / 2;
    for (y, s) in [(0, format!("+{range:.0}")), (mid, "0 dB".into()), (h - 1, format!("-{range:.0}"))] {
        put(f, Paragraph::new(Span::styled(format!("{s:>5}"), dim(t))), Rect { x: area.x, y: top + y, width: 5, height: 1 });
    }
    let line_w = col * n;
    put(f, Paragraph::new(Span::styled("┈".repeat(line_w as usize), dim(t))), Rect { x: x0, y: top + mid, width: line_w, height: 1 });
    for (k, &i) in bands.iter().enumerate() {
        let Some((_, gain)) = rows[i].band(p) else { continue };
        let x = x0 + k as u16 * col;
        let c = x + (col - 1) / 2;
        let is = i == chosen;
        let fill_colour = if !on_ { t.dim } else if is { t.accent } else { t.text };
        let knob_y = (mid as f32 - (gain / range).clamp(-1.0, 1.0) * mid as f32).round() as u16;
        for y in 0..h {
            let (sym, style) = if y == knob_y {
                (if col >= 5 { "━●━" } else { "●" }, Style::default().fg(if is { t.accent } else { fill_colour }).add_modifier(Modifier::BOLD))
            } else if (y > knob_y && y < mid) || (y < knob_y && y > mid) || (y == mid && knob_y != mid) {
                ("┃", Style::default().fg(fill_colour))
            } else {
                ("│", dim(t))
            };
            let w = sym.width() as u16;
            put(f, Paragraph::new(Span::styled(sym, style)), Rect { x: c + 1 - w.div_ceil(2), y: top + y, width: w, height: 1 });
        }
        let (name, _) = rows[i].words(p);
        let label_style = if is { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { dim(t) };
        let cw = (col - 1) as usize;
        let centre = |s: &str| format!("{:^cw$}", fit(s, cw));
        put(f, Paragraph::new(Span::styled(centre(&name), label_style)), Rect { x, y: top + h, width: col - 1, height: 1 });
        let value = crate::text::signed_db(gain);
        let value_style = if is { Style::default().fg(t.accent) } else { dim(t) };
        put(f, Paragraph::new(Span::styled(centre(&value), value_style)), Rect { x, y: top + h + 1, width: col - 1, height: 1 });
        app.hits.push((Rect { x, y: top, width: col, height: h + 2 }.intersection(area), Hit::Row(ListRef::Eq, i)));
    }
}

fn settings(f: &mut Frame, area: Rect, app: &mut App, t: &Theme, focused: bool) {
    let prefs = app.prefs.clone();
    let own = &app.settings.own;
    app.settings.own = settings_view::Own { mouse: app.mouse, images: app.images, card_covers: app.card_covers, protocol: app.protocol.to_string(), data: own.data.clone(), device: own.device.clone() };
    let body = heading(f, area, "Settings", "enter switches or opens · ← → change · [ ] groups", t);
    let App { settings: v, hits, .. } = app;
    let pages = v.pages(&prefs).to_vec();
    let lines = SettingsView::lines(&pages);
    let len = lines.len();
    if v.row.at == 0 && len > 0 {
        v.skip_titles(true);
    }
    // Group index on the left when wide enough.
    let body = if body.width >= 96 {
        let at = v.group_at();
        for (g, group) in settings_view::GROUPS.iter().enumerate() {
            let r = Rect { x: body.x, y: body.y + g as u16, width: 18, height: 1 };
            if r.y >= body.y + body.height {
                break;
            }
            let style = if g == at { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { dim(t) };
            let mark = if g == at { "▌ " } else { "  " };
            put(f, Paragraph::new(Line::from(vec![Span::styled(mark, Style::default().fg(t.accent)), Span::styled(group.title, style)])), r);
            hits.push((r, Hit::Group(g)));
        }
        Rect { x: body.x + 20, width: (body.width - 20).min(110), ..body }
    } else {
        Rect { width: body.width.min(110), ..body }
    };
    list(f, body, &mut v.row, len, ListRef::Settings, hits, t, focused, &|i, w| setting_line(&lines[i], w, t));
}

/// One settings page line.
fn setting_line(line: &SLine<'_>, w: usize, t: &Theme) -> Line<'static> {
    let row = match line {
        SLine::Group(title) => return Line::from(vec![Span::styled(format!("{title}  "), Style::default().fg(t.accent).add_modifier(Modifier::BOLD)), Span::styled("─".repeat(w.saturating_sub(title.width() + 3)), dim(t))]),
        SLine::Title(title) => return Line::from(Span::styled(format!("  {title}"), bold().fg(t.text))),
        SLine::Row(r) => *r,
    };
    let (title, detail) = settings_view::row_words(row);
    let live = settings_view::row_enabled(row);
    let text_style = if live { Style::default().fg(t.text) } else { dim(t) };
    if let settings_view::Row::Palette { colours, chosen, .. } = row {
        let mut spans = vec![Span::styled("    Accent color  ", text_style)];
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
        bar.push(' ');
        return spread(vec![Span::styled(format!("    {title}"), text_style)], Span::styled(bar, Style::default().fg(t.accent)), w);
    }
    if title.is_empty() && !detail.is_empty() {
        return Line::from(Span::styled(fit(&format!("    {detail}"), w).into_owned(), dim(t).add_modifier(Modifier::ITALIC)));
    }
    let value = match row {
        settings_view::Row::Toggle { on, .. } | settings_view::Row::Ranked { on, .. } => (if *on { "━━●" } else { "○──" }).to_string(),
        _ => settings_view::row_value(row),
    };
    let mut left = vec![Span::styled(format!("    {title}"), text_style)];
    if !detail.is_empty() {
        left.push(Span::styled(format!("  {detail}"), dim(t)));
    }
    let value_style = if live { Style::default().fg(t.accent) } else { dim(t) };
    spread(left, Span::styled(value + " ", value_style), w)
}

// ---- the panel on the right ----

/// The right panel in the cover's colours: tabs, then the chosen panel.
fn right_panel(f: &mut Frame, area: Rect, app: &mut App, art: Option<&mut Art>) {
    let Some(which) = app.panel else { return };
    if area.width < 10 || area.height < 4 {
        return;
    }
    let t = app.theme;
    match t.page {
        Some(page) => put(f, Block::default().style(Style::default().bg(page).fg(t.text)), area),
        None => {
            for y in area.top()..area.bottom() {
                put(f, Paragraph::new(Span::styled("│", dim(&t))), Rect { x: area.x, y, width: 1, height: 1 });
            }
        }
    }
    let area = Rect { x: area.x + 2, width: area.width.saturating_sub(3), y: area.y + 1, height: area.height.saturating_sub(1) };
    let focused = app.focus == Focus::Panel;
    let mut x = area.x;
    for (p, name) in PANELS {
        let chosen = p == which;
        let style = if chosen && focused {
            Style::default().bg(t.accent).fg(on(t.accent)).add_modifier(Modifier::BOLD)
        } else if chosen {
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            dim(&t)
        };
        let label = format!(" {name} ");
        let w = label.width() as u16;
        if x + w > area.x + area.width {
            break;
        }
        let r = Rect { x, y: area.y, width: w, height: 1 };
        put(f, Paragraph::new(Span::styled(label, style)), r);
        app.hits.push((r, Hit::Button(Button::Panel(p))));
        x += w + 1;
    }
    let body = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
    match which {
        Panel::Playing => now_playing(f, body, app, art, focused),
        Panel::Queue => queue(f, body, app, focused),
        Panel::Lyrics => lyrics(f, body, app),
    }
}

fn now_playing(f: &mut Frame, area: Rect, app: &mut App, art: Option<&mut Art>, focused: bool) {
    let t = app.theme;
    let Some(song) = app.song.clone() else {
        let lines = vec![Line::from(Span::styled("♫", Style::default().fg(t.accent))), Line::from(""), Line::from(Span::styled("Nothing playing", bold())), Line::from(Span::styled("Pick something and press enter", dim(&t)))];
        put(f, Paragraph::new(lines).alignment(Alignment::Center), centred(area, area.width, 4));
        return;
    };
    let mut y = area.y;
    if app.images && area.height >= 16 {
        let rows = (area.width / 2).min(area.height / 2);
        let c = Rect { x: area.x + (area.width - rows * 2) / 2, y, width: rows * 2, height: rows };
        cover(f, c, art, song.cover_art.as_deref(), &t);
        y += rows + 1;
    }
    let w = area.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(fit(&song.title, w).into_owned(), bold().fg(t.text))),
        Line::from(Span::styled(fit(&song.artist, w).into_owned(), Style::default().fg(t.accent))),
    ];
    let mut facts = Vec::new();
    if !song.album.is_empty() {
        facts.push(song.album.clone());
    }
    if song.year > 0 {
        facts.push(song.year.to_string());
    }
    lines.push(Line::from(Span::styled(fit(&facts.join(" · "), w).into_owned(), dim(&t))));
    if let Some(q) = crate::text::quality(&song) {
        lines.push(Line::from(Span::styled(fit(&q, w).into_owned(), dim(&t))));
    }
    lines.push(Line::from(""));
    lines.extend(mix_lines(app, w));
    let n = lines.len() as u16;
    put(f, Paragraph::new(lines), Rect { y, height: n.min((area.y + area.height).saturating_sub(y)), ..area });
    y += n + 1;
    if y + 2 >= area.y + area.height {
        return;
    }
    up_next(f, Rect { y, height: (area.y + area.height).saturating_sub(y), ..area }, app, focused, true);
}

/// "Next up" and the upcoming songs.
fn up_next(f: &mut Frame, area: Rect, app: &mut App, focused: bool, say_end: bool) {
    let t = app.theme;
    text(f, area, "Next up", bold().fg(t.text));
    let body = Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area };
    let upcoming = app.up_next();
    let App { queue, up_next_sel, hits, .. } = app;
    let Some(q) = queue.as_ref() else { return };
    if upcoming.is_empty() && say_end {
        return text(f, body, "The queue ends here", dim(&t));
    }
    list(f, body, up_next_sel, upcoming.len(), ListRef::UpNext, hits, &t, focused, &|row, w| match q.songs.get(upcoming[row]) {
        Some(s) => spread(vec![Span::styled(s.title.clone(), Style::default().fg(t.text)), Span::styled(format!("  {}", s.artist), dim(&t))], Span::styled(clock(s.duration as i64 * 1000), dim(&t)), w),
        None => Line::from(""),
    });
}

/// The current mix and the planned transition.
fn mix_lines(app: &App, w: usize) -> Vec<Line<'static>> {
    let t = app.theme;
    let mut lines = Vec::new();
    // While mixing, `song` is the louder of the two.
    if app.now.mixing {
        let from = app.mixed_in.as_ref().and_then(|n| crate::backend::app().song(&n.outgoing_id)).map(|s| s.title);
        let heard = match &from {
            Some(from) => format!("Mixing in from “{from}”"),
            None => "Mixing into the next".to_string(),
        };
        lines.push(Line::from(vec![Span::styled("◇ ", Style::default().fg(t.accent)), Span::styled(fit(&heard, w.saturating_sub(2)).into_owned(), Style::default().fg(t.text))]));
        if let Some(n) = &app.mixed_in {
            lines.push(Line::from(Span::styled(fit(&format!("  {}", how_mixed(n)), w).into_owned(), dim(&t))));
        }
    }
    if let Some(n) = &app.transition {
        let next = crate::backend::app().song(&n.incoming_id).map(|s| s.title).unwrap_or_default();
        lines.push(Line::from(vec![Span::styled("⇢ ", Style::default().fg(t.accent)), Span::styled(fit(&format!("Next: “{next}”"), w.saturating_sub(2)).into_owned(), Style::default().fg(t.text))]));
        lines.push(Line::from(Span::styled(fit(&format!("  {}", how_mixed(n)), w).into_owned(), dim(&t))));
        if !n.reason.is_empty() {
            lines.push(Line::from(Span::styled(fit(&format!("  {}", n.reason), w).into_owned(), dim(&t))));
        }
    } else if app.prefs.auto_mix || app.prefs.crossfade_sec > 0 {
        let what = if app.prefs.auto_mix { "AutoMix" } else { "Crossfade" };
        lines.push(Line::from(Span::styled(fit(&format!("⇢ {what} on · planned as this plays"), w).into_owned(), dim(&t))));
    }
    lines
}

/// A transition in words: kind, length, start and tempo ratio.
fn how_mixed(n: &nori_core::automix::planner::TransitionNote) -> String {
    if n.duration_ms <= 0 {
        return "Gapless".to_string();
    }
    let tempo = if (n.tempo_ratio - 1.0).abs() > 0.001 { format!(", tempo ×{:.3}", n.tempo_ratio) } else { String::new() };
    format!("{}, {:.1} s from {}{tempo}", words_kind(&n.kind), n.duration_ms as f32 / 1000.0, clock(n.start_ms))
}

/// A planner transition kind in words.
fn words_kind(kind: &str) -> &'static str {
    match kind {
        "BeatMatched" => "AutoMix: beat-matched mix",
        "EchoOut" => "AutoMix: echo out",
        "MixRampFade" => "AutoMix: fade",
        "EqualPowerFade" => "Crossfade",
        _ => "Transition",
    }
}

fn queue(f: &mut Frame, area: Rect, app: &mut App, focused: bool) {
    let t = app.theme;
    let order = app.queue_order();
    let Some(q) = app.queue.as_ref().filter(|q| q.len > 0) else {
        let lines = vec![Line::from(Span::styled("The queue is empty", bold())), Line::from(Span::styled("enter plays, a adds to the queue", dim(&t)))];
        put(f, Paragraph::new(lines).alignment(Alignment::Center), centred(area, area.width, 2));
        return;
    };
    let secs: u64 = q.songs.iter().map(|s| s.duration as u64).sum();
    let mut modes = String::new();
    if q.shuffle {
        modes.push_str(" · shuffle");
    }
    match q.repeat {
        crate::app::REPEAT_ONE => modes.push_str(" · repeat one"),
        crate::app::REPEAT_ALL => modes.push_str(" · repeat all"),
        _ => {}
    }
    text(f, Rect { height: 1, ..area }, &format!("{}{modes}", crate::text::songs_caption(q.len, secs)), dim(&t));
    let body = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
    let App { queue, queue_sel, hits, .. } = app;
    let q = queue.as_ref().expect("checked");
    let current = q.index;
    let by_hand: std::collections::HashSet<u32> = q.queued.iter().copied().collect();
    list(f, body, queue_sel, order.len(), ListRef::Queue, hits, &t, focused, &|row, w| {
        let i = order[row];
        let Some(s) = q.songs.get(i) else { return Line::from("") };
        let playing = i as i32 == current;
        let mark = if playing { "▶ " } else if by_hand.contains(&(i as u32)) { "+ " } else { "  " };
        let title_style = if playing { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(t.text) };
        spread(
            vec![Span::styled(mark, Style::default().fg(t.accent)), Span::styled(s.title.clone(), title_style), Span::styled(format!("  {}", s.artist), dim(&t))],
            Span::styled(clock(s.duration as i64 * 1000), dim(&t)),
            w,
        )
    });
}

/// `s` word-wrapped to `w` columns.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split(' ') {
        let ww = word.width();
        if !line.is_empty() && line.width() + 1 + ww > w {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        while line.width() > w {
            let cut = fit(&line, w).trim_end_matches('…').to_string();
            let rest = line[cut.len()..].to_string();
            if cut.is_empty() {
                break;
            }
            out.push(cut);
            line = rest;
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
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
    // After the last line `active` is past the end; stay on the last line.
    let focus = app.lyrics_sel.unwrap_or((active.max(0) as usize).min(l.pick.lyrics.lines.len().saturating_sub(1)));
    let credit = l.credit();
    let body = if credit.is_some() { Rect { height: area.height.saturating_sub(1), ..area } } else { area };
    let w = body.width as usize;
    // A lyric line's rows: text (0), backing vocals (1), translation (2), each wrapped.
    let block = |i: usize| -> Vec<(String, u8)> {
        let line = &l.pick.lyrics.lines[i];
        let mut v: Vec<(String, u8)> = wrap(&line.text, w.saturating_sub(2)).into_iter().map(|s| (s, 0)).collect();
        if !line.backing.is_empty() {
            v.extend(wrap(&format!("({})", line.backing), w.saturating_sub(2)).into_iter().map(|s| (s, 1)));
        }
        if translate {
            if let Some(tr) = line.translation.as_ref().filter(|x| !x.is_empty()) {
                v.extend(wrap(tr, w.saturating_sub(2)).into_iter().map(|s| (s, 2)));
            }
        }
        v
    };
    // The focused line sits a third of the way down, as on Android.
    let anchor = body.y + body.height / 3;
    let mut y = anchor as i32;
    let mut i = focus as i32;
    while i > 0 && y > body.y as i32 {
        i -= 1;
        y -= block(i as usize).len() as i32 + 1;
    }
    app.hits.push((body, Hit::List(ListRef::Lyrics)));
    let mut at = y;
    for n in (i.max(0) as usize)..l.pick.lyrics.lines.len() {
        if at >= (body.y + body.height) as i32 {
            break;
        }
        let strength = nori_look::lyrics::line_strength(synced, n as i32, active);
        let lit = t.lit(strength);
        let is_active = synced && n as i32 == active;
        let mut sung = if is_active && sweep { l.sung_chars(n, frame.sung) } else { 0 };
        for (part, kind) in block(n) {
            let spans = match kind {
                0 if is_active && sweep => {
                    let chars = part.chars().count();
                    let cut_at = sung.min(chars);
                    sung = sung.saturating_sub(chars + 1);
                    let cut = part.char_indices().nth(cut_at).map_or(part.len(), |(b, _)| b);
                    vec![
                        Span::styled(part[..cut].to_string(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD)),
                        Span::styled(part[cut..].to_string(), Style::default().fg(t.lit(nori_look::lyrics::UNSUNG)).add_modifier(Modifier::BOLD)),
                    ]
                }
                0 if is_active => vec![Span::styled(part, Style::default().fg(t.accent).add_modifier(Modifier::BOLD))],
                0 => vec![Span::styled(part, Style::default().fg(lit))],
                1 => vec![Span::styled(part, Style::default().fg(t.lit(strength * 0.7)).add_modifier(Modifier::ITALIC))],
                _ => vec![Span::styled(part, Style::default().fg(t.lit(strength * 0.8)))],
            };
            let mut row = Line::from(spans).alignment(Alignment::Center);
            if app.lyrics_sel == Some(n) {
                row = row.style(Style::default().add_modifier(Modifier::UNDERLINED));
            }
            if at >= body.y as i32 && at < (body.y + body.height) as i32 {
                let r = Rect { x: body.x, y: at as u16, width: body.width, height: 1 };
                put(f, Paragraph::new(row), r);
                app.hits.push((r, Hit::Row(ListRef::Lyrics, n)));
            }
            at += 1;
        }
        at += 1;
    }
    if let Some(c) = credit {
        text(f, Rect { y: area.y + area.height - 1, height: 1, ..area }, &c, dim(&t));
    }
}

// ---- the full player ----

/// The full-window player: large cover, song info, and lyrics or the up-next list.
fn full_player(f: &mut Frame, area: Rect, app: &mut App, art: Option<&mut Art>) {
    let t = app.theme;
    if let Some(page) = t.page {
        put(f, Block::default().style(Style::default().bg(page).fg(t.text)), area);
    }
    if area.width < 10 || area.height < 4 {
        return;
    }
    let Some(song) = app.song.clone() else {
        return empty(f, area, &t, "Nothing playing · F or esc goes back");
    };
    let inner = Rect { x: area.x + 3, y: area.y + 1, width: area.width.saturating_sub(6), height: area.height.saturating_sub(2) };
    let wide = inner.width >= 90 && app.images;
    let mut words = inner;
    if wide {
        let rows = inner.height.min(inner.width / 4);
        let c = Rect { x: inner.x, y: inner.y + (inner.height - rows) / 2, width: rows * 2, height: rows };
        cover(f, c, art, song.cover_art.as_deref(), &t);
        words = Rect { x: c.x + c.width + 4, width: inner.width.saturating_sub(c.width + 4), ..inner };
    }
    let w = words.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(fit(&song.title, w).into_owned(), bold().fg(t.text))),
        Line::from(Span::styled(fit(&song.artist, w).into_owned(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(fit(&[song.album.as_str(), &if song.year > 0 { song.year.to_string() } else { String::new() }].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · "), w).into_owned(), dim(&t))),
    ];
    lines.extend(mix_lines(app, w));
    let n = lines.len() as u16;
    put(f, Paragraph::new(lines), Rect { height: n.min(words.height), ..words });
    let rest = Rect { y: words.y + n + 1, height: words.height.saturating_sub(n + 1), ..words };
    if app.lyrics.as_ref().is_some_and(|l| !l.pick.lyrics.lines.is_empty()) || app.lyrics_for.is_some() {
        lyrics(f, rest, app);
    } else if rest.height >= 3 {
        up_next(f, rest, app, false, false);
    }
}

// ---- the player bar ----

/// The player bar: song on the left, controls and seek bar in the middle, panel switches and volume
/// on the right.
fn player_bar(f: &mut Frame, area: Rect, app: &mut App, ui: &Theme) {
    let t = *ui;
    put(f, Paragraph::new(Span::styled("─".repeat(area.width as usize), dim(&t))), Rect { height: 1, ..area });
    let l1 = Rect { y: area.y + 1, height: 1, ..area };
    let l2 = Rect { y: area.y + 2, height: 1, ..area };
    let w = area.width;
    let wide = w >= 100;
    let left_w = if wide { (w / 4).min(40) } else if w >= 60 { w / 4 } else { 0 };
    let right_w = if wide { 30 } else { 0 };
    let mid = Rect { x: area.x + left_w, width: w.saturating_sub(left_w + right_w), ..area };
    if left_w > 4 {
        let lw = left_w as usize - 2;
        match &app.song {
            Some(s) => {
                let heart = if s.starred { "♥ " } else { "♡ " };
                let hr = Rect { x: area.x + 1, width: 2, ..l1 };
                put(f, Paragraph::new(Span::styled(heart, Style::default().fg(if s.starred { t.accent } else { t.dim }))), hr);
                app.hits.push((hr, Hit::Button(Button::StarSong)));
                put(f, Paragraph::new(Span::styled(fit(&s.title, lw.saturating_sub(2)).into_owned(), bold())), Rect { x: area.x + 3, width: left_w.saturating_sub(3), ..l1 });
                text(f, Rect { x: area.x + 3, width: left_w.saturating_sub(3), ..l2 }, &s.artist, dim(&t));
            }
            None => text(f, Rect { x: area.x + 1, width: left_w - 1, ..l1 }, "Nothing playing", dim(&t)),
        }
    }
    let playing = app.now.state == State::Playing;
    let lit = |on: bool| if on { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { dim(&t) };
    let repeat = if app.repeat() == crate::app::REPEAT_ONE { "↻¹" } else { "↻" };
    let toggle = if app.now.buffering { " ⋯ " } else if playing { " ⏸ " } else { " ▶ " };
    let controls: [(&str, Style, Button); 5] = [
        ("⤮", lit(app.queue_shuffled()), Button::Shuffle),
        ("⏮", Style::default().fg(t.text), Button::Previous),
        (toggle, Style::default().bg(t.accent).fg(on(t.accent)).add_modifier(Modifier::BOLD), Button::Toggle),
        ("⏭", Style::default().fg(t.text), Button::Next),
        (repeat, lit(app.repeat() != crate::app::REPEAT_OFF), Button::Repeat),
    ];
    let gap = 3u16;
    let total: u16 = controls.iter().map(|c| c.0.width() as u16).sum::<u16>() + gap * 4;
    let mut x = mid.x + mid.width.saturating_sub(total) / 2;
    for (label, style, b) in controls {
        let cw = label.width() as u16;
        if x + cw > mid.x + mid.width {
            break;
        }
        let r = Rect { x, y: l1.y, width: cw, height: 1 };
        put(f, Paragraph::new(Span::styled(label, style)), r);
        app.hits.push((Rect { x: r.x.saturating_sub(1), width: r.width + 2, ..r }, Hit::Button(b)));
        x += cw + gap;
    }
    let now = Instant::now();
    let len = app.song.as_ref().map_or(0, |s| s.duration as i64 * 1000);
    let pos = match app.drag {
        Some(crate::app::Drag::Seek(share)) => (share * len as f32) as i64,
        _ => app.now.position(now).clamp(0, len.max(0)),
    };
    let (a, b) = (clock(pos), clock(len));
    let bar_w = mid.width.saturating_sub(a.width() as u16 + b.width() as u16 + 6).min(80);
    let total = bar_w + a.width() as u16 + b.width() as u16 + 2;
    let mut x = mid.x + mid.width.saturating_sub(total) / 2;
    text(f, Rect { x, width: a.width() as u16, ..l2 }, &a, dim(&t));
    x += a.width() as u16 + 1;
    if bar_w >= 4 {
        let share = if len > 0 { pos as f32 / len as f32 } else { 0.0 };
        let filled = ((share * bar_w as f32) as u16).min(bar_w.saturating_sub(1));
        let knob = if app.song.is_some() { "●" } else { "─" };
        let bar = Line::from(vec![
            Span::styled("━".repeat(filled as usize), Style::default().fg(t.accent)),
            Span::styled(knob, Style::default().fg(t.accent)),
            Span::styled("─".repeat((bar_w - filled - 1) as usize), dim(&t)),
        ]);
        let r = Rect { x, width: bar_w, ..l2 };
        put(f, Paragraph::new(bar), r);
        app.seek_rect = r;
        app.hits.push((r, Hit::Seek));
        x += bar_w + 1;
    }
    text(f, Rect { x, width: b.width() as u16, ..l2 }, &b, dim(&t));
    if right_w > 0 {
        let rx = area.x + w - right_w;
        let mut x = rx;
        for (p, label) in [(Panel::Playing, "♫"), (Panel::Queue, "≡"), (Panel::Lyrics, "❝")] {
            let on = app.panel == Some(p) && !app.full;
            let r = Rect { x, width: 1, ..l1 };
            put(f, Paragraph::new(Span::styled(label, lit(on))), r);
            app.hits.push((Rect { x: x.saturating_sub(1), width: 3, ..r }, Hit::Button(Button::Panel(p))));
            x += 4;
        }
        let r = Rect { x, width: 1, ..l1 };
        put(f, Paragraph::new(Span::styled("⤢", lit(app.full))), r);
        app.hits.push((Rect { x: x.saturating_sub(1), width: 3, ..r }, Hit::Button(Button::Full)));
        let vw = 12u16;
        let label = format!("{:>3}%", (app.volume * 100.0).round() as i32);
        let icon = if app.volume <= 0.0 { "mute" } else { "vol" };
        text(f, Rect { x: rx, width: 5, ..l2 }, icon, dim(&t));
        let filled = ((app.volume * vw as f32).round() as u16).min(vw);
        let bar = Line::from(vec![Span::styled("━".repeat(filled as usize), Style::default().fg(t.text)), Span::styled("─".repeat((vw - filled) as usize), dim(&t))]);
        let vr = Rect { x: rx + 5, width: vw, ..l2 };
        put(f, Paragraph::new(bar), vr);
        app.volume_rect = vr;
        app.hits.push((vr, Hit::Volume));
        text(f, Rect { x: rx + 6 + vw, width: 5, ..l2 }, &label, dim(&t));
    }
}

// ---- the login and what goes on top ----

fn login(f: &mut Frame, area: Rect, app: &mut App, t: &Theme) {
    let servers = app.prefs.servers.len() as u16;
    let h = 17 + if servers > 0 { servers + 2 } else { 0 };
    let r = centred(area, 64, h);
    let inner = popup(f, r, Span::styled(" ♫ nori · connect to a server ", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)), t);
    let l = &app.login;
    let bottom = inner.y + inner.height;
    let mut y = inner.y + 1;
    text(f, Rect { x: inner.x + 2, y, width: inner.width.saturating_sub(4), height: 1 }, "Navidrome, octo-fiesta or any Subsonic server", dim(t));
    y += 2;
    for (i, label) in LOGIN_FIELDS.iter().enumerate() {
        if y >= bottom {
            break;
        }
        let focused = l.focus == i && !l.on_list;
        let value = if i == 3 { "•".repeat(l.fields[i].chars().count()) } else { l.fields[i].clone() };
        let cursor = if focused { "▏" } else { "" };
        let line = Line::from(vec![
            Span::styled(format!("  {label:<16}"), if focused { Style::default().fg(t.accent).add_modifier(Modifier::BOLD) } else { dim(t) }),
            Span::styled(format!("{value}{cursor}"), Style::default().fg(t.text)),
        ]);
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        put(f, Paragraph::new(line), row);
        app.hits.push((row, Hit::LoginField(i)));
        y += 2;
    }
    let button = if l.busy { " Connecting… " } else { " Connect " };
    let br = Rect { x: inner.x + 18, y, width: button.width() as u16, height: 1 };
    put(f, Paragraph::new(Span::styled(button, Style::default().bg(t.accent).fg(on(t.accent)).add_modifier(Modifier::BOLD))), br);
    app.hits.push((br, Hit::Button(Button::Connect)));
    y += 2;
    let msg = match &l.error {
        Some(e) => Span::styled(format!("  {e}"), Style::default().fg(Color::LightRed)),
        None => Span::styled("  tab next field · enter connect · esc back", dim(t)),
    };
    put(f, Paragraph::new(msg).wrap(ratatui::widgets::Wrap { trim: false }), Rect { x: inner.x, y, width: inner.width, height: 2.min(bottom.saturating_sub(y)) });
    y += 3;
    if servers > 0 && y + 1 < bottom {
        text(f, Rect { x: inner.x + 2, y, width: inner.width.saturating_sub(2), height: 1 }, "Or use a saved server", Style::default().fg(t.accent).add_modifier(Modifier::BOLD));
        y += 1;
        let area = Rect { x: inner.x + 2, y, width: inner.width.saturating_sub(4), height: servers.min(bottom - y) };
        let App { login, prefs, hits, .. } = app;
        let names: Vec<String> = prefs.servers.iter().map(|s| format!("{}  {}", nori_core::settings::label(&s.name, &s.url), s.user)).collect();
        let focused = login.on_list;
        list(f, area, &mut login.sel, names.len(), ListRef::Profiles, hits, t, focused, &|i, w| Line::from(fit(&names[i], w).into_owned()));
    }
}

fn overlay(f: &mut Frame, area: Rect, app: &mut App, t: &Theme) {
    let App { overlay, hits, .. } = app;
    let Some(o) = overlay else { return };
    match o {
        Overlay::Help { scroll } => {
            let r = centred(area, 88, area.height.saturating_sub(4));
            let inner = popup(f, r, Span::styled(" Keys · any key closes ", Style::default().fg(t.accent).add_modifier(Modifier::BOLD)), t);
            let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
            let mut lines = Vec::new();
            for scope in Scope::ALL {
                lines.push(Line::from(Span::styled(scope.title(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD))));
                for b in BINDINGS.iter().filter(|b| b.scope == scope && !b.label.is_empty()) {
                    lines.push(Line::from(vec![Span::styled(format!("  {:<22}", b.label), Style::default().fg(t.text)), Span::styled(b.help, dim(t))]));
                }
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled("The mouse: click the sidebar, cards, rows and buttons; click or drag the seek bar and the volume; the wheel scrolls.", dim(t))));
            *scroll = (*scroll).min(lines.len().saturating_sub(inner.height as usize));
            put(f, Paragraph::new(lines).scroll((*scroll as u16, 0)), inner);
            hits.push((r, Hit::List(ListRef::Help)));
        }
        Overlay::Picker { title, options, sel, .. } => {
            let h = (options.len() as u16 + 2).min(area.height.saturating_sub(4));
            let w = options.iter().map(|o| o.0.width()).max().unwrap_or(10).max(title.width()) as u16 + 8;
            let r = centred(area, w, h);
            let inner = popup(f, r, Span::styled(format!(" {title} "), Style::default().fg(t.accent)), t);
            hits.push((r, Hit::List(ListRef::Picker)));
            let opts = options.clone();
            list(f, inner, sel, opts.len(), ListRef::Picker, hits, t, true, &|i, w| Line::from(fit(&format!(" {}", opts[i].0), w).into_owned()));
        }
        Overlay::Input { title, text: typed, secret, .. } => {
            let r = centred(area, 60, 3);
            let inner = popup(f, r, Span::styled(format!(" {title} · enter keeps, esc drops "), Style::default().fg(t.accent)), t);
            let shown = if *secret { "•".repeat(typed.chars().count()) } else { typed.clone() };
            put(f, Paragraph::new(format!(" {shown}▏")), inner);
        }
    }
}
