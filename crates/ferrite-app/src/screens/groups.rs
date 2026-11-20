//! A settings group (MENU 0 n) and a third-level list (MENU 0 n m): one list widget for both (manual chapter
//! 11, MENU-3 to MENU-24; MenuGroup mock-up). The lists are data: [`GROUP_ITEMS`] and [`SUBS`] hold every item
//! of the manual's Appendix A with its value and help line. [`SETTINGS`] has each wired setting once: its path,
//! its field, how it shows and steps, and what the Radio task is told; [`wired`] (the live value), [`change`] (the
//! knob) and [`reset`] (#) work from it, with a short match for the settings that need more. The rest show their
//! default, read only, until their feature lands.
//!
//! Keys: ▲▼ move, the knob or ◀▶ change the highlighted value (MENU-16), OK opens a screen or list, a digit
//! picks its row and opens it (MENU-5: a path typed straight through lands where it would typed slowly), #
//! resets (MENU-18), BACK goes up keeping the place (MENU-24).

use super::nav::{self, Dest};
use super::setup::ExtendTx;
use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::ChannelStore;
use crate::keys::{KeyEvent, Press};
use crate::settings::*;
use crate::slot::Item;
use crate::{App, Screen, MENU_TIMEOUT_MS};
use ferrite_core::keys::Key;
use ferrite_core::msg::{CalState, Device, RadioCmd};
use ferrite_core::VERSION;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets::Row;
use ufmt::uwrite;

/// One item: name, value shown until it is wired (a list shows a summary) and help line, in one string as
/// "name|value|help". One string per item, not three: 8 bytes of table per item instead of 24 (about 3 KB). The
/// help line is in the text pack (UPD-22): the string holds its number (`txtc!`, two characters).
#[derive(Clone, Copy)]
pub struct Entry(&'static str);

impl Entry {
    fn parts(self) -> (&'static str, &'static str, &'static str) {
        let (name, rest) = self.0.split_once('|').unwrap_or((self.0, ""));
        let (value, help) = rest.split_once('|').unwrap_or((rest, ""));
        (name, value, help)
    }

    pub fn name(self) -> &'static str {
        self.parts().0
    }

    /// The value shown until the setting is wired (a list shows a summary).
    pub fn value(self) -> &'static str {
        self.parts().1
    }

    /// The help line's text in the pack (MENU-15; UPD-22).
    pub fn help(self) -> Option<crate::text::Txt> {
        crate::text::code(self.parts().2)
    }
}

/// What a menu's help box says: the item's note, from the text pack (MENU-15, UPD-22: none without the pack), or
/// why the item is greyed (MENU-14).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Help {
    Note(Option<crate::text::Txt>),
    Why(&'static str),
}

impl Help {
    /// Its words (a note read from the pack: when drawn or sent, never per frame).
    pub(crate) fn load(self) -> crate::text::Text {
        match self {
            Help::Note(t) => t.map_or(crate::text::Text::EMPTY, crate::text::load),
            Help::Why(w) => crate::text::Text::from_bytes(w.as_bytes()),
        }
    }
}

/// SND-3: the beep volume (0 7 5 2), three DAC levels (decision 100).
const BEEP_VOLS: [&str; 3] = ["Low", "Mid", "High"];
const G1: [Entry; 6] = [
    Entry(concat!("Watch|A+B|", txtc!("Which slots are heard besides the active one."))),
    Entry(concat!("Resume|5 s|", txtc!("When a stopped scan moves on."))),
    Entry(concat!("Priority check|Every 2 s|", txtc!("Look at the priority channels."))),
    Entry(concat!("Range presets|4 presets|", txtc!("Named ranges to scan."))),
    Entry(concat!("Skip list|Empty|", txtc!("Frequencies * skipped in this scan."))),
    Entry(concat!("Advanced||", txtc!("Pause time, direction, scan inputs, return to."))),
];
const G2: [Entry; 6] = [
    Entry(concat!("Browse||", txtc!("Zones, all channels, nearby, recent."))),
    Entry(concat!("Add repeater||", txtc!("Frequency, shift, tone, name."))),
    Entry(concat!("Zones||", txtc!("Name, scan options, priority channels."))),
    Entry(concat!("Channel shows|Name|", txtc!("Name, number or frequency on home."))),
    Entry(concat!("Local zone|50 km|", txtc!("A zone of the channels within this range of you, nearest first. Needs a GPS fix or the fixed position (0 6 9)."))),
    Entry(concat!("FM broadcast||", txtc!("Step, AGC, radio monitor, sleep timer."))),
];
const G3: [Entry; 7] = [
    Entry(concat!("Band scope|60 bars|", txtc!("Bars, step, speed, trigger, waterfall."))),
    Entry(concat!("Frequency counter||", txtc!("Find a nearby transmitter's frequency."))),
    Entry(concat!("HF mode||", txtc!("Below 64 MHz. Everything else pauses."))),
    Entry(concat!("Crossband repeater||", txtc!("Repeat one slot's band on another's."))),
    Entry(concat!("Tone search||", txtc!("Find the CTCSS or DCS in use."))),
    Entry(concat!("Satellites||", txtc!("Passes, pointing and Doppler for FM satellites."))),
    Entry(concat!("Snake||", txtc!("Steer the snake and collect food."))),
];
const G4: [Entry; 5] = [
    Entry(concat!("Event log||", txtc!("Every decode and alert."))),
    Entry(concat!("POCSAG|439.9875|", txtc!("Pager frequency, baud, capcodes."))),
    Entry(concat!("DTMF decode|On|", txtc!("DTMF digits heard, on the slot's card."))),
    Entry(concat!("Popups|On|", txtc!("A 3 s toast for each decode."))),
    Entry(concat!("Weather alerts|On|", txtc!("SAME and your location codes."))),
];
const G5: [Entry; 9] = [
    Entry(concat!("Heard||", txtc!("Stations heard, newest first."))),
    Entry(concat!("Messages||", txtc!("Messages sent to you and by you."))),
    Entry(concat!("APRS mode|Off|", txtc!("Slot C becomes the APRS slot."))),
    Entry(concat!("Frequency|144.390|", txtc!("OK types it; # the region's."))),
    Entry(concat!("My station|SSID 9|", txtc!("SSID, symbol, comment, position."))),
    Entry(concat!("Beacon|Off|", txtc!("When to beacon: manual, timed, smart."))),
    Entry(concat!("Digipeat|Off|", txtc!("Relay others' packets."))),
    // The KISS TNC (the mailbox beside it waits until it is built, decision 245).
    Entry(concat!("KISS TNC|Off|", txtc!("Raw KISS on the cable or Bluetooth."))),
    Entry(concat!("Advanced||", txtc!("Path, TX delay and level, FX.25."))),
];
const G6: [Entry; 9] = [
    Entry(concat!("GPS|Auto|", txtc!("Off, Auto (when needed) or On."))),
    Entry(concat!("Position||", txtc!("Fix, satellites, position, grid."))),
    Entry(concat!("Coordinates|DDM|", txtc!("Decimal, DDM, DMS or grid square."))),
    Entry(concat!("Units|Metric|", txtc!("Distance, altitude and speed units."))),
    Entry(concat!("Time zone|UTC|", txtc!("The clock's offset from UTC."))),
    Entry(concat!("Clock|24 h|", txtc!("12 or 24 h, from GPS; OK sets it by hand."))),
    Entry(concat!("Waypoints||", txtc!("Marked places; go to one."))),
    Entry(concat!("GPS off during|Never|", txtc!("Turn the GPS off while receiving, transmitting or both."))),
    Entry(concat!("Fixed position|None|", txtc!("Used when there is no fix."))),
];
const G7: [Entry; 9] = [
    Entry(concat!("Theme|Dark|", txtc!("Dark, daylight, night red or cyberpunk."))),
    Entry(concat!("Layout|Watch|", txtc!("What home shows."))),
    Entry(concat!("Display|Bars|", txtc!("S-meter, power-on message, boot screen."))),
    Entry(concat!("Backlight|15 s|", txtc!("Timeout, brightness, wake."))),
    Entry(concat!("Sounds|Beep on|", txtc!("Key beep, volume, alert tones."))),
    Entry(concat!("LEDs|RX, TX, heartbeat|", txtc!("And a flash while the screen is dark."))),
    Entry(concat!("Power saving|Level 1|", txtc!("Battery Save, Low Power Mode."))),
    Entry(concat!("Auto power off|Off|", txtc!("Off after a time without key or signal."))),
    Entry(concat!("Menu timeout|30 s|", txtc!("An idle menu returns home."))),
];
const G8: [Entry; 8] = [
    Entry(concat!("Timeout|120 s|", txtc!("Transmit stops after this; a beep says so."))),
    Entry(concat!("VOX|Off|", txtc!("Voice-operated transmit."))),
    Entry(concat!("Mic gain|5|", txtc!("Higher is louder on air: 1-9. 8-9 for a quiet speaker mic."))),
    Entry(concat!("End tone|Off|", txtc!("A short tone as you let go of PTT."))),
    Entry(concat!("DTMF|100 ms|", txtc!("Dialer, memories, timing."))),
    Entry(concat!("Callsign and ID|Empty|", txtc!("Your callsign, and CW ID."))),
    Entry(concat!("Busy lockout default|Off|", txtc!("For new channels and the VFO."))),
    Entry(concat!("Advanced||", txtc!("Subtone level, tail, Extend TX, Dry TX."))),
];
const G9: [Entry; 9] = [
    Entry(concat!("Keys and lock|Manual lock|", txtc!("Auto lock, key test, shortcuts."))),
    Entry(concat!("Connections|BT off|", txtc!("Bluetooth, link status, cable port."))),
    Entry(concat!("PIN and lock|Off|", txtc!("PIN lock, channel-only mode, PIN."))),
    Entry(concat!("Region|Not set|", txtc!("Your amateur bands and defaults."))),
    Entry(concat!("Squelch|Software|", txtc!("Decided in software, or by the chip as stock."))),
    Entry(concat!("SSB patch||", txtc!("USB, LSB and CW in HF mode."))),
    Entry(concat!("Setup and reset||", txtc!("Run setup again, reset settings."))),
    Entry(concat!("About||", txtc!("Version, identity, checks, battery."))),
    Entry(concat!("Advanced||", txtc!("Squelch, calibration, cable log, MCU clock."))),
];
/// The nine groups' items (MENU 0 1 to MENU 0 9).
pub(crate) const GROUP_ITEMS: [&[Entry]; 9] = [&G1, &G2, &G3, &G4, &G5, &G6, &G7, &G8, &G9];
/// Short names for the status bar (up to 10 characters).
const SHORT: [&str; 9] = ["Scan", "Channels", "Tools", "Decoders", "APRS", "GPS", "Display", "Transmit", "System"];

