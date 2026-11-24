//! The SSTV screen (chapter 27a): the picture scaled to 240 wide at the top (240 x 192 for 256 lines), drawn from
//! the SPI flash a few dozen rows a pass, and as a received picture's lines come in; what it is below. ◀▶ step
//! through the pictures, OK opens Send (the knob picks the mode, OK sends), # picks the receive mode (Auto, or one by hand: SSTV-2.5).

use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::ChannelStore;
use crate::keys::{KeyEvent, Press};
use crate::sstv::{headers, mode_secs, views, SEND_MODES, VIEW_CARD};
use crate::slot::Item;
use crate::{App, Screen};
use ferrite_core::keys::Key;
use ferrite_core::sstv::{self, Header, HEADER_LEN, LINES, ROW_LEN, SLOTS, TEST_CARD, WIDTH};
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::{Buf, Bytes};
use ferrite_ui::widgets as w;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sstv {
    /// The Send page: the mode picked (an index into [`SEND_MODES`]).
    pub send: Option<u8>,
}

/// The picture area: its top and its height (256 lines at three quarters).
const TOP: i32 = 22;
const H: i32 = 192;
/// SSTV-5.5: the picture while sending, half size: 160 x 128 at x 40.
const TX_W: i32 = 160;
const TX_H: i32 = 128;
const TX_X: i32 = 40;
/// A picture coming in: the source rows drawn again each time more arrive (more than come between two passes).
const RECENT: i32 = 16;

/// SSTV-1: refused while transmitting or repeating, as the satellite screen (SAT-1); a running scan stops.
pub fn open(app: &mut App) -> Screen {
    if app.radio.keyed.is_some() || app.radio.mode == super::activity::Mode::Xb {
        app.show_toast(b"SSTV", if app.radio.keyed.is_some() { b"not keyed" } else { b"stop repeater first" }, false);
        return app.screen;
    }
    app.sstv_enter();
    Screen::Sstv(Sstv { send: None })
}

/// The header of slot `s` (None: empty, or the test card).
fn header(st: &impl ChannelStore, s: u8) -> Option<Header> {
    if s >= SLOTS {
        return None;
    }
    let mut b = [0xFFu8; HEADER_LEN];
    st.data().data_read(sstv::slot_addr(s), &mut b).then(|| Header::decode(&b)).flatten()
}

impl Sstv {
    fn step_view(app: &mut App, st: &impl ChannelStore, d: i32) {
        let hs = headers(st);
        let mut v = [0u8; SLOTS as usize + 1];
        let n = views(&hs, &mut v);
        let at = v[..n].iter().position(|&x| x == app.sstv.view).unwrap_or(0) as i32;
        app.sstv.view = v[(at + d).rem_euclid(n as i32) as usize];
    }

    /// The picture's lines that can be drawn now: a received one's so far (the last a row behind, for the store
    /// to write it), a stored one's, the test card's 256.
    fn avail(app: &App, st: &impl ChannelStore) -> (u16, u16) {
        let v = app.sstv.view;
        if v == VIEW_CARD {
            return (LINES as u16, LINES as u16);
        }
        if let Some(rx) = app.sstv.rx.filter(|r| r.slot == v) {
            let total = ferrite_core::usstv::Mode::by_vis(rx.vis).map_or(LINES as u16, |m| m.lines);
            return (rx.lines.saturating_sub(1), total);
        }
        match header(st, v) {
            Some(h) => {
                // SSTV-3.2: a picture just ended whose header isn't finished yet: the lines it ended with
                let l = h.lines.or((app.sstv.ended.0 == v && v != 0).then_some(app.sstv.ended.1)).unwrap_or(0);
                (l, l)
            }
            None => (0, 0),
        }
    }

