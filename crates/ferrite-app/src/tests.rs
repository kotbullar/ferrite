//! The app's rules as host tests: keys in, screens, commands and pixels out.

use super::*;
use crate::chan::{channel, Table};
use crate::screens::{advanced::Advanced, edit::Edit, quick::Quick, status::Status};
use crate::slot::{DEFAULT_CFG, Item};
use ferrite_core::msg::{Device, GpsState, SlotLive};
use ferrite_ui::fb::FrameBuffer;
use ferrite_ui::gfx::{BG, BLUE, GREEN};
use std::vec::Vec;

static ZONES: [&[u8]; 3] = [b"Local", b"Empty", b"Repeaters"];
static CHANNELS: [Channel; 4] = [
    channel(0, 0, 14_652_000, "Calling", DEFAULT_CFG),
    channel(1, 0, 44_600_000, "UHF call", DEFAULT_CFG),
    channel(10, 2, 14_694_000, "North hill", DEFAULT_CFG),
    channel(11, 2, 44_347_500, "Club repeater", DEFAULT_CFG),
];

fn store() -> Table<'static> {
    Table { zones: &ZONES, channels: &CHANNELS }
}

/// Fresh settings for one test's App (the firmware keeps them in a static; a test leaks a few hundred bytes).
pub(crate) fn set() -> &'static mut crate::settings::Settings {
    std::boxed::Box::leak(std::boxed::Box::new(crate::settings::Settings::DEFAULT))
}

/// A fresh last-alert record for one test's App.
pub(crate) fn alert() -> &'static mut crate::screens::alert::AlertInfo {
    std::boxed::Box::leak(std::boxed::Box::new(crate::screens::alert::AlertInfo::NONE))
}

/// An app, a clock and a quiet radio.
struct T {
    app: App,
    t: u32,
    live: LiveAll,
}

impl T {
    fn new() -> Self {
        let mut app = App::new(crate::tests::set(), crate::tests::alert());
        while app.pop_cmd().is_some() {}
        T { app, t: 1000, live: LiveAll::default() }
    }

    fn press(&mut self, k: Key, hold: u32) {
        let s = store();
        self.app.key_down(k, self.t, &s);
        let end = self.t + hold;
        while self.t < end {
            self.t += 10;
            self.app.tick(self.t, &self.live, &s);
        }
        self.app.key_up(k, self.t, &s);
        self.t += 50;
        self.app.tick(self.t, &self.live, &s);
    }

    fn tap(&mut self, k: Key) {
        self.press(k, 100);
    }

    fn hold(&mut self, k: Key) {
        self.press(k, 800);
    }

    /// Open a screen from home, as its menu path or a shortcut would.
    fn open(&mut self, d: crate::screens::nav::Dest) {
        self.app.open(d);
        self.t += 50;
        self.app.tick(self.t, &self.live, &store());
    }

    fn knob(&mut self, d: i8) {
        self.app.knob(d, self.t, &store());
    }

    fn wait(&mut self, ms: u32) {
        let end = self.t + ms;
        while self.t < end {
            self.t += 100;
            self.app.tick(self.t, &self.live, &store());
        }
    }

    /// A key press that only lights a dark screen (decision 63: the waking press does nothing else).
    fn wake(&mut self) {
        if self.app.dark() {
            self.tap(Key::Exit);
            assert!(!self.app.dark());
        }
    }

    fn cmds(&mut self) -> Vec<RadioCmd> {
        let mut v = Vec::new();
        while let Some(c) = self.app.pop_cmd() {
            v.push(c);
        }
        v
    }

    fn freq(&self) -> Freq10 {
        self.app.slot(self.app.active()).freq()
    }

    fn render(&mut self, fb: &mut FrameBuffer) {
        let dev = Device { clock_min: Some(14 * 60 + 32), gps: GpsState::Fix, battery_pct: 80, ..Default::default() };
        self.app.render(fb, &self.live, &dev, &store());
    }
}

#[test]
fn ram_is_small() {
    let n = core::mem::size_of::<App>();
    std::println!("App: {n} bytes");
    // 1024 until Oct 7, 2026: the scan hit list (SCAN-23: 16 hits of 8 bytes, 13 more than the probes' 3, +112)
    // and the slots' clarifier (VFO-22, +8 with padding) took it to 1136; channel scans in the Radio task
    // (the set being sent, 4 bytes, and the last ask answered, 2: +8 with padding) to 1144; a channel's end tone
    // and tail (decision 86: one byte, but Channel goes from 40 to 44 bytes with its alignment, and the App holds a
    // Channel in each slot and in the queued write, plus the byte in undo) to 1176; the APRS settings' path,
    // digipeater and beacon after voice (APRS-26, APRS-27: 3 bytes in the App's AprsCfg, 4 to 7, +8 with padding)
    // to 1184; the scroll cue's redraw region (DISP-24: one more 4-byte hash, +8 with padding) to 1192; the custom
    // APRS path (APRS-26: 16 bytes of text in the App's AprsCfg) and a clock set by hand waiting for its owner
    // (GPS-23: an Option<u32>, 8) to 1216; a settings-list change kept for "* Undo" (MENU-22: path, direction
    // and the shown value's hash, 16 with its Option), VOX holding the key and the alert tone's second (VOX-13,
    // SND-6: 3 bytes, 8 with padding) to 1240; each home card's geometry hash, so a border colour change redraws
    // only the border (DISP-25: 3 x 4 bytes, +8 with the packing) to 1248; the scroll cue as drawn, so a row
    // redrawn beside it draws its part too instead of blanking it (DISP-25: 4 x 2 bytes) to 1256; the modes by
    // state (MODE-1 to MODE-4: the repeater started, the active frequency and when it last moved, the lease's
    // renewal and the Radio task's drop count) and the knob's last step (HOME-13.1), 32 with padding, to 1288; the
    // broadcast slot, the slots in the band, the station sent and a seek landing (BCR-22, BCR-25), 8 to 1296;
    // quick mute (HOME-15, a byte), auto off's own activity time and the last key for the keypad light (DISP-15.6,
    // DISP-22: 2 x 4), 16 with padding, to 1312; the quick menu (HOME-17: its last input and the rows as drawn,
    // 12 with its Option), the PTTs it holds dead, the theme before night red and its repaint flag, 24 with
    // padding, to 1336; remote PTT (LINK-20.4: the transmission, its keepalive and why it ended, the local PTTs held
    // and cut), 16 with padding, to 1352; a remote key-up's release as a cut (LINK-20.4, TX-47.3: a byte), 8 with
    // padding, to 1360; remote keys (LINK-19: the keys the link holds, its long press and when the last went
    // down, 16 with its Option) and the readback's redraw count and time (8), to 1384; each group's and
    // third-level list's last item (MENU-24: 38 nibbles, 19 bytes), 24 with padding, to 1408 (who holds the remote
    // keys, LINK-28's byte, fits in that padding); the zones page's Nearby answer (CHAN-29: 12 bytes) and a queued
    // channel write that carries a position (CHAN-2, `ch N pos=`: 4 bytes more), 24 with padding, to 1432; the
    // refusal beeps sent (SND-4, the readback's `beeps=`: a byte) and a hash of the callsign CW ID has (VOX-12: 4,
    // sent again when the setting's differs), 8 with padding, to 1440; the status bar as drawn holds the callsign
    // shown in the clock's place without a time (SBAR-2: 6 characters and a length), 16 with padding, to 1456;
    // Dry TX (TX-48: the switch, its bar mark, and 0 8 8's list's remembered row) 8 with padding, to 1464; each
    // kept hit's stop start for its length (SCAN-23.1: 2 bytes, 4 with padding, x16), to 1528; the contact whose name
    // is being typed (APRS-37: a callsign, 10 bytes), 16 with padding, to 1544; Low Power Mode (PWR-24: its flags, the
    // battery % Auto looks at, the side keys' hold time, 0 7 7's new list's remembered row and the bar's mark), 8
    // with padding, to 1552; its home's last reception (PWR-24.8: slot, opened, closed, peak: 11 bytes), 16 with
    // padding, to 1568; each slot's scan's last hit for Return to (SCAN-6: 3 x 4 bytes), 16 with padding, to 1584;
    // the beacon comment and ambiguity (APRS-50, APRS-51: 41 bytes in the App's AprsCfg), 40 with padding, to 1624;
    // the slots turned off (HOME-15.1: a byte), 8 with padding, to 1632; the Local zone (CHAN-34: 48 channel
    // numbers, their count and state, the range, where and for which channels it was found), 112, to 1744; the last link transfer
    // frame's time for the LED's flash (DISP-30: 4 bytes), 8 with padding, to 1752; the channel version a running
    // channel scan's list was sent for and the slots whose list goes again (SCAN-25: 5 bytes), 8 with padding, to 1760;
    // satellite mode (chapter 28a: the source's pointer, 16 on the PC and 8 on the radio, and the App's own clock,
    // observer, alarm, open pass and what the Radio task was sent, 72), 96 with padding, to 1856; the Radio task's
    // queue 16 to 32 commands (MODE-6.5: the power-on burst with APRS mode and C muted is 17), 128, to 1984; when the
    // broadcast slot took the broadcast, for the sleep timer (BCR-5, decision 352: 4 bytes), 8 with padding, to 1992;
    // the shortcut popup as last drawn on home, so home waits under it (HOME-8: 4 bytes), 8 with padding, to 2000;
    // auto reply's settings in the App's AprsCfg (APRS-57: on and delay, the pattern, the 45-character reply), 56
    // to 2056; the Nearby view's rows found once, not per draw (CHAN-15, Nov 17, 2026: 32 channel numbers, their
    // count and what they were found for), 72, to 2128; the channel browser's counts kept between redraws (its view's,
    // 8, and the picker's band views', 16: a full radio counted 4000 records a redraw), 24, to 2152;
    // the status bar as last drawn gains VOX's mark (SBAR-25: 1 byte), 8 with padding, to 2160;
    // Snake's board, occupied rows, movement clock and state (SNAKE-2), 256 with padding, to 2416.
    assert!(n <= 2416, "App is {n} bytes");
    let s = core::mem::size_of::<crate::settings::Settings>();
    std::println!("Settings: {s} bytes");
    assert!(s <= 512, "Settings is {s} bytes");
}

