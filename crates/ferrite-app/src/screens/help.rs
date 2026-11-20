//! Help, inside About (MENU 0 9 8; manual chapter 36, HELP-1 to HELP-5): the manual's QR code and the key card as
//! pages, reached with ◀▶ like the self-test. Read only, rarely opened: the text lives in slow flash.

use super::shared::*;
use crate::App;
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::qr::{MANUAL_URL, QR_SIZE};
use ferrite_ui::widgets as w;

/// A key and what it does; a row with an empty key is a section heading.
pub(crate) type Row = (&'static str, &'static str);

/// Where the action column starts; keys are drawn in bold from x = 8, as in the hint bar.
pub(crate) const ACTION_X: i32 = 80;
/// The action column's width (to an 8 px right margin).
pub(crate) const ACTION_W: i32 = 240 - 8 - ACTION_X;
const ROW_H: i32 = 18;

/// HELP-3: home's keys (Appendix B, the key card), one line each.
pub(crate) const HOME_KEYS: [Row; 15] = [
    ("Knob \x18\x19", "Tune or step channels"),
    ("\x1b\x1a", "Band, or zone"),
    ("0-9", "Type freq or channel"),
    ("*", "Mute slot; point"),
    ("ABC", "Next slot, A-B-C"), // the SLOT key, named as it is marked
    ("Hold ABC", "Edit the slot"),
    ("V/M", "VFO or channel"),
    ("Hold V/M", "Zones"),
    ("MENU", "Quick settings"),
    ("Hold MENU", "More settings"),
    ("MENU 0", "Menu by number"),
    ("#", "Help; pause watch"), // HOME-10 (decision 376): Status is MENU 0 0
    ("Hold #", "Key lock"),
    ("Hold *", "Quick menu"),
    ("Hold BACK", "Home, from anywhere"),
];

/// HELP-3: the side keys and PTTs.
pub(crate) const SIDE_KEYS: [Row; 14] = [
    ("", "SIDE1 (PF1)"),
    ("Tap", "Monitor on or off"),
    ("Hold", "Monitor while held"),
    ("+ Knob \x18\x19", "Squelch"),
    ("+ \x1b\x1a", "Clarifier"),
    ("", "SIDE2 (PF2)"),
    ("Tap", "Scan start or stop"),
    ("Hold", "Frequency counter"),
    ("+ Knob", "RF gain"),
    ("+ \x18\x19", "1 MHz or 10 channels"),
    ("", "PTT"),
    ("PTT A", "Transmit, active slot"),
    ("PTT B", "Transmit on slot B"),
    ("PTT+SIDE2", "Tone burst"),
];

/// HELP-5: the menu numbers most often wanted (MENU, then the digits).
pub(crate) const MENU_NOS: [Row; 14] = [
    ("0 0", "Status page"),
    ("0 2 1", "Channels"),
    ("0 2 3", "Zones"),
    ("0 2 6", "FM broadcast"),
    ("0 3 1 6", "Band scope"),
    ("0 3 2", "Frequency counter"),
    ("0 3 3", "HF mode"),
    ("0 3 5", "Tone search"),
    ("0 4 1", "Event log"),
    ("0 5 1", "APRS heard"),
    ("0 5 2", "Messages"),
    ("0 7 2", "Home layout"),
    ("0 9 2 1", "Bluetooth"),
    ("0 9 7 1", "Run setup again"),
];

/// One key | action line at baseline `b`, or a section heading when the key is empty.
fn row(lcd: &mut impl Panel, b: i32, (key, action): Row) {
    if key.is_empty() {
        w::field(lcd, 8, 224, b - 13, b + 4, b, action.as_bytes(), const { &w::st(&UI_12B, BLUE, BG, Align::Left) });
        return;
    }
    w::key_label(lcd, 8, b, key, "", BG, true);
    w::field(lcd, ACTION_X, ACTION_W, b - 13, b + 4, b, action.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
}

/// A page of rows from the top of the content area, `h` px apart, with an optional heading line.
fn rows(lcd: &mut impl Panel, head: &[u8], rows: impl Iterator<Item = Row>, h: i32) {
    let mut b = 37;
    if !head.is_empty() {
        w::field(lcd, 8, 224, 22, 40, 36, head, const { &w::st(&UI_12B, TEXT, BG, Align::Left) });
        lcd.fill(0, 41, 240, 1, LINE);
        b = 58;
    }
    for r in rows {
        row(lcd, b, r);
        b += h;
    }
}

/// HELP-4: the holds page's rows: closer than the others' so its 17 lines fit above the hint bar.
const HOLD_ROW_H: i32 = 16;

/// HELP-4: the ten hold digits, from the table the shortcut list (MENU 0 9 1 3) runs.
const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// HELP-4: the holds that aren't digit shortcuts, before the ten (EDIT-1, KEY-26, MENU-2, KEY-5, HOME-17,
/// KEY-13; decision 376: the key holds first, as # opens this page first).
pub(crate) const HOLD_KEYS: [Row; 6] = [
    ("ABC", "Edit the slot"),
    ("V/M", "Zones"),
    ("MENU", "More settings"),
    ("BACK", "Home, from anywhere"),
    ("*", "Quick menu"),
    ("#", "Key lock"),
];

/// HELP-2: the QR code, centred, and the address under it.
fn manual(lcd: &mut impl Panel) {
    const SCALE: i32 = 5;
    let side = (QR_SIZE + 8) * SCALE;
    let y = 30;
    w::manual_qr(lcd, (240 - side) / 2, y, SCALE);
    let t = y + side + 8;
    w::field(lcd, 8, 224, t, t + 17, t + 13, "The full manual".as_bytes(), const { &w::st(&UI_12B, TEXT, BG, Align::Center) });
    w::field(lcd, 8, 224, t + 18, t + 35, t + 31, MANUAL_URL.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Center) });
}

/// Which help page, drawn once per visit (the page is static).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Help {
    Manual,
    HomeKeys,
    SideKeys,
    Digits,
    MenuNos,
}

pub(crate) fn render(app: &mut App, lcd: &mut impl Panel, h: Help) {
    if !app.dirty(R_TOP, Mix::new().any(&h).get()) {
        return;
    }
    lcd.fill(0, 21, 240, 279, BG);
    match h {
        Help::Manual => manual(lcd),
        Help::HomeKeys => rows(lcd, b"", HOME_KEYS.iter().copied(), ROW_H),
        Help::SideKeys => rows(lcd, b"", SIDE_KEYS.iter().copied(), ROW_H),
        Help::Digits => {
            // a plain array: core's iterator adapters (a zip) would land in fast flash; the heading a row of its own
            const N: usize = 1 + HOLD_KEYS.len() + 10;
            let mut all: [Row; N] = [("", ""); N];
            for (i, r) in all.iter_mut().enumerate() {
                *r = match i {
                    0 => ("", "Shortcuts - long press (hold)"),
                    i if i <= HOLD_KEYS.len() => HOLD_KEYS[i - 1],
                    i => (DIGITS[i - 1 - HOLD_KEYS.len()], super::shortcut::SHORTCUTS[i - 1 - HOLD_KEYS.len()]),
                };
            }
            rows(lcd, b"", all.iter().copied(), HOLD_ROW_H)
        }
        Help::MenuNos => rows(lcd, "MENU, then the number".as_bytes(), MENU_NOS.iter().copied(), ROW_H),
    }
}
