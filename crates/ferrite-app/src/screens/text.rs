//! Text entry (manual chapter 7, KEY-12; SETUP-12): multi-tap on the digits, \* cycles upper case, lower case,
//! digits and symbols, # is a space, BACK deletes (with nothing left it leaves), OK keeps. The text being typed
//! is the App's one text buffer; the screen that opened it reads it back (the save form's name) or OK hands it
//! over (a channel's name, the callsign). A key tapped again within a second steps through its letters.
//! Callsigns are capitals and digits only, and * switches letters and digits (the SetupCall mock-up). ◀▶ move
//! the cursor (TEXT-12): typing, spaces and BACK work at it.

use super::forms::{field_box, keypad, para};
use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::{ChannelStore, ZoneId};
use crate::keys::{KeyEvent, Press};
use crate::{App, Screen, MENU_TIMEOUT_MS};
use ferrite_core::keys::Key;
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;
use ufmt::uwrite;

/// Whose text it is: where OK writes it. Every caller names its own target ([`super::nav::Dest::Text`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextFor {
    /// The channel the active slot is on, in the channel editor (A4).
    ChannelName,
    /// A stored channel by number: rename from the channel browser (C8).
    Channel(u16),
    /// The new channel in Save as channel or Add repeater (A9, A10): the form reads the buffer.
    SaveName,
    /// A zone by number: rename in the zone editor (A12).
    Zone(ZoneId),
    /// A range preset by index: its name in the preset editor (A11).
    Preset(u8),
    /// 0 8 6 1 (SETUP-12).
    Callsign,
    /// 0 7 3 2's text, the boot screen's line (BOOT-9).
    PowerOn,
    /// A DTMF memory's name (DTMF-12).
    DtmfName(u8),
    /// A grid square (GPS-21): the fixed position (A8), or a new waypoint.
    GridFixed,
    GridWaypoint,
    /// 0 4 2 3: the one pager capcode kept (DEC-4), digits only; empty keeps every capcode.
    Capcode,
    /// 0 6 6: the clock set by hand (GPS-23), 12 digits YYYYMMDDhhmm in local time.
    Clock,
    /// 0 5 9 2: a custom APRS path (APRS-26), capitals and digits, # for the comma.
    AprsPath,
    /// A new contact's callsign, then a contact's name (APRS-37): the App keeps the callsign between them.
    ContactCall,
    ContactName,
    /// 0 5 9 9: who auto reply answers (APRS-57.1): capitals, digits and `-`, * typing the wildcard.
    ReplyTo,
    ActivationRef,
    ActivationCall,
}

/// What a text takes: any text, a channel name (the name set, CHAN-31), capitals and digits (a callsign, a grid
/// square), digits only, or a path (capitals and digits with # typing the comma between entries).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Any,
    Name,
    Caps,
    Digits,
    Path,
    /// APRS-57.1: a callsign pattern: capitals and digits, * typing the wildcard.
    Pattern,
}