#[test]
fn vfo_steps_and_bands() {
    let mut t = T::new();
    let f0 = t.freq();
    t.tap(Key::Up); // NAV-2: one step (12.5 kHz on VHF)
    assert_eq!(t.freq(), f0 + 1_250);
    assert!(t.cmds().contains(&RadioCmd::TuneSlot { slot: 0, freq: f0 + 1_250 }));
    t.knob(-2);
    assert_eq!(t.freq(), f0 - 1_250);
    // NAV-3: SIDE2 + Up, 1 MHz
    let s = store();
    t.app.key_down(Key::Side(2), t.t, &s);
    t.tap(Key::Up);
    t.app.key_up(Key::Side(2), t.t + 100, &s);
    assert_eq!(t.freq(), f0 - 1_250 + 100_000);
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Scan { .. })), "SIDE2 as a modifier doesn't scan");
    // ◀▶: bands, and back to where the VFO was on this band (VFO-1)
    let here = t.freq();
    t.tap(Key::Right);
    assert_eq!(slot::band_name(t.freq()), "Marine");
    t.tap(Key::Left);
    assert_eq!(t.freq(), here);
}

#[test]
fn arrows_repeat_while_held() {
    let mut t = T::new();
    let f0 = t.freq();
    t.press(Key::Up, 1000); // repeats from 400 ms at 10/s (KEY-4): 400..1000 is 7 steps, none on release
    assert_eq!(t.freq(), f0 + 7 * 1_250);
}

#[test]
fn channel_mode_steps_and_zones() {
    let mut t = T::new();
    t.tap(Key::VfoMem);
    let s = t.app.slot(0);
    assert!(s.on_channel());
    assert_eq!(s.ch.unwrap().name(), b"Calling");
    t.tap(Key::Down); // NAV-1: wraps within the zone
    assert_eq!(t.app.slot(0).ch.unwrap().name(), b"UHF call");
    t.tap(Key::Right); // the empty zone is skipped (CHAN-11)
    assert_eq!(t.app.slot(0).zone, 2);
    assert_eq!(t.app.slot(0).ch.unwrap().name(), b"North hill");
    assert!(t.cmds().contains(&RadioCmd::TuneSlot { slot: 0, freq: 14_694_000 }));
    t.tap(Key::VfoMem); // back to the VFO, unchanged
    assert!(!t.app.slot(0).on_channel());
    assert_eq!(t.freq(), 14_652_000);
}

#[test]
fn slot_cycles_and_edit_in_place() {
    let mut t = T::new();
    t.tap(Key::Slot); // NAV-5
    assert_eq!(t.app.active(), 1);
    assert!(t.cmds().contains(&RadioCmd::Active(1)));
    t.tap(Key::Slot);
    t.tap(Key::Slot);
    assert_eq!(t.app.active(), 0);
    t.hold(Key::Slot); // EDIT-1: first setting focused
    assert_eq!(t.app.screen(), Screen::Edit(Edit { cell: 0 }));
    t.tap(Key::Right);
    t.tap(Key::Right);
    t.tap(Key::Right); // stops at the edge
    t.tap(Key::Down);
    assert_eq!(t.app.screen(), Screen::Edit(Edit { cell: 5 })); // Step
    t.tap(Key::Left);
    t.tap(Key::Up);
    assert_eq!(t.app.screen(), Screen::Edit(Edit { cell: 1 })); // RX tone
    let rx0 = t.app.slot(0).cfg().rx_tone;
    t.knob(1); // EDIT-3: the knob changes, at once (EDIT-4)
    let rx1 = t.app.slot(0).cfg().rx_tone;
    assert_ne!(rx1, rx0);
    assert!(t.cmds().contains(&RadioCmd::SlotParam { slot: 0, param: Param::RxTone, value: rx1 as i32 }));
    t.tap(Key::Star); // undo within 5 s
    assert_eq!(t.app.slot(0).cfg().rx_tone, rx0);
    t.tap(Key::Slot); // EDIT-6: SLOT leaves
    assert_eq!(t.app.screen(), Screen::Home);
    assert_eq!(t.app.active(), 0, "leaving edit doesn't switch slots");
    t.hold(Key::Slot);
    t.wait(EDIT_IDLE_MS + 200); // EDIT-6: 8 s idle
    assert_eq!(t.app.screen(), Screen::Home);
}

#[test]
fn step_is_vfo_only() {
    let mut t = T::new();
    t.tap(Key::VfoMem);
    t.hold(Key::Slot);
    for _ in 0..2 {
        t.tap(Key::Right);
    }
    t.tap(Key::Down); // Step
    let before = *t.app.slot(0).cfg();
    t.knob(1);
    assert_eq!(*t.app.slot(0).cfg(), before, "greyed on a channel (VFO-8, EDIT-7)");
    assert_eq!(t.app.greyed(Item::Step), Some("VFO only"));
}

#[test]
fn quick_settings() {
    let mut t = T::new();
    t.tap(Key::Menu); // MENU-1
    assert_eq!(t.app.screen(), Screen::Quick(Quick { row: 0 }));
    t.tap(Key::Right); // MENU-16: ◀▶ change one step
    assert_eq!(t.app.slot(0).cfg().squelch, 5);
    t.knob(2);
    assert_eq!(t.app.slot(0).cfg().squelch, 7);
    t.tap(Key::Hash); // MENU-18: default
    assert_eq!(t.app.slot(0).cfg().squelch, 4);
    t.tap(Key::Up); // wraps to "0 More"
    assert_eq!(t.app.screen(), Screen::Quick(Quick { row: crate::screens::quick::MORE_ROW }));
    t.tap(Key::Digit(7)); // MENU-5: digits pick
    assert_eq!(t.app.screen(), Screen::Quick(Quick { row: 6 }));
    t.knob(1);
    assert_eq!(t.app.watch(), 2);
    assert!(t.cmds().contains(&RadioCmd::Watch(0b111)));
    t.tap(Key::Digit(0)); // advanced, then BACK returns to quick where it was (MENU-24)
    assert_eq!(t.app.screen(), Screen::Advanced(Advanced { row: 0, from_quick: true }));
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Quick(Quick { row: 6 }));
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
}

