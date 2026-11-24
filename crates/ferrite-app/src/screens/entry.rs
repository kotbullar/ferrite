//! Direct entry (manual chapters 6, 9 and 10; NAV-4, VFO-11 to VFO-14): a digit on home opens it with that
//! digit. In VFO mode it is a frequency, \* the point (the point comes by itself after three MHz digits, so
//! 1 2 1 5 is 121.5; a second \* after 100-130 takes the point away again, for a fourth MHz digit: 1 2 9 \* \* 6
//! is 1296 MHz, the only way to type 1000-1300 MHz, with hints for both steps, VFO-21), and
//! \* after a digit past the point is OK (146 \* 52 \*, VFO-24);
//! the screen shows the digits, the band, the frequency it snaps to on the step, and whether transmit is
//! allowed there or why not. In channel mode it is a channel number, shown with its name or "empty". OK goes
//! there, BACK deletes a digit (with none left, it leaves). The same screen types a frequency for the channel
//! editor and for Add repeater.

use super::forms::para;
use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::{ChannelStore, MAX_CHANNELS};
use crate::keys::{KeyEvent, Press};
use crate::slot::{band_name, band_of, band_near, chan_label, STEPS};
use crate::{App, Screen, MENU_TIMEOUT_MS};
use ferrite_core::freq::{mhz, Freq10};
use ferrite_core::keys::Key;
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;
use ufmt::uwrite;

/// What the typed value is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryFor {
    /// The active slot's VFO (A1).
    Vfo,
    /// A channel number, in channel mode (A2).
    Channel,
    /// The channel being edited: its frequency (A4).
    ChannelFreq,
    /// Add repeater's frequency (A10).
    Repeater,
    /// Split's TX frequency on the active slot (MENU 5's 4th choice): kept as typed; BACK keeps the old shift.
    SplitTx,
    /// The band scope's centre (SCOPE-3).
    ScopeCentre,
    /// A setting that holds a frequency (MENU-31): OK keeps it when it is in the setting's range.
    Setting(FreqSet),
    /// The active slot's offset amount (hold 8, VFO-26): MHz, 10 kHz to 100 MHz, in the direction it has.
    OffsetAmount,
}

/// The settings that hold a frequency, each typed in this one entry box (MENU-31), and by the terminal's `set`
/// with the same checks (TERM-12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FreqSet {
    /// 0 5 4, the APRS frequency (64-300 MHz, 5 kHz steps).
    Aprs,
    /// 0 4 2 2, the pager frequency (136-520 MHz).
    Pager,
    /// A range preset's start or end (0 1 4, A11; 18-1300 MHz).
    RangeStart(u8),
    RangeEnd(u8),
}

impl FreqSet {
    /// The settings among them (not a store's record): [`FreqSet::set`] without the store, for the terminal.
    pub fn set_setting(self, app: &mut App, f: Freq10) -> Result<(), &'static str> {
        match self {
            FreqSet::Aprs => app.set_aprs_freq(f),
            FreqSet::Pager if !(mhz(136)..=mhz(520)).contains(&f) => Err("Out of range: pagers take 136-520 MHz"),
            FreqSet::Pager => {
                app.set.pocsag_freq = f; // saved with the settings, as the knob's change
                Ok(())
            }
            _ => Err("Not a setting: a range edge is in the store"),
        }
    }
}

/// MENU-31: a range preset with its start (or end) at `f`, with the knob's checks: 18-1300 MHz, start under end,
/// never across 18 or 64 MHz (SCAN-14).
pub(crate) fn range_edge(mut p: crate::data::RangePreset, start: bool, f: Freq10) -> Result<crate::data::RangePreset, &'static str> {
    if !(LO..=HI).contains(&f) {
        return Err("Out of range: ranges take 18-1300 MHz");
    }
    if start {
        p.start = f;
    } else {
        p.end = f;
    }
    if p.start >= p.end {
        return Err("Start under end: the range runs upward");
    }
    // SCAN-14: named as the preset editor's knob names it; the toast's head holds 20 characters
    if let Some(e) = [mhz(18), mhz(64)].into_iter().find(|&e| p.start < e && p.end > e) {
        return Err(if e == mhz(18) { "Can't span 18 MHz: one side only" } else { "Can't span 64 MHz: one side only" });
    }
    Ok(p)
}

