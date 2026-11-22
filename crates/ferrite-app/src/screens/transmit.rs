//! Transmitting (manual chapter 15): the checks at PTT (TX-1 to TX-5, TX-20, TX-35), the Transmit screen
//! (TX-12, TX-22, TX-30; Transmit mock-up), the refused screen with its fix (TX-20; TxRefused mock-up) and the
//! emergency-frequency prompt (TX-3).
//!
//! PTT goes home first (KEY-7) and runs the checks the UI can make: the crossband repeater (XB-14), the build,
//! the battery cut-off, the region's bands or Extend TX, busy lockout, the emergency frequencies. The Radio
//! task checks again when it keys (TX-17) and any refusal of its own comes back in `LiveAll::tx`.

use super::activity::{mmss, txt};
use super::nav::Dest;
use super::parts::{self, wrap};
use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::ChannelStore;
use crate::keys::{KeyEvent, Press};
use crate::slot::{band_name, mode_chip, tone_text, Item};
use crate::{App, Screen};
use ferrite_core::freq::Freq10;
use ferrite_core::keys::Key;
use ferrite_core::msg::{LiveAll, RadioCmd, SLOTS};
use ferrite_core::txplan::{self, Refusal};
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;
use ufmt::uwrite;

/// TX-30.2: the timeout bar turns to its warning colour this long before the timeout (the pre-alert is the bar,
/// decision 249); the timeout itself is 0 8 1 (`Settings::tx_timeout_s`).
pub const PREALERT_S: u32 = 10;
/// The emergency prompt waits this long for the second PTT (TX-3).
pub const EMERGENCY_MS: u32 = 5000;
/// "HF": the slot number HF mode transmits as.
pub const HF_SLOT: u8 = 3;

/// The frequency a slot transmits on: the offset applied unless reversed (TX-6 checks this one).
pub fn tx_freq(app: &App, slot: u8) -> Freq10 {
    if slot == HF_SLOT {
        return app.set.hf.freq;
    }
    app.tuned_cfg(slot).tx_of(app.tuned_freq(slot)) // the APRS frequency on the APRS slot (APRS-20)
}

/// Why this slot can't transmit now, by the checks the UI can make. An automatic transmission (a beacon, KISS)
/// is also refused while the crossband repeater runs; a person's PTT isn't ([`ptt_refusal`], XB-14).
pub fn refusal(app: &App, slot: u8) -> Option<Refusal> {
    if app.radio.mode == super::activity::Mode::Xb {
        return Some(Refusal::Crossband);
    }
    ptt_refusal(app, slot)
}

/// [`refusal`] for PTT: the same checks, but a running crossband repeater doesn't refuse it.
pub fn ptt_refusal(app: &App, slot: u8) -> Option<Refusal> {
    if app.radio.keyed.is_some() {
        return Some(Refusal::InProgress);
    }
    if !app.radio.tx_built {
        return Some(Refusal::RxOnlyBuild);
    }
    if app.radio.batt_cut {
        return Some(Refusal::Battery);
    }
    if let Some(r) = tx_block(app, slot) {
        return Some(r);
    }
    // TX-21: busy lockout, on or off as nicFW: refuse while the squelch is open.
    if slot < SLOTS as u8 && app.rx_mask & 1 << slot != 0 && app.tuned_cfg(slot).busy != 0 {
        return Some(Refusal::Busy);
    }
    None
}

/// PTT down for a slot (KEY-7): home first, then refuse, prompt (emergency) or key.
pub fn ptt(app: &mut App, slot: u8) {
    if app.qmenu.is_some() || app.remote_tx_refused() {
        return; // HOME-17 (safety): never while the quick menu is up (its PTTs never get here: the last check)
    }
    if app.radio.mode != super::activity::Mode::Xb {
        app.go_home();
    } // XB-14: while the repeater runs, PTT transmits on its own slot and the repeater carries on after
    if slot < SLOTS as u8 && app.off_refuse(slot) {
        return; // HOME-15.4: an off slot is turned on first (\*)
    }
    if let Some(r) = ptt_refusal(app, slot) {
        return refuse(app, slot, r);
    }
    let emergency = txplan::emergency(tx_freq(app, slot));
    if emergency || app.aprs_voice_gate(slot) {
        // TX-3, APRS-21.3: the second PTT within 5 s transmits.
        app.radio.t0 = app.now.wrapping_add(EMERGENCY_MS);
        app.screen = Screen::Emergency(Emergency { slot, aprs: !emergency });
        return;
    }
    key(app, slot);
}

/// The dialer's Send (DTMF-1): the transmit checks, then the sequence to the Radio task, keyed like PTT; the
/// Radio task waits the first digit delay, sends each digit (DTMF-3) and says so, and the Transmit screen unkeys.
/// Not on an emergency frequency (its second-PTT prompt is for a person talking). True when it keyed.
pub fn dial(app: &mut App, slot: u8, keys: &[u8]) -> bool {
    if app.remote_tx_refused() {
        return false; // LINK-19: the dialer's Send from a remote key
    }
    if let Some(r) = ptt_refusal(app, slot) {
        refuse(app, slot, r);
        return false;
    }
    if txplan::emergency(tx_freq(app, slot)) {
        app.show_toast(b"No DTMF", b"emergency frequency", false);
        return false;
    }
    let n = keys.len().min(16);
    for chunk in 0..n.div_ceil(5) as u8 {
        let mut k = [0u8; 5];
        for (j, b) in k.iter_mut().enumerate() {
            *b = keys.get(chunk as usize * 5 + j).copied().filter(|_| chunk as usize * 5 + j < n).unwrap_or(0);
        }
        app.out.push(RadioCmd::DtmfSeq { chunk, keys: k });
    }
    key(app, slot);
    if app.radio.keyed != Some(slot) {
        return false;
    }
    app.out.push(RadioCmd::DtmfSend { len: n as u8 });
    if let Screen::Transmit(t) = &mut app.screen {
        t.dial = true;
    }
    true
}