/// TX-9: 1.25 m's power is chosen like any band's (stock's levels on the 200-300 MHz PA path).
#[test]
fn power_levels_on_125m() {
    let mut t = T::new();
    for _ in 0..2 {
        t.tap(Key::Right); // VHF -> Marine -> 1.25 m
    }
    assert_eq!(slot::band_name(t.freq()), "1.25 m");
    t.tap(Key::Menu);
    t.tap(Key::Down); // TX power
    t.knob(2);
    assert_eq!(t.app.slot(0).cfg().power, 2, "High on 1.25 m");
    assert!(t.app.greyed(Item::Power).is_none());
}

/// TX-36: tuning onto HF (18-64 MHz) puts the power at Low, whatever it was: typed, by the knob across 64 MHz
/// and by ◀▶ onto an HF band; retuning within HF keeps what it was raised to; leaving HF changes nothing.
#[test]
fn hf_starts_on_low() {
    let mut t = T::new();
    let type_in = |t: &mut T, s: &str| {
        for c in s.chars() {
            t.tap(if c == '*' { Key::Star } else { Key::Digit(c as u8 - b'0') });
        }
        t.tap(Key::Menu);
    };
    let high = |t: &mut T| {
        let a = t.app.active();
        let mut c = t.app.slot(a).cfg;
        c.power = 2;
        t.app.set_vfo_cfg(a, c);
    };
    let power = |t: &T| t.app.slot(t.app.active()).cfg().power;
    high(&mut t);
    type_in(&mut t, "28*4");
    assert_eq!((t.freq(), power(&t)), (mhz(28) + 40_000, 0), "typed onto 28.4 MHz: Low");
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::SlotParam { param: Param::Power, value: 0, .. })), "sent");
    high(&mut t);
    type_in(&mut t, "29*6");
    assert_eq!(power(&t), 2, "within HF: kept");
    type_in(&mut t, "146*52");
    assert_eq!(power(&t), 2, "leaving HF: kept");
    // the knob across 64 MHz
    type_in(&mut t, "64");
    t.knob(-1);
    assert!(t.freq() < mhz(64));
    assert_eq!(power(&t), 0, "knob onto HF: Low");
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::SlotParam { param: Param::Power, value: 0, .. })));
    // ◀▶ onto an HF band
    type_in(&mut t, "146*52");
    high(&mut t);
    for _ in 0..slot::BANDS.len() {
        t.tap(Key::Left);
        if t.freq() < mhz(64) {
            break;
        }
    }
    assert!(t.freq() < mhz(64));
    assert_eq!(power(&t), 0, "◀ onto an HF band: Low");
}

/// MENU-24: each group (and third-level list) remembers its last item for the visit to the menu: back in it
/// from MENU 0 it opens there; a number path picks by number and that is its last item; home forgets them.
#[test]
fn groups_remember_their_last_item() {
    use crate::screens::groups::Group;
    let mut t = T::new();
    let row = |t: &T| match t.app.screen() {
        Screen::Group(g) => (g.g, g.sub, g.row),
        s => panic!("{s:?}"),
    };
    t.hold(Key::Menu); // MENU 0
    t.tap(Key::Digit(7));
    assert_eq!(row(&t), (7, 0, 0), "first time: row 1");
    t.tap(Key::Down);
    t.tap(Key::Down);
    t.tap(Key::Exit);
    t.tap(Key::Digit(7));
    assert_eq!(row(&t), (7, 0, 2), "again: on its last item");
    t.tap(Key::Exit);
    t.tap(Key::Digit(6));
    assert_eq!(row(&t), (6, 0, 0), "another group keeps its own");
    // a number path (0 7 5: Sounds) picks by number, and that is group 7's last item; a third-level list too
    t.tap(Key::Exit);
    for d in [7, 5] {
        t.tap(Key::Digit(d));
    }
    t.tap(Key::Down);
    t.tap(Key::Exit);
    assert_eq!(row(&t), (7, 0, 4), "BACK: group 7 on Sounds");
    t.tap(Key::Digit(5));
    assert_eq!(row(&t), (7, 5, 1), "Sounds on its last item");
    t.tap(Key::Exit);
    t.tap(Key::Exit);
    assert_eq!(Group::reopen(&t.app, 7, 0).row, 4, "group 7 on Sounds, the last item opened");
    // home forgets
    t.hold(Key::Exit);
    t.hold(Key::Menu);
    t.tap(Key::Digit(7));
    assert_eq!(row(&t), (7, 0, 0), "after home: row 1");
}

/// HOME-8, WATCH-7: hold 4 turns watch off, and on again as the set it was (A+B+C stays A+B+C), also after
/// watch was turned off in the menu.
#[test]
fn hold_4_restores_the_watch_set() {
    let mut t = T::new();
    t.app.set_watch(2);
    t.hold(Key::Digit(4));
    assert_eq!(t.app.watch, 0, "off");
    t.hold(Key::Digit(4));
    assert_eq!(t.app.watch, 2, "back on as A+B+C");
    assert!(t.app.toast.is_some_and(|x| x.body.as_bytes() == b"A+B+C"), "the popup names it");
    t.app.change(Item::Watch, -2); // MENU 7: off
    t.hold(Key::Digit(4));
    assert_eq!(t.app.watch, 2, "off in the menu, hold 4: A+B+C again");
    t.app.set_watch(1);
    t.hold(Key::Digit(4));
    t.hold(Key::Digit(4));
    assert_eq!(t.app.watch, 1, "A+B stays A+B");
}

/// MENU-24 with SBAR-15: a group opened to a setting (the Status page's VOX row) is on that setting, whatever
/// the group remembers; plain menu navigation still brings the remembered row back.
#[test]
fn status_link_opens_its_setting_not_the_remembered_row() {
    use crate::screens::status::Status;
    let mut t = T::new();
    let row = |t: &T| match t.app.screen() {
        Screen::Group(g) => (g.g, g.sub, g.row),
        s => panic!("{s:?}"),
    };
    t.hold(Key::Menu);
    t.tap(Key::Digit(8));
    t.tap(Key::Digit(2)); // VOX's list
    t.tap(Key::Down);
    t.tap(Key::Down); // Tail
    assert_eq!(row(&t), (8, 2, 2));
    t.tap(Key::Exit);
    t.tap(Key::Exit);
    t.tap(Key::Digit(0)); // MENU 0 0: Status
    assert!(matches!(t.app.screen(), Screen::Status(_)), "{:?}", t.app.screen());
    t.app.screen = Screen::Status(Status { row: 17, from_menu: true }); // the VOX row
    t.tap(Key::Menu);
    assert_eq!(row(&t), (8, 2, 0), "the VOX setting, not Tail");
    t.tap(Key::Exit);
    t.tap(Key::Exit);
    t.tap(Key::Digit(8));
    t.tap(Key::Digit(2));
    assert_eq!(row(&t), (8, 2, 2), "by the menu: Tail, remembered");
}

/// TONE-16: MENU 0 8 8 1 is Subtone level itself, a single setting like Mic gain; the knob steps it (Low, Normal,
/// High) and tells the Radio task. TONE-13.1: the tone burst is 1750 Hz only, no setting in the Transmit group.
#[test]
fn subtone_level_from_its_path() {
    use crate::screens::groups::Group;
    let mut t = T::new();
    t.hold(Key::Menu);
    for d in [8, 8, 1] {
        t.tap(Key::Digit(d));
    }
    assert!(matches!(t.app.screen(), Screen::Group(Group { g: 8, sub: 8, row: 0 })), "{:?}", t.app.screen());
    t.cmds();
    t.knob(1);
    assert_eq!(t.app.set.tone_level, 2, "High");
    assert!(t.cmds().contains(&RadioCmd::ToneLevel(2)), "told to the Radio task");
    t.knob(-2);
    assert_eq!(t.app.set.tone_level, 0, "Low");
    let tx = crate::screens::groups::items(8, 0);
    assert!(tx.iter().all(|e| !e.name().contains("burst")), "no tone burst setting");
}

/// MENU-6.1: before 1.0.0 a dropped item leaves no empty number: every group and list is filled
/// from 1 with real items.
#[test]
fn menu_numbers_have_no_gaps() {
    use crate::screens::groups::{items, GROUP_ITEMS, SUBS};
    for (g, list) in GROUP_ITEMS.iter().enumerate() {
        assert!(list.iter().all(|e| e.name() != "-" && !e.name().is_empty()), "0 {}: an empty number", g + 1);
    }
    for ((g, m), _) in SUBS {
        assert!(items(g, m).iter().all(|e| e.name() != "-" && !e.name().is_empty()), "0 {g} {m}: an empty number");
    }
}