/// MENU-31: the entry box for a frequency setting, empty (the setting's value shows under it), BACK returning to
/// the screen that opened it.
pub(crate) fn open_setting(app: &mut App, fs: FreqSet) {
    app.open_screen(Screen::Entry(Entry::new(EntryFor::Setting(fs), None)));
}

impl FreqSet {
    /// The entry's heading.
    pub fn label(self) -> &'static [u8] {
        match self {
            FreqSet::Aprs => b"APRS frequency, MHz",
            FreqSet::Pager => b"Pager frequency, MHz",
            FreqSet::RangeStart(_) => b"Range start, MHz",
            FreqSet::RangeEnd(_) => b"Range end, MHz",
        }
    }

    /// What it takes, for the entry's row.
    pub fn takes(self) -> &'static str {
        match self {
            FreqSet::Aprs => "64-300 MHz, 5k",
            FreqSet::Pager => "136-520 MHz",
            FreqSet::RangeStart(_) | FreqSet::RangeEnd(_) => "18-1300 MHz",
        }
    }

    /// The value now.
    pub fn current(self, app: &App, store: &impl ChannelStore) -> Option<Freq10> {
        match self {
            FreqSet::Aprs => Some(app.aprs_freq()),
            FreqSet::Pager => Some(app.set.pocsag_freq),
            FreqSet::RangeStart(i) => store.data().preset(i).map(|p| p.start),
            FreqSet::RangeEnd(i) => store.data().preset(i).map(|p| p.end),
        }
    }

    /// Sets it to `f`, or says why not as "head: body" (nothing changes then).
    pub fn set(self, app: &mut App, store: &impl ChannelStore, f: Freq10) -> Result<(), &'static str> {
        match self {
            FreqSet::Aprs | FreqSet::Pager => self.set_setting(app, f),
            FreqSet::RangeStart(i) | FreqSet::RangeEnd(i) => {
                let data = store.data();
                let Some(p) = data.preset(i) else { return Err("No preset: deleted meanwhile") };
                let p = range_edge(p, matches!(self, FreqSet::RangeStart(_)), f)?;
                data.request(crate::data::Op::Preset(i, p));
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub to: EntryFor,
    /// Digits and the point as typed.
    pub buf: [u8; 10],
    pub n: u8,
    /// Four MHz digits (1000-1300 MHz): the automatic point comes after the fourth.
    pub wide: bool,
    /// VFO-26.5: the offset amount's sign chosen with ▲ (+1) or ▼ (-1); 0 keeps the slot's (`offset_sign`).
    pub sign: i8,
}

/// VFO-21: the hint after \* \* on 100-130: what the fourth MHz digit makes.
const WIDE: [&str; 31] = [
    "1000-1009 MHz",
    "1010-1019 MHz",
    "1020-1029 MHz",
    "1030-1039 MHz",
    "1040-1049 MHz",
    "1050-1059 MHz",
    "1060-1069 MHz",
    "1070-1079 MHz",
    "1080-1089 MHz",
    "1090-1099 MHz",
    "1100-1109 MHz",
    "1110-1119 MHz",
    "1120-1129 MHz",
    "1130-1139 MHz",
    "1140-1149 MHz",
    "1150-1159 MHz",
    "1160-1169 MHz",
    "1170-1179 MHz",
    "1180-1189 MHz",
    "1190-1199 MHz",
    "1200-1209 MHz",
    "1210-1219 MHz",
    "1220-1229 MHz",
    "1230-1239 MHz",
    "1240-1249 MHz",
    "1250-1259 MHz",
    "1260-1269 MHz",
    "1270-1279 MHz",
    "1280-1289 MHz",
    "1290-1299 MHz",
    "1300 MHz",
];

/// VFO-26: the offset amounts hold 8 takes, 10 kHz to 100 MHz (10 Hz units).
const OFFSET_MIN: Freq10 = 1_000;
const OFFSET_MAX: Freq10 = mhz(100);

/// VFO-26: the direction a typed offset amount goes: the slot's, or from simplex or split the band's usual one
/// (up where a band has none).
/// VFO-26.5: the sign the amount goes in with: ▲ or ▼ if pressed, else the slot's ([`offset_sign`]).
fn amount_sign(e: &Entry, app: &App) -> i32 {
    if e.sign != 0 { e.sign as i32 } else { offset_sign(app) }
}

fn offset_sign(app: &App) -> i32 {
    let s = app.slot(app.active());
    match s.cfg().offset.signum() {
        d if d != 0 && !s.cfg().split => d,
        _ => match crate::slot::usual_shift(s.freq(), app.set.region()).signum() {
            0 => 1,
            d => d,
        },
    }
}

/// Home VFOs tune 18 MHz and up (VFO-14), to what the chips reach.
const LO: Freq10 = mhz(18);
const HI: Freq10 = mhz(1300);

impl Entry {
    pub fn new(to: EntryFor, first: Option<u8>) -> Self {
        let mut e = Entry { to, buf: [0; 10], n: 0, wide: false, sign: 0 };
        if let Some(d) = first {
            e.push(b'0' + d);
        }
        e
    }

    /// Prefilled with a frequency ("146.52"; trailing zeros after the point dropped), to edit or keep with OK.
    pub fn with_freq(to: EntryFor, f: Freq10) -> Self {
        let mut e = Entry { to, buf: [0; 10], n: 0, wide: f >= mhz(1000), sign: 0 };
        let (int, mut frac, mut k) = (f / 100_000, f % 100_000, 5);
        while k > 1 && frac % 10 == 0 {
            frac /= 10;
            k -= 1;
        }
        let mut idig = [0u8; 4];
        let mut m = 0;
        let mut v = int;
        loop {
            idig[m] = b'0' + (v % 10) as u8;
            m += 1;
            v /= 10;
            if v == 0 || m == idig.len() {
                break;
            }
        }
        for i in (0..m).rev() {
            e.push(idig[i]);
        }
        e.push(b'.'); // after three digits the point is already there: a second one is ignored
        let mut digits = [0u8; 5];
        for i in (0..k).rev() {
            digits[i] = b'0' + (frac % 10) as u8;
            frac /= 10;
        }
        for &c in &digits[..k] {
            e.push(c);
        }
        e
    }

    /// The three digits typed first, as a number (VFO-21's 100-130 for a fourth MHz digit).
    fn three(&self) -> u32 {
        self.buf[..3].iter().fold(0u32, |a, &c| a * 10 + (c.wrapping_sub(b'0')) as u32)
    }

    /// VFO-21: "129." typed (the point by \* on 100-130): a second \* takes it away for a fourth MHz digit.
    fn wide_next(&self) -> bool {
        self.freq_mode() && !self.wide && self.n == 4 && self.buf[3] == b'.' && (100..=130).contains(&self.three())
    }

    fn typed(&self) -> &[u8] {
        &self.buf[..self.n as usize]
    }

    fn freq_mode(&self) -> bool {
        self.to != EntryFor::Channel
    }

    fn push(&mut self, c: u8) {
        let point = self.typed().contains(&b'.');
        if !self.freq_mode() {
            if c != b'.' && self.n < 4 {
                self.buf[self.n as usize] = c;
                self.n += 1;
            }
            return;
        }
        if c == b'.' {
            // VFO-21: a second * right after the point on 100-130 takes it away, for 1000-1300 MHz
            if self.wide_next() {
                (self.n, self.wide) = (3, true);
                return;
            }
            if !point && self.n > 0 && (self.n as usize) < self.buf.len() {
                self.buf[self.n as usize] = c;
                self.n += 1;
            }
            return;
        }
        let mhz_digits = if self.wide { 4 } else { 3 };
        if !point && self.n == mhz_digits {
            self.buf[self.n as usize] = b'.'; // three (or four) MHz digits: the point comes by itself
            self.n += 1;
        }
        if (self.n as usize) < self.buf.len() {
            self.buf[self.n as usize] = c;
            self.n += 1;
        }
    }

    /// The typed frequency, 10 Hz (digits after the point beyond five are not accepted).
    pub fn freq(&self) -> Option<Freq10> {
        let t = self.typed();
        if t.is_empty() {
            return None;
        }
        let p = t.iter().position(|&c| c == b'.').unwrap_or(t.len());
        let int = t[..p].iter().fold(0u32, |a, &c| a * 10 + (c - b'0') as u32);
        let mut frac = 0u32;
        for i in 0..5 {
            frac = frac * 10 + t.get(p + 1 + i).map_or(0, |&c| (c - b'0') as u32);
        }
        Some(mhz(int) + frac)
    }

    /// A whole frequency is typed, so it goes in without OK (VFO-17): from three decimals (kHz) on, once it is on
    /// the step's grid, and always at five. Returns it, exactly as typed (VFO-12). A leading 0 counts as a MHz
    /// digit, so 0 7 8 0 0 0 is 78.000 MHz.
    fn complete(&self, app: &App) -> Option<Freq10> {
        let t = self.typed();
        let p = t.iter().position(|&c| c == b'.').filter(|_| self.freq_mode())?;
        let decimals = t.len() - p - 1;
        let f = self.freq()?;
        (decimals >= 5 || decimals >= 3 && f % step_at(app, f) == 0).then_some(f)
    }

    /// The entry holding exactly f (5 decimals), so OK's path commits it.
    fn set_freq(&mut self, f: Freq10) {
        self.wide = f >= mhz(1000);
        let mut b = Buf::<12>::new();
        let _ = uwrite!(b, "{}.{}", f / 100_000, ferrite_ui::text::Zero(f % 100_000, 5));
        let n = b.as_bytes().len().min(self.buf.len());
        self.buf[..n].copy_from_slice(&b.as_bytes()[..n]);
        self.n = n as u8;
    }

    /// VFO-24: \* now does what OK does: a frequency with the point and at least one digit after it.
    fn accepts_star(&self) -> bool {
        let t = self.typed();
        self.freq_mode() && t.iter().position(|&c| c == b'.').is_some_and(|p| p + 1 < t.len())
    }

    /// The typed channel number (0-3999, typed as 1-4000).
    pub fn number(&self) -> Option<u16> {
        let v = self.typed().iter().fold(0u32, |a, &c| a * 10 + (c - b'0') as u32);
        (v >= 1 && v <= MAX_CHANNELS as u32).then(|| v as u16 - 1)
    }
}

/// The step at f: its band's own on another band, else the active slot's (VFO-12).
fn step_at(app: &App, f: Freq10) -> Freq10 {
    let s = &app.slots[app.active as usize];
    match band_of(f) {
        Some(b) if band_of(s.vfo) != Some(b) => STEPS[crate::slot::band_step(b, app.set.region()) as usize].0,
        _ => STEPS[s.cfg.step as usize % STEPS.len()].0,
    }
}

/// Why a frequency can't be tuned here, if it can't.
/// Whether transmit is allowed at f, and the reason either way (VFO-12; the rules are chapter 15's,
/// [`ferrite_core::txplan`]). Display only: the Radio task decides when it keys.
fn tx_text(app: &App, f: Freq10, tx_built: bool) -> Result<&'static str, &'static str> {
    let r = app.set.tx_region();
    if !tx_built {
        return Err("RX-only build");
    }
    match ferrite_core::txplan::check(f, r, app.set.extend_tx) {
        Ok(()) if ferrite_core::txplan::check(f, r, false).is_err() => Ok("Extend TX"),
        Ok(()) => Ok("Allowed"),
        Err(_) if app.set.region().is_none() => Err("Region not set"),
        Err(_) => Err("Not amateur"),
    }
}

/// Why a frequency can't be tuned, as "head: body" (the toast splits it): below 18 MHz is HF mode's (VFO-14);
/// anything else no chip receives names the ranges that are (VFO-21). Each fits the entry's note on one line.
pub(crate) fn refuse(f: Freq10) -> Option<&'static str> {
    if f < LO {
        Some("< 18 MHz: HF mode, MENU 0 3 3") // VFO-14
    } else if f > HI || !ferrite_core::freq::receivable(f) {
        Some("Can't tune: 18-630, 756-1300 MHz")
    } else {
        None
    }
}