    /// Rows `from..to` of the picture area: source row `d * 4 / 3`, source column `x * 4 / 3`.
    fn draw_rows(app: &App, lcd: &mut impl Panel, st: &impl ChannelStore, from: i32, to: i32) {
        let v = app.sstv.view;
        let call = app.set.callsign.as_bytes();
        let mut src = [0u8; ROW_LEN];
        let mut px = [0u16; 240];
        // only the rows of the band being drawn (`banded`: each band calls this again)
        let (r0, r1) = lcd.rows();
        for d in from.max(r0 - TOP)..to.min(r1 - TOP + 1) {
            let sy = (d * 4 / 3) as u16;
            if v == VIEW_CARD {
                for (x, p) in px.iter_mut().enumerate() {
                    let (r, g, b) = sstv::test_card(x * 4 / 3, sy as usize, call);
                    *p = ferrite_core::usstv::color::rgb565(r, g, b);
                }
            } else {
                if !st.data().data_read(sstv::row_addr(v, sy), &mut src) {
                    return;
                }
                for (x, p) in px.iter_mut().enumerate() {
                    let sx = (x * 4 / 3).min(WIDTH - 1);
                    *p = u16::from_le_bytes([src[2 * sx], src[2 * sx + 1]]);
                }
            }
            lcd.window(0, TOP + d, 239, TOP + d);
            lcd.push_buf(&px);
        }
    }

    /// SSTV-5.5: while sending, the picture at half size (160 x 128 for 256 lines), centred: rows `from..to` of it,
    /// source row and column `d * 2` and `x * 2`.
    fn draw_small(app: &App, lcd: &mut impl Panel, st: &impl ChannelStore, from: i32, to: i32) {
        let v = app.sstv.view;
        let call = app.set.callsign.as_bytes();
        let mut src = [0u8; ROW_LEN];
        let mut px = [0u16; TX_W as usize];
        let (r0, r1) = lcd.rows();
        for d in from.max(r0 - TOP)..to.min(r1 - TOP + 1) {
            let sy = (d * 2) as u16;
            if v == VIEW_CARD {
                for (x, p) in px.iter_mut().enumerate() {
                    let (r, g, b) = sstv::test_card(x * 2, sy as usize, call);
                    *p = ferrite_core::usstv::color::rgb565(r, g, b);
                }
            } else {
                if !st.data().data_read(sstv::row_addr(v, sy), &mut src) {
                    return;
                }
                for (x, p) in px.iter_mut().enumerate() {
                    let sx = (x * 2).min(WIDTH - 1);
                    *p = u16::from_le_bytes([src[2 * sx], src[2 * sx + 1]]);
                }
            }
            lcd.window(TX_X, TOP + d, TX_X + TX_W - 1, TOP + d);
            lcd.push_buf(&px);
        }
    }

    /// SSTV-5.5: the screen while a picture goes out: the picture small, the mode with the slot and power, the TX
    /// frequency large, a progress bar, the line and the time left (TEXT, not dimmed).
    fn render_tx(app: &mut App, lcd: &mut impl Panel, st: &impl ChannelStore, t: crate::sstv::Tx) {
        let (_, total) = Self::avail(app, st);
        let height = (total as i32 / 2).min(TX_H);
        if app.dirty(R_FRAME, Mix::new().h(&0x55u8).h(&app.sstv.view).h(&app.sstv.ver).h(&total).get()) {
            ferrite_ui::canvas::banded(lcd, 0, TOP, 240, w::HINT_Y - 4 - TOP, BG, |b| Self::draw_small(app, b, st, 0, height));
            app.dirty(R_HEAD, 0);
        }
        let f = app.tuned_freq(app.active);
        let mut pw = Buf::<16>::new();
        app.item_text(Item::Power, false, &mut pw);
        let mut side = Buf::<24>::new();
        let _ = ufmt::uwrite!(side, "{} \u{b7} {}", ["A", "B", "C"][app.active as usize % 3], Bytes(pw.as_bytes()));
        let mut title = Buf::<40>::new();
        let _ = ufmt::uwrite!(title, "Sending {}", sstv::mode_name(t.vis));
        if app.dirty(R_INFO, Mix::new().h(&0x55u8).h(&title).h(&f).h(&side).get()) {
            ferrite_ui::canvas::banded(lcd, 0, TOP + TX_H + 4, 240, 62, BG, |lcd| {
                let y = TOP + TX_H + 4;
                lcd.text4(8, y + 18, title.as_bytes(), &UI_15B, TEXT, BG);
                let sw = text4_width(side.as_bytes(), &UI_12);
                lcd.text4(232 - sw, y + 17, side.as_bytes(), &UI_12, SUB, BG);
                w::freq(lcd, 8, 232, y + 54, f, &DIG_28, &DIG_17, TEXT, SUB, BG);
            });
        }
        let frames = ferrite_core::usstv::Mode::by_vis(t.vis).map_or(1, |m| m.frames).max(1);
        let line = (app.sstv.frame + 1).min(frames);
        let left = mode_secs(t.vis).saturating_sub(app.now.wrapping_sub(t.at_ms) / 1000);
        if app.dirty(R_SUB, Mix::new().h(&line).h(&frames).h(&left).get()) {
            let y = TOP + TX_H + 70;
            ferrite_ui::canvas::banded(lcd, 0, y, 240, 40, BG, |lcd| {
                lcd.fill_r(8, y + 2, 224, 8, 4, LINE);
                let wd = (224 * line as i32 / frames as i32).clamp(8, 224);
                lcd.fill_r(8, y + 2, wd, 8, 4, BLUE);
                let mut a = Buf::<24>::new();
                let _ = ufmt::uwrite!(a, "Line {} of {}", line, frames);
                lcd.text4(8, y + 30, a.as_bytes(), &UI_12, TEXT, BG);
                let mut b = Buf::<16>::new();
                let _ = ufmt::uwrite!(b, "{}:{}{} left", left / 60, if left % 60 < 10 { "0" } else { "" }, left % 60);
                let bw = text4_width(b.as_bytes(), &UI_12);
                lcd.text4(232 - bw, y + 30, b.as_bytes(), &UI_12, TEXT, BG);
            });
        }
    }