#[test]
fn menu_zero_and_status() {
    let mut t = T::new();
    t.hold(Key::Menu); // MENU-2: hold MENU on home
    assert_eq!(t.app.screen(), Screen::Advanced(Advanced { row: 0, from_quick: false }));
    t.tap(Key::Digit(0)); // MENU 0 0 (SBAR-13)
    assert_eq!(t.app.screen(), Screen::Status(Status { row: 0, from_menu: true }));
    t.tap(Key::Exit);
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
    t.tap(Key::Hash); // HOME-10 (decision 376): the quick help, not the Status page
    assert!(matches!(t.app.screen(), Screen::Info(_)), "{:?}", t.app.screen());
    t.hold(Key::Exit); // KEY-5: hold BACK goes home
    assert_eq!(t.app.screen(), Screen::Home);
    t.tap(Key::Menu);
    t.wait(MENU_TIMEOUT_MS + 200); // MENU-28
    assert_eq!(t.app.screen(), Screen::Home);
}

#[test]
fn hash_pauses_watch_while_a_watched_slot_receives() {
    let mut t = T::new();
    t.live.slots[1] = SlotLive { squelch_open: true, rssi: -90, ..Default::default() };
    t.wait(100);
    t.tap(Key::Hash); // WATCH-6
    assert_eq!(t.app.screen(), Screen::Home);
    assert!(t.cmds().contains(&RadioCmd::PauseWatch(PAUSE_S)));
}

#[test]
fn key_lock() {
    let mut t = T::new();
    t.hold(Key::Hash); // KEY-13
    assert!(t.app.locked());
    let f = t.freq();
    t.tap(Key::Up);
    t.knob(3);
    t.tap(Key::Menu);
    assert_eq!((t.freq(), t.app.screen()), (f, Screen::Home), "KEY-14.1, KEY-14.4: other keys do nothing");
    t.hold(Key::Hash);
    assert!(!t.app.locked());
}

/// SQL-11, KEY-14.1: locked (the keypad's lock), SIDE1 still latches the monitor (and again releases it), and SIDE1 + knob or ▲▼
/// still set the squelch; SIDE1 + ◀▶ (the clarifier) and the knob alone stay locked.
#[test]
fn monitor_and_squelch_while_locked() {
    let mut t = T::new();
    let (s, f) = (store(), t.freq());
    t.hold(Key::Hash);
    assert!(t.app.locked());
    t.cmds();
    t.tap(Key::Side(1));
    assert!(t.cmds().contains(&RadioCmd::Monitor { slot: 0, on: true }), "SIDE1: monitor on");
    t.tap(Key::Side(1));
    assert!(t.cmds().contains(&RadioCmd::Monitor { slot: 0, on: false }), "SIDE1 again: off");
    let sq = t.app.slot(0).cfg().squelch;
    t.app.key_down(Key::Side(1), t.t, &s);
    t.knob(1);
    t.app.key_down(Key::Up, t.t, &s);
    t.app.key_up(Key::Up, t.t + 100, &s);
    t.app.key_down(Key::Right, t.t + 150, &s);
    t.app.key_up(Key::Right, t.t + 250, &s);
    assert_eq!(t.app.slot(0).clar, 0, "no clarifier while locked");
    t.app.key_up(Key::Side(1), t.t + 300, &s);
    assert_eq!(t.app.slot(0).cfg().squelch, sq + 2, "SIDE1 + knob and SIDE1 + ▲: squelch");
    t.knob(2);
    assert_eq!(t.freq(), f, "the knob alone: locked");
    assert!(t.app.locked());
}

/// SQL-9.1: SIDE1 held alone opens the active slot's squelch while it is held and closes it at the release,
/// without latching (no MON); SQL-9.2's tap still latches.
#[test]
fn side1_held_opens_while_held() {
    let mut t = T::new();
    let s = store();
    t.cmds();
    t.app.key_down(Key::Side(1), t.t, &s);
    t.wait(1000);
    assert!(t.cmds().contains(&RadioCmd::Monitor { slot: 0, on: true }), "held: open");
    assert_eq!(t.app.monitor, 0, "not latched");
    t.app.key_up(Key::Side(1), t.t, &s);
    t.wait(100);
    assert!(t.cmds().contains(&RadioCmd::Monitor { slot: 0, on: false }), "let go: closed");
    t.tap(Key::Side(1));
    assert_eq!(t.app.monitor, 1, "a tap latches");
}

#[test]
fn side1_and_knob_set_squelch() {
    let mut t = T::new();
    let (s, f) = (store(), t.freq());
    t.app.key_down(Key::Side(1), t.t, &s);
    t.knob(2);
    t.app.key_down(Key::Up, t.t, &s);
    t.app.key_up(Key::Up, t.t + 100, &s);
    t.knob(20);
    assert_eq!(t.app.slot(0).cfg().squelch, 9, "4 + 2 + 1, then stops at 9");
    assert_eq!(t.freq(), f, "no tuning while SIDE1 is held");
    t.app.key_up(Key::Side(1), t.t + 200, &s);
    assert!(t.cmds().contains(&RadioCmd::SlotParam { slot: 0, param: Param::Squelch, value: 9 }));
}

#[test]
fn side2_scans_the_active_slot() {
    let mut t = T::new();
    t.tap(Key::Side(2));
    assert!(t.cmds().contains(&RadioCmd::Scan { slot: 0, on: true }));
}

/// Chapter 7's hold-digit shortcuts, MENU 0 paths and BACK through them (MENU-24).
#[test]
fn shortcuts_and_menu_paths() {
    let mut t = T::new();
    t.app.radio.tx_built = true; // hold 2 sets power where the slot can transmit (HOME-4.2)
    t.app.set_region(Some(ferrite_core::txplan::Region::Us), false);
    // KEY-11: 1 opens the scope, 3/5/7 their editors, 9 on a VFO the save form (KEY-23); BACK comes home.
    for (d, name) in [(1, "Scope"), (3, "Value editor"), (5, "Tone picker"), (7, "Value editor"), (9, "Save as channel")] {
        t.hold(Key::Digit(d));
        assert_eq!(t.app.screen().name(), name, "hold {d}");
        t.tap(Key::Exit);
        assert_eq!(t.app.screen(), Screen::Home);
    }
    // VFO-26: 8 types the offset's amount, from empty; BACK comes home
    t.hold(Key::Digit(8));
    assert_eq!(t.app.screen().name(), "Frequency entry", "hold 8");
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
    t.hold(Key::Digit(2)); // TX power: low -> mid
    assert_eq!(t.app.slot(0).cfg().power, 1);
    let step = t.app.slot(0).cfg().step;
    t.hold(Key::Digit(6)); // step: the next one
    assert_ne!(t.app.slot(0).cfg().step, step);
    t.hold(Key::Digit(7)); // VFO-26: the shift's options, the knob picks +
    t.knob(1);
    assert!(t.app.slot(0).cfg().offset > 0);
    t.tap(Key::Menu); // OK keeps it
    assert_eq!(t.app.screen(), Screen::Home);
    let w = t.app.watch;
    t.hold(Key::Digit(4)); // watch on/off
    assert_ne!(t.app.watch, w);
    t.tap(Key::Digit(4)); // NAV-4: entry
    assert!(matches!(t.app.screen(), Screen::Entry(_)));
    t.hold(Key::Exit);
    // MENU 0 9 2 2: link status, typed straight through (MENU-5)
    use crate::screens::groups::Group;
    for d in [0, 9, 2, 2] {
        t.tap(if d == 0 { Key::Menu } else { Key::Digit(d) });
        if d == 0 {
            t.tap(Key::Digit(0));
        }
    }
    assert!(matches!(t.app.screen(), Screen::Link(_)));
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Group(Group { g: 9, sub: 2, row: 1 }), "the list, on its row");
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Group(Group { g: 9, sub: 0, row: 1 }));
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Advanced(Advanced { row: 8, from_quick: true }), "keeps the place");
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Quick(Quick { row: 0 }));
    t.tap(Key::Digit(4)); // MENU 4, OK: tones
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Tones(_)));
    t.wait(MENU_TIMEOUT_MS + 200);
    assert_eq!(t.app.screen(), Screen::Home);
}