/// Key up (after the checks): the Transmit screen over where it was.
pub fn key(app: &mut App, slot: u8) {
    key_as(app, slot, false);
}

/// [`key`], saying whether a person's second PTT on the emergency prompt confirmed it (TX-3): the Radio task
/// refuses an emergency frequency without it (TX-35, TX audit F11).
fn key_as(app: &mut App, slot: u8, confirmed: bool) {
    // HOME-17 (safety): nothing keys while the quick menu is up. Its PTTs never get this far and the side
    // connector's closes it first; this is the last check before the one key-up command.
    if app.qmenu.is_some() {
        return;
    }
    // LINK-19 (safety): nothing a remote key does keys the transmitter; the last check before the key-up too.
    if app.remote_tx_refused() {
        return;
    }
    if slot < SLOTS as u8 && app.off_refuse(slot) {
        return; // HOME-15.4: VOX, the dialer and a remote key-up too
    }
    if let Some(r) = ptt_refusal(app, slot) {
        return refuse(app, slot, r);
    }
    app.radio.keyed = Some(slot);
    app.radio.refusals_at_key = app.radio.refusals_seen; // TX-50: only a refusal after this one ends it
    app.reconcile_mode(); // MODE-1: a mode that doesn't run keyed stops before the key-up (BCR-16, SCOPE-12)
    app.radio.t0 = app.now;
    let region = txplan::Region::ALL.iter().position(|&r| r == app.set.tx_region()).unwrap_or(0) as u8;
    app.out.push(RadioCmd::TxPlan { tx: tx_freq(app, slot), region, extend: app.set.extend_tx, confirmed });
    app.out.push(RadioCmd::Transmit { slot, on: true });
    app.open(Dest::Transmit);
    if let Screen::Transmit(t) = &mut app.screen {
        t.slot = slot;
    }
}

/// The refused screen (TX-20) and the refusal beep (SND-4), over the screen BACK returns to: a PTT's refusal and
/// a remote key-up's alike (LINK-20.3).
pub fn refuse(app: &mut App, slot: u8, why: Refusal) {
    app.beep_refused();
    app.open(Dest::TxRefused);
    app.screen = Screen::TxRefused(TxRefused { why, slot });
}

/// TX-35: an automatic transmission (beacon, KISS frame, VOX, remote KEY) on this slot may go, or is refused
/// with a toast saying why. Only a person pressing PTT twice transmits on an emergency frequency.
pub fn auto_tx(app: &mut App, slot: u8, what: &[u8]) -> bool {
    let f = tx_freq(app, slot);
    let why: &[u8] = if txplan::emergency(f) {
        b"emergency frequency"
    } else if app.set.callsign.as_bytes().is_empty() {
        b"no callsign set"
    } else if app.set.callsign.as_bytes().split(|&c| c == b'-').next().map_or(0, <[u8]>::len) > AX25_CALL && what != b"KISS" {
        // SETUP-13: AX.25 carries 6; KISS frames carry the client's own addresses. An SSID typed into the callsign
        // isn't counted: 0 5 5 1's replaces it (APRS-30).
        b"callsign over 6"
    } else if let Some(r) = refusal(app, slot) {
        short(r).as_bytes()
    } else {
        return true;
    };
    let mut h = Buf::<20>::new();
    h.push(what);
    h.push(b" not sent:");
    app.show_toast(h.as_bytes(), why, false);
    false
}

/// SETUP-13: an AX.25 address holds 6 characters of callsign (the SSID apart).
pub const AX25_CALL: usize = 6;

/// APRS-7, APRS-20: a data transmission on `slot` (the APRS task's frame): an automatic one's checks (TX-35,
/// [`auto_tx`]), then the plan and the data key-up at `power` (0 5 6 3). No screen opens (the card shows it).
/// True when it was asked for; the Radio task may still refuse ([`ferrite_core::msg::TxLive`]), and the caller
/// unkeys with [`data_unkey`] once the frame is out.
pub fn data_key(app: &mut App, slot: u8, power: u8, what: &[u8]) -> bool {
    // APRS-10.1: a data frame keys only while APRS mode, KISS or beacons hold C, so it goes on the APRS frequency,
    // never on C's own channel given back meanwhile (refused quietly: the frame was dropped as it was let go).
    if slot == crate::aprs::APRS_SLOT && !app.aprs_c() {
        return false;
    }
    if app.remote_tx_refused() || !auto_tx(app, slot, what) {
        return false;
    }
    let region = txplan::Region::ALL.iter().position(|&r| r == app.set.tx_region()).unwrap_or(0) as u8;
    app.out.push(RadioCmd::TxPlan { tx: tx_freq(app, slot), region, extend: app.set.extend_tx, confirmed: false });
    app.out.push(RadioCmd::TransmitData { slot, power, on: true });
    true
}

