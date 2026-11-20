//! Home, watch layout (manual chapter 6; HomeA mock-up): the slots stacked A, B, C, the active one large.
//! Tuning, bands and zones, SLOT, V/M, scan, squelch and the hold-digit shortcuts ([`super::nav::hold_digit`]).
//! The other two layouts (single + scope, watch + activity: HOME-3) are drawn by [`super::layouts`].

use super::nav::Dest;
use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::{ChannelStore, ZoneId};
use crate::local::LOCAL;
use crate::data::Data;
use crate::keys::{KeyEvent, Press};
use crate::slot::{self, band_near, card_tone, mode_chip, shift_chip, shift_text, BANDS, STEPS};
use crate::{App, Screen};
use ferrite_core::freq::{mhz, Freq10};
use ferrite_core::keys::Key;
use ferrite_core::msg::{LiveAll, ModeLive, RadioCmd, SLOTS};
use ferrite_ui::assets::*;
use ferrite_ui::canvas;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;
use ufmt::uwrite;

/// Home has no state of its own: everything it shows is the App's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Home;

/// Watch layout: the slots always stacked A, B, C; the active one is the large card, in its own place.
/// The three cards fill the content area with 5 px clear above the hint bar, as below the status bar (HOME-11).
const CARD_TOP: i32 = 25;
const CARD_BIG: i32 = 124;
const CARD_SMALL: i32 = 67;
const CARD_GAP: i32 = 6;
/// Top of the big card's chip row, from the card's top.
pub(crate) const CHIP_Y: i32 = 82;

/// Card position (y, height) of slot `s` when `active` is the large one (HOME-11).
pub(crate) fn card(s: u8, active: u8) -> (i32, i32) {
    let h = |i: u8| if i == active { CARD_BIG } else { CARD_SMALL };
    let y = CARD_TOP + (0..s).map(|i| h(i) + CARD_GAP).sum::<i32>();
    (y, h(s))
}

/// A box over home went (the quick menu, HOME-17; a shortcut's popup, HOME-8): in the cards layout (HOME-3's
/// Watch) the gaps it covered go back to the background, and the cards it covered are marked new, so the render
/// that follows draws each whole, recorded once and replayed into its own bands (DISP-29), straight from the box
/// to home (DISP-25). Only what the box covered is drawn again (Nov 9, 2026: the quick menu's repaint ran the whole
/// of home once per band, about 12 passes; a popup's cleared the box and redrew every region). False in the other
/// layouts: the caller repaints its own way.
pub(crate) fn uncover(app: &mut App, lcd: &mut impl Panel, (x, y, bw, bh): (i32, i32, i32, i32)) -> bool {
    if app.home_layout() != 0 {
        return false;
    }
    let a = app.active;
    let mut above = y; // the box's rows not yet covered by a card or a gap
    for s in 0..SLOTS as u8 {
        let (cy, ch) = card(s, a);
        // the gap above this card, inside the box: the background it was before the box
        let (g0, g1) = (above.max(y), cy.min(y + bh));
        if g1 > g0 {
            lcd.fill(x, g0, bw, g1 - g0, BG);
        }
        if cy < y + bh && cy + ch > y {
            // drawn whole by the render that follows: its geometry and every part of it unknown
            let p = s as usize;
            app.drawn.card[p] = 0;
            app.drawn.meter[p] = None;
            for r in [R_FRAME, R_HEAD, R_FREQ, R_INFO, R_SUB] {
                app.drawn.hash[r + p] = 0;
            }
        }
        above = above.max(cy + ch);
    }
    if above < y + bh {
        lcd.fill(x, above, bw, y + bh - above, BG); // below the last card
    }
    // the sides of the cards (x 0-5 and 234-239) inside the box: background too
    for (sx, sw) in [(0, 6), (234, 6)] {
        let (l, r) = (x.max(sx), (x + bw).min(sx + sw));
        if r > l {
            lcd.fill(l, y, r - l, bh, BG);
        }
    }
    true
}

/// What a card shows: a slot's own (0), the APRS card (1, APRS-17), the broadcast card (2, BCR-26) or an off
/// slot's (3, HOME-15.2). A change redraws the card whole.
pub(crate) fn card_kind(app: &App, s: u8) -> u8 {
    if app.slot_off(s) {
        3
    } else if app.aprs_slot(s) {
        1
    } else if app.in_bcast_band(s) {
        2
    } else {
        0
    }
}

/// BCR-26: a slot in the broadcast band: the frequency with the stereo pilot and SNR, the RDS text (the radio text
/// whole, wrapped on the large card; else the station name) and the signal. A slot in the band while another has
/// the broadcast says where it plays instead (BCR-22).
/// A broadcast card's report (BCR-26), as drawn and read back (LINK-19): the stereo pilot, the signal (dBuV) and
/// SNR while this slot plays, the RDS text (or, while another slot has the broadcast, where it plays), whether
/// that is radio text, and whether it is that note.
pub(crate) fn bcast_shown(app: &App, live: &LiveAll, s: u8) -> (bool, u8, u8, Buf<64>, bool, bool) {
    use crate::bcast::{rds_text, trim};
    let mine = app.bcast_slot() == Some(s);
    let (ps, rt, rssi, snr, stereo) = match live.mode {
        ModeLive::Fm { ps, rt, rssi, snr, stereo, .. } if mine && app.radio.fm_on => (ps, rt, rssi, snr, stereo),
        _ => ([b' '; 8], [0; 64], 0, 0, false),
    };
    match app.bcast_slot().filter(|_| !mine) {
        Some(b) => {
            let mut note = Buf::<64>::new();
            let _ = uwrite!(note, "Broadcast plays on {}", w::LETTERS[b as usize % 3] as char);
            (stereo, rssi, snr, note, true, true)
        }
        None => (stereo, rssi, snr, Buf::from(rds_text(&ps, &rt)), !trim(&rt).is_empty(), false),
    }
}

fn bcast_card(app: &mut App, lcd: &mut impl Panel, live: &LiveAll, s: u8, y: i32, big: bool) {
    let p = s as usize;
    let f = (app.tuned_freq(s) / 1000) as u16;
    let mine = app.bcast_slot() == Some(s);
    let (stereo, rssi, snr, shown, is_rt, note) = bcast_shown(app, live, s);
    let text = shown.as_bytes();
    let tc = if note { DIM } else { TEXT };
    let mut fb = Buf::<8>::new();
    super::fm::fm_mhz(f, &mut fb);
    let mut snr_t = Buf::<16>::new();
    if rssi > 0 {
        let _ = uwrite!(snr_t, "SNR {} dB", snr);
    }
    let heard = app.bcast_heard(live) && mine;
    if big {
        // The frequency row: MHz at 28 px, the stereo tag and SNR at the right.
        if app.dirty(R_FREQ + p, Mix::new().n(f as u32 | (stereo as u32) << 16).b(snr_t.as_bytes()).get()) {
            canvas::banded(lcd, 14, y + 26, 212, 35, SURFACE, |lcd| {
                let x2 = 15 + lcd.text4_rows(15, y + 56, fb.as_bytes(), &DIG_28, TEXT, SURFACE, y + 26, y + 61);
                lcd.text4(x2 + 5, y + 56, b"MHz", &UI_12B, SUB, SURFACE);
                if stereo {
                    w::tag(lcd, 226, y + 28, b"STEREO", SUB, SURFACE);
                }
                w::field(lcd, 140, 86, y + 44, y + 61, y + 57, snr_t.as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Right) });
            });
        }
        // Three lines of RDS: 64 characters wrap into them.
        if app.dirty(R_INFO + p, Mix::new().b(text).n(is_rt as u32 | (tc as u32) << 1).get()) {
            canvas::banded(lcd, 14, y + 62, 212, 45, SURFACE, |lcd| {
                if text.is_empty() {
                    w::field(lcd, 15, 211, y + 62, y + 80, y + 76, b"No RDS", const { &w::st(&UI_12, DIM, SURFACE, Align::Left) });
                } else if is_rt {
                    w::wrap(lcd, 15, 211, y + 62, y + 107, y + 75, 15, text, &UI_12, tc, SURFACE);
                } else {
                    w::field(lcd, 15, 211, y + 62, y + 84, y + 80, text, const { &w::st(&UI_15B, TEXT, SURFACE, Align::Left) });
                }
            });
        }
        // The signal: the FM screen's scale, green while it plays.
        let lit = (rssi as u32 * w::METER_SEGS as u32 / 70).min(w::METER_SEGS as u32) as u8;
        let old = app.drawn.meter[p].filter(|&(_, o)| o == heard).map(|(l, _)| l);
        if app.drawn.meter[p] != Some((lit, heard)) {
            w::meter(lcd, 14, y + 109, old, lit, if heard { GREEN } else { DIM }, SURFACE);
            app.drawn.meter[p] = Some((lit, heard));
        }
    } else {
        app.drawn.meter[p] = None;
        if app.dirty(R_FREQ + p, Mix::new().n(f as u32 | (stereo as u32) << 16).get()) {
            canvas::banded(lcd, 14, y + 25, 212, 25, SURFACE, |lcd| {
                let x2 = 15 + lcd.text4_rows(15, y + 45, fb.as_bytes(), &DIG_26, TEXT, SURFACE, y + 25, y + 50);
                lcd.text4(x2 + 5, y + 45, b"MHz", &UI_12B, SUB, SURFACE);
                if stereo {
                    w::tag(lcd, 226, y + 30, b"STEREO", SUB, SURFACE);
                }
            });
        }
        // One line: as much of the text as fits.
        if app.dirty(R_SUB + p, Mix::new().b(text).n(tc as u32).get()) {
            canvas::banded(lcd, 15, y + 51, 211, 15, SURFACE, |lcd| {
                w::field(lcd, 15, 211, y + 51, y + 65, y + 61, w::fit(text, &UI_12, 211), &w::st(&UI_12, tc, SURFACE, Align::Left));
            });
        }
    }
}