/// KEY-7.3: PTT transmits from any screen, going home first; locked too (KEY-14.1).
#[test]
fn ptt_from_any_screen() {
    let mut t = T::new();
    t.app.radio.tx_built = true;
    t.app.set_region(Some(ferrite_core::txplan::Region::Us), false);
    t.tap(Key::Menu);
    t.hold(Key::Hash);
    let s = store();
    t.app.key_down(Key::PTT, t.t, &s);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
    t.app.key_up(Key::PTT, t.t + 300, &s);
    assert_eq!(t.app.screen(), Screen::Home);
}

/// Every destination opens and draws (its placeholder until it is built), and BACK leaves it.
#[test]
fn every_destination_draws() {
    use crate::screens::nav::{Dest, ALL, MENU0};
    let mut t = T::new();
    let mut fb = FrameBuffer::new();
    for dest in ALL.into_iter().chain(MENU0.map(|(_, d)| d)) {
        t.hold(Key::Exit);
        t.render(&mut fb);
        t.app.open(dest);
        t.render(&mut fb);
        assert!(count_px(&fb, 20, 300, |c| c != BG) > 100, "{dest:?} drew nothing");
        let mut b = ferrite_ui::text::Buf::<10>::new();
        t.app.screen().title(&t.app, &mut b);
        assert!(!b.is_empty(), "{dest:?}: no status bar name");
        if dest != Dest::Transmit {
            t.tap(Key::Exit);
            assert_ne!(t.app.screen(), Screen::Coming(crate::screens::coming::Coming { dest }), "{dest:?}: BACK");
        }
    }
    let ids: std::collections::BTreeSet<_> = ALL.iter().map(|d| d.info().2).collect();
    assert_eq!(ids.len(), 66, "one inventory ID per destination (B11 FM broadcast: decision 352; groups and lists share D3; B16 weather radio removed: decision 63; C7 sensor reader: decision 75; D18-D20 approvals: decision 104; C5a contacts: decision 226; C3a sent beacons: APRS-53; C19 FM presets: decision 258; D25 finish the update: decision 333; B22 satellites: chapter 28a; A16b auto reply: APRS-57; D26 Snake: SNAKE-1)");
}

mod group_a;
mod watch_heard;
mod ports;
mod sound_vox;
#[cfg(feature = "line-cmds")] // the `slot` command's tests; the link's SLOT is tested in link::tests
mod slotctl;
/// DISP-29: what redraws cost.
mod redraw_cost;

fn count_px(fb: &FrameBuffer, y0: i32, y1: i32, f: impl Fn(u16) -> bool) -> usize {
    (y0..y1).flat_map(|y| (0..240).map(move |x| (x, y))).filter(|&(x, y)| f(fb.at(x, y))).count()
}

#[test]
fn screens_draw() {
    let mut t = T::new();
    let mut fb = FrameBuffer::new();
    t.live.slots[0] = SlotLive { freq: t.freq(), squelch_open: true, rssi: -85, ..Default::default() };
    t.wait(100);
    t.render(&mut fb);
    // Home: a blue-bordered big card, a green meter (receiving), a hint bar.
    assert_eq!(fb.at(6, 80), BLUE);
    assert!(count_px(&fb, 134, 142, |c| c == GREEN) > 100, "meter lit green");
    assert!(count_px(&fb, 301, 320, |c| c != BG) > 50, "hint bar text");
    for (k, hold) in [(Key::Slot, 800), (Key::Exit, 100), (Key::Menu, 100), (Key::Digit(0), 100), (Key::Exit, 100)] {
        t.press(k, hold);
        t.render(&mut fb);
        assert!(count_px(&fb, 20, 300, |c| c != BG) > 1000, "{:?} drew its content", t.app.screen());
        assert!(count_px(&fb, 0, 19, |c| c != BG) > 50, "status bar");
    }
    t.tap(Key::Exit);
    t.hold(Key::Menu);
    t.tap(Key::Digit(0)); // MENU 0 0
    t.render(&mut fb);
    assert!(matches!(t.app.screen(), Screen::Status(_)));
    assert!(count_px(&fb, 20, 38, |c| c == BLUE) > 2000, "first row selected");
}

/// HOME-7: a meter change redraws the meter, not the frequency; a still screen draws nothing.
#[test]
fn redraws_only_what_changed() {
    struct Count(FrameBuffer, Vec<(i32, i32, i32, i32)>, u64);
    impl Panel for Count {
        fn window(&mut self, a: i32, b: i32, c: i32, d: i32) {
            self.1.push((a, b, c, d));
            self.0.window(a, b, c, d)
        }
        fn push(&mut self, c: u16, n: u32) {
            self.2 += n as u64;
            self.0.push(c, n)
        }
        fn push_buf(&mut self, p: &[u16]) {
            self.2 += p.len() as u64;
            self.0.push_buf(p)
        }
    }
    let mut t = T::new();
    let mut lcd = Count(FrameBuffer::new(), Vec::new(), 0);
    let dev = Device { clock_min: Some(600), ..Default::default() };
    t.live.slots[0] = SlotLive { freq: t.freq(), squelch_open: true, rssi: -100, ..Default::default() };
    t.app.render(&mut lcd, &t.live, &dev, &store());
    lcd.1.clear();
    lcd.2 = 0;
    t.app.render(&mut lcd, &t.live, &dev, &store());
    assert_eq!(lcd.2, 0, "nothing changed, nothing drawn");
    t.live.slots[0].rssi = -90;
    t.app.render(&mut lcd, &t.live, &dev, &store());
    assert!(lcd.2 > 0 && lcd.2 < 1500, "meter only: {} px", lcd.2);
    assert!(lcd.1.iter().all(|r| r.1 >= 110), "nothing above the meter row: {:?}", lcd.1);
    lcd.1.clear();
    lcd.2 = 0;
    t.tap(Key::Up);
    t.app.render(&mut lcd, &t.live, &dev, &store());
    assert!(lcd.1.iter().all(|r| r.1 >= 50 && r.3 <= 115), "frequency only: {:?}", lcd.1);
}

/// DISP-24, DISP-25: on the Status page a changing value (the GPS countdown, the battery, the clock's source, the
/// speaker mic) redraws its own row and nothing else: not the other rows, not the whole scroll cue (each row draws
/// its own part of it). Moving the selection within the page redraws the two rows; only scrolling the page (or a
/// new screen) draws every row.
#[test]
fn status_value_change_redraws_its_row_only() {
    use ferrite_core::gpspower::GpsPhase;
    // windows that pixels went to (the simulator's text-hint pass sends windows with no pixels)
    struct Count(FrameBuffer, Vec<(i32, i32, i32, i32)>, u64, Option<(i32, i32, i32, i32)>);
    impl Count {
        fn sent(&mut self, n: u64) {
            self.2 += n;
            if let Some(w) = self.3.take() {
                self.1.push(w);
            }
        }
    }
    impl Panel for Count {
        fn window(&mut self, a: i32, b: i32, c: i32, d: i32) {
            self.3 = Some((a, b, c, d));
            self.0.window(a, b, c, d)
        }
        fn push(&mut self, c: u16, n: u32) {
            self.sent(n as u64);
            self.0.push(c, n)
        }
        fn push_buf(&mut self, p: &[u16]) {
            self.sent(p.len() as u64);
            self.0.push_buf(p)
        }
    }
    const ROW_PX: u64 = 240 * 18;
    let mut t = T::new();
    t.hold(Key::Menu);
    t.tap(Key::Digit(0)); // MENU 0 0
    assert!(matches!(t.app.screen(), Screen::Status(_)));
    let mut lcd = Count(FrameBuffer::new(), Vec::new(), 0, None);
    let mut dev = Device { clock_min: Some(600), gps: GpsState::Fix, gps_phase: GpsPhase::Standby(42), battery_pct: 80, ..Default::default() };
    t.app.render(&mut lcd, &t.live, &dev, &store());
    type Rect = (i32, i32, i32, i32);
    let pass = |t: &mut T, lcd: &mut Count, dev: &Device| -> (Vec<Rect>, u64) {
        lcd.1.clear();
        lcd.2 = 0;
        t.app.render(lcd, &t.live, dev, &store());
        (lcd.1.clone(), lcd.2)
    };
    assert_eq!(pass(&mut t, &mut lcd, &dev).1, 0, "nothing changed, nothing drawn");
    // (row, the change): GPS is row 9, Clock 11, Speaker mic 13, Battery 14 (y = 20 + 18 row)
    type Change = (i32, fn(&mut Device));
    let changes: [Change; 5] = [
        (9, |d| d.gps_phase = GpsPhase::Standby(41)),
        (9, |d| d.gps_phase = GpsPhase::Running),
        (14, |d| d.battery_pct = 79),
        (11, |d| d.gps = GpsState::Searching),
        (13, |d| d.spkmic = true),
    ];
    for (row, change) in changes {
        change(&mut dev);
        let (wins, px) = pass(&mut t, &mut lcd, &dev);
        let (y0, y1) = (20 + 18 * row, 20 + 18 * row + 17);
        if row == 11 {
            // Searching: the GPS row says so too, the clock's source is now "Set by hand" and the status bar's GPS
            // icon changes; nothing else
            let ok = |w: &(i32, i32, i32, i32)| w.3 < 20 || (w.1 >= 182 && w.3 <= 199) || (w.1 >= y0 && w.3 <= y1);
            assert!(wins.iter().all(ok), "rows 9 and 11 and the status bar only: {wins:?}");
            assert!(px <= 3 * ROW_PX, "{px} px");
            continue;
        }
        assert!(px > 0 && px <= ROW_PX, "row {row}: {px} px, one row is {ROW_PX}");
        assert!(wins.iter().all(|w| w.1 >= y0 && w.3 <= y1), "row {row} ({y0}-{y1}) only: {wins:?}");
    }
    // the selection one row down: the two rows, nothing else
    t.tap(Key::Down);
    let (wins, px) = pass(&mut t, &mut lcd, &dev);
    assert!(px <= 2 * ROW_PX, "{px} px");
    assert!(wins.iter().all(|w| w.1 >= 20 && w.3 < 20 + 36), "rows 0 and 1 only: {wins:?}");
}