/// The third-level lists, by group and item.
pub(crate) const SUBS: [((u8, u8), &[Entry]); 22] = [
    ((1, 6), &[
        Entry(concat!("Pause time|1 min|", txtc!("# on home pauses watch this long."))),
        Entry(concat!("Direction|Up|", txtc!("Scan up or down."))),
        Entry(concat!("Scan inputs|Off|", txtc!("Also a channel's TX frequency."))),
        Entry(concat!("Return to|Start|", txtc!("Where a stopped scan returns."))),
    ]),
    ((2, 6), &[
        Entry(concat!("Step|100 kHz|", txtc!("Tuning step."))),
        Entry(concat!("AGC|Auto|", txtc!("Auto, or a fixed gain: 1 the most, 10 the least."))),
        Entry(concat!("Radio monitor|On|", txtc!("A slot opening interrupts the broadcast."))),
        Entry(concat!("Sleep timer|Off|", txtc!("End the broadcast after a time."))),
    ]),
    ((3, 1), &[
        Entry(concat!("Bars|60|", txtc!("Bars across; span is bars x step."))),
        Entry(concat!("Step|25 kHz|", txtc!("Distance between bars."))),
        Entry(concat!("Sweep speed|1 ms|", txtc!("Time on each point."))),
        Entry(concat!("Trigger|Off|", txtc!("Over the line: monitor or log."))),
        Entry(concat!("Waterfall row|Off|", txtc!("History under the scope."))),
        Entry(concat!("Open band scope||", txtc!("Sweep around the active slot. Home: hold 1."))),
    ]),
    ((4, 2), &[
        Entry(concat!("Decode|Off|", txtc!("Pagers on a slot tuned to the frequency."))),
        Entry(concat!("Frequency|439.9875|", txtc!("The pager channel, any baud; OK types it."))),
        Entry(concat!("Capcode filter|None|", txtc!("Only this capcode: OK types it, # clears it."))),
    ]),
    ((4, 5), &[
        Entry(concat!("SAME|On|", txtc!("Decode weather alerts."))),
        Entry(concat!("Location codes|None|", txtc!("Up to 8 FIPS or CLC codes."))),
    ]),
    ((5, 5), &[
        Entry(concat!("SSID|7 Handheld|", txtc!("0-15 after your callsign."))),
        Entry(concat!("Symbol|Person|", txtc!("Your map symbol."))),
        Entry(concat!("Comment|Empty|", txtc!("Up to 40 characters."))),
        Entry(concat!("Position source|GPS|", txtc!("GPS, or always the fixed position."))),
        Entry(concat!("Fixed position|None|", txtc!("Used without GPS."))),
        Entry(concat!("Ambiguity|Exact|", txtc!("Digits dropped from your position."))),
        Entry(concat!("Mic-E|Off|", txtc!("Short beacons with a status."))),
    ]),
    ((5, 6), &[
        Entry(concat!("Mode|Off|", txtc!("Manual, timed or smart."))),
        Entry(concat!("After voice TX|Off|", txtc!("Beacon after you talk."))),
        Entry(concat!("Power|Low|", txtc!("Beacon and message power."))),
        Entry(concat!("Telemetry|Off|", txtc!("Battery, temperature, power in the comment."))),
        Entry(concat!("Text rate|All|", txtc!("How often timed beacons carry your beacon text."))),
    ]),
    ((5, 7), &[
        Entry(concat!("Digipeat|Off|", txtc!("Relay others' packets."))),
        Entry(concat!("Aliases|WIDE1-1|", txtc!("WIDE1-1, and your call if chosen."))),
        Entry(concat!("Wait|0 s|", txtc!("Delay before relaying, 0-5 s."))),
    ]),
    ((5, 9), &[
        Entry(concat!("Path|WIDE1-1,WIDE2-1|", txtc!("The digipeater path; most use WIDE1-1,WIDE2-1."))),
        Entry(concat!("Custom path|None|", txtc!("Your own path, like WIDE2-2: OK types it."))),
        Entry(concat!("TX delay|300 ms|", txtc!("Carrier before data."))),
        Entry(concat!("TX level|Mid|", txtc!("Modulation level."))),
        Entry(concat!("FX.25 on TX|16 bytes|", txtc!("Error correction on what you send."))),
        Entry(concat!("Mute speaker|On|", txtc!("Mute packets on the speaker."))),
        Entry(concat!("Shared VHF|APRS first|", txtc!("When A or B shares C's VHF chip: APRS first gives C long turns and A and B short looks; A·B first gives A and B their usual turns, and C misses more packets."))),
        Entry(concat!("Decode log|Off|", txtc!("Packets heard but not decoded (bad checksum) go to the event log, with the sender's best guess. For finding missed packets."))),
        Entry(concat!("Auto reply|Off|", txtc!("Answer messages while you're away: who, after how long, and what. OK opens it."))),
    ]),
    ((7, 3), &[
        Entry(concat!("S-meter|Bars|", txtc!("Bars or dBm."))),
        Entry(concat!("Power-on text|Callsign|", txtc!("Off, callsign or text."))),
        Entry(concat!("Boot screen|Logo|", txtc!("Logo, or the owner card."))),
    ]),
    ((7, 4), &[
        Entry(concat!("Timeout|1 min|", txtc!("Backlight off after this."))),
        Entry(concat!("Brightness|7|", txtc!("1-10; night red keeps its own."))),
        Entry(concat!("On transmit|On|", txtc!("Light up while transmitting."))),
        Entry(concat!("Wake on|Keys and RX|", txtc!("What wakes the screen."))),
        Entry(concat!("Key light|With screen|", txtc!("Off, with the screen, on key press, always."))),
    ]),
    ((7, 5), &[
        Entry(concat!("Key beep|On|", txtc!("Beep on key presses."))),
        Entry(concat!("Beep volume|3|", txtc!("Low, mid or high."))),
        Entry(concat!("Alert tones|On|", txtc!("Tones for alerts."))),
        Entry(concat!("Arrival tone|On|", txtc!("A tone on reaching a waypoint."))),
        Entry(concat!("Message sound|Packet|", txtc!("Sound for a message to you."))),
        Entry(concat!("Message volume|Max|", txtc!("How loud the message sound is."))),
    ]),
    ((7, 7), &[
        Entry(concat!("Battery Save|Level 1|", txtc!("Receiver sleep: longer life, slower."))),
        Entry(concat!("Low Power Mode|Off|", txtc!("One VFO, everything else off. SIDE1+SIDE2 hold: leave."))),
        Entry(concat!("Auto below|20 %|", txtc!("Auto turns it on below this battery."))),
    ]),
    ((8, 2), &[
        Entry(concat!("VOX|Off|", txtc!("Transmit when you talk."))),
        Entry(concat!("Level|5|", txtc!("Higher needs a louder voice: 1 keys easiest."))),
        Entry(concat!("Tail|2 s|", txtc!("Hang time after speech."))),
    ]),
    ((8, 5), &[
        Entry(concat!("Dialer||", txtc!("Type and send DTMF."))),
        Entry(concat!("Memories||", txtc!("16 stored sequences."))),
        Entry(concat!("Speed|100 ms|", txtc!("Each tone and each gap."))),
        Entry(concat!("Sidetone|On|", txtc!("Hear the digits you send."))),
        Entry(concat!("Start delay|300 ms|", txtc!("Wait after key-up so a repeater opens."))),
    ]),
    ((8, 6), &[
        Entry(concat!("Callsign|Empty|", txtc!("For APRS, CW ID, power-on message."))),
        Entry(concat!("CW ID|Off|", txtc!("Send your callsign in CW."))),
    ]),
    ((8, 8), &[
        Entry(concat!("Subtone level|Normal|", txtc!("CTCSS/DCS strength: raise for a deaf repeater."))),
        Entry(concat!("Tail removal|Off|", txtc!("Receive, transmit or both."))),
        Entry(concat!("Phase|55 Hz|", txtc!("55 Hz or a phase shift."))),
        Entry(concat!("Extend TX|Off|", txtc!("Transmit beyond the amateur bands."))),
        Entry(concat!("Dry TX|Off|", txtc!("Key with no RF out: bench tests."))),
    ]),
    ((9, 1), &[
        Entry(concat!("Auto lock|Off|", txtc!("Lock the keys after a time."))),
        Entry(concat!("Key test||", txtc!("Each key's code, name and press."))),
        Entry(concat!("Shortcuts||", txtc!("What holding each digit on home does."))),
    ]),
    ((9, 2), &[
        Entry(concat!("Bluetooth|Off|", txtc!("The Bluetooth link to the web app."))),
        Entry(concat!("Link status||", txtc!("Cable and Bluetooth now."))),
        Entry(concat!("Cable port|Auto|", txtc!("Auto, link or terminal; KISS is 0 5 8."))),
    ]),
    ((9, 3), &[
        Entry(concat!("PIN lock|Off|", txtc!("Ask for the PIN at power on, or unlock too."))),
        Entry(concat!("Channel-only mode|Off|", txtc!("Hide VFO and editing."))),
        Entry(concat!("Change PIN|None|", txtc!("Type a new PIN; the old one first."))),
    ]),
    ((9, 7), &[
        Entry(concat!("Run setup again||", txtc!("The first-boot questions."))),
        Entry(concat!("Reset||", txtc!("Everything but calibration."))),
    ]),
    ((9, 9), &[
        Entry(concat!("Advanced squelch||", txtc!("Thresholds per band."))),
        Entry(concat!("Calibration||", txtc!("The factory calibration, read only."))),
        Entry(concat!("Terminal colour|Off|", txtc!("ANSI colour on the cable terminal."))),
        Entry(concat!("MCU clock|Dynamic|", txtc!("Fast only while needed, or always."))),
        // TERM-28: the cable log is in bench builds only; last, so the items above keep their numbers.
        #[cfg(feature = "bench")]
        Entry(concat!("Cable log|Off|", txtc!("Plain log on the cable at power on."))),
    ]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Group {
    /// MENU 0 g, and m for a third-level list (0: the group itself).
    pub g: u8,
    pub sub: u8,
    pub row: u8,
}

impl Group {
    pub fn items(self) -> &'static [Entry] {
        items(self.g, self.sub)
    }

    /// The highlighted item's path after MENU 0: g, m and (in a list) the item.
    fn path(self, row: u8) -> ([u8; 3], usize) {
        if self.sub == 0 { ([self.g, row + 1, 0], 2) } else { ([self.g, self.sub, row + 1], 3) }
    }

    fn set_row(self, app: &mut App, row: u8) {
        app.screen = Screen::Group(Group { row, ..self });
        if let Some(i) = memo(self.g, self.sub) {
            let b = &mut app.group_rows[i / 2];
            *b = if i % 2 == 0 { *b & 0xF0 | row & 0x0F } else { *b & 0x0F | row << 4 }; // MENU-24
        }
    }

    /// MENU-24: a group (or third-level list) opens on its last item.
    pub(crate) fn reopen(app: &App, g: u8, sub: u8) -> Self {
        let row = memo(g, sub).map_or(0, |i| app.group_rows[i / 2] >> (i % 2 * 4) & 0x0F);
        let n = items(g, sub).len() as u8;
        Group { g, sub, row: if row < n { row } else { 0 } }
    }
}

/// MENU-24: a group (or third-level list) just opened by plain menu navigation (MENU 0's digit or OK, a group's
/// row) goes to its remembered row; one opened to a setting (the Status page, a shortcut) keeps that setting.
pub(crate) fn restore_row(app: &mut App) {
    if let Screen::Group(g) = app.screen {
        app.screen = Screen::Group(Group::reopen(app, g.g, g.sub));
    }
}

/// The nine groups and the third-level lists: where each keeps its last item (MENU-24).
pub(crate) const MEMO_BYTES: usize = (9 + SUBS.len()).div_ceil(2);

/// A group's (g 1-9, sub 0) or third-level list's place in [`App`]'s `group_rows`, in nibbles.
pub(crate) fn memo(g: u8, sub: u8) -> Option<usize> {
    match sub {
        0 => (1..=9).contains(&g).then(|| g as usize - 1),
        _ => SUBS.iter().position(|(p, _)| *p == (g, sub)).map(|i| 9 + i),
    }
}

/// The menu item at path `p` (MENU 0 g i, or MENU 0 g s i): its name, its shown value and its help line.
pub(crate) fn entry_of(p: &[u8]) -> Option<Entry> {
    match *p {
        [g, i] => items(g, 0).get((i as usize).checked_sub(1)?).copied(),
        [g, s, i] => items(g, s).get((i as usize).checked_sub(1)?).copied(),
        _ => None,
    }
}

pub(crate) fn items(g: u8, sub: u8) -> &'static [Entry] {
    if sub == 0 {
        return GROUP_ITEMS[(g as usize).clamp(1, 9) - 1];
    }
    SUBS.iter().find(|(p, _)| *p == (g, sub)).map_or(&[], |(_, l)| l)
}

