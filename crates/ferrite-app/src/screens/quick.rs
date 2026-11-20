//! Quick settings (MENU on home; manual chapter 11, MENU-1 and MENU-12 to MENU-22; MenuQuick mock-up): the
//! active slot's settings, RF gain and "0 More". The knob or ◀▶ change the highlighted value live. On a slot in
//! the broadcast band the rows are FM's own (step, AGC, radio monitor, sleep timer: BCR-28).

use super::advanced::Advanced;
use super::fm;
use super::nav::Dest;
use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::ChannelStore;
use crate::keys::{KeyEvent, Press};
use crate::slot::{band_name, chan_label, mode_chip, Item, QUICK};
use crate::{App, Screen, MENU_TIMEOUT_MS};
use ferrite_core::keys::Key;
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets::{self as w, Row};
use ufmt::uwrite;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Quick {
    /// The highlighted row.
    pub row: u8,
}

pub(crate) const RF_GAIN_ROW: u8 = QUICK.len() as u8 - 1;
pub(crate) const MORE_ROW: u8 = RF_GAIN_ROW + 1;
const ROW_COUNT: u8 = MORE_ROW + 1;

fn slot_item(row: u8) -> Item {
    QUICK[row.min(RF_GAIN_ROW) as usize]
}

impl View for Quick {
    fn name(self) -> &'static str {
        "Quick"
    }

    fn mockup(self) -> Option<&'static str> {
        Some("MenuQuick.png")
    }

    fn timeout(self) -> Option<u32> {
        Some(MENU_TIMEOUT_MS) // MENU-28
    }

    fn key(self, app: &mut App, e: KeyEvent, store: &impl ChannelStore) {
        use Press::*;
        let row = self.row;
        if bcast(app) {
            let n = FM_ROWS + 2;
            // BCR-31 (decision 352): the saved stations, the channel browser's Broadcast view
            let stations = |app: &mut App| app.open_screen(Screen::Browse(super::browse::broadcast_view(store, app.set.show_hidden)));
            match (e.key, e.press) {
                (Key::Up, Short | Repeat) => app.screen = Screen::Quick(Quick { row: (row % n + n - 1) % n }),
                (Key::Down, Short | Repeat) => app.screen = Screen::Quick(Quick { row: (row + 1) % n }),
                (Key::Left | Key::Right, Short | Repeat) if row < FM_ROWS => fm::option_change(app, row, if e.key == Key::Right { 1 } else { -1 }),
                (Key::Digit(0), Short) => to_advanced(app),
                (Key::Digit(d @ 1..=FM_ROWS), Short) => app.screen = Screen::Quick(Quick { row: d - 1 }),
                (Key::Digit(STATIONS_KEY), Short) => stations(app),
                (Key::Menu, Short) if row == FM_ROWS => stations(app),
                (Key::Menu, Short) if row > FM_ROWS => to_advanced(app),
                (Key::Menu, Short) => fm::option_change(app, row, 1),
                (Key::Exit, Short) => app.back(),
                _ => {}
            }
            return;
        }
        let item = slot_item(row);
        match (e.key, e.press) {
            (Key::Up, Short | Repeat) => app.screen = Screen::Quick(Quick { row: (row + ROW_COUNT - 1) % ROW_COUNT }),
            (Key::Down, Short | Repeat) => app.screen = Screen::Quick(Quick { row: (row + 1) % ROW_COUNT }),
            (Key::Digit(0), Short) => to_advanced(app),
            (Key::Digit(d), Short) => app.screen = Screen::Quick(Quick { row: d - 1 }), // MENU-5
            (Key::Side(2), Short) => app.screen = Screen::Quick(Quick { row: RF_GAIN_ROW }),
            (Key::Menu, Short) if row == RF_GAIN_ROW => {}
            (Key::Hash, Short) if row == RF_GAIN_ROW => app.set_rf_gain(Some(0)),
            (Key::Menu, Short) => match item {
                Item::Advanced => to_advanced(app),
                // TSRCH-16: a tone hint: OK uses the heard tone for RX, then asks about TX.
                Item::Tones if heard(app) != 0 => {
                    let (a, t) = (app.active, heard(app));
                    super::probe::set_tone(app, a, t, false);
                    super::confirm::ask(app, super::confirm::Ask::ToneTx(t));
                }
                // MENU 8: save the VFO as a channel, or edit the channel.
                Item::Channel if app.slots[app.active as usize].on_channel() => app.open(Dest::ChannelEdit),
                Item::Channel => app.open(Dest::SaveChannel),
                // VFO-26: on Split, OK types its TX frequency at once, as its toast says
                Item::Offset if app.slot(app.active).cfg().split => super::value::split_entry(app),
                _ => app.open(Dest::value(item)), // MENU-17
            },
            (Key::Hash, Short) => app.reset(item), // MENU-18
            (Key::Star, Short) => app.do_undo(),   // MENU-22
            (Key::Exit, Short) => app.back(),
            _ => {}
        }
    }

    fn knob(self, app: &mut App, d: i32, _: &impl ChannelStore) {
        if bcast(app) {
            if self.row < FM_ROWS {
                fm::option_change(app, self.row, d);
            }
            return;
        }
        if self.row == RF_GAIN_ROW {
            app.step_rf_gain(d, false);
        } else {
            app.change(slot_item(self.row), d);
        }
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        let row = self.row;
        if bcast(app) {
            return render_fm(app, lcd, cx, row);
        }
        let a = app.active;
        let sl = app.slots[a as usize];
        let mut head = Buf::<32>::new();
        if sl.on_channel() {
            chan_label(sl.ch.map_or(0, |c| c.num), &mut head);
        } else {
            head.push("VFO \u{b7} ".as_bytes());
            head.push(band_name(sl.freq()).as_bytes());
        }
        let mut m = Buf::<16>::new();
        mode_chip(sl.cfg(), &mut m);
        let f = sl.freq();
        if app.dirty(R_TOP, Mix::new().h(&a).h(&head).h(&m).h(&f).get()) {
            // MENU-13: whose settings these are.
            w::badge(lcd, 8, 23, a, true, BG);
            w::field(lcd, 22, 4, 21, 39, 35, b"", const { &w::st(&UI_12, SUB, BG, Align::Left) });
            // a longer head ("VFO · 1.25 m") moves the frequency right: it has room to spare
            let hw = ferrite_ui::gfx::text4_width(head.as_bytes(), &UI_12).max(66);
            w::field(lcd, 26, hw, 21, 39, 35, head.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
            let mut fb = [0u8; 16];
            let n = ferrite_core::freq::format(f, &mut fb);
            w::field(lcd, 26 + hw, 174 - hw, 21, 39, 35, &fb[..n], const { &w::st(&UI_12B, TEXT, BG, Align::Left) });
            w::field(lcd, 200, 32, 21, 39, 35, m.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Right) });
            lcd.fill(0, 40, 240, 1, LINE);
        }
        for i in 0..ROW_COUNT {
            let item = slot_item(i);
            let gain = i == RF_GAIN_ROW;
            let v = if gain { Buf::from(app.rf_gain_text()) } else { row_value(app, item) };
            let r = Row {
                num: if gain { None } else if i == MORE_ROW { Some(0) } else { Some(i + 1) },
                name: if gain { b"PF2  RF gain" } else { item.name().as_bytes() },
                value: v.as_bytes(),
                dot: if gain { app.rf_gain != Some(0) } else { !app.is_default(item) },
                selected: i == row,
                grey: !gain && app.greyed(item).is_some(),
            };
            app.menu_row(lcd, i as usize, 41 + 20 * i as i32, 20, &r);
        }
        if row == RF_GAIN_ROW {
            app.help_note(lcd, 265, 33, super::groups::Help::Note(Some(txt!("Off disables SIDE2 + knob on home."))), b"");
            return;
        }
        let item = slot_item(row);
        let why = app.greyed(item);
        let mut p = Buf::<16>::new();
        let _ = uwrite!(p, "MENU {}", if row == MORE_ROW { 0 } else { row + 1 });
        use super::groups::Help;
        let help = match why {
            _ if item == Item::Tones && heard(app) != 0 => Help::Why("OK uses the heard tone."),
            Some(w) => Help::Why(w),
            None => Help::Note(Some(item.help(!sl.on_channel()))),
        };
        app.help_note(lcd, 265, 33, help, p.as_bytes());
    }

    fn hints(self, app: &App) -> Hints {
        if bcast(app) {
            return if self.row == 0 || self.row >= FM_ROWS {
                Hints::new(&[(ARROWS_UD, "Move"), ("OK", "Open"), ("BACK", "Back")])
            } else {
                Hints::new(&[(ARROWS_UD, "Move"), ("Knob", "Change"), ("BACK", "Back")])
            };
        }
        if self.row == RF_GAIN_ROW {
            Hints::new(&[(ARROWS_UD, "Move"), ("Knob", "Change"), ("#", "Auto")])
        } else if app.undo.is_some() {
            Hints::new(&[("Knob", "Change"), ("*", "Undo"), ("BACK", "Back")])
        } else if self.row == MORE_ROW {
            Hints::new(&[(ARROWS_UD, "Move"), ("OK", "Open"), ("BACK", "Back")])
        } else if slot_item(self.row) == Item::Tones && heard(app) != 0 {
            Hints::new(&[(ARROWS_UD, "Move"), ("OK", "Use heard"), ("BACK", "Back")])
        } else {
            Hints::new(&[(ARROWS_UD, "Move"), ("Knob", "Change"), ("BACK", "Back")])
        }
    }
}