#[test]
fn toasts_replace_the_hint_bar_for_3_s() {
    let mut t = T::new();
    let mut fb = FrameBuffer::new();
    t.app.notify(b"VE3KPW-9", b"On my way", false);
    t.render(&mut fb);
    assert_eq!(fb.at(120, 300), BLUE, "toast's top line");
    t.wait(TOAST_MS + 200);
    t.render(&mut fb);
    assert_ne!(fb.at(120, 300), BLUE, "hint bar back");
}

/// HOME-11: the slots stay stacked A, B, C; SLOT only moves which one is large, in its own place. HOME-12: they
/// fill the content area, 5 px clear above the hint bar as below the status bar.
#[test]
fn slots_stay_in_order() {
    use crate::screens::home::card;
    for active in 0..3u8 {
        let (ya, _) = card(0, active);
        let (yb, _) = card(1, active);
        let (yc, hc) = card(2, active);
        assert!(ya < yb && yb < yc, "A above B above C");
        assert_eq!(ya, 25);
        assert!(yc + hc <= 296, "fits above the hint bar");
        assert_eq!(card(active, active).1, 124);
    }
    assert_eq!(card(1, 1), (98, 124));
    assert_eq!(card(2, 2).0 + card(2, 2).1, 295, "5 px clear above the hint bar, as below the status bar");
}

/// HOME-11: after SLOT moves the large card, nothing of the old large card is left in the new gaps.
#[test]
fn slot_change_leaves_no_trace() {
    let mut t = T::new();
    let mut fb = FrameBuffer::new();
    t.render(&mut fb);
    t.render(&mut fb);
    // A is large (25..149). After SLOT, B is large: A shrinks to 25..92 and the gap 92..98 must be background.
    t.tap(Key::Slot);
    t.render(&mut fb);
    let (ya, ha) = crate::screens::home::card(0, 1);
    let (yb, _) = crate::screens::home::card(1, 1);
    for y in ya + ha + 1..yb - 1 {
        for x in 10..230 {
            assert_eq!(fb.at(x, y), BG, "stale pixel at {x},{y} between A and B");
        }
    }
}

/// Every character in every string literal of the UI's sources is in the text fonts: a font is cut to its
/// character set (tools/mkassets.py), and a missing character would silently draw as '?'.
#[test]
fn fonts_have_every_character_the_ui_draws() {
    use ferrite_ui::assets::{UI_12, UI_12B};
    let sources = [
        ("lib.rs", include_str!("lib.rs")),
        ("screens/mod.rs", include_str!("screens/mod.rs")),
        ("screens/shared.rs", include_str!("screens/shared.rs")),
        ("screens/nav.rs", include_str!("screens/nav.rs")),
        ("screens/home.rs", include_str!("screens/home.rs")),
        ("screens/edit.rs", include_str!("screens/edit.rs")),
        ("screens/quick.rs", include_str!("screens/quick.rs")),
        ("screens/advanced.rs", include_str!("screens/advanced.rs")),
        ("screens/status.rs", include_str!("screens/status.rs")),
        ("screens/coming.rs", include_str!("screens/coming.rs")),
        // group C: lists and data
        ("screens/lists.rs", include_str!("screens/lists.rs")),
        ("screens/log.rs", include_str!("screens/log.rs")),
        ("screens/opts.rs", include_str!("screens/opts.rs")),
        ("screens/aprs.rs", include_str!("screens/aprs.rs")),
        ("screens/msgs.rs", include_str!("screens/msgs.rs")),
        ("screens/gps.rs", include_str!("screens/gps.rs")),
        ("screens/browse.rs", include_str!("screens/browse.rs")),
        ("screens/zones.rs", include_str!("screens/zones.rs")),
        ("screens/scanlists.rs", include_str!("screens/scanlists.rs")),
        ("screens/recall.rs", include_str!("screens/recall.rs")),
        ("data.rs", include_str!("data.rs")),
        ("sample.rs", include_str!("sample.rs")),
        ("screens/groups.rs", include_str!("screens/groups.rs")),
        ("screens/setup.rs", include_str!("screens/setup.rs")),
        ("screens/pin.rs", include_str!("screens/pin.rs")),
        ("screens/info.rs", include_str!("screens/info.rs")),
        ("screens/keytest.rs", include_str!("screens/keytest.rs")),
        ("screens/link.rs", include_str!("screens/link.rs")),
        ("screens/fm.rs", include_str!("screens/fm.rs")),
        ("screens/hf.rs", include_str!("screens/hf.rs")),
        ("screens/alert.rs", include_str!("screens/alert.rs")),
        ("settings.rs", include_str!("settings.rs")),
        ("slot.rs", include_str!("slot.rs")),
        ("chan.rs", include_str!("chan.rs")),
        ("widgets.rs", include_str!("../../ferrite-ui/src/widgets.rs")),
        ("boot.rs", include_str!("../../ferrite-ui/src/boot.rs")),
        ("home.rs", include_str!("../../ferrite-ui/src/home.rs")),
    ];
    let mut checked = 0;
    for (file, src) in sources {
        for s in string_literals(src) {
            for c in s.chars().filter(|c| !('\x18'..='\x1b').contains(c) && *c != '\n') {
                assert!(UI_12.has(c) && UI_12B.has(c), "{file}: {c:?} (U+{:04X}) in {s:?} is not in the text fonts", c as u32);
                checked += 1;
            }
        }
    }
    assert!(checked > 2000, "the scan found only {checked} characters");
    for c in "0123456789.- ".chars() {
        for f in [&ferrite_ui::assets::DIG_46, &ferrite_ui::assets::DIG_28, &ferrite_ui::assets::DIG_26, &ferrite_ui::assets::DIG_17] {
            assert!(f.has(c), "digit fonts lack {c:?}");
        }
    }
}