/// Why an item can't be used here, if it can't (MENU-14).
pub(crate) fn greyed(app: &App, p: &[u8]) -> Option<&'static str> {
    match p {
        [4, 5, ..] if !app.set.wx_region() => Some("US/CA only"), // WX-3
        [5, 3] | [5, 6, 1] if app.set.kiss_on() => app.kiss_holds_aprs(), // KISS-10
        [8, 8, 5] if !app.radio.tx_built => Some("not in this build"), // TX-48
        [8, 8, 5] if app.remote_in => Some("on the radio only"), // TX-48: never the link's (LINK-19)
        _ => None,
    }
}

/// What the Radio task is told after a setting changes (the knob, #, the terminal).
#[derive(Clone, Copy)]
enum Tell {
    Nothing,
    ScanTx,
    BeepVol,
    Vox,
    MicGain,
    TxTimeout,
    TxTones,
    SlotTones,
    BusyDefault,
    MsgVol,
    AprsMute,
    /// BCR-32, HF-35: the SI4732's receive settings.
    SiOpts,
}

/// How a setting in [`SETTINGS`] shows, steps and resets.
#[derive(Clone, Copy)]
enum Kind {
    /// Two values (Off/On, or the two names given): any knob step flips it.
    Flip(&'static [&'static str; 2]),
    /// One of a list's values, round and round.
    Pick(&'static [&'static str]),
    /// 0 to n - 1, round and round, shown by the function.
    Round(u8, fn(u8, &mut Buf<24>)),
    /// lo to hi in steps, stopping at the ends, shown by the function.
    Level(u8, u8, u8, fn(u8, &mut Buf<24>)),
    /// Shown, stepped and reset by name in [`wired`], [`change`] and [`reset`] (more than one field, a typed
    /// value, a store of its own, or a step of its own).
    Special,
}

/// One setting: its menu path (a 0 ends a two-number path), whether the terminal's `set` and `dump` take it
/// (TERM-12, TERM-13), how it behaves, the `Settings` field it reads and writes as a number (a bool is 0 or 1),
/// and what the Radio task is told after a change. Its default is the field's value in `Settings::DEFAULT`.
#[derive(Clone, Copy)]
pub(crate) struct Setting {
    path: [u8; 3],
    term: bool,
    kind: Kind,
    get: fn(&Settings) -> u8,
    set: fn(&mut Settings, u8),
    tell: Tell,
}

impl Setting {
    fn path(&self) -> &[u8] {
        &self.path[..if self.path[2] == 0 { 2 } else { 3 }]
    }
}

/// A `Setting` for a field: `field!(scan_tx: bool, ...)` or `field!(theme: u8, ...)`.
macro_rules! field {
    ($path:expr, $f:ident: bool, $kind:expr, $tell:ident) => {
        Setting { path: $path, term: true, kind: $kind, get: |s| s.$f as u8, set: |s, v| s.$f = v != 0, tell: Tell::$tell }
    };
    ($path:expr, $f:ident: u8, $kind:expr, $tell:ident) => {
        Setting { path: $path, term: true, kind: $kind, get: |s| s.$f, set: |s, v| s.$f = v, tell: Tell::$tell }
    };
    ($path:expr, special) => {
        Setting { path: $path, term: true, kind: Kind::Special, get: |_| 0, set: |_, _| {}, tell: Tell::Nothing }
    };
}

/// Every value each setting can show, with its menu path (tests: the menus' widths).
#[cfg(test)]
pub(crate) fn all_values() -> std::vec::Vec<([u8; 3], std::vec::Vec<std::string::String>)> {
    let text = |b: &Buf<24>| std::string::String::from_utf8_lossy(b.as_bytes()).into_owned();
    SETTINGS
        .iter()
        .map(|st| {
            let v: std::vec::Vec<std::string::String> = match st.kind {
                Kind::Flip(l) => l.iter().map(|x| std::string::String::from(*x)).collect(),
                Kind::Pick(l) => l.iter().map(|x| std::string::String::from(*x)).collect(),
                Kind::Round(n, f) => (0..n).map(|i| { let mut b = Buf::new(); f(i, &mut b); text(&b) }).collect(),
                Kind::Level(lo, hi, step, f) => (lo..=hi).step_by(step.max(1) as usize).map(|i| { let mut b = Buf::new(); f(i, &mut b); text(&b) }).collect(),
                _ => std::vec::Vec::new(),
            };
            (st.path, v)
        })
        .collect()
}