fn hf_scope_entry(to: EntryFor, app: &App) -> bool {
    to == EntryFor::ScopeCentre && super::scope::from_hf(app)
}

fn entry_refusal(to: EntryFor, app: &App, f: Freq10) -> Option<&'static str> {
    if hf_scope_entry(to, app) {
        super::scope::hf_centre_refusal(app, f)
    } else {
        refuse(f)
    }
}

impl View for Entry {
    fn name(self) -> &'static str {
        match self.to {
            EntryFor::Channel => "Channel entry",
            _ => "Frequency entry",
        }
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        b.set(if self.freq_mode() { b"Freq" } else { b"Channel" }); // 8 or fewer (SBAR-5)
    }

    fn timeout(self) -> Option<u32> {
        Some(MENU_TIMEOUT_MS)
    }

    fn key(self, app: &mut App, e: KeyEvent, store: &impl ChannelStore) {
        let mut s = self;
        match (e.key, e.press) {
            (Key::Digit(d), Press::Short) => {
                s.push(b'0' + d);
                if let Some(f) = s.complete(app) {
                    app.screen = Screen::Entry(s); // a refusal leaves the digits on show
                    let mut whole = s;
                    whole.set_freq(f);
                    return commit(whole, app, store);
                }
            }
            // VFO-24: \* after the point and a digit is OK (146 \* 52 \*), one-handed; before that it is the point
            // (and \* \* on 100-130 the fourth MHz digit, VFO-11). It never types a second point.
            (Key::Star, Press::Short) if s.accepts_star() => return commit(s, app, store),
            (Key::Star, Press::Short) => s.push(b'.'),
            // VFO-26.5: ▲ makes the offset +, ▼ makes it -; the TX row follows at once
            (Key::Up | Key::Down, Press::Short) if s.to == EntryFor::OffsetAmount => s.sign = if e.key == Key::Up { 1 } else { -1 },
            (Key::Exit, Press::Short) if s.n == 0 => return app.back(),
            // APRS-15.1: # with nothing typed puts the APRS frequency back on the region's (MENU-18's #, here too)
            (Key::Hash, Press::Short) if s.n == 0 && s.to == EntryFor::Setting(FreqSet::Aprs) => {
                app.set_aprs(crate::aprs::AprsCfg { f5k: 0, ..app.aprs_cfg() });
                let f = crate::slot::mhz3(app.aprs_freq());
                app.show_toast(b"Region default", f.as_bytes(), false);
                return app.back();
            }
            (Key::Exit, Press::Short) => {
                s.n -= 1;
                let auto = if s.wide { 4 } else { 3 };
                if s.n == auto && s.buf[auto as usize] == b'.' && s.freq_mode() {
                    s.n = auto - 1; // the automatic point goes with the digit before it
                }
                if s.n < 3 {
                    s.wide = false;
                }
            }
            (Key::Menu, Press::Short) => return commit(s, app, store),
            _ => return,
        }
        app.screen = Screen::Entry(s);
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        let a = app.active;
        // What is typed, big, with a block cursor.
        if app.dirty(R_TOP, Mix::new().h(&a).any(&self.to).h(&self.typed()).get()) {
            ferrite_ui::canvas::banded(lcd, 0, 21, 240, 92, BG, |lcd| {
                w::badge(lcd, 8, 26, a, true, BG);
                let what = self.what();
                w::field(lcd, 28, 204, 24, 41, 37, what, const { &w::st(&UI_12, SUB, BG, Align::Left) });
                // As home shows a frequency: MHz and three decimals big, the 10 Hz digits small, so the size never
                // changes while typing (a digit lands in the size it will have on the card).
                let t = self.typed();
                let cut = match t.iter().position(|&c| c == b'.') {
                    Some(p) if self.freq_mode() => t.len().min(p + 4),
                    _ => t.len(),
                };
                let mut x = 8 + lcd.text4_rows(8, 96, &t[..cut], &DIG_46, TEXT, BG, 48, 100);
                if cut < t.len() {
                    x += lcd.text4_rows(x, 96, &t[cut..], &DIG_28, TEXT, BG, 48, 100);
                }
                lcd.fill(x + 3, 92, 16, 4, BLUE);
                lcd.fill(0, 112, 240, 1, LINE);
            });
        }
        let Info { rows, note } = self.info(app, cx.store, cx.dev);
        for (i, (label, v, c)) in rows.iter().enumerate() {
            app.kv(lcd, i, 116 + 22 * i as i32, 22, &w::KV_STATUS, label, v.as_bytes(), *c, false);
        }
        if app.dirty(R_NOTE, Mix::new().h(&note).get()) {
            para(lcd, 8, 210, 224, 2, 17, note, &UI_12, AMBER, BG);
        }
        let tip = self.tip();
        if app.dirty(R_HELP, Mix::new().h(&tip.0).get()) {
            para(lcd, 8, 250, 224, 3, 15, crate::text::load(tip).as_bytes(), &UI_12, SUB, BG);
        }
    }

    fn hints(self, _: &App) -> Hints {
        let back = if self.n == 0 { ("BACK", "Back") } else { ("BACK", "Delete") };
        if self.accepts_star() {
            Hints::new(&[("*/OK", "Go"), back]) // VFO-24
        } else if self.wide_next() {
            Hints::new(&[("*", "again: 4-digit MHz"), back]) // VFO-21: "129." on 100-130
        } else if self.freq_mode() && self.wide && self.n == 3 {
            // VFO-21: "129" after \* \*: the fourth MHz digit's range (130 has only 1300 in reach)
            let range = WIDE[(self.three() as usize).saturating_sub(100).min(WIDE.len() - 1)];
            Hints::new(&[(if self.three() == 130 { "0" } else { "0-9" }, range), back])
        } else if self.to == EntryFor::OffsetAmount && !self.accepts_star() {
            Hints::new(&[("^v", "+/-"), ("OK", "Go"), back]) // VFO-26.5
        } else if self.n == 0 && self.to == EntryFor::Setting(FreqSet::Aprs) {
            Hints::new(&[("#", "Region default"), ("OK", "Go"), back]) // APRS-15.1
        } else if self.freq_mode() {
            Hints::new(&[("*", "Point"), ("OK", "Go"), back])
        } else {
            Hints::new(&[("0-9", "Number"), ("OK", "Go"), back])
        }
    }
}