/// CHAN-31: the name fonts hold every character a channel name may (`ferrite_core::name`), the 22 px one also the
/// GPS screen's degree sign and double quote (GPS-15), and the sample channels' names are in the set.
#[test]
fn chan_31_name_fonts_have_the_name_set() {
    use ferrite_core::name::name_char;
    use ferrite_ui::assets::{NAME_22, NAME_26};
    let set: std::vec::Vec<u8> = (0..=255u8).filter(|&c| name_char(c)).collect();
    for &c in &set {
        assert!(NAME_26.has(c as char) && NAME_22.has(c as char), "name fonts lack {:?}", c as char);
    }
    assert!(NAME_22.has('\u{b0}') && NAME_22.has('"'), "the GPS screen's position");
    // cut to the set: nothing else (the 22 px: the GPS screen's two)
    assert_eq!(NAME_26.glyphs.len(), set.len());
    assert_eq!(NAME_22.glyphs.len(), set.len() + 2);
    let st = crate::sample::Sample::new();
    let mut n = 0;
    for num in 0..crate::chan::MAX_CHANNELS {
        if let Some(c) = st.get(num) {
            assert!(c.name().iter().all(|&b| name_char(b)), "sample channel {num}: {:?}", c.name());
            n += 1;
        }
    }
    assert!(n > 10);
    // and a name typed or loaded with others in it maps them to '-'
    let mut c = st.get((0..crate::chan::MAX_CHANNELS).find(|&i| st.get(i).is_some()).unwrap()).unwrap();
    c.set_name("Caf\u{e9} @ 5*".as_bytes());
    assert_eq!(c.name(), b"Caf-- - 5-");
}