impl TextFor {
    fn label(self) -> &'static str {
        match self {
            TextFor::ChannelName | TextFor::Channel(_) | TextFor::SaveName => "Channel name",
            TextFor::Zone(_) => "Zone name",
            TextFor::Preset(_) => "Range name",
            TextFor::Callsign => "Callsign",
            TextFor::PowerOn => "Power-on text",
            TextFor::DtmfName(_) => "DTMF memory name",
            TextFor::GridFixed | TextFor::GridWaypoint => "Grid square",
            TextFor::Capcode => "Pager capcode",
            TextFor::Clock => "Date and time",
            TextFor::AprsPath => "APRS path",
            TextFor::ContactCall => "Contact callsign",
            TextFor::ContactName => "Contact name",
            TextFor::ReplyTo => "Reply to",
            TextFor::ActivationRef => "Activation reference",
            TextFor::ActivationCall => "Contact callsign",
        }
    }

    pub fn max(self) -> usize {
        match self {
            TextFor::Callsign => 10,
            TextFor::Preset(_) | TextFor::ContactName => 12,
            TextFor::ContactCall | TextFor::ReplyTo => 9,
            TextFor::DtmfName(_) => 10,
            TextFor::GridFixed | TextFor::GridWaypoint => 6,
            TextFor::Capcode => 7,
            TextFor::Clock => 12,
            _ => 16,
        }
    }

    /// What the text starts as: the name or callsign being changed (SaveName: the form's own, already there).
    pub(crate) fn current(self, app: &App, store: &impl ChannelStore) -> Buf<16> {
        let s = &app.slots[app.active as usize];
        match self {
            TextFor::ChannelName => s.ch.map_or(Buf::new(), |c| Buf::from(c.name())),
            TextFor::Channel(n) => store.get(n).map_or(Buf::new(), |c| Buf::from(c.name())),
            TextFor::SaveName => app.text,
            TextFor::Zone(z) => store.zone_name(z).unwrap_or_default(),
            TextFor::Preset(i) => store.data().preset(i).map_or(Buf::new(), |p| Buf::from(p.name.as_bytes())),
            TextFor::Callsign => Buf::from(app.set.callsign.as_bytes()),
            TextFor::PowerOn => Buf::from(app.set.pon_text.as_bytes()),
            TextFor::DtmfName(i) => Buf::from(store.data().dtmf_name(i).as_bytes()),
            TextFor::GridFixed if app.set.fixed_on => Buf::from(&super::lists::grid(app.set.fixed)),
            TextFor::GridFixed | TextFor::GridWaypoint | TextFor::Clock => Buf::new(),
            TextFor::Capcode if app.set.pocsag_cap == 0 => Buf::new(),
            TextFor::Capcode => {
                let mut b = Buf::new();
                let _ = uwrite!(b, "{}", app.set.pocsag_cap);
                b
            }
            TextFor::AprsPath => Buf::from(app.aprs_custom_path()),
            TextFor::ActivationRef => Buf::from(app.activation.reference.as_bytes()),
            TextFor::ActivationCall => Buf::new(),
            TextFor::ReplyTo => Buf::from(app.aprs_cfg().reply_to_text()),
            TextFor::ContactCall => Buf::from(app.contact_call.as_bytes()),
            TextFor::ContactName => store.data().contact_of(app.contact_call.as_bytes()).map_or(Buf::new(), |c| Buf::from(c.name.as_bytes())),
        }
    }

    fn kind(self) -> Kind {
        match self {
            TextFor::Callsign | TextFor::GridFixed | TextFor::GridWaypoint | TextFor::ContactCall | TextFor::ActivationRef | TextFor::ActivationCall => Kind::Caps,
            TextFor::Capcode | TextFor::Clock => Kind::Digits,
            TextFor::AprsPath => Kind::Path,
            TextFor::ReplyTo => Kind::Pattern,
            TextFor::ChannelName | TextFor::Channel(_) | TextFor::SaveName | TextFor::ContactName => Kind::Name,
            _ => Kind::Any,
        }
    }

    /// Capitals and digits only, * switching them (a callsign, a grid square, a path).
    fn callsign(self) -> bool {
        matches!(self.kind(), Kind::Caps | Kind::Path | Kind::Pattern)
    }
}

/// Multi-tap: what each key types in each mode (KEY-12). Mode 2 (digits) types the digit itself.
pub(crate) const UPPER: [&[u8]; 10] = [b"0", b".-/1", b"ABC2", b"DEF3", b"GHI4", b"JKL5", b"MNO6", b"PQRS7", b"TUV8", b"WXYZ9"];
pub(crate) const LOWER: [&[u8]; 10] = [b"0", b".-/1", b"abc2", b"def3", b"ghi4", b"jkl5", b"mno6", b"pqrs7", b"tuv8", b"wxyz9"];
const SYMBOLS: [&[u8]; 10] = [b"0", b".,?!", b"-/_'", b"()&+", b"@#*=", b":;%\"", b"<>[]", b"$^~|", b"{}\\`", b"0123"];
/// A channel name's symbols (CHAN-31): only those the name set has, on the keys that type them in [`SYMBOLS`].
const NAME_SYMBOLS: [&[u8]; 10] = [b"0", b".!", b"-/_'", b"()&+", b"#", b"5", b"6", b"7", b"8", b"9"];
const DIGITS: [&[u8]; 10] = [b"0", b"1", b"2", b"3", b"4", b"5", b"6", b"7", b"8", b"9"];
/// Callsigns' capitals (APRS-37, SETUP-12): an APRS callsign, pattern or path takes only "-" besides capitals and
/// digits (AX.25: `VE7XYZ-9`, `WIDE1-1`); the station callsign (0 8 6 1) "-" and "/" (`VE7XYZ-7`, `VE7XYZ/P`), as setup's page; a grid
/// square none.
const APRS_CALL: [&[u8]; 10] = [b"0", b"-1", b"ABC2", b"DEF3", b"GHI4", b"JKL5", b"MNO6", b"PQRS7", b"TUV8", b"WXYZ9"];
const STATION_CALL: [&[u8]; 10] = [b"0", b"-/1", b"ABC2", b"DEF3", b"GHI4", b"JKL5", b"MNO6", b"PQRS7", b"TUV8", b"WXYZ9"];
const GRID: [&[u8]; 10] = [b"0", b"1", b"ABC2", b"DEF3", b"GHI4", b"JKL5", b"MNO6", b"PQRS7", b"TUV8", b"WXYZ9"];
pub(crate) const MODES: [&str; 4] = ["ABC", "abc", "123", "Sym"];
/// A key tapped again within this steps through its letters.
pub(crate) const TAP_MS: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Text {
    pub to: TextFor,
    /// Index into [`MODES`].
    pub mode: u8,
    /// Multi-tap: the last digit key + 1 (0 none) and when it was tapped.
    pub last: u8,
    /// The cursor (TEXT-12): how many characters are after it; 0 at the end.
    pub tail: u8,
    pub at: u32,
}