/// OK: go there (the VFO, a channel), or hand the frequency back to the editor that asked.
/// A frequency typed for a slot's VFO (VFO-11; the terminal's and the link's `slot`, TERM-35): the slot goes to
/// its VFO there; a new band brings its mode and step, and the offset's direction kept with its amount (NAV-2).
/// `f` has passed [`refuse`].
pub(crate) fn tune_vfo(app: &mut App, slot: u8, f: Freq10) {
    let region = app.set.region();
    let s = &mut app.slots[slot as usize];
    let (old, new) = (band_near(s.vfo), band_near(f));
    s.mem[old] = s.vfo;
    if old != new {
        s.cfg.mode = crate::slot::band_mode(new, region);
        s.cfg.narrow = s.cfg.mode == 1;
        s.cfg.step = crate::slot::band_step(new, region);
        if s.cfg.offset != 0 && !s.cfg.split {
            s.cfg.offset = s.cfg.offset.signum() * crate::slot::band_shift(new, region);
        }
    }
    let from = s.vfo;
    (s.vfo, s.mem[new], s.vfo_mode) = (f, f, true);
    app.hf_starts_low(slot, from); // TX-36
    app.send_slot(slot);
}

fn commit(e: Entry, app: &mut App, store: &impl ChannelStore) {
    if e.to == EntryFor::Channel {
        let Some(num) = e.number() else { return };
        match store.by_num(num) {
            Some((c, _)) if c.hf_mode_only() => super::hf::recall(app, &c), // CHAN-26
            Some((c, pos)) => {
                let a = app.active as usize;
                let s = &mut app.slots[a];
                (s.zone, s.pos, s.ch, s.vfo_mode) = (c.zone, pos, Some(c), false);
                app.send_slot(a as u8);
                app.go_home();
            }
            None => {
                let mut l = Buf::<32>::new();
                chan_label(num, &mut l);
                app.show_toast(l.as_bytes(), b"is empty", false);
            }
        }
        return;
    }
    let Some(f) = e.freq() else { return };
    if let EntryFor::Setting(fs) = e.to {
        // MENU-31: the setting's own range; a refusal leaves the digits on show to fix
        return match fs.set(app, store, f) {
            Ok(()) => app.back(),
            Err(why) => {
                let (head, body) = why.split_once(": ").unwrap_or((why, ""));
                app.show_toast(head.as_bytes(), body.as_bytes(), false)
            }
        };
    }
    if e.to == EntryFor::OffsetAmount {
        // VFO-26: an amount, not a frequency; a refusal (range, TX-37's relay, a channel's rules) leaves the digits
        if !(OFFSET_MIN..=OFFSET_MAX).contains(&f) {
            return app.show_toast(b"Offset", b"10 kHz to 100 MHz", false);
        }
        if app.set_shift(amount_sign(&e, app) * f as i32) {
            app.back();
        }
        return;
    }
    if let Some(why) = entry_refusal(e.to, app, f) {
        // VFO-21: the entry stays open with its digits, so they can be fixed
        let (head, body) = why.split_once(": ").unwrap_or((why, ""));
        return app.show_toast(head.as_bytes(), body.as_bytes(), false);
    }
    match e.to {
        EntryFor::Vfo => {
            tune_vfo(app, app.active, f);
            app.go_home();
        }
        EntryFor::ChannelFreq => {
            if f < crate::chan::HF_TOP {
                return app.show_toast(b"Below 64 MHz", b"save it in HF mode", false); // CHAN-26
            }
            // The channel's own rules (CHAN-22: a TX apart from RX within 64-1000 MHz)
            let ch = app.slots[app.active as usize].ch;
            if let Some(Err(r)) = ch.map(|c| crate::chfield::check_edit(&crate::chan::Channel { rx: f, ..c })) {
                return app.refusal_toast(r);
            }
            super::chedit::edit(app, |c| c.rx = f);
            app.back();
        }
        EntryFor::Repeater => {
            app.back();
            if let Screen::Save(mut s) = app.screen {
                s.set_freq(f, app.set.region());
                app.screen = Screen::Save(s);
            }
        }
        EntryFor::SplitTx => {
            // TX-37: refused when it needs the other relay state; entry stays open to type another
            if app.set_split(f) {
                app.back();
            }
        }
        EntryFor::ScopeCentre => {
            app.back();
            if let Screen::Scope(s) = app.screen {
                app.radio.centre = f;
                app.screen = super::scope::recentre(app, s, f);
            }
        }
        EntryFor::Channel | EntryFor::Setting(_) | EntryFor::OffsetAmount => {}
    }
}