    /// The three lines under the picture: what it is (l1, l2) and the slot listened on or the mode # picked (l3).
    pub fn info(app: &App, st: &impl ChannelStore, send: Option<u8>, l1: &mut Buf<40>, l2: &mut Buf<40>, l3: &mut Buf<40>) {
        let v = app.sstv.view;
        if let Some(t) = app.sstv.tx {
            let _ = ufmt::uwrite!(l1, "Sending {}", sstv::mode_name(t.vis));
            let frames = ferrite_core::usstv::Mode::by_vis(t.vis).map_or(1, |m| m.frames);
            let left = mode_secs(t.vis).saturating_sub(app.now.wrapping_sub(t.at_ms) / 1000);
            let _ = ufmt::uwrite!(l2, "Line {} of {}, {}:{}{} left", app.sstv.frame + 1, frames, left / 60, if left % 60 < 10 { "0" } else { "" }, left % 60);
            l3.set(b"BACK stops");
            return;
        }
        if app.sstv.send.is_some() {
            l1.set(b"Keying...");
            return;
        }
        if let Some(i) = send {
            let vis = SEND_MODES[i as usize % SEND_MODES.len()];
            let _ = ufmt::uwrite!(l1, "Send in {}", sstv::mode_name(vis));
            let s = mode_secs(vis);
            let f = w::freq3(app.tuned_freq(app.active));
            let _ = ufmt::uwrite!(l2, "{}:{}{} on {}, {}", s / 60, if s % 60 < 10 { "0" } else { "" }, s % 60, ["A", "B", "C"][app.active as usize % 3], Bytes(f.as_bytes()));
            l3.set(b"OK sends, BACK goes back");
            return;
        }
        let listening = |l: &mut Buf<40>| {
            let f = w::freq3(app.tuned_freq(app.active));
            let _ = ufmt::uwrite!(l, "Listening on {}, {}", ["A", "B", "C"][app.active as usize % 3], Bytes(f.as_bytes()));
        };
        if let Some(rx) = app.sstv.rx.filter(|r| r.slot == v && r.lines == 0 && app.sstv.forced_vis().is_some()) {
            // SSTV-2.5: the mode # picked, waiting for its first sync (no picture yet)
            let _ = ufmt::uwrite!(l1, "{}, waiting for sync", sstv::mode_name(rx.vis));
            listening(l2);
        } else if let Some(rx) = app.sstv.rx.filter(|r| r.slot == v) {
            let _ = ufmt::uwrite!(l1, "{}, receiving", sstv::mode_name(rx.vis));
            let total = ferrite_core::usstv::Mode::by_vis(rx.vis).map_or(LINES as u16, |m| m.lines);
            let _ = ufmt::uwrite!(l2, "Line {} of {}", rx.lines, total);
        } else if v == VIEW_CARD {
            l1.set(b"Test card");
            l2.set(b"Drawn with your callsign");
        } else {
            match header(st, v) {
                Some(h) if h.kind == sstv::KIND_SEND => {
                    let _ = ufmt::uwrite!(l1, "To send {}", v + 1);
                    let n = h.name.iter().position(|&c| c == 0).unwrap_or(16);
                    l2.push(&h.name[..n]);
                }
                Some(h) => {
                    l1.push(sstv::mode_name(h.vis).as_bytes());
                    l1.push(b", received");
                    if h.freq != 0 {
                        l2.push(w::freq3(h.freq).as_bytes());
                    }
                    if !h.complete {
                        l2.push(if l2.is_empty() { b"incomplete" } else { b", incomplete" });
                    }
                }
                None if v < sstv::FIRST_RX => {
                    let _ = ufmt::uwrite!(l1, "To send {}: empty", v + 1);
                    l2.set(b"Console writes pictures here");
                }
                None => l1.set(b"Nothing received yet"),
            }
        }
        // SSTV-2.5: once # is pressed, the mode it picked (Auto: the VIS picks) and what the next press does
        match app.sstv.forced_vis() {
            None if app.sstv.iss => {
                let f = w::freq3(app.tuned_freq(app.active));
                let _ = ufmt::uwrite!(l3, "ISS {} FM  /  Auto", Bytes(f.as_bytes()));
            }
            None if !app.sstv.picked() => listening(l3),
            None => l3.set(b"Mode: Auto (# next)"),
            Some(vis) => {
                let _ = ufmt::uwrite!(l3, "Mode: {} (# next)", sstv::mode_name(vis));
            }
        }
    }
}