impl Text {
    pub fn new(to: TextFor) -> Self {
        let mode = if to.kind() == Kind::Digits { 2 } else { 0 };
        Text { to, mode, last: 0, tail: 0, at: 0 }
    }

    /// The layout the keys type in ([`legend`]): a channel name's symbols in place of the full set (CHAN-31).
    fn keys(self) -> u8 {
        match (self.to, self.to.kind(), self.mode) {
            (_, Kind::Name, 3) => 4,
            // the capitals of a callsign, grid square or path: their own key 1 (APRS-37, SETUP-12)
            (TextFor::Callsign | TextFor::ActivationRef | TextFor::ActivationCall, _, 0) => 6,
            (TextFor::GridFixed | TextFor::GridWaypoint, _, 0) => 7,
            (_, Kind::Caps | Kind::Path | Kind::Pattern, 0) => 5,
            (_, _, m) => m,
        }
    }

    /// The mode # goes to: KEY-12's case and symbols; a callsign has capitals and digits only (SETUP-12).
    fn next_mode(self) -> u8 {
        if self.to.kind() == Kind::Digits {
            return 2; // digits only: # changes nothing
        }
        match (self.to.callsign(), self.mode) {
            (true, 0) => 2,
            (true, _) => 0,
            (false, m) => (m + 1) % 4,
        }
    }
}

/// Opens text entry for `to`, starting from its current text, remembering the screen that asked for BACK.
pub(crate) fn open(app: &mut App, to: TextFor, store: &impl ChannelStore) {
    app.text = to.current(app, store);
    app.open(super::nav::Dest::Text(to));
}

/// The characters key d types in a mode (4: a channel name's symbols, CHAN-31; 5-7 the capitals of an APRS
/// callsign, the station callsign and a grid square).
pub(crate) fn legend(mode: u8, d: u8) -> &'static [u8] {
    [UPPER, LOWER, DIGITS, SYMBOLS, NAME_SYMBOLS, APRS_CALL, STATION_CALL, GRID][mode as usize % 8][d as usize % 10]
}

/// One digit key into a text: a new character, or the next one on the same key within [`TAP_MS`]. Returns
/// the (last, at) pair to remember. Shared with setup's callsign page.
pub(crate) fn tap(b: &mut Buf<16>, max: usize, mode: u8, d: u8, last: u8, at: u32, now: u32) -> (u8, u32) {
    let keys = legend(mode, d);
    let again = last == d + 1 && now.wrapping_sub(at) < TAP_MS && !b.is_empty() && keys.len() > 1;
    if again {
        let cur = b.as_bytes()[b.len() - 1];
        let i = keys.iter().position(|&c| c == cur).map_or(0, |i| (i + 1) % keys.len());
        let mut t = Buf::<16>::from(&b.as_bytes()[..b.len() - 1]);
        t.push(&keys[i..i + 1]);
        *b = t;
    } else if b.len() < max {
        b.push(&keys[..1]);
    }
    (d + 1, now)
}

/// BACK in a text: the last character goes.
pub(crate) fn delete(b: &mut Buf<16>) {
    let n = b.len().saturating_sub(1);
    *b = Buf::from(&b.as_bytes()[..n]);
}