/// What the entry screen shows under the digits (LINK-19.4: drawn and read back alike).
pub struct Info {
    /// Up to four label, value and colour rows (an empty label: no row).
    pub rows: [(&'static [u8], Buf<40>, u16); 4],
    /// The amber note under them (empty: none).
    pub note: &'static [u8],
}

impl Entry {
    /// The header: what is being typed.
    pub fn what(self) -> &'static [u8] {
match self.to {
            EntryFor::Vfo => b"Frequency, MHz",
            EntryFor::Channel => b"Channel number",
            EntryFor::ChannelFreq => b"Channel frequency, MHz",
            EntryFor::Repeater => b"Repeater output, MHz",
            EntryFor::SplitTx => b"Split: TX frequency, MHz",
            EntryFor::ScopeCentre => b"Scope centre, MHz",
            EntryFor::OffsetAmount => b"Offset, MHz",
            EntryFor::Setting(fs) => fs.label(),
}
    }

    /// The rows and note for what is typed so far.
    pub fn info(self, app: &App, store: &impl ChannelStore, dev: &ferrite_core::msg::Device) -> Info {
            let mut rows: [(&'static [u8], Buf<40>, u16); 4] = [(b"", Buf::new(), TEXT), (b"", Buf::new(), TEXT), (b"", Buf::new(), TEXT), (b"", Buf::new(), TEXT)];
            let mut note: &'static [u8] = b"";
            if self.to == EntryFor::OffsetAmount {
                // VFO-26: the offset now, and where this one would transmit
                rows[0].0 = b"Now";
                let mut b = Buf::<16>::new();
                app.item_text(crate::slot::Item::Offset, true, &mut b);
                rows[0].1.set(b.as_bytes());
                // VFO-26.5: the sign it goes in with (▲ +, ▼ -), and where it would transmit
                let sign = amount_sign(&self, app);
                rows[1].0 = b"Shift";
                rows[1].1.set(if sign < 0 { b"-" } else { b"+" });
                if let Some(f) = self.freq().filter(|f| (OFFSET_MIN..=OFFSET_MAX).contains(f)) {
                    rows[2].0 = b"TX";
                    let rx = app.slot(app.active()).freq() as i64;
                    rows[2].1.set(w::freq3((rx + sign as i64 * f as i64).max(0) as Freq10).as_bytes());
                } else if self.n > 0 {
                    note = b"10 kHz to 100 MHz";
                }
            } else if let EntryFor::Setting(fs) = self.to {
                // MENU-31: what the setting holds now and what it takes; no band, step or transmit rows
                rows[0].0 = b"Now";
                match fs.current(app, store) {
                    Some(f) => rows[0].1.set(w::freq3(f).as_bytes()),
                    None => rows[0].1.set(b"-"),
                }
                rows[1].0 = b"Takes";
                rows[1].1.set(fs.takes().as_bytes());
            } else if self.freq_mode() {
                if let Some(f) = self.freq() {
                    // VFO-12: tuned exactly as typed; the step row says whether it sits on the step's grid.
                    rows[0].0 = b"Band";
                    rows[0].1.set(band_name(f).as_bytes());
                    rows[1].0 = b"Step";
                    let step = step_at(app, f);
                    let name = STEPS.iter().find(|s| s.0 == step).map_or("", |s| s.1);
                    rows[1].1.set(name.as_bytes());
                    rows[1].1.push(if f % step == 0 { b", on it" } else { b", off it" });
                    rows[2].0 = b"Transmit";
                    match entry_refusal(self.to, app, f) {
                        Some(why) => {
                            rows[2].1.set(b"-");
                            (rows[2].2, note) = (AMBER, why.as_bytes());
                        }
                        None if hf_scope_entry(self.to, app) => rows[2].1.set(b"RX only"),
                        None => match tx_text(app, f, dev.tx_built) {
                            Ok(t) => (rows[2].1, rows[2].2) = (Buf::from(t.as_bytes()), GREEN),
                            Err(t) => (rows[2].1, rows[2].2) = (Buf::from(t.as_bytes()), AMBER),
                        },
                    }
                }
            } else if let Some(num) = self.number() {
                rows[0].0 = b"Channel";
                let mut l = Buf::<32>::new();
                chan_label(num, &mut l);
                rows[0].1.set(l.as_bytes());
                rows[1].0 = b"Name";
                match store.get(num) {
                    Some(c) => {
                        rows[1].1.set(if c.name().is_empty() { b"(no name)" } else { c.name() });
                        rows[2].0 = b"Zone";
                        rows[2].1.set(store.zone_label(c.zone, b"None", b"?").as_bytes());
                        rows[3].0 = b"Frequency";
                        rows[3].1.set(w::freq3(c.rx).as_bytes());
                    }
                    None => (rows[1].1, rows[1].2) = (Buf::from(b"empty"), AMBER),
                }
            } else if self.n > 0 {
                note = b"Channels are 1 to 4000";
            }
        Info { rows, note }
    }

    /// The help paragraph at the bottom (in the text pack, UPD-22).
    pub fn tip(self) -> crate::text::Txt {
        match self.to {
            EntryFor::Vfo => txt!("After three digits the point comes by itself; * types it, and * again enters (52*525*)."),
            EntryFor::Channel => txt!("Type the channel number, then OK."),
            EntryFor::OffsetAmount => txt!("The repeater offset in MHz: 7*6 is 7.6. Up makes it +, Down -; OK keeps it."),
            EntryFor::SplitTx => txt!("Transmit here whatever RX is on. OK or * keeps it; BACK keeps the old shift."),
            _ => txt!("* is the point, then * or OK keeps it. BACK deletes."),
        }
    }
}