/// A row's value as drawn (the UI readback reads it too, LINK-19).
pub(crate) fn row_value(app: &App, item: Item) -> Buf<32> {
    let mut v = Buf::<32>::new();
    let mut t = Buf::<16>::new();
    app.item_text(item, true, &mut t);
    v.set(t.as_bytes());
    if item == Item::Tones && heard(app) != 0 {
        // TSRCH-16: "heard 88.5" (decision 372: the set tone beside it no longer fit at 14 px; the help line says
        // OK uses it)
        let mut h = Buf::<24>::new();
        super::shared::heard_text(heard(app), app.slot(app.active).cfg().rx_tone, &mut h);
        v.set(b"heard");
        v.push(&h.as_bytes()[5..]);
    }
    v
}

/// The active slot is in the broadcast band: quick settings are FM's (the readback says so).
pub(crate) fn fm_rows(app: &App) -> bool {
    bcast(app)
}

/// FM's rows on a broadcast slot (BCR-28), then "5 Saved stations" (BCR-31) and "0 More".
const FM_ROWS: u8 = fm::OPTIONS.len() as u8;
const STATIONS_KEY: u8 = FM_ROWS + 1;

/// The active slot is in the broadcast band: quick settings are FM's.
fn bcast(app: &App) -> bool {
    app.in_bcast_band(app.active)
}