impl View for Text {
    fn name(self) -> &'static str {
        "Text entry"
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        b.set(b"Text");
    }

    fn timeout(self) -> Option<u32> {
        Some(MENU_TIMEOUT_MS)
    }

    fn own_arrows(self) -> bool {
        true
    }

    fn key(self, app: &mut App, e: KeyEvent, store: &impl ChannelStore) {
        let mut s = self;
        // TEXT-12: the keys work on the text before the cursor; what is after it is put back after.
        let n = app.text.len();
        let k = (s.tail as usize).min(n);
        let after = Buf::<16>::from(&app.text.as_bytes()[n - k..]);
        app.text = Buf::from(&app.text.as_bytes()[..n - k]);
        let max = self.to.max() - k;
        match (e.key, e.press) {
            (Key::Left, Press::Short | Press::Repeat) => {
                s.tail = (k + !app.text.is_empty() as usize) as u8;
                s.last = 0;
            }
            (Key::Right, Press::Short | Press::Repeat) => {
                s.tail = k.saturating_sub(1) as u8;
                s.last = 0;
            }
            (Key::Digit(d), Press::Short) => {
                (s.last, s.at) = tap(&mut app.text, max, s.keys(), d, s.last, s.at, app.now);
            }
            (Key::Hash, Press::Short) => {
                // KEY-12: # (marked ^) case and symbols; a callsign has capitals and digits only (SETUP-12).
                s.mode = self.next_mode();
                s.last = 0;
            }
            // KEY-12: * (marked _) the space; a path's comma.
            (Key::Star, Press::Short) if matches!(self.to.kind(), Kind::Any | Kind::Name | Kind::Path | Kind::Pattern) => {
                if app.text.len() < max {
                    app.text.push(match self.to.kind() {
                        Kind::Path => b",",
                        Kind::Pattern => b"*", // APRS-57.1: the wildcard
                        _ => b" ",
                    });
                }
                s.last = 0;
            }
            (Key::Exit, Press::Short) if app.text.is_empty() && after.is_empty() => return app.back(),
            (Key::Exit, Press::Short) => {
                delete(&mut app.text); // at the start, nothing before the cursor to delete
                s.last = 0;
            }
            (Key::Menu, Press::Short) => {
                app.text.push(after.as_bytes());
                return keep(self.to, app, store);
            }
            _ => {}
        }
        app.text.push(after.as_bytes());
        app.screen = Screen::Text(s);
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, _: &Cx<impl ChannelStore>) {
        let t = app.text;
        if app.dirty(R_TOP, Mix::new().any(&self.to).h(&t).h(&self.mode).h(&self.tail).get()) {
            w::field(lcd, 8, 224, 24, 41, 37, self.to.label().as_bytes(), const { &w::st(&UI_12B, TEXT, BG, Align::Left) });
            field_box(lcd, 8, 44, 224, 30, b"", t.as_bytes(), true, self.tail == 0);
            if self.tail > 0 {
                // TEXT-12: the cursor inside the text, where field_box draws it (8 px in, baseline 64)
                let k = t.len() - (self.tail as usize).min(t.len());
                let x = 16 + text4_width(&t.as_bytes()[..k], &UI_12B);
                lcd.fill(x, 64 - 12, 2, 15, TEXT);
            }
            let mut c = Buf::<16>::new();
            let _ = uwrite!(c, "{}/{}", t.len(), self.to.max());
            w::field(lcd, 8, 100, 78, 95, 91, c.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
            ferrite_ui::canvas::banded(lcd, 108, 78, 132, 17, BG, |lcd| {
                w::tag(lcd, 232, 78, MODES[self.mode as usize % 4].as_bytes(), BLUE, BG);
            });
        }
        if app.dirty(R_HELP, Mix::new().h(&self.mode).any(&self.to).get()) {
            let m = self.keys();
            let (star, hash_): (&[u8], &[u8]) = if self.to.kind() == Kind::Digits {
                (b"", b"")
            } else if self.to.callsign() {
                // KEY-12: * (marked _) the space or a path's comma, # (marked ^) the letters' mode.
                (
                    match self.to.kind() {
                        Kind::Path => b"comma",
                        Kind::Pattern => b"any",
                        _ => b"",
                    },
                    if self.mode == 0 { b"digits" } else { b"letters" },
                )
            } else {
                (b"space", MODES[self.next_mode() as usize].as_bytes())
            };
            keypad(lcd, 104, |d| legend(m, d), star, hash_);
            let tip = if self.to == TextFor::Clock {
                txt!("Year, month, day, hour, minute: 202610081430. Local time.")
            } else if self.to == TextFor::Capcode {
                txt!("The capcode's digits. Empty keeps every capcode.")
            } else if self.to == TextFor::ActivationRef {
                txt!("Enter a summit like I/LO-001 or a park like IT-0001.")
            } else if self.to == TextFor::ReplyTo {
                txt!("* alone: everyone. VE7*: every call starting VE7. Else one callsign-SSID.")
            } else if self.to == TextFor::AprsPath {
                txt!("Like WIDE1-1,WIDE2-1: up to 2 entries, * between.")
            } else if self.to.callsign() {
                txt!("Tap a key again for its next letter. Capitals only.")
            } else {
                txt!("Tap a key again for its next letter; # changes case, * is a space.")
            };
            para(lcd, 8, 206, 224, 2, 17, crate::text::load(tip).as_bytes(), &UI_12, SUB, BG);
        }
    }

    fn hints(self, app: &App) -> Hints {
        let back = if app.text.is_empty() { ("BACK", "Back") } else { ("BACK", "Delete") };
        if self.to.kind() == Kind::Digits {
            return Hints::new(&[("OK", "Keep"), back]);
        }
        Hints::new(&[("#", MODES[self.next_mode() as usize]), ("OK", "Keep"), back])
    }
}