impl View for Sstv {
    fn name(self) -> &'static str {
        "SSTV"
    }

    fn key(self, app: &mut App, e: KeyEvent, st: &impl ChannelStore) {
        use Press::*;
        // SSTV-5.4: BACK stops a picture going out
        if app.sstv.tx.is_some() || app.sstv.send.is_some() {
            if (e.key, e.press) == (Key::Exit, Short) {
                app.sstv.stop = true;
            }
            return;
        }
        if let Some(i) = self.send {
            match (e.key, e.press) {
                (Key::Left | Key::Up, Short | Repeat) => return self.knob(app, -1, st),
                (Key::Right | Key::Down, Short | Repeat) => return self.knob(app, 1, st),
                (Key::Menu, Short) => {
                    app.sstv_send(SEND_MODES[i as usize % SEND_MODES.len()], if app.sstv.view == VIEW_CARD { TEST_CARD } else { app.sstv.view });
                    app.screen = Screen::Sstv(Sstv { send: None });
                }
                (Key::Exit, Short) => app.screen = Screen::Sstv(Sstv { send: None }),
                _ => {}
            }
            return;
        }
        match (e.key, e.press) {
            (Key::Left, Short | Repeat) => Self::step_view(app, st, -1),
            (Key::Right, Short | Repeat) => Self::step_view(app, st, 1),
            (Key::Digit(1), Short) => app.sstv_iss(st),
            (Key::Menu, Short) if app.sstv.iss => app.show_toast(b"ISS SSTV", b"Receive only", false),
            (Key::Menu, Short) => {
                let v = app.sstv.view;
                if v != VIEW_CARD && header(st, v).is_none_or(|h| h.lines.is_none()) {
                    return app.show_toast(b"Nothing to send", b"", false);
                }
                app.screen = Screen::Sstv(Sstv { send: Some(0) });
            }
            (Key::Hash, Short) => app.sstv_force_next(),
            (Key::Exit, Short) => {
                app.sstv_close(st);
                app.back();
            }
            _ => {}
        }
    }

    fn ptt(self, app: &mut App, down: bool) {
        if down {
            if app.sstv.iss {
                app.show_toast(b"ISS SSTV", b"Receive only", false);
            } else {
                super::nav::transmit(app);
            }
        }
    }

    fn knob(self, app: &mut App, d: i32, st: &impl ChannelStore) {
        match self.send {
            Some(i) => app.screen = Screen::Sstv(Sstv { send: Some((i as i32 + d).rem_euclid(SEND_MODES.len() as i32) as u8) }),
            None if app.sstv.tx.is_none() => Self::step_view(app, st, d.signum()),
            None => {}
        }
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        let st = cx.store;
        if let Some(t) = app.sstv.tx {
            return Self::render_tx(app, lcd, st, t);
        }
        // the picture: all of it when the view or its picture changed (about 30 ms), else a received picture's
        // newest rows as they come
        // SSTV-3.2: the lines the picture has are in the key too, should they change under the same view (a header
        // finished later than `ended` covers)
        let (avail, total) = Self::avail(app, st);
        let key = Mix::new().h(&app.sstv.view).h(&app.sstv.ver).h(&total).get();
        let height = (total as i32 * 3 / 4).min(H);
        let target = (avail as i32 * 3 / 4).min(height);
        if app.dirty(R_FRAME, key) {
            ferrite_ui::canvas::banded(lcd, 0, TOP, 240, target, BG, |b| Self::draw_rows(app, b, st, 0, target));
            // what a shorter picture (or none) leaves, cleared
            if height < H || target < height {
                let top = if total == 0 { 0 } else { target };
                ferrite_ui::canvas::banded(lcd, 0, TOP + top, 240, H - top, BG, |lcd| {
                    if total == 0 {
                        w::field(lcd, 8, 224, TOP + 80, TOP + 104, TOP + 97, b"No picture", const { &w::st(&UI_12, DIM, BG, Align::Center) });
                    }
                });
            }
            app.dirty(R_HEAD, Mix::new().h(&key).h(&avail).get());
        } else if app.dirty(R_HEAD, Mix::new().h(&key).h(&avail).get()) {
            let from = ((avail as i32 - RECENT).max(0) * 3 / 4).min(target);
            ferrite_ui::canvas::banded(lcd, 0, TOP + from, 240, target - from, BG, |b| Self::draw_rows(app, b, st, from, target));
        }
        let (mut l1, mut l2, mut l3) = (Buf::<40>::new(), Buf::<40>::new(), Buf::<40>::new());
        Self::info(app, st, self.send, &mut l1, &mut l2, &mut l3);
        if app.dirty(R_INFO, Mix::new().h(&l1).h(&self.send.is_some()).get()) {
            ferrite_ui::canvas::banded(lcd, 0, 218, 240, 24, BG, |lcd| {
                w::field(lcd, 8, 224, 220, 240, 235, l1.as_bytes(), const { &w::st(&UI_12B, TEXT, BG, Align::Left) });
            });
        }
        if app.dirty(R_SUB, Mix::new().h(&l2).get()) {
            ferrite_ui::canvas::banded(lcd, 0, 242, 240, 22, BG, |lcd| {
                w::field(lcd, 8, 224, 244, 262, 257, l2.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
            });
        }
        if app.dirty(R_NOTE, Mix::new().h(&l3).get()) {
            ferrite_ui::canvas::banded(lcd, 0, 266, 240, 22, BG, |lcd| {
                w::field(lcd, 8, 224, 268, 286, 281, l3.as_bytes(), const { &w::st(&UI_12, DIM, BG, Align::Left) });
            });
        }
    }

    fn hints(self, app: &App) -> Hints {
        if app.sstv.tx.is_some() || app.sstv.send.is_some() {
            Hints::new(&[("BACK", "Stop")])
        } else if self.send.is_some() {
            Hints::new(&[("Knob", "Mode"), ("OK", "Send"), ("BACK", "Back")])
        } else if app.sstv.iss {
            Hints::new(&[(ARROWS_LR, "Picture"), ("#", "Mode"), ("BACK", "Back")])
        } else {
            Hints::new(&[(ARROWS_LR, "Picture"), ("1", "ISS RX"), ("OK", "Send")])
        }
    }
}