/// An automatic transmission that couldn't be built, in a toast: e.g. a beacon with no position.
pub fn not_sent(app: &mut App, what: &[u8], why: &[u8]) {
    let mut h = Buf::<20>::new();
    h.push(what);
    h.push(b" not sent:");
    app.show_toast(h.as_bytes(), why, false);
}

/// The data transmission's end (after [`data_key`]).
pub fn data_unkey(app: &mut App, slot: u8) {
    app.out.push(RadioCmd::TransmitData { slot, power: 0, on: false });
}

/// A refusal in a few words (toasts).
pub fn short(r: Refusal) -> &'static str {
    match r {
        Refusal::OutOfBand => "outside the band plan",
        Refusal::NotCalibrated => "not calibrated there",
        Refusal::RxOnlyChannel => "receive-only channel",
        Refusal::Busy => "channel busy",
        Refusal::Battery => "battery low",
        Refusal::Locked => "transmit locked",
        Refusal::InProgress => "already transmitting",
        Refusal::NoCallsign => "no callsign set",
        Refusal::RxOnlyBuild => "receive-only build",
        Refusal::ReceiveOnlyHere => "receive only here",
        Refusal::Crossband => "repeater running",
        Refusal::RelayMismatch => "TX needs the HF relay",
        Refusal::AmRxOnly => "AM is receive only",
        Refusal::WfmRxOnly => "WFM is broadcast: RX only",
        Refusal::Emergency => "emergency: PTT twice",
        Refusal::ReleasePtt => "release PTT first",
        Refusal::PttAtBoot => "PTT held at power-on",
        Refusal::Hot => "transmitter cooling",
        Refusal::Broadcast => "broadcast playing",
    }
}

/// The screen and menu path that fix a refusal (TX-20), if one does.
pub(crate) fn fix(app: &App, r: Refusal) -> Option<(&'static str, &'static str, Dest)> {
    match r {
        Refusal::OutOfBand if app.set.region().is_none() => Some(("MENU 0 9 4", "Region", Dest::Group(9))),
        Refusal::OutOfBand => Some(("MENU 0 8 8", "Extend TX", Dest::Group(8))),
        Refusal::NotCalibrated => Some(("MENU 0 9 9 2", "Calibration", Dest::Calibration)),
        Refusal::RxOnlyChannel => Some(("MENU 8", "Channel editor", Dest::ChannelEdit)),
        Refusal::Busy => Some(("MENU 0 8 7", "Busy lockout", Dest::Group(8))),
        Refusal::Battery => Some(("#", "Status page", Dest::Status)),
        Refusal::Locked | Refusal::RxOnlyBuild => Some(("MENU 0 9 8", "About", Dest::About)),
        Refusal::NoCallsign => Some(("MENU 0 8 6 1", "Callsign", Dest::Setup)),
        Refusal::RelayMismatch => Some(("MENU 5", "Offset", Dest::Value(Item::Offset))),
        Refusal::AmRxOnly | Refusal::WfmRxOnly => Some(("MENU 3", "Mode", Dest::Value(Item::Mode))),
        Refusal::InProgress
        | Refusal::ReceiveOnlyHere
        | Refusal::Crossband
        | Refusal::Emergency
        | Refusal::ReleasePtt
        | Refusal::PttAtBoot
        | Refusal::Hot
        | Refusal::Broadcast => None,
    }
}

// ---- Transmit ----

/// TX-48: the Transmit screen's note while the PA drive is held at 0.
pub const DRY_NOTE: &[u8] = b"Dry TX: no RF";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Transmit {
    /// The slot keyed (3 = HF mode).
    pub slot: u8,
    /// A dialled sequence is being sent (DTMF-1): the transmission ends by itself when the Radio task has sent it.
    pub dial: bool,
}

/// HOME-4.2: why PTT can never transmit on this slot where it is now, whatever happens meanwhile: a receive-only
/// build or channel, AM, outside the band plan or the calibration, the relay. The checks in
/// [`ptt_refusal`] that pass with time (busy, battery, cooling, keyed elsewhere) aren't these.
pub fn tx_block(app: &App, slot: u8) -> Option<Refusal> {
    if !app.radio.tx_built {
        return Some(Refusal::RxOnlyBuild);
    }
    // CHAN-4 (safety): a receive-only channel never transmits.
    if app.rx_only(slot) {
        return Some(Refusal::RxOnlyChannel);
    }
    // TX-20: the BK4829 has no AM modulator; BCR-33: WFM is broadcast listening, receive only.
    if slot < SLOTS as u8 && app.tuned_cfg(slot).mode == 2 {
        return Some(Refusal::AmRxOnly);
    }
    if slot < SLOTS as u8 && app.tuned_cfg(slot).mode == crate::slot::WFM {
        return Some(Refusal::WfmRxOnly); // TX-20.6: says it's broadcast FM, not HF mode's "receive only here"
    }
    if let Err(r) = txplan::check(tx_freq(app, slot), app.set.tx_region(), app.set.extend_tx) {
        return Some(r);
    }
    // TX-37: the band relay never switches at key-up. A split (or a VFO retuned under one) whose TX needs the
    // other relay state from RX is refused here, as a backstop to the check when it is set.
    if slot < SLOTS as u8 && !txplan::relay_ok(app.tuned_cfg(slot).rx_of(app.tuned_freq(slot)), tx_freq(app, slot)) {
        return Some(Refusal::RelayMismatch);
    }
    None
}