/// OK: the text goes to the record it was typed for, and the screen that asked comes back.
fn keep(to: TextFor, app: &mut App, store: &impl ChannelStore) {
    let t = app.text;
    match to {
        TextFor::ActivationRef | TextFor::ActivationCall => return super::activation::keep(app, to, t.as_bytes(), store),
        TextFor::ContactCall | TextFor::ContactName => return super::contacts::typed(app, to, t.as_bytes(), store),
        TextFor::ChannelName => super::chedit::edit(app, |c| c.set_name(t.as_bytes())),
        TextFor::Channel(n) => {
            if let Some(mut c) = store.get(n) {
                c.set_name(t.as_bytes());
                app.write(crate::chan::ChanWrite::Put { ch: c, from: None });
                // A slot on that channel shows the new name too.
                for s in app.slots.iter_mut().filter(|s| s.ch.is_some_and(|c| c.num == n)) {
                    if let Some(sc) = &mut s.ch {
                        sc.set_name(t.as_bytes());
                    }
                }
            }
        }
        TextFor::SaveName => {}
        TextFor::Zone(z) => {
            store.data().request(crate::data::Op::RenameZone(z, t));
        }
        TextFor::Preset(i) => {
            let d = store.data();
            if let Some(mut p) = d.preset(i) {
                p.name = Buf::from(t.as_bytes());
                d.request(crate::data::Op::Preset(i, p));
            }
        }
        TextFor::Callsign => {
            app.set.set_callsign(t.as_bytes()); // SETUP-12
            app.send_tx_tones(); // CW ID sends it (VOX-12)
        }
        TextFor::PowerOn => {
            app.set.pon_text = crate::settings::Text::EMPTY;
            t.as_bytes().iter().for_each(|&c| _ = app.set.pon_text.push(c));
        }
        TextFor::DtmfName(i) => {
            store.data().request(crate::data::Op::DtmfName(i, Buf::from(t.as_bytes())));
        }
        TextFor::Capcode => {
            // DEC-4: one capcode (21 bits, up to 2,097,151); empty keeps every capcode
            let v = t.as_bytes().iter().fold(0u32, |a, &c| a.saturating_mul(10).saturating_add((c - b'0') as u32));
            if v > ferrite_core::decoders::CAPCODE_MAX {
                return app.show_toast(b"Capcode", b"up to 2097151", false);
            }
            app.set.pocsag_cap = v;
        }
        TextFor::Clock => {
            // GPS-23: a manual set, in local time; GPS time takes over when a fix comes, if 0 6 6 says "GPS"
            let Some(local) = crate::gpsdata::parse_clock(t.as_bytes()) else {
                return app.show_toast(b"Date and time", b"like 202610081430", false);
            };
            let utc = crate::gpsdata::local_to_utc(local, app.set.tz);
            app.set_clock_by_hand(utc);
            app.back();
            return app.show_toast(b"Clock", b"set", false);
        }
        TextFor::ReplyTo => {
            // APRS-57.1: empty is everyone, as *
            app.set_aprs_reply_to(if t.is_empty() { b"*".as_slice() } else { t.as_bytes() });
        }
        TextFor::AprsPath => {
            // APRS-26: up to 2 entries like WIDE2-1, chosen at once
            if !crate::aprs::path_ok(t.as_bytes()) {
                return app.show_toast(b"APRS path", b"like WIDE1-1,WIDE2-1", false);
            }
            app.set_aprs_custom_path(t.as_bytes());
        }
        TextFor::GridFixed | TextFor::GridWaypoint => {
            // GPS-21: 4 or 6 characters, the square's centre; anything else stays here to be fixed
            let Some(p) = super::lists::grid_pos(t.as_bytes()) else {
                return app.show_toast(b"Grid square", b"like FN31 or FN31PR", false);
            };
            app.back();
            if to == TextFor::GridFixed {
                (app.set.fixed, app.set.fixed_on) = (p, true);
                return app.show_toast(b"Fixed position", &super::lists::grid(p), false);
            }
            if store.data().request(crate::data::Op::AddWaypoint(p, Buf::from(t.as_bytes()))) {
                app.show_toast(b"Waypoint added", t.as_bytes(), false);
            } else {
                app.show_toast(b"Waypoint", b"no room", false);
            }
            return;
        }
    }
    app.back();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CHAN-31: a channel name's symbols page types only the name set's symbols, every one of them; other text
    /// (a zone's name, the power-on message) keeps the full symbols page.
    #[test]
    fn chan_31_name_symbols() {
        use ferrite_core::name::{name_char, NAME_SYMBOLS};
        for to in [TextFor::ChannelName, TextFor::Channel(3), TextFor::SaveName] {
            let t = Text { mode: 3, ..Text::new(to) };
            assert_eq!(t.keys(), 4);
            let all: std::vec::Vec<u8> = (0..10).flat_map(|d| legend(t.keys(), d).iter().copied()).collect();
            assert!(all.iter().all(|&c| name_char(c)), "{to:?}");
            assert!(NAME_SYMBOLS.iter().all(|c| all.contains(c) || *c == b' '), "{to:?}: a symbol missing");
        }
        for to in [TextFor::Zone(0), TextFor::PowerOn] {
            let t = Text { mode: 3, ..Text::new(to) };
            assert_eq!(t.keys(), 3);
            assert!(legend(t.keys(), 4).contains(&b'@'));
        }
    }

    /// APRS-37, APRS-26, APRS-57.1: an APRS callsign (a contact's, auto reply's pattern) or path types capitals,
    /// digits and "-" only, in both its modes; SETUP-12: the station callsign "-" and "/"; a grid square
    /// letters and digits alone. Never lower case.
    #[test]
    fn callsign_keys() {
        let typed = |t: Text| -> std::vec::Vec<u8> { (0..10).flat_map(|d| legend(t.keys(), d).iter().copied()).collect() };
        for to in [TextFor::ContactCall, TextFor::ReplyTo, TextFor::AprsPath] {
            for mode in [0, Text::new(to).next_mode()] {
                let all = typed(Text { mode, ..Text::new(to) });
                assert!(all.iter().all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-'), "{to:?} mode {mode}: {:?}", core::str::from_utf8(&all));
            }
            assert!(typed(Text::new(to)).contains(&b'-'), "{to:?}: the SSID's -");
        }
        let all = typed(Text::new(TextFor::Callsign));
        assert!(all.contains(&b'/') && all.contains(&b'-') && !all.contains(&b'.'));
        let all = typed(Text::new(TextFor::GridFixed));
        assert!(all.iter().all(|c| c.is_ascii_alphanumeric()));
        // tapping 1 twice on a contact's callsign: - then 1, then - again
        let mut b = Buf::<16>::new();
        let t = Text::new(TextFor::ContactCall);
        let (l, a) = tap(&mut b, 9, t.keys(), 1, 0, 0, 0);
        let (l, a) = tap(&mut b, 9, t.keys(), 1, l, a, 100);
        assert_eq!(b.as_bytes(), b"1");
        tap(&mut b, 9, t.keys(), 1, l, a, 200);
        assert_eq!(b.as_bytes(), b"-");
    }
}