/// Every setting the menu changes, in menu order: what the terminal's `set` takes and `dump` writes (less those
/// that change only on the radio: PIN, region, Extend TX), and for most of them all the menu needs. A new
/// setting is one row here (and its field, default and place in `persist.rs`).
#[cfg_attr(target_os = "none", link_section = ".slow_rodata")]
static SETTINGS: [Setting; 98 + cfg!(feature = "bench") as usize] = [
    field!([1, 1, 0], special),
    field!([1, 2, 0], special),
    field!([1, 3, 0], scan_prio: u8, Kind::Round(6, prio_text), Nothing),
    field!([1, 6, 1], pause_time: u8, Kind::Pick(&crate::settings::PAUSE_TIME_NAMES), Nothing),
    field!([1, 6, 2], scan_down: bool, Kind::Flip(&["Up", "Down"]), Nothing),
    field!([1, 6, 3], scan_tx: bool, Kind::Flip(&ON_OFF), ScanTx),
    field!([1, 6, 4], scan_return_hit: bool, Kind::Flip(&["Start", "Last hit"]), Nothing),
    field!([2, 4, 0], ch_display: u8, Kind::Pick(&crate::settings::CHANNEL_DISPLAY), Nothing),
    field!([2, 5, 0], local: u8, Kind::Pick(&crate::local::RANGES), Nothing), // CHAN-34
    field!([2, 6, 1], fm_step: u8, Kind::Round(FM_STEPS.len() as u8, fm_step_text), SiOpts),
    field!([2, 6, 2], fm_agc: u8, Kind::Round(crate::settings::AGC_MAX + 1, agc_text), SiOpts),
    field!([2, 6, 3], fm_monitor: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([2, 6, 4], fm_sleep: u8, Kind::Round(SLEEP_TIMES.len() as u8, sleep_text), Nothing),
    field!([3, 1, 1], scope_bars: u8, Kind::Round(SCOPE_BAR_COUNTS.len() as u8, bars_text), Nothing),
    field!([3, 1, 2], scope_step: u8, Kind::Round(crate::slot::STEPS.len() as u8, scope_step_text), Nothing),
    field!([3, 1, 3], special),
    field!([3, 1, 4], scope_trig: u8, Kind::Pick(&SCOPE_TRIGGERS), Nothing),
    field!([3, 1, 5], scope_wf: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([4, 2, 1], pocsag_on: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([4, 2, 2], special),
    field!([4, 2, 3], special),
    field!([4, 3, 0], dtmf_rx: bool, Kind::Flip(&ON_OFF), Nothing), // DEC-6
    field!([4, 4, 0], popups: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([4, 5, 1], same: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([5, 3, 0], special),
    field!([5, 4, 0], special), // MENU-31: typed as a frequency
    field!([5, 5, 1], special), // APRS-30
    field!([5, 5, 2], special), // APRS-47: the symbol
    field!([5, 5, 3], special), // APRS-50: the comment
    field!([5, 5, 4], special), // APRS-31
    field!([5, 5, 6], special), // APRS-51: ambiguity
    field!([5, 5, 7], special), // APRS-52: Mic-E
    field!([5, 6, 1], special),
    field!([5, 6, 2], special),
    field!([5, 6, 3], special),
    field!([5, 6, 4], special), // APRS-54: telemetry
    field!([5, 6, 5], special), // APRS-50.1: the beacon text's rate
    field!([5, 7, 1], special),
    field!([5, 7, 2], special),
    field!([5, 7, 3], special),
    field!([5, 8, 0], kiss_port: u8, Kind::Pick(&crate::settings::KISS_PORTS), Nothing),
    field!([5, 9, 1], special),
    field!([5, 9, 2], special),
    field!([5, 9, 3], special), // APRS-48: TX delay
    field!([5, 9, 4], special), // APRS-49: TX level
    field!([5, 9, 5], special), // APRS-45
    field!([5, 9, 6], aprs_mute: bool, Kind::Flip(&ON_OFF), AprsMute), // APRS-2
    field!([5, 9, 7], aprs_share: u8, Kind::Pick(&crate::settings::APRS_SHARE), Nothing), // WATCH-17.3
    field!([5, 9, 8], decode_log: bool, Kind::Flip(&ON_OFF), Nothing), // APRS-56
    field!([5, 9, 9], special), // APRS-57: auto reply's page
    // GPS-1: Off, Auto, On in menu order (stored 0, 2, 1: the old on/off byte reads as it was, GPS-1.4)
    Setting {
        path: [6, 1, 0],
        term: true,
        kind: Kind::Pick(&crate::settings::GPS_MODES),
        get: |s| crate::settings::GPS_ORDER.iter().position(|&m| m == s.gps).unwrap_or(1) as u8,
        set: |s, v| s.gps = crate::settings::GPS_ORDER[v as usize % 3],
        tell: Tell::Nothing,
    },
    field!([6, 3, 0], coord_fmt: u8, Kind::Pick(&COORD_FORMATS), Nothing),
    field!([6, 4, 0], units: u8, Kind::Pick(&UNITS), Nothing),
    field!([6, 5, 0], tz: u8, Kind::Round(TIME_ZONES.len() as u8, zone_text), Nothing),
    field!([6, 6, 0], special),
    field!([6, 8, 0], gps_rxtx: u8, Kind::Pick(&GPS_RXTX), Nothing),
    field!([7, 1, 0], theme: u8, Kind::Pick(&THEMES), Nothing),
    field!([7, 2, 0], layout: u8, Kind::Pick(&LAYOUTS), Nothing),
    field!([7, 3, 1], smeter_dbm: bool, Kind::Flip(&["Bars", "dBm"]), Nothing),
    field!([7, 3, 2], pon: u8, Kind::Pick(&POWER_ON), Nothing),
    field!([7, 3, 3], boot_card: bool, Kind::Flip(&BOOT_SCREENS), Nothing),
    field!([7, 4, 1], backlight: u8, Kind::Round(BACKLIGHT_TIMES.len() as u8, backlight_text), Nothing),
    field!([7, 4, 2], special),
    field!([7, 4, 3], tx_light: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([7, 4, 4], wake_on: u8, Kind::Pick(&WAKE_ON), Nothing),
    field!([7, 4, 5], keypad_light: u8, Kind::Pick(&KEYPAD_LIGHTS), Nothing),
    field!([7, 5, 1], key_beep: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([7, 5, 2], beep_vol: u8, Kind::Level(1, 3, 1, beep_vol_text), BeepVol),
    field!([7, 5, 3], alert_tones: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([7, 5, 4], arrival_tone: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([7, 5, 5], special), // APRS-6.1
    field!([7, 5, 6], msg_vol: u8, Kind::Pick(&ferrite_core::tonegen::CHIME_VOLUMES), MsgVol),
    field!([7, 6, 0], leds: u8, Kind::Pick(&LED_MODES), Nothing),
    field!([7, 7, 1], special),
    field!([7, 7, 2], special), // PWR-24: it acts (in or out) as it changes
    field!([7, 7, 3], lpm_below: u8, Kind::Level(10, 40, 10, pct_text), Nothing),
    field!([7, 8, 0], auto_off: u8, Kind::Round(AUTO_OFF_TIMES.len() as u8, auto_off_text), Nothing),
    field!([7, 9, 0], menu_timeout: u8, Kind::Pick(&crate::settings::MENU_TIMEOUT_NAMES), Nothing),
    field!([8, 1, 0], tx_timeout: u8, Kind::Level(1, 20, 1, tx_timeout_text), TxTimeout),
    field!([8, 2, 1], vox: bool, Kind::Flip(&ON_OFF), Vox),
    field!([8, 2, 2], vox_level: u8, Kind::Level(1, 9, 1, vox_level_text), Vox),
    field!([8, 2, 3], vox_tail: u8, Kind::Round(VOX_TAILS.len() as u8, vox_tail_text), Vox),
    field!([8, 3, 0], mic_gain: u8, Kind::Level(1, 9, 1, vox_level_text), MicGain),
    field!([8, 4, 0], end_tone: u8, Kind::Pick(&END_TONE_NAMES), SlotTones),
    // 50-200 ms in 10 ms steps (DTMF-3)
    field!([8, 5, 3], dtmf_ms: u8, Kind::Level(50, 200, 10, dtmf_ms_text), TxTones),
    field!([8, 5, 4], dtmf_sidetone: bool, Kind::Flip(&ON_OFF), TxTones),
    // 0-1000 ms in 50 ms steps (DTMF-3)
    field!([8, 5, 5], dtmf_delay: u8, Kind::Level(0, 100, 5, dtmf_delay_text), TxTones),
    field!([8, 6, 2], cw_id: u8, Kind::Pick(&CW_ID_MODES), TxTones),
    field!([8, 7, 0], busy_default: bool, Kind::Flip(&ON_OFF), BusyDefault),
    field!([8, 8, 1], special),
    field!([8, 8, 2], tail_mode: u8, Kind::Pick(&TAIL_MODES), SlotTones),
    field!([8, 8, 3], tail_phase: u8, Kind::Pick(&TAIL_PHASES), TxTones),
    field!([9, 1, 1], auto_lock: u8, Kind::Round(AUTO_LOCK_TIMES.len() as u8, auto_lock_text), Nothing), // KEY-20
    field!([9, 2, 1], bt: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([9, 2, 3], cable_port: u8, Kind::Pick(&CABLE_PORTS), Nothing),
    field!([9, 5, 0], sq_chip: bool, Kind::Flip(&["Software", "Chip"]), Nothing), // SQL-18.3
    field!([9, 9, 3], term_colour: bool, Kind::Flip(&ON_OFF), Nothing),
    field!([9, 9, 4], mcu_fixed: bool, Kind::Flip(&["Dynamic", "Fixed"]), Nothing), // PWR-26.5
    #[cfg(feature = "bench")]
    field!([9, 9, 5], cable_log: bool, Kind::Flip(&ON_OFF), Nothing), // TERM-28: bench builds only
];

fn row(p: &[u8]) -> Option<&'static Setting> {
    SETTINGS.iter().find(|r| r.path() == p)
}

/// The settings the terminal's `set` takes and `dump` writes, in menu order (TERM-12, TERM-13).
pub(crate) fn settable() -> impl Iterator<Item = &'static [u8]> + Clone {
    SETTINGS.iter().filter(|r| r.term).map(|r| r.path())
}

/// VOX-10: what the knob shows (and the terminal and link say) when CW ID is turned on with no callsign.
pub(crate) const CW_ID_NO_CALL: &str = "set a callsign first";

/// Why the knob won't step this setting now, in the radio's own words (its toast), when it says so; the terminal's
/// `set` (and so the link's `CMD`) gives the same reason instead of a bare "one of".
pub(crate) fn refusal(app: &App, p: &[u8]) -> Option<&'static str> {
    match p {
        [8, 6, 2] if app.set.callsign.as_bytes().is_empty() => Some(CW_ID_NO_CALL), // VOX-10
        _ => None,
    }
}

/// Whether the terminal's `set` takes this path.
pub(crate) fn is_settable(p: &[u8]) -> bool {
    row(p).is_some_and(|r| r.term)
}

fn tell(app: &mut App, t: Tell) {
    match t {
        Tell::Nothing => {}
        Tell::ScanTx => app.out.push(RadioCmd::ScanTx(app.set.scan_tx)),
        Tell::BeepVol => app.out.push(RadioCmd::BeepVol(app.set.beep_vol)),
        Tell::Vox => app.send_vox(),
        Tell::MicGain => app.out.push(RadioCmd::MicGain(app.set.mic_gain.clamp(1, 9))),
        Tell::TxTimeout => app.out.push(RadioCmd::TxTimeout(app.set.tx_timeout_s() as u16)),
        Tell::TxTones => app.send_tx_tones(),
        Tell::SlotTones => app.send_slot_tones(),
        Tell::BusyDefault => app.set_busy_default(),
        // APRS-6.2: the sound at its new volume, to hear it
        Tell::AprsMute => app.aprs_mute_sync(),
        Tell::SiOpts => app.send_si_opts(),
        Tell::MsgVol => app.out.push(RadioCmd::Chime { sound: app.aprs_cfg().sound.max(1), level: app.set.msg_level() }),
    }
}

// The texts of the Round and Level settings.
fn prio_text(v: u8, b: &mut Buf<24>) {
    if v == 0 {
        b.set(b"Off");
    } else {
        let _ = uwrite!(b, "Every {} s", v);
    }
}

fn fm_step_text(v: u8, b: &mut Buf<24>) {
    b.set(FM_STEPS[v as usize % 3].1.as_bytes());
}

fn sleep_text(v: u8, b: &mut Buf<24>) {
    match SLEEP_TIMES[v as usize % SLEEP_TIMES.len()] {
        0 => b.set(b"Off"),
        m => {
            let _ = uwrite!(b, "{} min", m);
        }
    }
}

fn bars_text(v: u8, b: &mut Buf<24>) {
    let _ = uwrite!(b, "{}", SCOPE_BAR_COUNTS[v as usize % 4]);
}

fn scope_step_text(v: u8, b: &mut Buf<24>) {
    b.set(crate::slot::STEPS[v as usize % crate::slot::STEPS.len()].1.as_bytes());
}

fn zone_text(v: u8, b: &mut Buf<24>) {
    let mut t = Buf::<16>::new();
    tz_text(TIME_ZONES[v as usize % TIME_ZONES.len()], &mut t);
    b.set(t.as_bytes());
}

fn pct_text(v: u8, b: &mut Buf<24>) {
    let _ = uwrite!(b, "{} %", v);
}

fn backlight_text(v: u8, b: &mut Buf<24>) {
    secs(b, BACKLIGHT_TIMES[v as usize % BACKLIGHT_TIMES.len()] as u32, "Never");
}

fn auto_off_text(v: u8, b: &mut Buf<24>) {
    secs(b, AUTO_OFF_TIMES[v as usize % AUTO_OFF_TIMES.len()] as u32 * 60, "Off");
}

fn auto_lock_text(v: u8, b: &mut Buf<24>) {
    secs(b, AUTO_LOCK_TIMES[v as usize % AUTO_LOCK_TIMES.len()] as u32, "Off");
}

fn vox_tail_text(v: u8, b: &mut Buf<24>) {
    let t = VOX_TAILS[v as usize % VOX_TAILS.len()];
    if t.is_multiple_of(10) {
        let _ = uwrite!(b, "{} s", t / 10);
    } else {
        let _ = uwrite!(b, "{}.{} s", t / 10, t % 10);
    }
}

fn beep_vol_text(v: u8, b: &mut Buf<24>) {
    b.set(BEEP_VOLS[(v.clamp(1, 3) - 1) as usize % BEEP_VOLS.len()].as_bytes());
}

fn vox_level_text(v: u8, b: &mut Buf<24>) {
    let _ = uwrite!(b, "{}", v.clamp(1, 9));
}

fn tx_timeout_text(v: u8, b: &mut Buf<24>) {
    let _ = uwrite!(b, "{} s", v.clamp(1, 20) as u16 * 30);
}

fn dtmf_ms_text(v: u8, b: &mut Buf<24>) {
    let _ = uwrite!(b, "{} ms", v.clamp(50, 200));
}

fn dtmf_delay_text(v: u8, b: &mut Buf<24>) {
    let _ = uwrite!(b, "{} ms", v.min(100) as u32 * 10);
}

/// The live value of a wired setting, and whether it differs from its default (MENU-12): the special cases by
/// name, the rest from [`SETTINGS`].
pub(crate) fn wired(app: &App, dev: &Device, p: &[u8], b: &mut Buf<24>) -> Option<bool> {
    let s = &app.set;
    let pick = |b: &mut Buf<24>, l: &[&str], i: u8| b.set(l[i as usize % l.len()].as_bytes());
    let d = match p {
        [1, 1] => {
            pick(b, &WATCH_TEXT, app.watch);
            app.watch != 1
        }
        [1, 2] => {
            resume_text(b, s.scan_resume);
            s.scan_resume != Settings::DEFAULT.scan_resume
        }
        // APRS-31: the beacon's position source; 0 5 5 5 is 0 6 9's fixed position
        [5, 5, 4] => {
            b.set(if s.aprs_fixed { b"Fixed" } else { b"GPS" });
            s.aprs_fixed
        }
        [6, 9] | [5, 5, 5] => {
            if s.fixed_on {
                b.set(&super::lists::grid(s.fixed));
            } else {
                b.set(b"None");
            }
            s.fixed_on
        }
        [4, 5, 2] => {
            let n = s.same_codes.iter().filter(|&&c| c != 0).count();
            if n == 0 {
                b.set(b"None");
            } else {
                let _ = ufmt::uwrite!(b, "{} code{}", n, if n == 1 { "" } else { "s" });
            }
            n != 0
        }
        [6, 6] => {
            pick(b, &CLOCKS, s.clock);
            if s.clock_gps {
                b.push(b", GPS");
            }
            s.clock != 0 || !s.clock_gps
        }
        [7, 3, 2] => {
            pick(b, &POWER_ON, s.pon);
            if s.pon == 2 && !s.pon_text.is_empty() {
                b.set(s.pon_text.as_bytes());
            }
            s.pon != 1
        }
        [7, 7] | [7, 7, 1] => {
            let mut t = Buf::<16>::new();
            app.item_text(Item::BatterySave, true, &mut t);
            b.set(t.as_bytes());
            app.bsave != 1
        }
        [7, 7, 2] => {
            pick(b, &LPM_MODES, s.lpm);
            if s.lpm == 2 && app.low_power() {
                b.push(b": on");
            }
            s.lpm != crate::settings::Settings::DEFAULT.lpm // Auto (2) since Oct 21, 2026: it showed as changed
        }
        [8, 6] | [8, 6, 1] => {
            b.set(if s.callsign.is_empty() { b"Empty".as_slice() } else { s.callsign.as_bytes() });
            !s.callsign.is_empty()
        }
        [8, 8, 4] => {
            pick(b, &ON_OFF, s.extend_tx as u8);
            s.extend_tx
        }
        [8, 8, 5] => {
            pick(b, &ON_OFF, app.dry_tx() as u8);
            app.dry_tx()
        }
        [9, 4] => {
            b.set(s.region_name().as_bytes());
            false
        }
        [9, 6] => {
            b.set(if dev.selftest.ssb_patch { b"Installed".as_slice() } else { b"Not installed" });
            false
        }
        [9, 8] => {
            b.set(VERSION.as_bytes());
            false
        }
        [3, 1, 3] => {
            let d = s.scope_dwell.clamp(1, 100);
            let _ = uwrite!(b, "{}.{} ms", d / 10, d % 10);
            s.scope_dwell != 100
        }
        [5, 3] => {
            pick(b, &ON_OFF, s.aprs_mode as u8);
            s.aprs_mode
        }
        [5, 6, 1] => {
            app.beacon_mode_text(b);
            app.aprs_cfg().beacon != 0
        }
        [5, 5, 1] => {
            // APRS-30: the number and its conventional name
            let n = app.aprs_cfg().ssid;
            let _ = uwrite!(b, "{} ", n);
            b.push(crate::aprs::SSID_NAMES[n as usize % 16].as_bytes());
            n != crate::aprs::SSID_DEFAULT
        }
        [5, 6, 3] => {
            pick(b, &crate::aprs::POWERS, app.aprs_cfg().power);
            app.aprs_cfg().power != 0
        }
        [5, 6, 2] => {
            pick(b, &ON_OFF, app.aprs_cfg().after_voice as u8);
            app.aprs_cfg().after_voice
        }
        [5, 6, 4] => {
            pick(b, &ON_OFF, app.aprs_cfg().telemetry as u8);
            app.aprs_cfg().telemetry
        }
        [5, 6, 5] => {
            pick(b, &crate::aprs::TEXT_EVERY, app.aprs_cfg().text_every);
            app.aprs_cfg().text_every != 0
        }
        // The group rows show their setting too (APRS-26).
        [5, 9, 1] => {
            b.set(app.aprs_cfg().path_name());
            app.aprs_cfg().path != crate::aprs::PATH_DEFAULT
        }
        [5, 9, 2] => {
            let c = app.aprs_cfg();
            b.set(if c.custom_text().is_empty() { b"None".as_slice() } else { c.custom_text() });
            !c.custom_text().is_empty()
        }
        // APRS-57: on or off; its settings are on its page
        [5, 9, 9] => {
            let on = app.aprs_cfg().reply_on();
            b.set(if on { b"On" } else { b"Off" });
            on
        }
        [5, 7] | [5, 7, 1] => {
            pick(b, &ON_OFF, app.aprs_cfg().digi_on() as u8);
            app.aprs_cfg().digi_on()
        }
        [5, 7, 2] => {
            pick(b, &crate::aprs::DIGI_ALIASES, app.aprs_cfg().digi_mycall() as u8);
            app.aprs_cfg().digi_mycall()
        }
        [5, 7, 3] => {
            let _ = uwrite!(b, "{} s", app.aprs_cfg().digi_wait_s());
            app.aprs_cfg().digi_wait_s() != 0
        }
        [5, 4] => {
            b.set(crate::slot::mhz3(app.aprs_freq()).as_bytes());
            app.aprs_cfg().f5k != 0
        }
        [7, 5, 5] => {
            pick(b, &ferrite_core::tonegen::CHIME_NAMES, app.aprs_cfg().sound);
            app.aprs_cfg().sound != ferrite_core::tonegen::CHIME_DEFAULT
        }
        [5, 5, 2] => {
            pick(b, &crate::aprs::SYMBOLS.map(|s| s.0), app.aprs_cfg().symbol);
            app.aprs_cfg().symbol != 0
        }
        // APRS-50: the comment, its start when long
        [5, 5, 3] => {
            let c = app.aprs_cfg();
            let t = c.comment_text();
            match t.len() {
                0 => b.set(b"Empty"),
                1..=18 => b.set(t),
                _ => {
                    b.set(&t[..15]);
                    b.push(b"...");
                }
            }
            !t.is_empty()
        }
        [5, 5, 6] => {
            pick(b, &crate::aprs::AMBIGUITY_NAMES, app.aprs_cfg().ambiguity);
            app.aprs_cfg().ambiguity != 0
        }
        [5, 5, 7] => {
            pick(b, &crate::aprs::MIC_E_NAMES, app.aprs_cfg().mic_e);
            app.aprs_cfg().mic_e != 0
        }
        [5, 9, 3] => {
            let _ = uwrite!(b, "{} ms", app.aprs_cfg().txdelay_10ms() as u16 * 10);
            app.aprs_cfg().txdelay_10ms() != crate::aprs::TXDELAY_DEFAULT
        }
        [5, 9, 4] => {
            pick(b, &crate::aprs::TX_LEVELS, app.aprs_cfg().tx_level);
            app.aprs_cfg().tx_level != 1
        }
        [5, 9, 5] => {
            pick(b, &crate::aprs::FX25_NAMES, app.aprs_cfg().fx25_index());
            app.aprs_cfg().fx25 != crate::aprs::FX25_DEFAULT
        }
        // A group's row shows its list's first setting.
        [7, 4] => return wired(app, dev, &[7, 4, 1], b),
        [8, 2] => return wired(app, dev, &[8, 2, 1], b),
        [7, 4, 2] => {
            let _ = uwrite!(b, "{}", s.brightness());
            let mut d = Settings::DEFAULT;
            d.theme = s.theme;
            s.brightness() != d.brightness()
        }
        [9, 1] => {
            let t = AUTO_LOCK_TIMES[s.auto_lock as usize % AUTO_LOCK_TIMES.len()] as u32;
            if t == 0 {
                b.set(b"Manual lock");
            } else {
                secs(b, t, "Off");
            }
            s.auto_lock != 0
        }
        // 0 4 2 (DEC-4, DEC-18): the row in the group shows the frequency while decoding, else Off.
        [4, 2] => {
            if s.pocsag_on {
                b.set(crate::slot::mhz3(s.pocsag_freq).as_bytes());
            } else {
                b.set(b"Off");
            }
            s.pocsag_on
        }
        [4, 2, 2] => {
            b.set(crate::slot::mhz3(s.pocsag_freq).as_bytes());
            s.pocsag_freq != Settings::DEFAULT.pocsag_freq
        }
        [4, 2, 3] => {
            if s.pocsag_cap == 0 {
                b.set(b"None");
            } else {
                let _ = uwrite!(b, "{}", s.pocsag_cap);
            }
            s.pocsag_cap != 0
        }
        [9, 2] => {
            b.set(if dev.bt { b"BT connected".as_slice() } else if s.bt { b"BT on" } else { b"BT off" });
            s.bt
        }
        [9, 9, 2] => {
            b.set(cal_text(dev.selftest.cal).as_bytes());
            false
        }
        // APRS-30: the address beacons send, callsign and 0 5 5 1's SSID (it said "-9" whatever the SSID)
        [5, 5] if !s.callsign.is_empty() => {
            b.set(app.aprs_call().as_bytes());
            false
        }
        [8, 8, 1] => {
            pick(b, &crate::settings::TONE_LEVELS, s.tone_level);
            s.tone_level != 1
        }
        [9, 3, 2] => {
            pick(b, &ON_OFF, s.chan_only as u8);
            s.chan_only
        }
        [9, 3] | [9, 3, 1] => {
            pick(b, &PIN_MODES, s.pin_mode);
            s.pin_mode != 0
        }
        [9, 3, 3] => {
            b.set(if s.pin_len > 0 { b"Set" } else { b"None" });
            s.pin_len > 0
        }
        _ => {
            let r = row(p)?;
            let v = (r.get)(s);
            match r.kind {
                Kind::Flip(l) => pick(b, l, v),
                Kind::Pick(l) => pick(b, l, v),
                Kind::Round(_, text) | Kind::Level(_, _, _, text) => text(v, b),
                Kind::Special => return None,
            }
            v != (r.get)(&Settings::DEFAULT)
        }
    };
    Some(d)
}

/// The knob or ◀▶ on a wired setting (MENU-16); false if it isn't changeable here. The special cases by name,
/// the rest from [`SETTINGS`].
pub(crate) fn change(app: &mut App, p: &[u8], d: i32) -> bool {
    let s = &mut app.set;
    match p {
        [1, 1] => app.change(Item::Watch, d),
        // 1-10 s, carrier drop, stop on hit (SCAN-5); the next scan takes it
        [1, 2] => s.scan_resume = 1 + cycle(s.scan_resume.clamp(1, 12) - 1, d, 12),
        [8, 8, 1] => {
            s.tone_level = (s.tone_level as i32 + d).clamp(0, 2) as u8;
            app.out.push(RadioCmd::ToneLevel(app.set.tone_level));
        }
        [7, 7] | [7, 7, 1] => app.change(Item::BatterySave, d),
        [7, 7, 2] => {
            // PWR-24.1: Off, Auto, On in turn (stored 0, 2, 1), so turning to Auto never passes On, which enters
            // the mode at once and leaves the menu.
            const ORDER: [u8; 3] = [0, 2, 1];
            let i = ORDER.iter().position(|&m| m == s.lpm).unwrap_or(0) as u8;
            app.set_lpm_mode(ORDER[cycle(i, d, LPM_MODES.len()) as usize]);
        }
        [6, 6] => {
            // 24 h + GPS, 24 h, 12 h + GPS, 12 h
            let v = cycle(s.clock.min(1) * 2 + !s.clock_gps as u8, d, 4);
            (s.clock, s.clock_gps) = (v / 2, v.is_multiple_of(2));
        }
        [7, 1] => {
            if s.theme != crate::settings::NIGHT_RED {
                app.day_theme = s.theme; // HOME-17: the quick menu's PTT B comes back to it from night red
            }
            s.theme = cycle(s.theme, d, THEMES.len());
        }
        [9, 4] => {
            // In the lists' order (Canada first, then by name): the stored values stay as they were.
            s.region = if s.region == 0 {
                crate::settings::region_at(if d > 0 { 0 } else { REGION_LIST.len() as u8 - 1 })
            } else {
                crate::settings::region_at(cycle(crate::settings::region_row(s.region), d, REGION_LIST.len()))
            };
            app.region_start(); // VFO-25: off the US factory frequencies
            app.region_steps(); // VFO-18
        }
        // 0.1 ms steps up to 1 ms, then 1 ms steps to 10 ms (stock's dwell, the default).
        [3, 1, 3] => {
            let v = s.scope_dwell.clamp(1, 100) as i32;
            let step = if v + d > 10 && v >= 10 { 10 } else { 1 };
            s.scope_dwell = (v + d * step).clamp(1, 100) as u8;
        }
        [5, 3] => {
            let on = !app.set.aprs_mode;
            app.set_aprs_mode(on); // APRS-15
        }
        [5, 6, 1] => app.step_beacon(d),
        [5, 5, 1] => app.set_aprs(crate::aprs::AprsCfg { ssid: cycle(app.aprs_cfg().ssid, d, 16), ..app.aprs_cfg() }),
        [5, 5, 4] => s.aprs_fixed = !s.aprs_fixed,
        [5, 6, 3] => app.set_aprs(crate::aprs::AprsCfg { power: cycle(app.aprs_cfg().power, d, 3), ..app.aprs_cfg() }),
        [5, 6, 2] => app.set_aprs(crate::aprs::AprsCfg { after_voice: !app.aprs_cfg().after_voice, ..app.aprs_cfg() }),
        [5, 6, 4] => app.set_aprs(crate::aprs::AprsCfg { telemetry: !app.aprs_cfg().telemetry, ..app.aprs_cfg() }),
        [5, 6, 5] => app.set_aprs(crate::aprs::AprsCfg { text_every: cycle(app.aprs_cfg().text_every, d, 4), ..app.aprs_cfg() }),
        [5, 9, 1] => {
            let c = app.aprs_cfg();
            app.set_aprs(crate::aprs::AprsCfg { path: c.path_step(d), ..c });
        }
        [5, 7, 1] => app.set_aprs(crate::aprs::AprsCfg { digi: app.aprs_cfg().digi ^ crate::aprs::DIGI_ON, ..app.aprs_cfg() }),
        [5, 7, 2] => app.set_aprs(crate::aprs::AprsCfg { digi: app.aprs_cfg().digi ^ crate::aprs::DIGI_MYCALL, ..app.aprs_cfg() }),
        [5, 7, 3] => {
            let c = app.aprs_cfg();
            let w = cycle(c.digi_wait_s(), d, crate::aprs::DIGI_WAIT_MAX as usize + 1);
            app.set_aprs(crate::aprs::AprsCfg { digi: c.digi & 0x0F | w << 4, ..c });
        }
        [5, 4] => app.step_aprs_freq(d),
        [7, 5, 5] => {
            // APRS-6.1: each choice plays as it is picked
            let sound = cycle(app.aprs_cfg().sound, d, ferrite_core::tonegen::CHIMES.len());
            app.set_aprs(crate::aprs::AprsCfg { sound, ..app.aprs_cfg() });
            app.out.push(ferrite_core::msg::RadioCmd::Chime { sound, level: app.set.msg_level() });
        }
        [5, 5, 2] => {
            let symbol = cycle(app.aprs_cfg().symbol, d, crate::aprs::SYMBOLS.len());
            app.set_aprs(crate::aprs::AprsCfg { symbol, ..app.aprs_cfg() })
        }
        // APRS-51: Exact, then 1-4 digits dropped, stopping at the ends
        [5, 5, 6] => {
            let ambiguity = (app.aprs_cfg().ambiguity as i32 + d).clamp(0, 4) as u8;
            app.set_aprs(crate::aprs::AprsCfg { ambiguity, ..app.aprs_cfg() })
        }
        // APRS-52: Off, then the seven statuses, stopping at the ends
        [5, 5, 7] => {
            let mic_e = (app.aprs_cfg().mic_e as i32 + d).clamp(0, crate::aprs::MIC_E_NAMES.len() as i32 - 1) as u8;
            app.set_aprs(crate::aprs::AprsCfg { mic_e, ..app.aprs_cfg() })
        }
        [5, 9, 3] => {
            let c = app.aprs_cfg();
            let txdelay = (c.txdelay_10ms() as i32 + d * 5).clamp(crate::aprs::TXDELAY_MIN as i32, crate::aprs::TXDELAY_MAX as i32) as u8;
            app.set_aprs(crate::aprs::AprsCfg { txdelay, ..c })
        }
        [5, 9, 4] => {
            let tx_level = (app.aprs_cfg().tx_level as i32 + d).clamp(0, 2) as u8;
            app.set_aprs(crate::aprs::AprsCfg { tx_level, ..app.aprs_cfg() })
        }
        [5, 9, 5] => {
            let i = cycle(app.aprs_cfg().fx25_index(), d, crate::aprs::FX25_CHECKS.len());
            app.set_aprs(crate::aprs::AprsCfg { fx25: crate::aprs::FX25_CHECKS[i as usize], ..app.aprs_cfg() })
        }
        // APRS-57: the knob turns auto reply on and off; OK opens its page
        [5, 9, 9] => app.set_aprs(crate::aprs::AprsCfg { reply: app.aprs_cfg().reply ^ 1, ..app.aprs_cfg() }),
        // 6.25 kHz steps across the VHF and UHF pager bands.
        [4, 2, 2] => s.pocsag_freq = (s.pocsag_freq as i64 + d as i64 * 625).clamp(13_600_000, 52_000_000) as ferrite_core::freq::Freq10,
        [7, 4, 2] => *s.brightness_mut() = (s.brightness() as i32 + d).clamp(1, 10) as u8, // DISP-16, DISP-23
        [8, 2] => return change(app, &[8, 2, 1], d),
        // VOX-10: CW ID needs a callsign
        [8, 6, 2] if s.callsign.as_bytes().is_empty() => app.show_toast(b"CW ID", CW_ID_NO_CALL.as_bytes(), false),
        [8, 8, 4] => set_extend(app, !app.set.extend_tx),
        // TX-48: the radio's own keys only, never the link's remote keys (also greyed for them).
        [8, 8, 5] if !app.remote_in => app.set_dry_tx(!app.dry_tx()),
        [8, 8, 5] => return false,
        [9, 3, 2] if !s.chan_only => app.set_chan_only(), // PIN-5: leaving is hold #
        [9, 3] | [9, 3, 1] => {
            let m = cycle(s.pin_mode, d, PIN_MODES.len());
            if m != 0 && s.pin_len == 0 {
                app.open(Dest::Pin); // a PIN first (PIN-1): the PIN screen sets it, then the lock
                app.screen = Screen::Pin(super::pin::Pin::set(m));
            } else if m == 0 && s.pin_mode != 0 {
                app.open(Dest::Pin); // turning the lock off: the PIN first (PIN-10)
                app.screen = Screen::Pin(super::pin::Pin::off());
            } else {
                s.pin_mode = m;
            }
        }
        _ => {
            let Some(r) = row(p) else { return false };
            let v = (r.get)(s);
            let v = match r.kind {
                Kind::Flip(_) => (v == 0) as u8,
                Kind::Pick(l) => cycle(v, d, l.len()),
                Kind::Round(n, _) => cycle(v, d, n as usize),
                Kind::Level(lo, hi, step, _) => (v.clamp(lo, hi) as i32 + d * step as i32).clamp(lo as i32, hi as i32) as u8,
                Kind::Special => return false,
            };
            (r.set)(s, v);
            tell(app, r.tell);
        }
    }
    true
}

/// # on a wired setting: its default (MENU-18). The special cases by name, the rest from [`SETTINGS`].
pub(crate) fn reset(app: &mut App, p: &[u8]) {
    let d = Settings::DEFAULT;
    let s = &mut app.set;
    match p {
        [1, 1] => app.change(Item::Watch, 1 - app.watch as i32),
        [7, 7] | [7, 7, 1] => app.change(Item::BatterySave, 1 - app.bsave as i32),
        [7, 7, 2] => app.set_lpm_mode(d.lpm),
        [1, 2] => s.scan_resume = d.scan_resume,
        [8, 8, 1] => {
            s.tone_level = d.tone_level;
            app.out.push(RadioCmd::ToneLevel(d.tone_level));
        }
        [6, 9] | [5, 5, 5] => s.fixed_on = false,
        [5, 5, 4] => s.aprs_fixed = false,
        [6, 6] => (s.clock, s.clock_gps) = (d.clock, d.clock_gps),
        [8, 8, 4] => s.extend_tx = false,
        [8, 8, 5] if !app.remote_in => app.set_dry_tx(false), // TX-48: # by the radio's own keys only
        [8, 8, 5] => {}
        [3, 1, 3] => s.scope_dwell = d.scope_dwell,
        [5, 3] if s.aprs_mode != d.aprs_mode => app.set_aprs_mode(d.aprs_mode),
        [5, 6, 1] => app.set_aprs(crate::aprs::AprsCfg { beacon: 0, ..app.aprs_cfg() }),
        [5, 5, 1] => app.set_aprs(crate::aprs::AprsCfg { ssid: crate::aprs::SSID_DEFAULT, ..app.aprs_cfg() }),
        [5, 6, 3] => app.set_aprs(crate::aprs::AprsCfg { power: 0, ..app.aprs_cfg() }),
        [5, 6, 2] => app.set_aprs(crate::aprs::AprsCfg { after_voice: false, ..app.aprs_cfg() }),
        [5, 6, 4] => app.set_aprs(crate::aprs::AprsCfg { telemetry: false, ..app.aprs_cfg() }),
        [5, 6, 5] => app.set_aprs(crate::aprs::AprsCfg { text_every: 0, ..app.aprs_cfg() }),
        [5, 9, 1] => app.set_aprs(crate::aprs::AprsCfg { path: crate::aprs::PATH_DEFAULT, ..app.aprs_cfg() }),
        [5, 7, 1] => app.set_aprs(crate::aprs::AprsCfg { digi: app.aprs_cfg().digi & !crate::aprs::DIGI_ON, ..app.aprs_cfg() }),
        [5, 7, 2] => app.set_aprs(crate::aprs::AprsCfg { digi: app.aprs_cfg().digi & !crate::aprs::DIGI_MYCALL, ..app.aprs_cfg() }),
        [5, 7, 3] => app.set_aprs(crate::aprs::AprsCfg { digi: app.aprs_cfg().digi & 0x0F, ..app.aprs_cfg() }),
        [5, 4] => app.set_aprs(crate::aprs::AprsCfg { f5k: 0, ..app.aprs_cfg() }),
        [7, 5, 5] => app.set_aprs(crate::aprs::AprsCfg { sound: ferrite_core::tonegen::CHIME_DEFAULT, ..app.aprs_cfg() }),
        [5, 5, 2] => app.set_aprs(crate::aprs::AprsCfg { symbol: 0, ..app.aprs_cfg() }),
        [5, 5, 3] => app.set_aprs_comment(b""),
        [5, 5, 6] => app.set_aprs(crate::aprs::AprsCfg { ambiguity: 0, ..app.aprs_cfg() }),
        [5, 5, 7] => app.set_aprs(crate::aprs::AprsCfg { mic_e: 0, ..app.aprs_cfg() }),
        [5, 9, 3] => app.set_aprs(crate::aprs::AprsCfg { txdelay: crate::aprs::TXDELAY_DEFAULT, ..app.aprs_cfg() }),
        [5, 9, 4] => app.set_aprs(crate::aprs::AprsCfg { tx_level: 1, ..app.aprs_cfg() }),
        [5, 9, 5] => app.set_aprs(crate::aprs::AprsCfg { fx25: crate::aprs::FX25_DEFAULT, ..app.aprs_cfg() }),
        [5, 9, 9] => app.set_aprs(crate::aprs::AprsCfg { reply: app.aprs_cfg().reply & !1, ..app.aprs_cfg() }), // APRS-57: off, the rest kept
        [4, 2, 2] => s.pocsag_freq = d.pocsag_freq,
        [4, 2, 3] => s.pocsag_cap = d.pocsag_cap,
        // # forgets the custom path; a path set to it goes back to the default (APRS-26)
        [5, 9, 2] => {
            let c = app.aprs_cfg();
            let path = if c.path == crate::aprs::PATH_CUSTOM { crate::aprs::PATH_DEFAULT } else { c.path };
            app.set_aprs(crate::aprs::AprsCfg { custom: [0; crate::aprs::CUSTOM_LEN], path, ..c });
        }
        [7, 4, 2] => {
            let night = s.theme == NIGHT_RED;
            *s.brightness_mut() = if night { d.brightness_night } else { d.brightness };
        }
        [8, 2] => reset(app, &[8, 2, 1]),
        _ => {
            if let Some(r) = row(p).filter(|r| !matches!(r.kind, Kind::Special)) {
                (r.set)(s, (r.get)(&d));
                tell(app, r.tell);
            }
        }
    }
}

/// "Auto", "12 dB".
pub(crate) fn agc_text(v: u8, b: &mut Buf<24>) {
    if v == 0 {
        b.set(b"Auto");
    } else {
        b.clear();
        // BCR-32, HF-35: a fixed gain index scaled to the chip's (FM 0-26, AM 0-37): 1 the most gain
        let _ = uwrite!(b, "Fixed {}", v.min(crate::settings::AGC_MAX));
    }
}

/// CAL-20.4: the check that locked transmit, and where for a curve check ("Power order: VHF 5, Mid").
pub(crate) fn cal_fault_text(f: ferrite_core::msg::CalFault, at: [u8; 3], b: &mut Buf<40>) {
    use ferrite_core::msg::CalFault::*;
    let row = ["UHF", "VHF", "200 MHz", "HF"][at[0] as usize % 4];
    let lvl = ["Low", "Mid", "High"][at[2] as usize % 3];
    b.clear();
    let _ = match f {
        None => uwrite!(b, "Passed"),
        PaCurve => uwrite!(b, "Power byte: {} {}, {}", row, at[1], lvl),
        LevelOrder => uwrite!(b, "Power order: {} {}", row, at[1]), // CAL-6.1: Low not below High
        Windows => uwrite!(b, "TX windows"),
        Trim => uwrite!(b, "Crystal trim"),
        ToneGain => uwrite!(b, "Sub-tone gain"),
        Unchecked => uwrite!(b, "Store not ready"),
    };
}

/// CAL-6.1: the band row a curve fault locks (the sector valid, that band only), if any.
pub(crate) fn band_locked(t: &ferrite_core::msg::SelfTest) -> Option<u8> {
    use ferrite_core::msg::CalFault::{LevelOrder, PaCurve};
    (t.cal == CalState::Ok && matches!(t.cal_fault, PaCurve | LevelOrder)).then_some(t.cal_at[0])
}

/// [`cal_text`], with CAL-20.4's "couldn't check" apart from a sector that changed.
pub(crate) fn cal_label(t: &ferrite_core::msg::SelfTest) -> &'static str {
    if t.cal == CalState::CrcMismatch && t.cal_fault == ferrite_core::msg::CalFault::Unchecked {
        return "Not checked: locked";
    }
    if let Some(row) = band_locked(t) {
        return ["UHF TX locked", "VHF TX locked", "200 MHz TX locked", "HF TX locked"][row as usize % 4]; // CAL-6.1
    }
    cal_text(t.cal)
}

pub(crate) fn cal_text(c: CalState) -> &'static str {
    match c {
        CalState::Unknown => "Not checked",
        CalState::Ok => "Valid",
        CalState::Blank => "Blank: TX locked",
        CalState::Invalid => "Invalid: TX locked",
        CalState::CrcMismatch => "Changed: TX locked",
    }
}

/// "15 s", "2 min", "1 h", or `zero` for 0.
fn secs(b: &mut Buf<24>, t: u32, zero: &str) {
    match t {
        0 => b.set(zero.as_bytes()),
        1..=59 => {
            let _ = uwrite!(b, "{} s", t);
        }
        60..=3599 => {
            let _ = uwrite!(b, "{} min", t / 60);
        }
        _ => {
            let _ = uwrite!(b, "{} h", t / 3600);
        }
    }
}

/// A resume rule's text (SCAN-5): "5 s", "Carrier drop", "Stop on hit".
pub(crate) fn resume_text(b: &mut Buf<24>, code: u8) {
    match code {
        11 => b.set(b"Carrier drop"),
        12 => b.set(b"Stop on hit"),
        c => {
            let _ = uwrite!(b, "{} s", c.clamp(1, 10));
        }
    }
}

fn cycle(v: u8, d: i32, n: usize) -> u8 {
    (v as i32 + d).rem_euclid(n as i32) as u8
}

/// Extend TX: turning it on shows the warning first, once (SETUP-8, TX-2).
pub(crate) fn set_extend(app: &mut App, on: bool) {
    if on && !app.set.extend_warned {
        app.open(Dest::ExtendTx);
        app.screen = Screen::ExtendTx(ExtendTx);
    } else {
        app.set.extend_tx = on;
    }
}

impl View for Group {
    fn name(self) -> &'static str {
        "Settings group"
    }

    fn mockup(self) -> Option<&'static str> {
        (self.sub == 0).then_some("MenuGroup.png")
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        if self.sub == 0 {
            b.set(SHORT[(self.g as usize).clamp(1, 9) - 1].as_bytes());
        } else {
            // The list's own name, whole words that fit.
            let n = items(self.g, 0).get(self.sub as usize - 1).map_or("", |e| e.name()).as_bytes();
            let cut = if n.len() <= 10 { n.len() } else { n[..11].iter().rposition(|&c| c == b' ').unwrap_or(10) };
            b.set(&n[..cut]);
        }
    }

    fn timeout(self) -> Option<u32> {
        Some(MENU_TIMEOUT_MS)
    }

    fn key(self, app: &mut App, e: KeyEvent, _: &impl ChannelStore) {
        use Press::*;
        let n = self.items().len() as u8;
        let (p, pl) = self.path(self.row);
        match (e.key, e.press) {
            (Key::Up, Short | Repeat) => self.set_row(app, (self.row + n - 1) % n),
            (Key::Down, Short | Repeat) => self.set_row(app, (self.row + 1) % n),
            (Key::Digit(d), Short) if d >= 1 && d <= n => {
                self.set_row(app, d - 1);
                let g = Group { row: d - 1, ..self };
                g.open(app, false);
            }
            (Key::Digit(_), Short) => app.show_toast(b"No item", b"with that number", false),
            (Key::Menu, Short) => self.open(app, true),
            (Key::Hash, Short) => reset(app, &p[..pl]),
            (Key::Star, Short) => app.do_undo(),
            (Key::Exit, Short) => app.back(),
            _ => {}
        }
    }

    fn knob(self, app: &mut App, d: i32, _: &impl ChannelStore) {
        self.turn(app, d);
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        self.draw(app, lcd, cx);
    }

    fn hints(self, app: &App) -> Hints {
        if app.set_undo.is_some() || app.undo.is_some() {
            Hints::new(&[("Knob", "Change"), ("*", "Undo"), ("BACK", "Back")]) // MENU-22
        } else {
            Hints::new(&[("Knob", "Change"), ("OK", "Open"), ("BACK", "Back")])
        }
    }
}

/// The value a setting shows, as a hash (MENU-22's undo compares it).
fn shown(app: &App, p: &[u8]) -> u32 {
    let mut b = Buf::<24>::new();
    let _ = wired(app, &Device::default(), p, &mut b);
    Mix::new().h(&b.as_bytes()).get()
}

/// MENU-22: steps the setting back the other way until it shows what it did before the run of changes (a
/// toggle flips back, a list walks back); bounded, so a value that can't come back stops trying.
pub(crate) fn undo_setting(app: &mut App, u: &crate::SetUndo) {
    let p = &u.path[..u.len as usize];
    for _ in 0..64 {
        if shown(app, p) == u.was || !change(app, p, -(u.dir as i32)) {
            break;
        }
    }
}

impl Group {
    /// The knob or ◀▶: change the highlighted value, or say why it can't.
    fn turn(self, app: &mut App, d: i32) {
        let (p, pl) = self.path(self.row);
        turn_path(app, &p[..pl], self.items()[self.row as usize].name(), d);
    }

    /// Row `i` as drawn: its name, the value it shows, the changed-from-default dot and whether it is greyed
    /// (MENU-14: a greyed row not selected shows why instead of its value). The UI readback reads it too (LINK-19).
    pub(crate) fn row(self, app: &App, dev: &Device, store: &impl ChannelStore, i: u8) -> (&'static str, Buf<24>, bool, bool) {
        let e = self.items()[i as usize];
        let (p, pl) = self.path(i);
        let mut v = Buf::<24>::new();
        let dot = match wired(app, dev, &p[..pl], &mut v) {
            Some(d) => d,
            None => {
                match &p[..pl] {
                    [2, 1] => {
                        let _ = uwrite!(v, "{} channels", store.count());
                    }
                    [4, 1] if app.log_unread > 0 => {
                        let _ = uwrite!(v, "{} new", app.log_unread);
                    }
                    [5, 5] => v.set(b"no callsign"),
                    _ => v.set(e.value().as_bytes()),
                }
                false
            }
        };
        let grey = greyed(app, &p[..pl]);
        if let (Some(why), true) = (grey, i != self.row) {
            v.set(why.as_bytes()); // MENU-14: greyed items say why
        }
        (e.name(), v, dot, grey.is_some())
    }

    /// The help box: the highlighted item's path ("MENU 0 7 1") and help line (or why it is greyed).
    pub(crate) fn help(self, app: &App) -> (Buf<20>, Help) {
        let (p, pl) = self.path(self.row);
        let mut path = Buf::<20>::new();
        path.push(b"MENU 0");
        for x in &p[..pl] {
            let _ = uwrite!(path, " {}", x);
        }
        let h = match greyed(app, &p[..pl]) {
            Some(why) => Help::Why(why),
            None => Help::Note(self.items().get(self.row as usize).and_then(|e| e.help())),
        };
        (path, h)
    }

    fn draw(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        app.new_page(lcd, Mix::new().h(&self.g).h(&self.sub).get());
        for i in 0..self.items().len() {
            let (name, v, dot, grey) = self.row(app, cx.dev, cx.store, i as u8);
            let r = Row { num: Some(i as u8 + 1), name: name.as_bytes(), value: v.as_bytes(), dot, selected: i as u8 == self.row, grey };
            app.menu_row(lcd, i, 23 + 25 * i as i32, 25, &r);
        }
        let (path, help) = self.help(app);
        app.help_note(lcd, 252, 44, help, path.as_bytes());
    }

    /// OK (or its digit) on the highlighted row: its screen, its list, or (OK only) a toggle.
    fn open(self, app: &mut App, ok: bool) {
        let (p, pl) = self.path(self.row);
        open_path(app, &p[..pl], self.items()[self.row as usize].name(), ok);
    }
}

/// The knob or ◀▶ on setting `p` (named `name` in a toast): change it, or say why it can't. A menu row and a quick
/// page's cell (MENU-34) both turn this way.
pub(crate) fn turn_path(app: &mut App, p: &[u8], name: &str, d: i32) {
    if let Some(why) = greyed(app, p) {
        app.show_toast(name.as_bytes(), why.as_bytes(), false);
    } else {
        // MENU-22: a radio-wide setting changed here offers "* Undo" (the slot's own items keep theirs).
        let was = shown(app, p);
        // Dry TX (TX-48) isn't a setting: no undo either.
        if change(app, p, d) && !matches!(p, [1, 1] | [7, 7] | [7, 7, 1] | [7, 7, 2] | [8, 8, 5]) && shown(app, p) != was {
            app.keep_set_undo(p, d, was);
        }
    }
}

/// OK (or its digit) on setting `p`: its screen, its list, or (OK only) a toggle. A menu row and a quick page's
/// cell (MENU-34) both open this way.
pub(crate) fn open_path(app: &mut App, p: &[u8], name: &str, ok: bool) {
    if let Some(why) = greyed(app, p) {
        return app.show_toast(name.as_bytes(), why.as_bytes(), false);
    }
    match p {
        [8, 6, 1] => {
            super::setup::callsign(app);
        }
        // OK on Text edits it (16 characters); the knob still picks off, callsign or text.
        [7, 3, 2] if ok && app.set.pon == 2 => {
            app.text = Buf::from(app.set.pon_text.as_bytes());
            app.open(Dest::Text(super::text::TextFor::PowerOn));
        }
        // OK types the capcode (DEC-4), the clock (GPS-23) or the custom path (APRS-26); the knob still
        // changes 0 6 6's format.
        [4, 2, 3] => {
            app.text = Buf::new();
            if app.set.pocsag_cap != 0 {
                let _ = uwrite!(app.text, "{}", app.set.pocsag_cap);
            }
            app.open(Dest::Text(super::text::TextFor::Capcode));
        }
        [6, 6] if ok => {
            app.text = Buf::new();
            app.open(Dest::Text(super::text::TextFor::Clock));
        }
        [5, 9, 2] => {
            app.text = Buf::from(app.aprs_custom_path());
            app.open(Dest::Text(super::text::TextFor::AprsPath));
        }
        // APRS-50: the comment in the message editor (40 characters, multi-tap or predictive)
        [5, 5, 3] => {
            app.text.clear();
            app.open_screen(crate::Screen::Compose(super::msgs::Compose::comment_edit()));
        }
        // MENU-31: a frequency setting opens the frequency entry box; the knob still steps it.
        [5, 4] => super::entry::open_setting(app, super::entry::FreqSet::Aprs),
        [4, 2, 2] => super::entry::open_setting(app, super::entry::FreqSet::Pager),
        _ => {
            if let Some(d) = nav::menu0(p) {
                app.open(d);
                restore_row(app); // MENU-24: a third-level list on its last item
            } else if ok && !change(app, p, 1) {
                let mut b = Buf::<20>::new();
                b.push(b"MENU 0");
                for x in p {
                    let _ = uwrite!(b, " {}", x);
                }
                app.show_toast(b.as_bytes(), b"setting comes next", false);
            }
        }
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    /// MENU-32: a group's Advanced row is its last, and opens a list (no deeper levels below it).
    #[test]
    fn advanced_is_the_last_row() {
        for (g, rows) in GROUP_ITEMS.iter().enumerate() {
            let g = g as u8 + 1;
            for (i, e) in rows.iter().enumerate() {
                if e.name() == "Advanced" {
                    assert_eq!(i + 1, rows.len(), "group {g}: Advanced is the last row");
                    assert!(!items(g, i as u8 + 1).is_empty(), "group {g}: Advanced opens a list");
                }
            }
        }
        for g in [1u8, 5, 8, 9] {
            assert_eq!(GROUP_ITEMS[g as usize - 1].last().unwrap().name(), "Advanced", "group {g}");
        }
    }

    /// MENU-3: at most 9 items per list, every list on one screen, and a group's third level is the deepest
    /// (MENU 0 g s i): sub-lists hang only off a group's own rows and have no sub-lists of their own.
    #[test]
    fn menu_shape() {
        for (g, items) in GROUP_ITEMS.iter().enumerate() {
            assert!(!items.is_empty() && items.len() <= 9, "group {} has {} items", g + 1, items.len());
        }
        for ((g, s), items) in SUBS {
            assert!((1..=9).contains(&g) && (1..=9).contains(&s), "sub-list ({g}, {s})");
            assert!(s as usize <= GROUP_ITEMS[g as usize - 1].len(), "({g}, {s}) hangs off a row its group has");
            assert!(!items.is_empty() && items.len() <= 9, "({g}, {s}) has {} items", items.len());
        }
        // SETTINGS: every path is a menu item, once.
        for (i, r) in SETTINGS.iter().enumerate() {
            let p = r.path();
            let list = items(p[0], if p.len() == 3 { p[1] } else { 0 });
            assert!(p.last().is_some_and(|&x| x >= 1 && x as usize <= list.len()), "{p:?} is not a menu item");
            assert!(SETTINGS[..i].iter().all(|q| q.path() != p), "{p:?} twice");
        }
        // Each item is "name|value|help": three parts, a name always.
        for e in GROUP_ITEMS.iter().copied().chain(SUBS.iter().map(|x| x.1)).flatten() {
            assert!(e.0.matches('|').count() == 2 && !e.name().is_empty(), "item {:?}", e.0);
        }
    }
}