/// The level name (TX-12): LOW, MID, HIGH; "No TX" where PTT can't transmit (HOME-4.2).
pub fn power_name(app: &App, slot: u8) -> &'static str {
    if tx_block(app, slot).is_some() {
        return "No TX";
    }
    let p = if slot < SLOTS as u8 { app.tuned_cfg(slot).power } else { 0 }; // the APRS power on the APRS slot (APRS-19.1)
    // TX-9: 1.25 m keys Low whatever is chosen, and says so.
    ["LOW", "MID", "HIGH"][ferrite_core::txcal::band_power(tx_freq(app, slot), p) as usize % 3]
}

impl View for Transmit {
    fn name(self) -> &'static str {
        "Transmit"
    }

    fn mockup(self) -> Option<&'static str> {
        Some("Transmit.png")
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        b.set(b"TX");
    }

    /// TX-22: while keyed, no key does anything (hold BACK still goes home, and PTT up unkeys).
    fn key(self, _: &mut App, _: KeyEvent, _: &impl ChannelStore) {}

    /// While keyed: SIDE2 sends the tone burst for as long as it is held (TONE-13), and the digits, \* and #
    /// send DTMF live for as long as each is held (DTMF-7), with A-D on MENU, ▲, ▼ and BACK (DTMF-11, as nicFW
    /// puts them on its four non-digit keys). The tones themselves are the Radio task's (milestone 4).
    fn raw(self, app: &mut App, key: Option<Key>, _: Option<u8>, down: bool) {
        if app.radio.keyed.is_none() {
            return;
        }
        match key {
            Some(Key::Side(2)) => {
                app.keys.used(Key::Side(2)); // a tone burst, not the counter (KEY-22)
                app.out.push(RadioCmd::ToneBurst { on: down })
            }
            Some(Key::Digit(d)) => app.out.push(RadioCmd::Dtmf { key: b'0' + d, on: down }),
            Some(Key::Star) => app.out.push(RadioCmd::Dtmf { key: b'*', on: down }),
            Some(Key::Hash) => app.out.push(RadioCmd::Dtmf { key: b'#', on: down }),
            Some(Key::Menu) => app.out.push(RadioCmd::Dtmf { key: b'A', on: down }),
            Some(Key::Up) => app.out.push(RadioCmd::Dtmf { key: b'B', on: down }),
            Some(Key::Down) => app.out.push(RadioCmd::Dtmf { key: b'C', on: down }),
            Some(Key::Exit) => app.out.push(RadioCmd::Dtmf { key: b'D', on: down }),
            _ => {}
        }
    }

    fn ptt(self, app: &mut App, down: bool) {
        if !down {
            app.back(); // home, or HF mode it was keyed from
        }
    }

    fn tick(self, app: &mut App, live: &LiveAll) {
        // DTMF-1: the dialled digits are out; unkey, back to the dialer.
        if self.dial && app.radio.keyed.is_some() && live.tx.dtmf_sent != app.radio.dtmf_seen {
            app.ptt_released();
            app.back();
            return;
        }
        // The Radio task refused (its own checks: calibration, read-back, battery under load), after this key-up
        // (TX-50: an earlier refusal still in its status isn't this one's); unkeyed all the same, in case it keyed.
        if let Some(r) = live.tx.refused.filter(|_| live.tx.slot == self.slot && app.radio.keyed.is_some() && live.tx.refusals != app.radio.refusals_at_key) {
            app.out.push(RadioCmd::Transmit { slot: self.slot, on: false });
            app.radio.keyed = None;
            app.beep_refused(); // SND-4: the Radio task's refusals beep as the UI's do
            app.screen = Screen::TxRefused(TxRefused { why: r, slot: self.slot });
            return;
        }
        // TX-30: unkey at the timeout; PTT must come up before the next transmission.
        if app.radio.keyed.is_some() && app.now.wrapping_sub(app.radio.t0) >= app.set.tx_timeout_s() * 1000 {
            app.ptt_released();
            app.back();
            app.show_toast(b"TX timeout", b"release PTT", false);
            app.timeout_beep = app.now.wrapping_add(400).max(1); // TX-30.2: a beep once it is off
        }
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        let live = cx.live;
        let s = self.slot;
        let hf = s == super::transmit::HF_SLOT;
        let secs = if app.radio.keyed.is_some() { app.now.wrapping_sub(app.radio.t0) / 1000 } else { 0 };
        let (y, h) = place(s, s);
        let remote = app.remote_keyed();
        if app.dirty(1, 1 + remote as u32) {
            parts::card(lcd, 6, y, 228, h, RED);
            lcd.fill_r(7, y + 1, 226, 20, 2, RED);
            lcd.fill(7, y + 12, 226, 9, RED);
            lcd.text4_rows(15, y + 15, b"TRANSMIT", &UI_12B, WHITE, RED, y + 2, y + 20);
            // LINK-20: keyed over the link, small beside the title.
            if remote {
                let x = 15 + text4_width(b"TRANSMIT", &UI_12B) + 8;
                lcd.text4_rows(x, y + 15, b"Remote", &UI_12, WHITE, RED, y + 2, y + 20);
            }
        }
        let mut t = Buf::<16>::new();
        mmss(secs, &mut t);
        if app.dirty(2, Mix::new().h(&t).get()) {
            w::field(lcd, 160, 66, y + 2, y + 20, y + 15, t.as_bytes(), const { &w::st(&UI_12B, WHITE, RED, Align::Right) });
        }
        // Slot, VFO/channel and band; what it receives on.
        let f = tx_freq(app, s);
        let rx = if hf { f } else { app.slot(s).cfg().rx_of(app.slot(s).freq()) }; // reverse listens on the TX side
        let mut head = Buf::<32>::new();
        if hf {
            head.set(b"HF mode");
        } else {
            app.head_text(s, live, cx.store, &mut head);
            // On a channel: its name in place of the band, "012 Club repeater" (HOME-4's short number).
            let name = app.chan_name(s, live);
            if !name.is_empty() {
                let sl = app.slot(s);
                head.clear();
                crate::slot::chan_num(sl.ch.map_or(0, |c| c.num), &mut head);
                head.push(b" ");
                head.push(name.as_bytes());
            }
        }
        let mut r = Buf::<16>::new();
        r.push(b"RX ");
        r.push(w::freq3(rx).as_bytes());
        if app.dirty(3, Mix::new().h(&head).h(&r).get()) {
            if hf {
                lcd.fill(15, y + 26, 14, 14, SURFACE);
            } else {
                w::badge(lcd, 15, y + 26, s, true, SURFACE);
            }
            w::field(lcd, 35, 110, y + 24, y + 42, y + 38, w::fit(head.as_bytes(), &UI_12, 108), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
            w::field(lcd, 145, 80, y + 24, y + 42, y + 38, r.as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Right) });
        }
        if app.dirty(4, Mix::new().h(&f).get()) {
            w::freq(lcd, 15, 226, y + 77, f, &DIG_46, &DIG_28, TEXT, SUB, SURFACE);
        }
        // Mode, the tone sent (red), the power level name (TX-12).
        let (mut m, mut tone) = (Buf::<16>::new(), Buf::<16>::new());
        if hf {
            m.set(super::hf::mode_name(app.set.hf.mode).as_bytes());
        } else {
            let c = app.slot(s).cfg();
            mode_chip(c, &mut m);
            tone_text(c.sent_tone(), &mut tone);
        }
        let pw = power_name(app, s);
        if app.dirty(5, Mix::new().h(&m).h(&tone).h(&pw).get()) {
            // Fixed slots (HOME-4): the tone being sent is the red chip; power is pinned right, wide enough for
            // "No TX".
            let cy = y + 89;
            w::chip_slot(lcd, w::SLOT_MODE, cy, m.as_bytes(), TEXT, LINE, SURFACE);
            w::chip_slot(lcd, w::SLOT_TONE, cy, tone.as_bytes(), RED, RED, SURFACE);
            lcd.fill(w::SLOT_TONE.0 + w::SLOT_TONE.1, cy, 136 - w::SLOT_TONE.0 - w::SLOT_TONE.1, 17, SURFACE);
            lcd.fill(136, cy, 90, 17, SURFACE);
            let pwd = text4_width(pw.as_bytes(), &UI_12) + 10;
            w::chip_slot(lcd, (226 - pwd, pwd), cy, pw.as_bytes(), if pw.len() > 4 { AMBER } else { TEXT }, LINE, SURFACE);
        }
        // TX-12.3: the upper bar tracks PA0. The lower row and label show the selected BK4829 mic gain.
        let mic = if live.tx.keyed && live.tx.slot == s { live.tx.mic } else { 0 };
        if app.dirty(6, u32::from(mic) + 1) {
            w::field(lcd, 15, 40, y + 110, y + 127, y + 123, b"Mic", const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
            let lit = ((mic as u32 * 13 + 50) / 100) as u8;
            parts::segs(lcd, 56, y + 112, 13, lit, 7, 9, 7, if mic >= 90 { AMBER } else { GREEN }, SURFACE);
        }
        let gain = app.set.mic_gain.clamp(1, ferrite_core::vox::MIC_GAIN_MAX);
        if app.dirty(10, u32::from(gain)) {
            let mut label = Buf::<8>::new();
            let _ = uwrite!(label, "Gain {}", gain);
            parts::segs(lcd, 56, y + 122, ferrite_core::vox::MIC_GAIN_MAX, gain, 7, 9, 3, VIOLET, SURFACE);
            w::field(lcd, 176, 50, y + 110, y + 127, y + 123, label.as_bytes(), const { &w::st(&UI_12, VIOLET, SURFACE, Align::Right) });
        }
        // Timeout: the bar fills to the limit; amber from the pre-alert (TX-30).
        if app.dirty(7, Mix::new().h(&secs).get()) {
            w::field(lcd, 15, 40, y + 127, y + 144, y + 140, b"TOT", const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
            let limit = app.set.tx_timeout_s();
            let c = if secs + PREALERT_S >= limit { AMBER } else { SUB };
            parts::bar(lcd, 56, y + 132, 110, 6, secs, limit, c, SURFACE);
            let mut b = Buf::<16>::new();
            b.push(b"of ");
            mmss(limit, &mut b);
            w::field(lcd, 168, 58, y + 127, y + 144, y + 140, b.as_bytes(), const { &w::st(&UI_12, TEXT, SURFACE, Align::Right) });
        }
        let mv = cx.dev.battery_mv as u32;
        // TX-48: with the PA drive held at 0 (Dry TX) the row says so first; the battery
        // isn't under a real load then, so it keeps only its volts.
        let dry = app.dry_tx();
        if app.dirty(8, Mix::new().h(&mv).h(&dry).get()) {
            let mut b = Buf::<32>::new();
            let c = if cx.dev.battery_low { AMBER } else { SUB };
            if dry {
                let _ = uwrite!(b, "{}.{} V", mv / 1000, mv % 1000 / 100);
                w::field(lcd, 15, 130, y + 144, y + 161, y + 157, DRY_NOTE, const { &w::st(&UI_12B, AMBER, SURFACE, Align::Left) });
                w::field(lcd, 145, 81, y + 144, y + 161, y + 157, b.as_bytes(), &w::st(&UI_12, c, SURFACE, Align::Right));
            } else {
                let _ = uwrite!(b, "Battery under load {}.{} V", mv / 1000, mv % 1000 / 100);
                w::field(lcd, 15, 211, y + 144, y + 161, y + 157, b.as_bytes(), &w::st(&UI_12, c, SURFACE, Align::Left));
            }
        }
        // The other slots, one row each in their fixed places: watched ones pause while transmitting (TX-34).
        if app.dirty(9, Mix::new().h(&s).h(&app.watch).get()) {
            for o in (0..SLOTS as u8).filter(|&o| o != s) {
                let (by, bh) = place(s, o);
                ferrite_ui::canvas::banded(lcd, 6, by, 228, bh, BG, |lcd| {
                    lcd.frame_r(6, by, 228, bh, 3, LINE);
                    let ry = by + 3;
                    w::badge(lcd, 12, ry + 2, o, false, BG);
                    let sl = app.slot(o);
                    let mut t = Buf::<40>::new();
                    t.push(w::freq3(sl.freq()).as_bytes());
                    if let Some(c) = sl.ch.filter(|_| sl.on_channel()) {
                        t.push(" \u{b7} ".as_bytes());
                        t.push(c.name());
                    } else {
                        t.push(" \u{b7} ".as_bytes());
                        t.push(band_name(sl.freq()).as_bytes());
                    }
                    // a long channel name cut on purpose, to the room the state leaves
                    w::field(lcd, 32, 152, ry, ry + 17, ry + 13, w::fit(t.as_bytes(), &UI_12, 152), const { &w::st(&UI_12, DIM, BG, Align::Left) });
                    let watched = app.watch_mask() & 1 << o != 0;
                    let st: &[u8] = if watched { b"paused" } else { b"off" };
                    w::field(lcd, 184, 44, ry, ry + 17, ry + 13, st, const { &w::st(&UI_12, DIM, BG, Align::Right) });
                });
            }
        }
    }

    fn hints(self, app: &App) -> Hints {
        if app.remote_keyed() {
            return Hints::new(&[("PTT", "Stop remote")]); // LINK-20: a press on the radio ends it
        }
        Hints::new(&[("PTT", "Release to stop")])
    }
}