/// BCR-28: FM's rows for a slot in the broadcast band, in quick settings' own style.
fn render_fm(app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>, row: u8) {
    let a = app.active;
    let f = (app.tuned_freq(a) / 1000) as u16;
    let mut fb = Buf::<16>::new();
    fm::fm_mhz(f, &mut fb);
    fb.push(b" MHz");
    if app.dirty(R_TOP, Mix::new().h(&a).h(&fb).h(&2u8).get()) {
        w::badge(lcd, 8, 23, a, true, BG);
        w::field(lcd, 22, 4, 21, 39, 35, b"", const { &w::st(&UI_12, SUB, BG, Align::Left) });
        w::field(lcd, 26, 66, 21, 39, 35, fb.as_bytes(), const { &w::st(&UI_12B, TEXT, BG, Align::Left) });
        w::field(lcd, 92, 140, 21, 39, 35, "Broadcast FM".as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Right) });
        lcd.fill(0, 40, 240, 1, LINE);
    }
    for i in 0..=FM_ROWS + 1 {
        let mut v = Buf::<24>::new();
        let (name, dot) = if i < FM_ROWS {
            (fm::OPTIONS[i as usize].0, fm::option_text(app, cx.dev, i, &mut v))
        } else {
            v.set(b">");
            (if i == FM_ROWS { "Saved stations" } else { "More" }, false)
        };
        let num = if i <= FM_ROWS { i + 1 } else { 0 };
        let r = Row { num: Some(num), name: name.as_bytes(), value: v.as_bytes(), dot, selected: i == row, grey: false };
        app.menu_row(lcd, i as usize, 41 + 25 * i as i32, 25, &r);
    }
    let i = row.min(FM_ROWS + 1);
    let help = match i {
        _ if i < FM_ROWS => fm::OPTIONS[i as usize].1,
        _ if i == FM_ROWS => txt!("Your broadcast channels: OK plays one."),
        _ => txt!("Every setting, by group."),
    };
    let mut p = Buf::<16>::new();
    let _ = uwrite!(p, "MENU {}", if i <= FM_ROWS { i + 1 } else { 0 });
    app.help_txt(lcd, 265, 33, help, p.as_bytes());
}

/// The active slot's tone hint (TSRCH-16), 0 none.
fn heard(app: &App) -> u16 {
    app.heard[app.active as usize]
}

/// MENU 0 from quick settings: advanced settings, one level down (BACK returns here, MENU-24).
fn to_advanced(app: &mut App) {
    app.push_back();
    app.screen = Screen::Advanced(Advanced { row: 0, from_quick: true });
}