/// APRS-17 (decisions 181, 313): slot C while it is the APRS slot is the APRS card ([`crate::aprs_card`]), below the
/// header, which names the holder. The large card (M3): the frequency at the compact size with the counts at its
/// right, two station rows, our last beacon's fate, the beacon row with the power. The small card: the frequency with
/// the holder's fact at its right, the bottom line with the power or the beacons' countdown.
fn aprs_card(app: &mut App, lcd: &mut impl Panel, data: &dyn Data, s: u8, y: i32, big: bool) {
    use crate::aprs::Holder;
    let p = s as usize;
    let f = app.aprs_freq();
    let Some(k) = app.aprs_card_as(data, big) else { return };
    app.drawn.meter[p] = None;
    if big {
        // APRS-17.2: the frequency at the compact cards' size, "3 heard · 11 /hr" at its right (" /hr" goes only
        // when the two don't fit); KISS its port.
        let fb = slot::mhz3(f);
        let fw = text4_width(fb.as_bytes(), &DIG_26);
        let (n, h) = k.counts.unwrap_or_default();
        let mut hr = Buf::<16>::new();
        let mut cnt = Buf::<16>::new();
        if k.holder == Holder::Kiss {
            cnt = k.right;
        } else {
            let _ = uwrite!(cnt, "{} heard", n);
            let _ = uwrite!(hr, " \u{b7} {} /hr", h);
        }
        let cw = text4_width(cnt.as_bytes(), &UI_12B);
        let mut hw = text4_width(hr.as_bytes(), &UI_12);
        if 15 + fw + 8 + cw + hw > 226 {
            (hr, hw) = (Buf::new(), 0);
        }
        if app.dirty(R_FREQ + p, Mix::new().h(&f).h(&cnt).h(&hr).get()) {
            canvas::banded(lcd, 14, y + 26, 212, 25, SURFACE, |lcd| {
                lcd.text4_rows(15, y + 47, fb.as_bytes(), &DIG_26, TEXT, SURFACE, y + 26, y + 51);
                w::field(lcd, 226 - hw - cw, cw, y + 34, y + 51, y + 47, cnt.as_bytes(), const { &w::st(&UI_12B, TEXT, SURFACE, Align::Left) });
                w::field(lcd, 226 - hw, hw, y + 34, y + 51, y + 47, hr.as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
            });
        }
        // APRS-17.2.2 – 17.2.4: the newest station, then unread messages or the next station; KISS its frames.
        let msg = k.unread > 0;
        let mut mt = Buf::<32>::new();
        if msg {
            let _ = uwrite!(mt, "{} new from {}", k.unread, crate::screens::lists::Txt(k.unread_from.as_bytes()));
        }
        let kiss = k.holder == Holder::Kiss;
        let first = if kiss { k.line.as_bytes() } else { app.nothing_heard(data).as_bytes() };
        if app.dirty(R_INFO + p, Mix::new().any(&k.rows[..2]).h(&k.rows_n.min(2)).h(&mt).h(&first).get()) {
            canvas::banded(lcd, 14, y + 52, 212, 34, SURFACE, |lcd| {
                if k.rows_n == 0 || kiss {
                    w::field(lcd, 15, 211, y + 52, y + 69, y + 65, first, const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
                } else {
                    station_row(lcd, y + 52, &k.rows[0], true);
                }
                if msg {
                    w::field(lcd, 15, 18, y + 69, y + 86, y + 82, b"", const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
                    w::envelope(lcd, 15, y + 73, VIOLET);
                    w::field(lcd, 33, 193, y + 69, y + 86, y + 82, w::fit(mt.as_bytes(), &UI_12, 193), const { &w::st(&UI_12, VIOLET, SURFACE, Align::Left) });
                } else if k.rows_n > 1 && !kiss {
                    station_row(lcd, y + 69, &k.rows[1], false);
                }
            });
        }
        // APRS-17.2.3: our last beacon's fate; before any beacon this power-on a third station (beacons alone: the
        // interval and the position's source).
        let third = if msg { 1 } else { 2 };
        let (ft, fc) = if !k.fate.is_empty() && !kiss {
            (k.fate.as_bytes(), if k.fate_warn { AMBER } else { SUB })
        } else if k.holder == Holder::Beacon {
            (k.line.as_bytes(), if k.line_warn { AMBER } else { SUB })
        } else {
            (&b""[..], SUB)
        };
        let row3 = (!kiss && k.holder != Holder::Beacon && ft.is_empty() && k.rows_n > third).then(|| k.rows[third as usize]);
        // APRS-17.2.1, APRS-17.2.5: the beacon row, "no fix" in amber left of the power without a position.
        let bc = if k.bottom_warn { AMBER } else { SUB };
        if app.dirty(R_SUB + p, Mix::new().h(&ft).h(&fc).any(&row3).h(&k.bottom).h(&bc).h(&k.fix_note).h(&k.power).get()) {
            canvas::banded(lcd, 14, y + 86, 212, 34, SURFACE, |lcd| {
                match &row3 {
                    Some(r) => station_row(lcd, y + 86, r, false),
                    None => {
                        w::field(lcd, 15, 211, y + 86, y + 103, y + 99, w::fit(ft, &UI_12, 211), &w::st(&UI_12, fc, SURFACE, Align::Left));
                    }
                }
                bottom_row(lcd, y + 103, k.bottom.as_bytes(), bc, k.fix_note, k.power);
            });
        }
    } else {
        let rc = if k.right_warn { AMBER } else if k.countdown { VIOLET } else { TEXT };
        let lc = if k.line_warn { AMBER } else { SUB };
        // APRS-17.2.1: with automatic beacons beside APRS mode or KISS, the small card's bottom right is the beacon's
        // countdown ("Beacon in 5m", or why it waits, in amber) in place of the power, which the large card keeps
        let next_here = k.holder != Holder::Beacon && !k.next.is_empty();
        let mut pwb = Buf::<16>::new();
        if next_here {
            if k.next_timed {
                pwb.push(if k.next.as_bytes() == b"soon" { b"Beacon ".as_slice() } else { b"Beacon in " });
            }
            pwb.push(k.soon.as_bytes());
        } else {
            pwb.push(k.power.as_bytes());
        }
        let pw = pwb.as_bytes();
        let pw_w = text4_width(pw, &UI_12);
        let pc = if next_here { if k.next_warn { AMBER } else { VIOLET } } else { SUB };
        if let Some(clear) = app.dirty_shape(R_FREQ + p, Mix::new().h(&f).h(&k.right).h(&k.countdown).h(&rc).h(&1u8).get(), true) {
            redraw_box!(clear, lcd, (15, y + 24, 211, 26, SURFACE), {
                w::freq(lcd, 15, 130, y + 45, f, &DIG_26, &DIG_17, TEXT, SUB, SURFACE);
                if k.countdown {
                    // Two lines: a small "Beacon in" over the countdown, bold, in C's colour.
                    w::field(lcd, 150, 76, y + 24, y + 34, y + 33, b"Beacon in", const { &w::st(&UI_12, SUB, SURFACE, Align::Right) });
                    w::field(lcd, 150, 76, y + 34, y + 50, y + 48, k.right.as_bytes(), &w::st(&UI_15B, rc, SURFACE, Align::Right));
                } else {
                    w::field(lcd, 130, 96, y + 29, y + 47, y + 45, k.right.as_bytes(), &w::st(&UI_12B, rc, SURFACE, Align::Right));
                }
            });
        }
        if app.dirty(R_SUB + p, Mix::new().h(&k.line).h(&lc).h(&pw).h(&pc).get()) {
            ferrite_ui::canvas::banded(lcd, 15, y + 51, 211, 15, SURFACE, |lcd| {
                let lw = 211 - pw_w - 8;
                // "Last " goes before the station's age would be clipped beside a countdown
                let mut l = k.line.as_bytes();
                if text4_width(l, &UI_12) > lw {
                    l = l.strip_prefix(b"Last ").unwrap_or(l);
                }
                w::field(lcd, 15, lw, y + 51, y + 65, y + 61, w::fit(l, &UI_12, lw), &w::st(&UI_12, lc, SURFACE, Align::Left));
                w::field(lcd, 226 - pw_w, pw_w, y + 51, y + 65, y + 61, pw, &w::st(&UI_12, pc, SURFACE, Align::Right));
            });
        }
    }
}

/// APRS-17.2.2: a station row of the large APRS card: the call (the newest bold), its age, and at the right the
/// distance and bearing or "via digi".
fn station_row(lcd: &mut impl Panel, y0: i32, r: &crate::aprs_card::Row, newest: bool) {
    let (f, c) = if newest { (&UI_12B, TEXT) } else { (&UI_12, SUB) };
    let b = y0 + 13;
    w::field(lcd, 15, 85, y0, y0 + 17, b, w::fit(r.call.as_bytes(), f, 80), &w::st(f, c, SURFACE, Align::Left));
    w::field(lcd, 100, 52, y0, y0 + 17, b, r.age.as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
    w::field(lcd, 152, 74, y0, y0 + 17, b, w::fit(r.right.as_bytes(), &UI_12, 74), const { &w::st(&UI_12, SUB, SURFACE, Align::Right) });
}

/// APRS-17.2.1, APRS-17.2.5: the large APRS card's beacon row: the beacon's state, "no fix" (amber) without a
/// position, the power at the right.
fn bottom_row(lcd: &mut impl Panel, y0: i32, t: &[u8], c: u16, fix_note: bool, power: &str) {
    let b = y0 + 13;
    let pw = power.as_bytes();
    let pw_w = text4_width(pw, &UI_12);
    let nb: &[u8] = if fix_note { b"no fix" } else { b"" };
    let nw = text4_width(nb, &UI_12);
    let lw = 211 - pw_w - nw - 16;
    w::field(lcd, 15, lw, y0, y0 + 17, b, w::fit(t, &UI_12, lw), &w::st(&UI_12, c, SURFACE, Align::Left));
    w::field(lcd, 15 + lw, 211 - lw - pw_w - 8, y0, y0 + 17, b, nb, const { &w::st(&UI_12, AMBER, SURFACE, Align::Right) });
    w::field(lcd, 226 - pw_w, pw_w, y0, y0 + 17, b, pw, const { &w::st(&UI_12, SUB, SURFACE, Align::Right) });
}

impl View for Home {
    fn own_side1(self) -> bool {
        true
    }

    /// KEY-19: ◀▶ step the band or zone (NAV-2).
    fn own_arrows(self) -> bool {
        true
    }

    fn own_hold_menu(self) -> bool {
        true // MENU-2: advanced settings
    }

    fn name(self) -> &'static str {
        "Home"
    }

    fn mockup(self) -> Option<&'static str> {
        Some("HomeA.png")
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        b.clear(); // home's status bar shows no screen name
    }

    fn key(self, app: &mut App, e: KeyEvent, store: &impl ChannelStore) {
        use Press::*;
        let big = app.keys.is_held(Key::Side(2));
        // PIN-5: channel-only mode: no V/M, editing, settings or MENU 0 (hold V/M, the zone list, still opens).
        // A digit on a VFO slot would open frequency entry, which channel-only mode hides too.
        if (matches!(
            (e.key, e.press),
            (Key::VfoMem, Short) | (Key::Slot | Key::Menu, Long) | (Key::Menu, Short) | (Key::Star, Long) | (Key::Side(2), LongUp)
        ) || matches!((e.key, e.press), (Key::Digit(_), Short)) && !app.slots[app.active as usize].on_channel())
            && app.chan_only_refuse()
        {
            return;
        }
        // APRS-19: the APRS slot can be looked at, not changed; APRS-17: hold MENU on it is its options.
        if app.aprs_slot(app.active) {
            match (e.key, e.press) {
                // APRS-17.3 (decisions 181, 257): OK (MENU) or hold MENU on the APRS slot opens the APRS list on the
                // row the hint bar names, one more OK runs it; a digit opens its row (OK 1: messages); 0 there is
                // still the full menu (MENU-25), so MENU 0 paths work as ever.
                (Key::Menu, Short | Long) => {
                    let row = if app.aprs_holder() == Some(crate::aprs::Holder::Beacon) { super::opts::APRS_ROW_BEACON_NOW } else { super::opts::APRS_ROW_HEARD };
                    return app.open_screen(Screen::Options(super::opts::Options { of: super::opts::Of::AprsCard, row, confirm: false }));
                }
                // the shortcuts that don't touch the slot (scope, watch) still run, and power, which is the APRS
                // power there (APRS-19.1)
                (Key::Digit(1 | 2 | 4), Long) => {}
                // MENU-34: hold ABC on the APRS slot opens its quick page, whatever holds C
                (Key::Slot, Long) => return app.open_screen(super::qgrid::Page::Aprs.screen()),
                // APRS-17.8: hold V/M on the APRS slot sends a beacon (a hold, so a bump never transmits)
                (Key::VfoMem, Long) => return super::opts::beacon_now(app, store),
                // a tap says how (KISS holds C with no beacons: its refusal, below)
                (Key::VfoMem, Short) if app.aprs_holder() != Some(crate::aprs::Holder::Kiss) => {
                    app.show_toast(b"Hold V/M", b"to send a beacon", false);
                    return;
                }
                (Key::Up | Key::Down | Key::Left | Key::Right, Short | Repeat)
                | (Key::Digit(_), Long)
                | (Key::VfoMem | Key::Side(2) | Key::Digit(_), Short) => {
                    app.aprs_refuse(app.active);
                    return;
                }
                _ => {}
            }
        }
        match (e.key, e.press) {
            (Key::Up | Key::Down, Short | Repeat) if app.keys.is_held(Key::Side(1)) => {
                super::squelch::step(app, if e.key == Key::Up { 1 } else { -1 })
            }
            (Key::Up | Key::Down, Short | Repeat) => {
                if big {
                    app.keys.used(Key::Side(2));
                }
                step(app, if e.key == Key::Up { 1 } else { -1 }, big, store);
            }
            // VFO-22: SIDE1 + ◀▶ is the clarifier (SIDE1 + ▲▼ the squelch: SIDE1 holds the receive side)
            (Key::Left | Key::Right, Short | Repeat) if app.keys.is_held(Key::Side(1)) => {
                app.keys.used(Key::Side(1));
                app.step_clarifier(if e.key == Key::Right { 1 } else { -1 });
            }
            // BCR-25: ◀▶ seek on the broadcast slot's VFO
            (Key::Left | Key::Right, Short) if app.bcast_slot() == Some(app.active) && !app.slots[app.active as usize].on_channel() => {
                app.fm_seek(e.key == Key::Right)
            }
            (Key::Left | Key::Right, Short | Repeat) => group(app, if e.key == Key::Right { 1 } else { -1 }, store),
            (Key::Slot, Short) => {
                // NAV-5
                app.active = (app.active + 1) % SLOTS as u8;
                app.out.push(RadioCmd::Active(app.active));
            }
            (Key::Slot, Long) if app.slots[app.active as usize].on_channel() => app.screen = Screen::ChEdit(super::chedit::ChEdit { cell: 0 }), // EDIT-2
            (Key::Slot, Long) => app.screen = Screen::Edit(super::edit::Edit { cell: 0 }), // EDIT-1
            (Key::VfoMem, Short) => toggle_vfo(app, store),
            (Key::VfoMem, Long) => app.open(Dest::Zones), // KEY-26: the zone list, in VFO and channel mode alike
            (Key::Hash, Short) => {
                if app.watched_rx() {
                    // HOME-10, WATCH-6
                    let pause = app.set.pause_s(); // 0 1 6 1
                    app.paused_until = Some(app.now.wrapping_add(pause as u32 * 1000));
                    app.out.push(RadioCmd::PauseWatch(pause));
                    app.show_toast(b"Watch paused", b"for 1 min", false);
                } else {
                    // HOME-10: the quick help, its first page the holds; # goes on through the help pages,
                    // BACK home (the Status page is MENU 0 0 since decision 376)
                    app.push_back();
                    app.screen = Screen::Info(super::info::Info { page: super::info::Page::Help(super::help::Help::Digits) });
                }
            }
            (Key::Menu, Short) => app.screen = Screen::Quick(super::quick::Quick { row: 0 }), // MENU-1
            (Key::Menu, Long) => app.screen = Screen::Advanced(super::advanced::Advanced { row: 0, from_quick: false }), // MENU-2
            (Key::Side(2), Short) => super::scan::side2(app, store), // SCAN-1
            // KEY-22: SIDE2 held alone opens the frequency counter on release. Using it for RF gain or big steps consumes it.
            (Key::Side(2), LongUp) => app.open(Dest::Counter),
            (Key::Side(1), Short | Long) => app.monitor_key(e.press), // SQL-9
            // KEY-18, SCAN-11: on a scanning slot \* skips the hit, as on the scan screen; otherwise quick mute.
            (Key::Star, Short) if app.scan_mask & 1 << app.active != 0 => super::scan::skip(app, app.active),
            (Key::Star, Short) => quick_mute(app), // HOME-15
            // HOME-17: hold \* is the quick menu (reverse is its PTT A); not while the active slot scans (KEY-18)
            (Key::Star, Long) if app.scan_mask & 1 << app.active != 0 => {}
            (Key::Star, Long) => super::qmenu::open(app),
            (Key::Digit(d), Short) => app.open(Dest::Entry(d)), // NAV-4, VFO-11
            // HOME-8: the shortcut runs at the long press, a popup names it (KEY-11).
            (Key::Digit(d), Long) => super::nav::hold_digit(app, d, store),
            _ => {}
        }
    }

    fn knob(self, app: &mut App, d: i32, store: &impl ChannelStore) {
        if app.aprs_refuse(app.active) {
            return; // APRS-19
        }
        if app.keys.is_held(Key::Side(1)) {
            return super::squelch::step(app, d);
        }
        if app.keys.is_held(Key::Side(2)) {
            app.keys.used(Key::Side(2));
            if app.bcast_slot() == Some(app.active) {
                app.show_toast(b"RF gain", b"BK4829 only", false);
                return;
            }
            app.step_rf_gain(d, true);
            app.show_toast(b"RF gain", app.rf_gain_text(), false);
            return;
        }
        step(app, d, false, store);
    }

    /// Home is drawn on the panel and, under a closing quick menu, in a canvas's bands (qmenu::repaint): through a
    /// [`canvas::direct`] band both ways, so its drawing (the cards, the other two layouts) is compiled once.
    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        render(app, &mut canvas::direct(lcd), cx);
    }

    fn hints(self, app: &App) -> Hints {
        if app.locked() {
            return Hints::new(&[("#", "Hold: unlock")]);
        }
        let group = if app.slots[app.active as usize].on_channel() { "Zone" } else { "Band" };
        let mid = if app.watched_rx() { ("#", "Pause") } else { ("OK", "Quick") };
        if app.aprs_slot(app.active) {
            // APRS-17 (decision 181): OK leads to the heard list, or with beacons alone to Beacon now; # pauses
            // watch while a watched slot is receiving (WATCH-6), else hold MENU is the APRS page.
            let ok = if app.aprs_holder() == Some(crate::aprs::Holder::Beacon) { ("OK", "Send") } else { ("OK", "Heard") };
            let third = if app.watched_rx() { ("#", "Pause") } else { ("Hold MENU", "APRS") };
            return Hints::new(&[("ABC", "Next"), ok, third]); // the key as it is marked
        }
        let a = app.active;
        let group = if app.bcast_slot() == Some(a) && !app.slots[a as usize].on_channel() { "Seek" } else { group }; // BCR-25
        // HOME-10.1: hold ABC (the SLOT key, as marked), the slot's settings (EDIT-1, EDIT-2), where a tap's A·B·C
        // told nothing new
        Hints::new(&[("Hold ABC", "Edit"), mid, (ARROWS_LR, group)])
    }
}

/// [`Home`]'s drawing, on the panel or in a band.
fn render(app: &mut App, lcd: &mut canvas::Band, cx: &Cx<impl ChannelStore>) {
    // HOME-3.1: the layout changed while home is up (from the terminal or Console: the menu's way goes through a
    // screen change, which clears): the whole area cleared and every region forgotten, as a new page. The cards
    // stayed behind under the strip's one card before. On a fresh screen (regions all forgotten) nothing more.
    // Low Power Mode's home keeps its own page state in R_PAGE (lowpower.rs); leaving it clears once from here.
    let key = Mix::new().h(&app.home_layout()).get() | 1;
    let was = app.drawn.hash[R_PAGE];
    if was != key && app.home_layout() != crate::lpm::HOME_LOW_POWER {
        if was != 0 {
            lcd.fill(0, w::SBAR_H, 240, w::HINT_Y - w::SBAR_H, BG);
            app.drawn.hash[R_HINT + 1..R_PAGE].fill(0);
            app.drawn.meter = [None; SLOTS];
        }
        app.drawn.hash[R_PAGE] = key;
    }
    // HOME-3: the other two layouts draw differently; the keys are the same.
    match app.home_layout() {
        crate::lpm::HOME_LOW_POWER => return super::lowpower::render(app, lcd, cx), // PWR-24.7
        1 => return super::layouts::single(app, lcd, cx),
        2 => return super::layouts::activity(app, lcd, cx),
        _ => {}
    }
    // The large card moved (SLOT): the gaps between cards moved too, so clear them and draw every card again;
    // otherwise slivers of the old large card stay behind (HOME-11). Only the gaps: each card paints its own
    // rectangle (clearing the whole area first was one of five passes over it, slow on the radio's bus).
    // Each gap is drawn with the card under it, never all first: that cut the old large card in half with a dark
    // line (on the radio, Oct 12, 2026).
    let gaps = app.dirty(R_NOTE, Mix::new().h(&app.active).get());
    if gaps {
        app.drawn.hash[R_FRAME..=R_SUB + 2].fill(0);
        app.drawn.meter = [None; SLOTS];
    }
    // DISP-29: after SLOT the new large card is drawn first, where the eye goes, then the others top to bottom.
    let a = app.active.min(SLOTS as u8 - 1);
    let order: [u8; SLOTS] = if gaps { [a, (a == 0) as u8, 2 - (a == 2) as u8] } else { [0, 1, 2] };
    // The gap above a card (and its sides) with it: before a small card, after the large one, which goes first
    // (before it, the gap would cut the old large card with a dark line).
    let gap = |lcd: &mut canvas::Band, s: u8| {
        let (y, h) = card(s, a);
        let top = if s == 0 { 21 } else { card(s - 1, a).0 + card(s - 1, a).1 };
        lcd.fill(0, top, 240, y - top, BG);
        lcd.fill(0, y, 6, h, BG);
        lcd.fill(234, y, 6, h, BG);
    };
    for s in order {
        let t0 = ferrite_ui::prof::now();
        let p = s as usize;
        let (y, h) = card(s, app.active);
        if gaps && s != a {
            gap(lcd, s);
        }
        let geom = Mix::new().h(&s).h(&y).h(&h).h(&card_kind(app, s)).get() | 1;
        if app.drawn.hash[R_FRAME + p] != 0 && app.drawn.card[p] == geom {
            // HOME-13: while the knob turns only the active card follows; the others wait in place.
            if s == app.active || !app.held(R_FRAME + p) {
                draw_card(app, lcd, cx, s);
            }
        } else {
            // DISP-25: a card in a new place or size (SLOT moved the large card, a scan began) is drawn whole in
            // bands, frame and content, so the panel goes from the old picture to the new with no blank card
            // between. DISP-29: its drawing is recorded once and replayed into each band; only when the recording
            // doesn't fit does it run per band, each band from the same state, the card's geometry too (without it
            // every band after the first took the card as in place and drew only its border: a card changing kind
            // kept the old margins, Oct 23, 2026).
            let (hash0, meter0, card0) = (app.drawn.hash, app.drawn.meter, app.drawn.card);
            canvas::banded(lcd, 6, y, 228, h, BG, |b| {
                (app.drawn.hash, app.drawn.meter, app.drawn.card) = (hash0, meter0, card0);
                draw_card(app, b, cx, s);
            });
        }
        if gaps && s == a {
            gap(lcd, s);
        }
        ferrite_ui::prof::add(ferrite_ui::prof::CARD + p, t0);
    }
    if gaps {
        let (y, h) = card(SLOTS as u8 - 1, a);
        lcd.fill(0, y + h, 240, 300 - y - h, BG);
    }
}

/// HOME-15, HOME-15.1: \* cycles the active slot Normal, Muted, Off, Normal. Muted: it still receives (state,
/// S-meter, the green LED), but its audio never reaches the speaker and never holds it from a watched slot. Off: no
/// chip listens to it at all, so its watch turns go to the other slots. The APRS slot keeps Normal and Muted. RAM
/// only; each stays with its slot until \* again.
/// HOME-15.1: how quickly a second \* must follow the first to turn the slot off.
const STAR_OFF_MS: u32 = 1000;

fn quick_mute(app: &mut App) {
    let a = app.active;
    let bit = 1 << a;
    // HOME-15.1: \* \* within a second turns a slot off; a later \* only unmutes
    let quick = app.star_at != 0 && ((app.now as u16).wrapping_sub(app.star_at & !1) as u32) < STAR_OFF_MS;
    app.star_at = 0;
    let (word, then): (&[u8], &[u8]) = if app.off & bit != 0 {
        app.off &= !bit;
        (b" on", b"")
    } else if app.mute & bit != 0 && quick && !app.aprs_slot(a) {
        // muted to off; a latched monitor goes with it
        (app.mute, app.off) = (app.mute & !bit, app.off | bit);
        if app.monitor & bit != 0 {
            app.monitor &= !bit;
            app.out.push(RadioCmd::Monitor { slot: a, on: false });
        }
        (b" off", b"* turns it back on")
    } else {
        app.mute ^= bit;
        if app.mute & bit != 0 {
            app.star_at = app.now as u16 | 1;
            (b" muted", if app.aprs_slot(a) { b"* again to hear it".as_slice() } else { b"* unmutes, ** off" })
        } else {
            (b" unmuted", b"")
        }
    };
    quiet_sent(app, a, word, then);
}

/// LINK-68: slot `s` straight to `want` (0 on, 1 muted, 2 off) with \*'s effects, from the link: the APRS slot is
/// never off (checked by the caller); off ends a latched monitor. Nothing changes, nothing is said.
pub(crate) fn set_quiet(app: &mut App, s: u8, want: u8) {
    let bit = 1 << s;
    let was = if app.off & bit != 0 { 2 } else if app.mute & bit != 0 { 1 } else { 0 };
    if want == was {
        return;
    }
    app.star_at = 0;
    (app.mute, app.off) = match want {
        0 => (app.mute & !bit, app.off & !bit),
        1 => (app.mute | bit, app.off & !bit),
        _ => (app.mute & !bit, app.off | bit),
    };
    if want == 2 && app.monitor & bit != 0 {
        app.monitor &= !bit;
        app.out.push(RadioCmd::Monitor { slot: s, on: false });
    }
    let word: &[u8] = match (want, was) {
        (1, _) => b" muted",
        (2, _) => b" off",
        (_, 2) => b" on",
        _ => b" unmuted",
    };
    quiet_sent(app, s, word, b"");
}

/// HOME-15: a slot's new mute and off to the Radio task and the watch, and the toast.
fn quiet_sent(app: &mut App, s: u8, word: &[u8], then: &[u8]) {
    app.send_mute();
    app.out.push(RadioCmd::SlotOff(app.off));
    app.out.push(RadioCmd::Watch(app.watch_mask())); // WATCH-2.1: an off slot's turns go to the others
    if s == crate::aprs::APRS_SLOT && app.set.aprs_mode {
        app.set.aprs_mute = app.mute & 1 << s != 0; // APRS-2: \* on C in APRS mode is 0 5 9 6
    }
    let mut t = Buf::<16>::new();
    t.set(b"Slot ");
    t.push(&[b'A' + s]);
    t.push(word);
    app.show_toast(t.as_bytes(), then, false);
}

/// HOME-15.2: an off slot's card, powered down (mock-up O1): no surface, a dashed outline (dim; blue on the active
/// card, which keeps its size so SLOT still shows where it is), the letter dim, a dim header naming what the slot is
/// on with "Off" at its right, a big dim "OFF" in the middle and "\* turns it back on" under it.
fn off_card(app: &mut App, lcd: &mut canvas::Band, cx: &Cx<impl ChannelStore>, s: u8, y: i32, h: i32, big: bool) {
    let p = s as usize;
    let line = if big { BLUE } else { DIM };
    if app.dirty(R_FRAME + p, Mix::new().h(&s).h(&y).h(&h).h(&line).h(&3u8).get()) {
        app.drawn.card[p] = Mix::new().h(&s).h(&y).h(&h).h(&3u8).get() | 1;
        lcd.fill(6, y, 228, h, BG);
        // 4 px dashes, 3 px gaps, the corners left open
        for i in (3..225).step_by(7) {
            let n = 4.min(225 - i);
            lcd.fill(6 + i, y, n, 1, line);
            lcd.fill(6 + i, y + h - 1, n, 1, line);
        }
        for j in (3..h - 3).step_by(7) {
            let n = 4.min(h - 3 - j);
            lcd.fill(6, y + j, 1, n, line);
            lcd.fill(233, y + j, 1, n, line);
        }
        let (font, base, hint) = if big { (&NAME_26, y + 70, y + 96) } else { (&NAME_22, y + 45, y + 60) };
        let top = base - font.ascent as i32;
        w::field(lcd, 14, 212, top, top + font.line_h as i32, base, b"OFF", &w::st(font, DIM, BG, Align::Center));
        w::field(lcd, 14, 212, hint - 13, hint + 4, hint, b"* turns it back on", const { &w::st(&UI_12, SUB, BG, Align::Center) });
        app.drawn.hash[R_HEAD + p] = 0;
        app.drawn.meter[p] = None;
    }
    // The header: "VFO · VHF · 146.520", or a channel's number and name (its frequency without one).
    let sl = &app.slots[p];
    let mut head = Buf::<32>::new();
    let name = app.chan_name(s, cx.live);
    if sl.on_channel() {
        slot::chan_num(sl.ch.map_or(0, |c| c.num), &mut head);
    } else {
        app.head_text(s, cx.live, cx.store, &mut head);
    }
    head.push(" \u{b7} ".as_bytes());
    if name.is_empty() {
        head.push(slot::mhz3(app.tuned_freq(s)).as_bytes());
    } else {
        head.push(name.as_bytes());
    }
    if app.dirty(R_HEAD + p, Mix::new().h(&s).h(&head).h(&big).get()) {
        let (hy, hb) = if big { (y + 7, y + 20) } else { (y + 5, y + 18) };
        // the letter, dim (as [`w::badge`] draws an outlined one)
        lcd.fill(14, hy + 1, 14, 14, BG);
        lcd.frame_r(14, hy + 1, 13, 13, 1, DIM);
        lcd.text4_rows(17, hy + 12, &[b'A' + s], &UI_12B, DIM, BG, hy + 2, hy + 13);
        let ow = text4_width(b"Off", &UI_12);
        let hw = 226 - ow - 8 - 34;
        lcd.fill(28, hy, 6, 17, BG);
        w::field(lcd, 34, hw, hy, hy + 17, hb, w::fit(head.as_bytes(), &UI_12, hw), const { &w::st(&UI_12, DIM, BG, Align::Left) });
        w::field(lcd, 34 + hw, ow + 8, hy, hy + 17, hb, b"Off", const { &w::st(&UI_12, SUB, BG, Align::Right) });
    }
}

/// Card `s` of the cards layout: its frame when it changed, and every part of it that changed.
fn draw_card(app: &mut App, lcd: &mut canvas::Band, cx: &Cx<impl ChannelStore>, s: u8) {
    let live = cx.live;
    // Fixed order A, B, C: the card's region index is its slot.
    let p = s as usize;
    let (y, h) = card(s, app.active);
    let big = s == app.active;
    let lv = live.slots[s as usize];
    // APRS-17.6 (decision 313): the APRS card's state is every card's, Muted and RX muted included
    let (state, sc) = app.state(s, live);
    let kind = card_kind(app, s);
    if kind == 3 {
        return off_card(app, lcd, cx, s, y, h, big);
    }
    // HOME-15.3: muted while receiving is "Muted" with the RX tag beside it, and the border of any receive
    let rxm = state == "RX muted";
    let border = if state == "TX" { RED } else if big { BLUE } else if state == "RX" || rxm { GREEN } else { LINE };
    let aprs = app.aprs_slot(s);
    let geom = Mix::new().h(&s).h(&y).h(&h).h(&kind).get() | 1;
    let drawn_before = app.drawn.hash[R_FRAME + p] != 0;
    if !app.dirty(R_FRAME + p, Mix::new().h(&s).h(&y).h(&h).h(&border).h(&kind).get()) {
        // unchanged
    } else if drawn_before && app.drawn.card[p] == geom {
        // Only the border's colour changed (RX, idle): redraw the border alone. Redrawing the whole card blanked
        // its content, which flickered on the radio (DISP-25).
        w::card_border(lcd, 6, y, 228, h, border, big, BG);
    } else {
        app.drawn.card[p] = geom;
        // The background only where the rounded corners leave it showing (3 px squares), not the whole card.
        for (cx, cy) in [(6, y), (231, y), (6, y + h - 3), (231, y + h - 3)] {
            lcd.fill(cx, cy, 3, 3, BG);
        }
        lcd.fill_r(6, y, 228, h, 3, SURFACE);
        w::card_border(lcd, 6, y, 228, h, border, big, BG);
        for r in [R_HEAD, R_FREQ, R_INFO, R_SUB] {
            app.drawn.hash[r + p] = 0;
        }
        app.drawn.hash[R_FREQ + p] = FRESH; // the frequency box is plain surface: no blank needed
        app.drawn.meter[p] = None;
    }
    // Header: letter, VFO/CH/SCAN and band, the state at the right (HOME-4, HOME-5).
    let mut head = Buf::<32>::new();
    app.head_text(s, live, cx.store, &mut head);
    let cfg = *app.slots[s as usize].cfg();
    if big && cfg.split && !lv.scanning {
        // A split's TX frequency takes the band's place in the header: its chip slot only says SPLIT.
        let n = head.as_bytes().iter().position(|&c| c == b' ').unwrap_or(head.len());
        let mut h = Buf::<32>::new();
        h.push(&head.as_bytes()[..n]);
        h.push(" \u{b7} TX ".as_bytes());
        h.push(slot::mhz3(cfg.offset.max(0) as Freq10).as_bytes());
        head = h;
    }
    // Header rows: y+7..y+24 on the big card, y+5..y+22 on the compact ones (their digits start at y+25).
    let (hy, hb) = if big { (y + 7, y + 20) } else { (y + 5, y + 18) };
    // Beside the state (left of an RX or MON tag): the clarifier (VFO-22), or on the APRS card while it takes turns
    // on chip 0 with A or B, who wins it (APRS-21, WATCH-17.3: "APRS first" grey, "A·B first" amber), only where it
    // fits beside the holder and the state (the state wins).
    let mut clr = Buf::<16>::new();
    let mut clr_c = AMBER;
    let c = app.slots[s as usize].clar;
    let tagged = matches!(state, "RX" | "TX" | "MON" | "FM");
    if let Some((t, ab)) = app.aprs_priority().filter(|_| aprs) {
        clr.set(t.as_bytes());
        clr_c = if ab { AMBER } else { SUB };
        let hb_ = head.as_bytes();
        let n = hb_.windows(3).position(|w| w == " \u{b7}".as_bytes()).unwrap_or(hb_.len());
        if text4_width(&hb_[..n], &UI_12B) + 8 + head_right_w(big, state, &clr) > 192 {
            clr.clear();
        }
    } else if c != 0 && !lv.scanning && (big && tagged || state == "idle") {
        slot::clar_text(c, &mut clr);
    }
    // TSRCH-13: a tone hint takes the state's place, in a wider field; so does a long state ("Watch off: relay",
    // BCR-15).
    let mut ht = Buf::<24>::new();
    if wide_state(state, &lv, app.slots[s as usize].cfg().rx_tone, &mut ht) {
    } else if state.len() > 8 {
        ht.set(state.as_bytes());
    }
    let wide = !ht.is_empty();
    let ww = if wide { (text4_width(ht.as_bytes(), &UI_12) + 2).max(HEARD_W) } else { 0 };
    if app.dirty(R_HEAD + p, Mix::new().h(&s).h(&head).h(&state).h(&big).h(&clr).h(&clr_c).h(&ht).get()) {
        w::badge(lcd, 14, hy + 1, s, big, BG);
        lcd.fill(28, hy, 6, 17, SURFACE);
        // HOME-4.3: the header takes what the state beside it leaves (a 16-character zone name fits beside an idle
        // state or an RX tag), never less than 120 px; clipped at a character, never wrapped.
        // (the APRS card's holder is short, and its priority shows only where it fits beside it: no minimum)
        let hw = if wide { 226 - ww - 34 } else { (226 - 6 - head_right_w(big, state, &clr) - 34).max(if aprs { 0 } else { 120 }) };
        let sx = 34 + hw;
        let hb_ = head.as_bytes();
        // the holder is what comes before " · " ("APRS Mode", "APRS KISS")
        let split = hb_.windows(3).position(|w| w == " \u{b7}".as_bytes());
        if let Some(n) = split.filter(|_| aprs) {
            // APRS-17: the holder ("APRS Mode") bold in C's colour, " · VHF" as any header; with APRS-21's
            // "APRS first" or "A·B first" beside the state the band goes (decision 313).
            let (word, rest) = hb_.split_at(n);
            let rest = if clr.is_empty() { rest } else { b"" };
            let ww0 = text4_width(word, &UI_12B).min(hw);
            lcd.fill(34, hy, hw, 17, SURFACE);
            w::field(lcd, 34, ww0, hy, hy + 17, hb, w::fit(word, &UI_12B, ww0), const { &w::st(&UI_12B, VIOLET, SURFACE, Align::Left) });
            w::field(lcd, 34 + ww0, hw - ww0, hy, hy + 17, hb, w::fit(rest, &UI_12, hw - ww0), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
        } else {
            w::field(lcd, 34, hw, hy, hy + 17, hb, w::fit(head.as_bytes(), &UI_12, hw), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
        }
        if wide {
            let top = if big { y + 6 } else { hy };
            ferrite_ui::canvas::banded(lcd, 226 - ww, top, ww + 2, 18, SURFACE, |lcd| {
                w::field(lcd, 226 - ww, ww, hy, hy + 17, hb, ht.as_bytes(), &w::st(&UI_12, sc, SURFACE, Align::Right));
            });
        } else if big {
            ferrite_ui::canvas::banded(lcd, sx, y + 6, 228 - sx, 18, SURFACE, |lcd| {
                // the state at the right (a tag, or its word; idle shows nothing), the clarifier or APRS priority left of it
                let right = if rxm {
                    // HOME-15.3: the RX tag at the right, "Muted" left of it
                    w::tag(lcd, 226, y + 7, b"RX", GREEN, SURFACE);
                    let mw = text4_width(MUTED, &UI_12);
                    w::field(lcd, 194 - mw, mw, y + 7, y + 24, y + 20, MUTED, const { &w::st(&UI_12, AMBER, SURFACE, Align::Right) });
                    194 - mw - 8
                } else if tagged {
                    w::tag(lcd, 226, y + 7, state.as_bytes(), sc, SURFACE);
                    194
                } else if state != "idle" {
                    w::field(lcd, sx, 226 - sx, y + 7, y + 24, y + 20, state.as_bytes(), &w::st(&UI_12, sc, SURFACE, Align::Right));
                    226 - text4_width(state.as_bytes(), &UI_12) - 8
                } else {
                    226
                };
                if !clr.is_empty() {
                    w::field(lcd, sx, right - sx, y + 7, y + 24, y + 20, clr.as_bytes(), &w::st(&UI_12, clr_c, SURFACE, Align::Right));
                }
            });
        } else {
            let (t, c, f) = compact_state(state, &clr, sc, clr_c);
            let tw = text4_width(t, f);
            w::field(lcd, 226 - tw, tw, hy, hy + 17, hb, t, &w::st(f, c, SURFACE, Align::Right));
            // HOME-15.3: "Muted" left of the bold RX
            let mut sx_ = 226 - tw;
            if rxm {
                let mw = text4_width(MUTED, &UI_12) + 8;
                sx_ -= mw;
                w::field(lcd, sx_, mw, hy, hy + 17, hb, MUTED, const { &w::st(&UI_12, AMBER, SURFACE, Align::Left) });
            }
            // APRS-21: the priority left of a state that isn't idle (an idle compact card shows it in idle's place)
            let note: &[u8] = if state != "idle" { clr.as_bytes() } else { b"" };
            let gx = (sx_ - 8).max(sx);
            w::field(lcd, sx, gx - sx, hy, hy + 17, hb, note, &w::st(&UI_12, clr_c, SURFACE, Align::Right));
            lcd.fill(gx, hy, sx_ - gx, 17, SURFACE);
        }
    }
    if aprs {
        let t0 = ferrite_ui::prof::now();
        aprs_card(app, lcd, cx.store.data(), s, y, big);
        ferrite_ui::prof::add(ferrite_ui::prof::APRS, t0);
        return;
    }
    if kind == 2 {
        bcast_card(app, lcd, live, s, y, big);
        return;
    }
    let f = app.shown_freq(s, live);
    // Channel mode puts the name first, the frequency small under it (HOME-4); a VFO, a scan or a channel
    // without a name shows the frequency large.
    let name = app.card_name(s, live, cx.store);
    if big {
        if let Some(clear) = app.dirty_shape(R_FREQ + p, Mix::new().h(&f).h(&name).get(), name.is_empty()) {
            redraw_box!(clear, lcd, (15, y + 28, 211, 49, SURFACE), {
                if name.is_empty() {
                    w::freq(lcd, 15, 226, y + 67, f, &DIG_46, &DIG_28, FREQ, SUB, SURFACE);
                } else {
                    // 26 px when the whole name fits (13 characters), else 22 px (16 always fit).
                    let nb = name.as_bytes();
                    let (font, base) = if text4_width(nb, &NAME_26) <= 211 { (&NAME_26, y + 54) } else { (&NAME_22, y + 52) };
                    w::field(lcd, 15, 211, y + 29, y + 60, base, w::fit(nb, font, 211), &w::st(font, TEXT, SURFACE, Align::Left));
                    w::freq(lcd, 15, 226, y + 75, f, &DIG_17, &DIG_17, SUB, SUB, SURFACE);
                }
            });
        }
        // Chips in fixed slots (HOME-4): mode, tone, shift (with R while reversed), power pinned right. An
        // absent value leaves its slot empty; nothing moves.
        let (mut m, mut t, mut o, mut pw) = (Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new());
        mode_chip(&cfg, &mut m);
        card_tone(&cfg, &mut t);
        shift_chip(&cfg, &mut o);
        pw.set(super::transmit::power_name(app, s).as_bytes()); // HOME-4.2: "No TX" where PTT can't transmit
        let pc = if super::transmit::tx_block(app, s).is_some() { AMBER } else { TEXT };
        if app.dirty(R_INFO + p, Mix::new().h(&m).h(&t).h(&o).h(&pw).h(&pc).get()) {
            let cy = y + CHIP_Y;
            w::chip_slot(lcd, w::SLOT_MODE, cy, m.as_bytes(), TEXT, LINE, SURFACE);
            w::chip_slot(lcd, w::SLOT_TONE, cy, t.as_bytes(), TEXT, LINE, SURFACE);
            w::chip_slot(lcd, w::SLOT_SHIFT, cy, o.as_bytes(), TEXT, LINE, SURFACE);
            w::chip_slot(lcd, w::SLOT_POWER, cy, pw.as_bytes(), pc, LINE, SURFACE);
        }
        // SQL-12.3: 0 7 3 1 dBm shows the level as a number where the bars are.
        let dbm = app.set.smeter_dbm;
        let st = if dbm { w::dbm_text(lv.rssi) } else { Buf::<12>::from(w::s_text(lv.rssi).as_bytes()) };
        if app.dirty(R_SUB + p, Mix::new().h(&st).h(&dbm).h(&(dbm && lv.squelch_open)).get()) {
            if dbm {
                let c = if lv.squelch_open { GREEN } else { TEXT };
                w::field(lcd, 14, 219, y + 103, y + 120, y + 117, st.as_bytes(), &w::st(&UI_12B, c, SURFACE, Align::Left));
                app.drawn.meter[p] = None; // the bars come back whole
            } else {
                if app.drawn.meter[p].is_none() {
                    w::field(lcd, 14, 219, y + 103, y + 120, y + 117, b"", const { &w::st(&UI_12B, TEXT, SURFACE, Align::Left) });
                }
                w::field(lcd, 212, 21, y + 103, y + 120, y + 117, st.as_bytes(), const { &w::st(&UI_12B, TEXT, SURFACE, Align::Right) });
            }
        }
        // S-meter: only the segments that changed (HOME-6, HOME-7).
        if !dbm {
            let lit = w::meter_level(lv.rssi);
            let col = if lv.squelch_open { GREEN } else { DIM };
            let old = app.drawn.meter[p].filter(|&(_, o)| o == lv.squelch_open).map(|(l, _)| l);
            if app.drawn.meter[p] != Some((lit, lv.squelch_open)) {
                w::meter(lcd, 14, y + 109, old, lit, col, SURFACE);
                app.drawn.meter[p] = Some((lit, lv.squelch_open));
            }
        }
    } else {
        // Left: the name (channel) or the frequency; right: the frequency small (channel) or the scan's hits.
        let mut right = Buf::<16>::new();
        if lv.scanning {
            let _ = uwrite!(right, "{} hits", lv.hits);
        } else if !name.is_empty() {
            right.set(w::freq3(f).as_bytes());
        }
        let (x, c) = if name.is_empty() { (130, TEXT) } else { (170, SUB) };
        let info = app.dirty(R_INFO + p, Mix::new().h(&right).get());
        let freq = app.dirty_shape(R_FREQ + p, Mix::new().h(&f).h(&name).get(), name.is_empty());
        if let Some(clear) = freq {
            // A new layout redraws the row in bands with the right-hand text, which shares it.
            redraw_box!(clear, lcd, (15, y + 25, 211, 24, SURFACE), {
                if name.is_empty() {
                    w::freq(lcd, 15, 130, y + 45, f, &DIG_26, &DIG_17, TEXT, SUB, SURFACE);
                } else {
                    // The 22 px name when it fits beside the frequency, else 15 px bold.
                    let nb = name.as_bytes();
                    let font = if text4_width(nb, &NAME_22) <= 150 { &NAME_22 } else { &UI_15B };
                    w::field(lcd, 15, 150, y + 25, y + 49, y + 44, w::fit(nb, font, 150), &w::st(font, TEXT, SURFACE, Align::Left));
                }
                if clear {
                    w::field(lcd, x, 226 - x, y + 29, y + 47, y + 45, right.as_bytes(), &w::st(&UI_12, c, SURFACE, Align::Right));
                }
            });
        }
        if info && freq != Some(true) {
            w::field(lcd, x, 226 - x, y + 29, y + 47, y + 45, right.as_bytes(), &w::st(&UI_12, c, SURFACE, Align::Right));
        }
        // The value line: the scan's mode and last hit, or mode, tone and shift in fixed columns (HOME-4), so a
        // value coming or going never moves the others.
        let mut cols = [Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new()];
        if lv.scanning {
            let c = &mut cols[0];
            c.set(slot::MODES[cfg.mode as usize % slot::MODES.len()].as_bytes());
            if lv.hits > 0 && lv.last_hit != f {
                let mut b = Buf::<16>::new();
                b.set(b"last ");
                let hn = app.hit_name(s, live, cx.store); // HOME-4.1: a channel scan's hit by name
                if hn.is_empty() {
                    b.push(w::freq3(lv.last_hit).as_bytes());
                } else {
                    b.push(hn.as_bytes());
                }
                cols[1] = b;
            }
        } else {
            mode_chip(&cfg, &mut cols[0]);
            card_tone(&cfg, &mut cols[1]);
            shift_text(&cfg, false, &mut cols[2]); // "TX 146.520" for a split
        }
        if app.dirty(R_SUB + p, Mix::new().h(&lv.scanning).h(&cols).get()) {
            // Off-screen, sent once (DISP-25).
            canvas::banded(lcd, 15, y + 51, 211, 15, SURFACE, |b| {
                let xs: [(i32, i32); 3] = if lv.scanning { [(15, 36), (51, 175), (226, 0)] } else { [(15, 36), (51, 56), (107, 119)] };
                for (c, (x, wd)) in cols.iter().zip(xs) {
                    if wd > 0 && !c.is_empty() {
                        w::field(b, x, wd, y + 51, y + 65, y + 61, c.as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
                    }
                }
            });
        }
    }
}

/// Knob or ▲▼ on home: a step in VFO mode (NAV-2), a channel in channel mode (NAV-1); big with SIDE2 (NAV-3).
pub(crate) fn step(app: &mut App, d: i32, big: bool, store: &impl ChannelStore) {
    let a = app.active as usize;
    // BCR-25: a VFO in the broadcast band steps by the FM step (0 2 6 1)
    let fm_step = (!big && app.in_bcast_band(a as u8)).then(|| crate::settings::FM_STEPS[app.set.fm_step as usize % 3].0 as i32 * 1000);
    if app.slots[a].on_channel() {
        let (z, pos) = (app.slots[a].zone, app.slots[a].pos);
        let n = app.zlen(store, z) as i32; // CHAN-34: Local's from its list
        if n == 0 {
            return;
        }
        let mut p = (pos as i32 + d * if big { 10 } else { 1 }).rem_euclid(n);
        // CHAN-26: channels below 18 MHz are HF mode's; stepping goes past them
        for _ in 0..n {
            if app.zat(store, z, p as u16).is_some_and(|c| !c.hf_mode_only()) {
                break;
            }
            p = (p + d.signum()).rem_euclid(n);
        }
        let ch = app.zat(store, z, p as u16);
        (app.slots[a].pos, app.slots[a].ch) = (p as u16, ch);
        app.send_slot(a as u8);
    } else {
        let s = &mut app.slots[a];
        let step = if big { 100_000 } else { fm_step.unwrap_or(STEPS[s.cfg.step as usize % STEPS.len()].0 as i32) };
        // Plain steps from where it is (146.520 stays on its 5 kHz raster with a 12.5 kHz step); snapping to
        // the step is for typed entry (VFO-12). Kept inside what the chips tune.
        let f = (s.vfo as i32 + d * step).clamp(mhz(1) as i32 + 80_000, mhz(1300) as i32) as Freq10;
        let from = s.vfo;
        (s.vfo, s.clar) = (f, 0); // VFO-22: a retune clears the clarifier
        s.mem[band_near(f)] = f;
        if app.hf_starts_low(a as u8, from) {
            app.send_slot(a as u8); // TX-36: the power went to Low with it
        } else {
            app.out.push(RadioCmd::TuneSlot { slot: a as u8, freq: f });
        }
    }
}

/// ◀▶ on home: next band in VFO mode (NAV-2), next zone with channels in channel mode (NAV-1, CHAN-11); Local,
/// when on, after the last stored zone (CHAN-34).
pub(crate) fn group(app: &mut App, d: i32, store: &impl ChannelStore) {
    let a = app.active as usize;
    let region = app.set.region();
    if app.slots[a].on_channel() {
        let from = app.slots[a].zone;
        let zones = store.zones() as i32;
        let ring = zones + crate::local::on(app) as i32;
        let zone = |r: i32| if r == zones { LOCAL } else { r as ZoneId };
        let mut r = if from == LOCAL { zones } else { from as i32 };
        for _ in 0..ring {
            r = (r + d).rem_euclid(ring);
            if first_slot_channel(app, store, zone(r)).is_some() {
                break;
            }
        }
        let z = zone(r);
        if let Some(p) = first_slot_channel(app, store, z).filter(|_| z != from) {
            let ch = app.zat(store, z, p);
            let s = &mut app.slots[a];
            (s.zone, s.pos, s.ch) = (z, p, ch);
            app.send_slot(a as u8);
        }
    } else {
        let s = &mut app.slots[a];
        let (b, from) = (band_near(s.vfo), s.vfo);
        s.mem[b] = s.vfo;
        let nb = (b as i32 + d).rem_euclid(BANDS.len() as i32) as usize;
        s.vfo = s.mem[nb];
        s.cfg.mode = crate::slot::band_mode(nb, region);
        s.cfg.narrow = s.cfg.mode == 1;
        s.cfg.step = crate::slot::band_step(nb, region);
        if s.cfg.offset != 0 && !s.cfg.split {
            s.cfg.offset = s.cfg.offset.signum() * crate::slot::band_shift(nb, region); // a split's TX stays where it is
        }
        app.hf_starts_low(a as u8, from); // TX-36
        app.send_slot(a as u8);
    }
}

/// V/M: VFO <-> channel (only with channels stored).
/// VFO-23 (nicFW's "Chan 2 VFO"): hold 9 on a channel, then OK, puts the slot's VFO on it, frequency and settings
/// (shift or split, tones, mode, power, step, busy lock), and switches to the VFO. The channel is untouched.
pub(crate) fn copy_to_vfo(app: &mut App) {
    let a = app.active as usize;
    let s = &mut app.slots[a];
    let Some(c) = s.ch.filter(|_| !s.vfo_mode) else { return };
    let mut cfg = c.cfg;
    if cfg.squelch == crate::slot::SQ_GLOBAL {
        cfg.squelch = s.cfg.squelch; // "Global" means the VFO's own level (SQL-6)
    }
    (s.vfo, s.cfg, s.vfo_mode) = (c.rx, cfg, true);
    app.send_slot(a as u8);
    app.show_toast(b"Copied to VFO", b"V/M: channel", false);
}

/// HOME-15.3: the word muted while receiving shows beside the RX tag.
const MUTED: &[u8] = b"Muted";

/// A compact card's header state: the clarifier (or APRS-21's priority) in its colour `cc` when idle with one, else the
/// state (RX in bold; muted while receiving the bold RX, with "Muted" left of it, HOME-15.3).
fn compact_state<'a>(state: &'a str, clr: &'a Buf<16>, sc: u16, cc: u16) -> (&'a [u8], u16, &'static ferrite_ui::font4::Font4) {
    if state == "RX muted" {
        return (b"RX", GREEN, &UI_12B);
    }
    let f = if matches!(state, "RX" | "TX") { &UI_12B } else { &UI_12 };
    if state == "idle" && !clr.is_empty() { (clr.as_bytes(), cc, f) } else { (state.as_bytes(), sc, f) }
}

/// How wide the header's right side draws (HOME-4.3): the state (a tag on the large card for RX, MON, FM; idle shows
/// nothing there) and the clarifier or APRS note beside it, so the header can take the rest.
pub(crate) fn head_right_w(big: bool, state: &str, clr: &Buf<16>) -> i32 {
    let cw = if clr.is_empty() { 0 } else { text4_width(clr.as_bytes(), &UI_12) };
    // the note beside a state word (the APRS priority, APRS-21): the word, 8 px, the note
    let beside = |w: i32| if cw > 0 { w + 8 + cw } else { w };
    let muted = if state == "RX muted" { text4_width(MUTED, &UI_12) + 8 } else { 0 };
    if !big {
        let (t, _, f) = compact_state(state, clr, 0, 0);
        let w = text4_width(t, f) + muted;
        return if state == "idle" { w } else { beside(w) };
    }
    match state {
        "RX muted" => beside(32 + muted - 8), // HOME-15.3: the RX tag and "Muted" left of it
        "RX" | "TX" | "MON" | "FM" => (text4_width(state.as_bytes(), &UI_12B) + 10).max(if cw > 0 { 32 + cw } else { 0 }),
        "idle" => cw,
        _ => beside(text4_width(state.as_bytes(), &UI_12)),
    }
}

/// The first channel in a zone a home slot can sit on: not one below 18 MHz (CHAN-26); Local's nearest (CHAN-34).
pub(crate) fn first_slot_channel(app: &App, store: &impl ChannelStore, z: ZoneId) -> Option<u16> {
    (0..app.zlen(store, z)).find(|&i| app.zat(store, z, i).is_some_and(|c| !c.hf_mode_only()))
}

fn toggle_vfo(app: &mut App, store: &impl ChannelStore) {
    let a = app.active as usize;
    let need = app.slots[a].vfo_mode && app.slots[a].ch.is_none();
    let first = if need { (0..store.zones()).find_map(|z| Some(z).zip(first_slot_channel(app, store, z))) } else { None };
    let s = &mut app.slots[a];
    if s.vfo_mode {
        if s.ch.is_none() {
            match first {
                Some((z, p)) => {
                    (s.zone, s.pos) = (z, p);
                    s.ch = store.zone_at(z, p);
                }
                None => return app.show_toast(b"No channels yet", b"", false),
            }
        }
        s.vfo_mode = false;
    } else {
        s.vfo_mode = true;
    }
    app.send_slot(a as u8);
}

#[cfg(test)]
mod tests {
    use crate::slot::{card_tone, DEFAULT_CFG, DCS_FLAG, DCS_INV};
    use ferrite_ui::assets::UI_12;
    use ferrite_ui::gfx::text4_width;
    use ferrite_ui::text::Buf;

    /// HOME-4: the card's tone chip shows both tones: the same both ways as before, an RX-only tone as "R ...", a
    /// different RX tone as a "*" after the sent one; reverse swaps sent and heard. Each fits the tone slot (57 px,
    /// 2 px a side of frame and padding) and the compact card's tone column (56 px).
    #[test]
    fn tone_chip_shows_both_tones() {
        let d023n = DCS_FLAG | 0o23;
        let d754i = DCS_FLAG | DCS_INV | 0o754;
        let show = |tx: u16, rx: u16, reverse: bool| {
            let c = crate::slot::SlotCfg { tx_tone: tx, rx_tone: rx, reverse, ..DEFAULT_CFG };
            let mut b = Buf::<16>::new();
            card_tone(&c, &mut b);
            assert!(text4_width(b.as_bytes(), &UI_12) <= 53, "{:?} too wide", core::str::from_utf8(b.as_bytes()));
            std::string::String::from_utf8(b.as_bytes().to_vec()).unwrap()
        };
        assert_eq!(show(1000, 1000, false), "T 100.0");
        assert_eq!(show(d023n, d023n, false), "D023N");
        assert_eq!(show(0, 0, false), "");
        assert_eq!(show(0, d023n, false), "R D023N", "RX only (the bench's case)");
        assert_eq!(show(0, 885, false), "R 88.5");
        assert_eq!(show(0, 2541, false), "R 254.1");
        assert_eq!(show(1000, 885, false), "T100.0*");
        assert_eq!(show(2541, 885, false), "T254.1*");
        assert_eq!(show(d754i, d023n, false), "D754I*");
        assert_eq!(show(1000, 0, false), "T 100.0", "TX only: as before");
        // Reverse: it sends the RX tone and listens for the TX one.
        assert_eq!(show(0, d023n, true), "D023N");
        assert_eq!(show(d023n, 0, true), "R D023N");
    }

    /// HOME-4.3 (decision 178): "012 · " and a 16-character zone name fit the header whole beside the idle and the
    /// tagged states (RX, MON, FM) on the large card and beside the compact cards' states; with a clarifier or the
    /// APRS note beside a tag, the header keeps at least 120 px and the name is clipped at a character.
    #[test]
    fn zone_name_fits_the_card_header() {
        let head = "999 \u{b7} WWWWWWWWWWWWWWWW";
        let hw = |big: bool, state: &str, clr: &[u8]| (226 - 6 - super::head_right_w(big, state, &Buf::from(clr)) - 34).max(120);
        let need = text4_width(head.as_bytes(), &UI_12);
        for st in ["idle", "RX", "MON", "FM"] {
            assert!(need <= hw(true, st, b""), "large card, {st}: {need} > {}", hw(true, st, b""));
        }
        // ("scanning" heads "SCAN · band": no zone name)
        for st in ["idle", "RX", "TX", "MON"] {
            assert!(need <= hw(false, st, b""), "compact card, {st}: {need} > {}", hw(false, st, b""));
        }
        assert!(hw(true, "RX", "A\u{b7}B first".as_bytes()) >= 120 && hw(false, "idle", b"CLR +2.5") >= 120);
        // the single + scope layout's header (26..232 beside the state)
        assert!(need <= (232 - 6 - super::head_right_w(true, "RX", &Buf::new()) - 26).max(140));
    }
}