const TOP: i32 = 25;
const CARD_H: i32 = 164;
const ROW_H: i32 = 23;
const GAP: i32 = 6;

/// Where slot `o` sits while slot `s` transmits: A, B, C keep their order (HOME-11) and the keyed slot's card
/// grows in its own place. HF mode's card is not a slot, so it goes on top with A, B, C below.
pub(crate) fn place(s: u8, o: u8) -> (i32, i32) {
    let h = |i: u8| if i == s { CARD_H } else { ROW_H };
    if o == HF_SLOT {
        return (TOP, CARD_H);
    }
    let top = if s == HF_SLOT { TOP + CARD_H + GAP } else { TOP };
    (top + (0..o).map(|i| h(i) + GAP).sum::<i32>(), h(o))
}

// ---- Refused ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TxRefused {
    pub why: Refusal,
    pub slot: u8,
}

impl View for TxRefused {
    fn name(self) -> &'static str {
        "Transmit refused"
    }

    fn mockup(self) -> Option<&'static str> {
        Some("TxRefused.png")
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        b.set(b"Refused");
    }

    fn key(self, app: &mut App, e: KeyEvent, _: &impl ChannelStore) {
        match (e.key, e.press) {
            (Key::Menu, Press::Short) => {
                // The fix replaces this screen: its BACK returns to where the refusal came over.
                match fix(app, self.why) {
                    Some(_) if self.why == Refusal::NoCallsign => {
                        app.back();
                        super::setup::callsign(app); // just the callsign page
                    }
                    Some((_, _, d)) => {
                        app.back();
                        app.open(d);
                    }
                    None => {}
                }
            }
            (Key::Exit, Press::Short) => app.back(),
            _ => {}
        }
    }

    /// PTT again tries again (home first).
    fn ptt(self, app: &mut App, down: bool) {
        if down {
            if self.slot == HF_SLOT {
                app.back();
                return super::hf::transmit(app);
            }
            ptt(app, self.slot);
        }
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        if !app.dirty(1, Mix::new().any(&self).get()) {
            return;
        }
        lcd.fill(0, w::SBAR_H, 240, w::HINT_Y - w::SBAR_H, BG);
        let f = tx_freq(app, self.slot);
        let mut a = Buf::<96>::new();
        let mut more: &[u8] = b"";
        let fq = w::freq3(f);
        match self.why {
            Refusal::OutOfBand => {
                let b = txplan::nearest(f, app.set.tx_region());
                let _ = uwrite!(a, "{} MHz is outside the {} band in {} (", txt(fq.as_bytes()), b.name, app.set.tx_region().name());
                mhz_text(b.lo, &mut a);
                a.push(b"-");
                mhz_text(b.hi, &mut a);
                a.push(b").");
                more = if app.set.region().is_none() {
                    b"Region isn't set. To set it:"
                } else {
                    b"To allow any band the radio is calibrated for:"
                };
            }
            Refusal::NotCalibrated => {
                let _ = uwrite!(a, "{} MHz is outside every band the radio is calibrated for.", txt(fq.as_bytes()));
                more = b"The calibration, read only:";
            }
            Refusal::RxOnlyChannel => a.set(b"This channel is receive only."),
            Refusal::Busy => a.set(b"Busy lockout: the channel is in use."),
            Refusal::Battery => {
                a.set(b"The battery is below the transmit cut-off.");
                more = b"Charge it or fit a fresh one. Its level:";
            }
            // CAL-20 (issue #40): which check, so the owner isn't told "Ready" on About and "locked" here
            Refusal::Locked => {
                use ferrite_core::msg::CalState;
                let t = &cx.dev.selftest;
                (a, more) = match (t.mcu_ok, t.cal) {
                    (false, _) => (Buf::from(b"Transmit is locked: the MCU isn't the one this radio has.".as_slice()), b"The check:".as_slice()),
                    (_, CalState::Blank) => (Buf::from(b"Transmit is locked: the calibration is blank.".as_slice()), b"The check:".as_slice()),
                    // CAL-6.1: the sector stands; this band's curves failed a check
                    (_, CalState::Ok) if super::groups::band_locked(t).is_some() => {
                        let mut w = Buf::<40>::new();
                        super::groups::cal_fault_text(t.cal_fault, t.cal_at, &mut w);
                        let mut x = Buf::<96>::new();
                        let _ = uwrite!(x, "Transmit is locked on this band: its calibration failed a check ({}).", super::lists::Txt(w.as_bytes()));
                        (x, b"The other bands transmit. The check:".as_slice())
                    }
                    (_, CalState::Invalid) => {
                        // CAL-20.4: which check, and where
                        let mut w = Buf::<40>::new();
                        super::groups::cal_fault_text(t.cal_fault, t.cal_at, &mut w);
                        let mut x = Buf::<96>::new();
                        let _ = uwrite!(x, "Transmit is locked: a calibration value failed its check ({}).", super::lists::Txt(w.as_bytes()));
                        (x, b"The check:".as_slice())
                    }
                    (_, CalState::CrcMismatch) if t.cal_fault == ferrite_core::msg::CalFault::Unchecked => (
                        Buf::from(b"Transmit is locked: the calibration couldn't be checked (the store isn't ready).".as_slice()),
                        b"Turn the radio off and on. The check:".as_slice(),
                    ),
                    (_, CalState::CrcMismatch) => (
                        Buf::from(b"Transmit is locked: the calibration changed since Ferrite first read it.".as_slice()),
                        b"OK on About uses it as it is, if it checks out:".as_slice(),
                    ),
                    _ => (Buf::from(b"Transmit is locked: a calibration or hardware check failed.".as_slice()), b"Which check failed:".as_slice()),
                };
            }
            Refusal::InProgress => a.set(b"Another transmission is in progress."),
            Refusal::NoCallsign => {
                a.set(b"Automatic transmit needs a callsign.");
                more = b"OK sets it, on its own page:";
            }
            Refusal::RxOnlyBuild => {
                a.set(b"This build of Ferrite has no transmit code.");
                more = b"Flash a transmit build. The version:";
            }
            Refusal::ReceiveOnlyHere => {
                a.set(b"Receive only here: below 18 MHz, USB, LSB and CW are on the SI4732.");
            }
            Refusal::Crossband => a.set(b"The crossband repeater is running. Any key on its screen stops it."),
            Refusal::RelayMismatch => {
                a.set(b"Split TX needs the HF relay: not allowed with this RX frequency.");
                more = b"Keep RX and TX on the same side of 64 MHz.";
            }
            Refusal::AmRxOnly => a.set(b"AM is receive only: the radio has no AM modulator."),
            Refusal::WfmRxOnly => a.set(b"WFM is broadcast FM, receive only. For 25 kHz two-way FM pick FM, for 12.5 kHz NFM."),
            Refusal::Emergency => a.set(b"An emergency frequency: only a person pressing PTT twice transmits here."),
            Refusal::ReleasePtt => a.set(b"The timeout ended the last transmission. Release PTT, then press it again."),
            Refusal::Broadcast => a.set(b"The broadcast receiver was still playing at key-up. Release PTT, then press it again. (TX-24)"),
            Refusal::PttAtBoot => a.set(b"PTT was held at power-on. Release every PTT, then press it again. (SAFE-14)"),
            Refusal::Hot => {
                a.set(b"The transmitter is cooling after a long time keyed.");
                more = b"It transmits again in a few minutes. (TX-31)";
            }
        }
        // The dialog sized to its text: title, the reason, what fixes it, the path.
        let n = parts::lines(a.as_bytes(), &UI_12, 204, 4);
        let m = parts::lines(more, &UI_12, 204, 2);
        let fixes = fix(app, self.why).is_some();
        let h = 38 + 17 * n + if m > 0 { 15 * m + 4 } else { 0 } + if fixes { 26 } else { 0 };
        let y = 150 - h / 2;
        parts::dialog(lcd, y, h, b"Can't transmit", RED);
        wrap(lcd, 19, 204, y + 45, 17, n, a.as_bytes(), &UI_12, TEXT, SURFACE);
        let mut ty = y + 45 + 17 * n + 4;
        if m > 0 {
            wrap(lcd, 19, 204, ty, 15, m, more, &UI_12, SUB, SURFACE);
            ty += 15 * m - 8;
        }
        if let Some((path, name, _)) = fix(app, self.why) {
            let wd = parts::path_chip(lcd, 19, ty, path.as_bytes(), SURFACE);
            lcd.text4_rows(19 + wd + 6, ty + 13, name.as_bytes(), &UI_12, TEXT, SURFACE, ty, ty + 18);
        }
    }

    fn hints(self, app: &App) -> Hints {
        if fix(app, self.why).is_some() {
            Hints::new(&[("OK", "Go there"), ("BACK", "Close")])
        } else {
            Hints::new(&[("BACK", "Close")])
        }
    }
}