/// The string literals of a Rust source, escapes decoded; comments and char literals skipped.
fn string_literals(src: &str) -> Vec<std::string::String> {
    let b: Vec<char> = src.chars().collect();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        match b[i] {
            '/' if b.get(i + 1) == Some(&'/') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '\'' => {
                // a char literal ('x', '\n', '\u{b7}') or a lifetime
                if b.get(i + 1) == Some(&'\\') {
                    i += 2;
                    while i < b.len() && b[i] != '\'' {
                        i += 1;
                    }
                    i += 1;
                } else if b.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            '"' => {
                let mut s = std::string::String::new();
                i += 1;
                while i < b.len() && b[i] != '"' {
                    if b[i] == '\\' {
                        i += 1;
                        match b[i] {
                            'n' => s.push('\n'),
                            't' | '0' | 'r' => {}
                            'x' => {
                                let v = u32::from_str_radix(&b[i + 1..i + 3].iter().collect::<std::string::String>(), 16).unwrap();
                                s.push(char::from_u32(v).unwrap());
                                i += 2;
                            }
                            'u' => {
                                let end = i + b[i..].iter().position(|&c| c == '}').unwrap();
                                let hex: std::string::String = b[i + 2..end].iter().collect();
                                s.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
                                i = end;
                            }
                            '\n' => {
                                while b.get(i + 1).is_some_and(|c| c.is_whitespace()) {
                                    i += 1;
                                }
                            }
                            c => s.push(c),
                        }
                    } else {
                        s.push(b[i]);
                    }
                    i += 1;
                }
                out.push(s);
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

#[test]
fn literal_scanner() {
    let v = string_literals("let a = \"A\\u{b7}B\"; // \"not this\"\nlet c = '\"'; let d = b\"x\\x18\";");
    assert_eq!(v, ["A\u{b7}B", "x\x18"]);
}

/// Group B: transmit, scan, scope, tone search, counter, FM, HF, weather, alert, crossband, layouts.
mod radio_activity;
mod bcast;
mod lights;
/// The settings wired in decision 243.
mod wired;
/// Low Power Mode (PWR-24).
mod lpm;
mod aprs;
mod aprs_card;
mod d64;
/// Decision 72: the nicFW gaps (HF channels, 1000-1300 MHz, the clarifier, grid entry).
mod nicfw;
mod help;
/// The tone hint (TSRCH-13, TSRCH-16).
mod tonehint;
/// Quick mute (HOME-15).
mod mute;
mod slot_off;
/// The quick menu on hold \* (HOME-17).
mod qmenu;
/// The UI readback (LINK-19).
mod uistate;
/// The simulators' store.
mod sample;
/// The store traits' defaults, Recent and the small records.
mod store;
/// The App's channel bookkeeping and boot calls.
mod app_api;
/// Remote keys and PTT at their edges.
mod remote_edges;
/// The APRS card's text lines.
mod aprs_text;
/// Satellite mode's radio commands, alarm and mark (chapter 28a).
mod sat;

/// Group D's screens: setup, PIN, key test, link, FM broadcast, HF mode, alert, the menu's groups.
#[path = "tests_d.rs"]
mod d;


/// One BACK rule everywhere (KEY-5, MENU-24): short BACK goes back one level, to the screen this one was opened
/// from; hold BACK goes home from anywhere. Paths that used to jump home: the Status page's Watch row (quick
/// settings), edit in place and the channel editor, scan, an alert, the shortcut map.
#[test]
fn back_is_one_level_hold_back_is_home() {
    let mut t = T::new();
    // Status page (MENU 0 0) -> Watch (quick settings) -> BACK -> Status -> BACK -> MENU 0 -> BACK -> home
    t.hold(Key::Menu);
    t.tap(Key::Digit(0));
    assert!(matches!(t.app.screen(), Screen::Status(_)));
    for _ in 0..5 {
        t.tap(Key::Down); // Watch
    }
    t.tap(Key::Menu);
    assert_eq!(t.app.screen(), Screen::Quick(Quick { row: 6 }));
    t.tap(Key::Exit);
    assert!(matches!(t.app.screen(), Screen::Status(s) if s.row == 5), "back to the Status page, on its row");
    t.tap(Key::Exit);
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
    // MENU 0 0 (Status from advanced) -> BACK -> advanced -> BACK -> home
    t.hold(Key::Menu);
    t.tap(Key::Digit(0));
    assert!(matches!(t.app.screen(), Screen::Status(_)));
    t.tap(Key::Exit);
    assert!(matches!(t.app.screen(), Screen::Advanced(_)));
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
    // Quick -> value editor -> BACK -> quick; edit in place -> BACK -> home
    t.tap(Key::Menu);
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Value(_)));
    t.tap(Key::Exit);
    assert!(matches!(t.app.screen(), Screen::Quick(_)));
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
    t.hold(Key::Slot);
    assert!(matches!(t.app.screen(), Screen::Edit(_)));
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
    // The channel editor from quick settings (MENU 8) -> BACK -> quick
    t.tap(Key::VfoMem);
    t.tap(Key::Menu);
    t.tap(Key::Digit(8));
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::ChEdit(_)));
    t.tap(Key::Exit);
    assert!(matches!(t.app.screen(), Screen::Quick(_)), "the editor goes back to quick settings");
    // Hold BACK: home from three levels down.
    t.tap(Key::Digit(0));
    t.tap(Key::Digit(9));
    assert!(matches!(t.app.screen(), Screen::Group(_)));
    t.hold(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
    // The key test is left with hold BACK; a short BACK is a key it shows.
    t.app.open(crate::screens::nav::Dest::KeyTest);
    t.tap(Key::Exit);
    assert!(matches!(t.app.screen(), Screen::KeyTest(_)), "short BACK is tested, not obeyed");
    t.hold(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
}

/// CHAN-23, VFO-4, BCR-33: a channel's or home VFO's mode is FM, NFM, AM or WFM; USB, LSB and CW are HF mode's
/// only.
#[test]
fn slot_modes_are_fm_nfm_am_wfm() {
    let mut c = DEFAULT_CFG;
    let mut seen = [false; 7];
    for _ in 0..9 {
        let v = Item::Mode.get(&c).0 + 1;
        Item::Mode.set(&mut c, v, 0);
        seen[c.mode as usize] = true;
    }
    assert_eq!(seen, [true, true, true, false, false, false, true]);
    let mut t = T::new();
    for on_channel in [false, true] {
        if on_channel {
            t.tap(Key::VfoMem);
            assert!(t.app.slot(0).on_channel());
        }
        for d in [1, 1, 1, 1, -1, -1, -1, -1] {
            t.app.change(Item::Mode, d);
            assert!(crate::slot::slot_mode_ok(t.app.slot(0).cfg().mode), "mode {} on {}", t.app.slot(0).cfg().mode, if on_channel { "a channel" } else { "the VFO" });
        }
    }
}

/// MENU-8: the Recent row on advanced settings can be selected; ◀▶ pick a recent setting and OK opens its
/// editor, BACK returns to the row.
#[test]
fn recent_row_is_selectable() {
    let mut t = T::new();
    t.hold(Key::Menu);
    t.tap(Key::Up);
    assert_eq!(t.app.screen(), Screen::Advanced(Advanced { row: 8, from_quick: false }), "nothing recent: wraps to the groups");
    t.hold(Key::Exit);
    t.tap(Key::Menu); // quick: squelch, then power
    t.knob(1);
    t.tap(Key::Digit(5));
    t.knob(1);
    t.hold(Key::Exit);
    t.hold(Key::Menu);
    t.tap(Key::Up);
    let recent = crate::screens::advanced::RECENT;
    assert_eq!(t.app.screen(), Screen::Advanced(Advanced { row: recent, from_quick: false }));
    t.tap(Key::Right);
    assert_eq!(t.app.screen(), Screen::Advanced(Advanced { row: recent + 1, from_quick: false }));
    t.tap(Key::Menu);
    let second = t.app.recent[1].unwrap();
    assert!(matches!(t.app.screen(), Screen::Value(v) if v.item == second), "{:?}", t.app.screen());
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Advanced(Advanced { row: recent + 1, from_quick: false }));
    t.tap(Key::Down);
    assert_eq!(t.app.screen(), Screen::Advanced(Advanced { row: 0, from_quick: false }));
    let mut fb = FrameBuffer::new();
    t.render(&mut fb);
}

/// 0 7 1: changing the theme redraws everything at once in the new colours (DISP-9); night red has no green or
/// blue anywhere on the screen.
#[test]
fn theme_change_redraws_everything() {
    let mut t = T::new();
    let mut fb = FrameBuffer::new();
    t.render(&mut fb);
    assert_eq!(fb.at(1, 318), BG, "dark: the hint bar's ground");
    t.app.settings_mut().theme = ferrite_ui::theme::NIGHT_RED;
    t.render(&mut fb);
    for (x, y) in [(5, 5), (120, 150), (1, 318), (60, 60)] {
        assert_eq!(fb.at(x, y) & 0x07FF, 0, "({x}, {y}) is red only");
    }
    t.app.settings_mut().theme = ferrite_ui::theme::DAYLIGHT;
    t.render(&mut fb);
    assert_eq!(fb.at(1, 318), ferrite_ui::gfx::WHITE, "daylight: white ground");
    t.app.settings_mut().theme = ferrite_ui::theme::DARK;
    t.render(&mut fb);
    assert_eq!(fb.at(1, 318), BG);
}

/// DISP-18 (strict night red): home is red or black only, nothing between but text's anti-aliasing (no dim fills,
/// no dim text); and the active card's heavier outline follows SLOT without leaving a stale ring behind (a whole
/// redraw draws the same pixels).
#[test]
fn night_red_is_red_or_black() {
    for layout in 0..3u8 {
        let mut t = T::new();
        t.app.settings_mut().theme = ferrite_ui::theme::NIGHT_RED;
        t.app.settings_mut().layout = layout;
        let mut fb = FrameBuffer::new();
        t.render(&mut fb);
        let (red, black) = (ferrite_ui::theme::NIGHT_INK, ferrite_ui::gfx::BLACK);
        let mut other = 0;
        for &c in &fb.px {
            assert_eq!(c & 0x07FF, 0, "red only");
            other += (c != red && c != black) as usize;
        }
        // text's anti-aliased edges are the only levels: a few per cent of the screen at most (layout 1's strip
        // is the scope's gradient, which keeps its red levels)
        assert!(layout == 1 || other * 100 / fb.px.len() < 6, "layout {layout}: {other} pixels neither red nor black");
        for _ in 0..3 {
            t.tap(Key::Slot);
            t.render(&mut fb);
            let mut whole = FrameBuffer::new();
            t.app.redraw_all();
            t.render(&mut whole);
            let diff = fb.px.iter().zip(&whole.px).filter(|(a, b)| a != b).count();
            assert_eq!(diff, 0, "layout {layout}, active {}: the redraw after SLOT left {diff} stale pixels", t.app.active);
        }
    }
}

/// KEY-19: ◀▶ do what the knob does on screens that don't give them a job of their own; the ones that do keep it.
#[test]
fn arrows_are_the_knob() {
    use crate::screens::nav::Dest;
    use crate::slot::Item;
    let dests = [
        Dest::Quick,
        Dest::Value(Item::Squelch),
        Dest::Group(7),
        Dest::Group(1),
        Dest::Sub(9, 1),
        Dest::Status,
        Dest::AprsHeard,
    ];
    for d in dests {
        for (key, step) in [(Key::Right, 1i8), (Key::Left, -1)] {
            let (mut a, mut b) = (T::new(), T::new());
            a.open(d);
            b.open(d);
            a.tap(key);
            b.knob(step);
            b.wait(150);
            assert_eq!(a.app.screen(), b.app.screen(), "{d:?} {key:?}");
            assert!(a.app.settings() == b.app.settings(), "{d:?} {key:?}: settings differ");
            assert_eq!(a.app.slot(0).cfg().squelch, b.app.slot(0).cfg().squelch, "{d:?} {key:?}");
        }
    }
    // And they do something: ▶ in the squelch editor raises the level, as the knob does.
    let mut a = T::new();
    let before = a.app.slot(0).cfg().squelch;
    a.open(Dest::Value(Item::Squelch));
    a.tap(Key::Right);
    assert_ne!(a.app.slot(0).cfg().squelch, before);
    // Home keeps ◀▶ for bands: ▶ is not the knob there.
    let (mut a, mut b) = (T::new(), T::new());
    a.tap(Key::Right);
    b.knob(1);
    assert_ne!(a.app.slot(0).freq(), b.app.slot(0).freq(), "home: ◀▶ step the band, the knob the frequency");
}

/// HOME-4: the big card's chips sit in fixed slots. Turning the tone off empties its slot and leaves every pixel of
/// the mode, shift and power slots as it was; a shorter shift leaves nothing behind.
#[test]
fn chip_slots_never_move() {
    use ferrite_ui::gfx::SURFACE;
    use ferrite_ui::widgets::{SLOT_MODE, SLOT_POWER, SLOT_SHIFT, SLOT_TONE};
    let mut t = T::new();
    let base = crate::slot::SlotCfg { tx_tone: 1000, offset: -60_000, ..DEFAULT_CFG };
    t.app.set_vfo(0, 14_652_000);
    t.app.set_vfo_cfg(0, base);
    let mut fb = FrameBuffer::new();
    t.render(&mut fb);
    let (y, _) = crate::screens::home::card(0, 0);
    let cy = y + crate::screens::home::CHIP_Y;
    let grab = |fb: &FrameBuffer, (x, w): (i32, i32)| (x..x + w).flat_map(|x| (cy..cy + 17).map(move |y| (x, y))).map(|(x, y)| fb.at(x, y)).collect::<Vec<_>>();
    let (mode, shift, power) = (grab(&fb, SLOT_MODE), grab(&fb, SLOT_SHIFT), grab(&fb, SLOT_POWER));
    assert!(grab(&fb, SLOT_TONE).iter().any(|&c| c != SURFACE), "a tone shows");
    // Tone off: its slot is blank, the others untouched.
    t.app.set_vfo_cfg(0, crate::slot::SlotCfg { tx_tone: 0, ..base });
    t.render(&mut fb);
    assert!(grab(&fb, SLOT_TONE).iter().all(|&c| c == SURFACE), "the tone slot is empty");
    assert_eq!(grab(&fb, SLOT_MODE), mode);
    assert_eq!(grab(&fb, SLOT_SHIFT), shift);
    assert_eq!(grab(&fb, SLOT_POWER), power, "power never moves");
    // Simplex: the shift slot empties too; power still where it was.
    t.app.set_vfo_cfg(0, crate::slot::SlotCfg { tx_tone: 0, offset: 0, ..base });
    t.render(&mut fb);
    assert!(grab(&fb, SLOT_SHIFT).iter().all(|&c| c == SURFACE));
    assert_eq!(grab(&fb, SLOT_POWER), power);
    // The widest text each slot shows fits it.
    use ferrite_ui::assets::UI_12;
    use ferrite_ui::gfx::text4_width;
    for (s, (_, w)) in [("NFM", SLOT_MODE), ("T 254.1", SLOT_TONE), ("R-12.5M", SLOT_SHIFT), ("R SPLIT", SLOT_SHIFT), ("No TX", SLOT_POWER)] {
        assert!(text4_width(s.as_bytes(), &UI_12) <= w - 4, "{s} fits its slot");
    }
    assert!(SLOT_POWER.0 + SLOT_POWER.1 == 226 && SLOT_SHIFT.0 + SLOT_SHIFT.1 < SLOT_POWER.0);
}