/// "430" or "24.89": a band edge in MHz, as short as it goes.
fn mhz_text(f: Freq10, b: &mut Buf<96>) {
    let (m, k) = (f / 100_000, f % 100_000 / 1000);
    let _ = uwrite!(b, "{}", m);
    if k != 0 {
        let _ = uwrite!(b, ".{}{}", k / 10, k % 10);
    }
}

// ---- Emergency prompt ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Emergency {
    pub slot: u8,
    /// APRS-21.3: the APRS frequency's prompt (data only), not an emergency frequency's.
    pub aprs: bool,
}

impl View for Emergency {
    fn name(self) -> &'static str {
        if self.aprs { "APRS frequency" } else { "Emergency frequency" }
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        b.set(if self.aprs { b"APRS" as &[u8] } else { b"Distress" }); // 8 or fewer (SBAR-5)
    }

    fn key(self, app: &mut App, e: KeyEvent, _: &impl ChannelStore) {
        if (e.key, e.press) == (Key::Exit, Press::Short) {
            app.back();
        }
    }

    /// TX-3: the second PTT within 5 s transmits; the first one's release does nothing. VOX and remote PTT never
    /// confirm it (TX-35, LINK-20): neither is a person pressing PTT.
    fn ptt(self, app: &mut App, down: bool) {
        if down && !app.vox_on && !app.virtual_ptt {
            app.screen = Screen::Home;
            key_as(app, self.slot, true);
        }
    }

    fn tick(self, app: &mut App, _: &LiveAll) {
        if crate::after(app.now, app.radio.t0) {
            app.go_home();
        }
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, _: &Cx<impl ChannelStore>) {
        let (y, h) = (80, 120);
        let f = tx_freq(app, self.slot);
        if app.dirty(1, Mix::new().h(&f).get()) {
            let mut a = Buf::<64>::new();
            if self.aprs {
                // APRS-21.3: data only while C is the APRS slot.
                parts::dialog(lcd, y, h, b"APRS frequency: data only", AMBER);
                let _ = uwrite!(a, "{} MHz carries APRS packets; voice blocks them.", txt(w::freq3(f).as_bytes()));
            } else {
                parts::dialog(lcd, y, h, b"Emergency frequency", AMBER);
                let _ = uwrite!(a, "{} MHz is kept for distress and rescue.", txt(w::freq3(f).as_bytes()));
            }
            wrap(lcd, 19, 204, y + 45, 17, 2, a.as_bytes(), &UI_12, TEXT, SURFACE);
            w::field(lcd, 19, 204, y + 70, y + 92, y + 86, b"PTT again to transmit", const { &w::st(&UI_12B, AMBER, SURFACE, Align::Left) });
        }
        let left = app.radio.t0.wrapping_sub(app.now).min(EMERGENCY_MS);
        let s = left.div_ceil(1000);
        if app.dirty(2, Mix::new().h(&(left / 100)).get()) {
            parts::bar(lcd, 19, y + 100, 170, 6, left, EMERGENCY_MS, AMBER, SURFACE);
            let mut b = Buf::<8>::new();
            let _ = uwrite!(b, "{} s", s);
            w::field(lcd, 192, 30, y + 95, y + 111, y + 107, b.as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Right) });
        }
    }

    fn hints(self, _: &App) -> Hints {
        Hints::new(&[("PTT", "Transmit"), ("BACK", "Cancel")])
    }
}
