//! # Navigation
//!
//! Every screen in the manual has a [`Dest`]: what [`App::open`] takes, from wherever the screen is reached.
//! [`enter`] turns a destination into a screen; until a screen is built it opens as a placeholder ("<name>:
//! coming", [`super::coming`]) so its ways in can be tried in the simulator. Building a screen means adding
//! its arm to [`enter`]; every way in then reaches it.
//!
//! The ways in, as the manual has them (IDs in `docs/screens.md`):
//!
//! | From | Keys | Goes to |
//! | --- | --- | --- |
//! | Home | hold 0-9 ([`hold_digit`]: runs [`shortcut`] at the long press, a popup names it, HOME-8, KEY-11) | 0 home channel, 1 band scope, 2 TX power, 3 squelch, 4 watch, 5 tones, 6 step, 7 shift, 8 offset, 9 copy (KEY-23: a VFO to [`Dest::SaveChannel`], a channel to "Copy to VFO?") |
//! | Home | a digit (NAV-4, VFO-11) | [`Dest::Entry`] with the digit |
//! | Home | hold V/M (KEY-23) | [`Dest::Zones`] |
//! | Home | hold SLOT / MENU / hold MENU / # | edit in place (on a channel: [`Dest::ChannelEdit`]) / quick / advanced / Status |
//! | Home | SIDE2 (SCAN-1) / SIDE1 + knob (SQL-10) | [`Dest::Scan`] / [`Dest::Squelch`] |
//! | Any screen | PTT down (KEY-7) | home, then [`Dest::Transmit`], or [`Dest::TxRefused`] (TX-20) or [`Dest::Emergency`] (TX-3); PTT up: home ([`super::View::ptt`]) |
//! | Any screen | BACK / hold BACK (KEY-5, MENU-24) | back one level, to the screen it was opened from / home (not the modal ones: PIN at power on, link write, power off) |
//! | Quick settings | OK on a row (MENU-17) | [`Dest::Value`] (tones: [`Dest::Tones`]); MENU 8: [`Dest::SaveChannel`] or [`Dest::ChannelEdit`] |
//! | Edit in place | OK on a cell | [`Dest::Value`] / [`Dest::Tones`] |
//! | Advanced (MENU 0) | 1-9 or OK | [`Dest::Group`]; inside, digits follow [`MENU0`] (MENU-5) |
//! | A list or mode screen | hold MENU (MENU-25) | that screen's options (D4) |
//! | Status page | OK on a row (SBAR-15) | the row's setting or screen |
//! | Locked, PIN "at power on and unlock" | hold # (KEY-16) | [`Dest::Pin`] |
//! | Power on | the firmware: [`App::first_boot`] / [`App::ask_pin`] | [`Dest::Setup`] (BOOT-12) / [`Dest::Pin`] (BOOT-13) |
//! | [`App::notify`], [`App::raise_alert`] | automatic (WX-4) | [`Dest::Alert`] |
//!
//! Crossband is MENU 0 3 4 (Tools); the mailbox is the Status page's last row. The screens an event opens
//! have App hooks the firmware (and the simulator's keys) call: [`App::power_off`], [`App::battery_empty`],
//! [`App::ssb_patch_found`], [`App::link_write_progress`] and
//! [`App::link_write_done`], [`App::same_alert`].
//! Still placeholders: position entry (A8), location codes (A13), the DTMF dialer and memories (A14, A15).

use super::advanced::{Advanced, GROUPS};
use super::coming::Coming;
use super::quick::Quick;
use super::status::Status;
use crate::chan::ChannelStore;
use crate::slot::Item;
use crate::{App, Screen};
use ferrite_ui::text::Buf;
use ufmt::uwrite;

/// Where a key can take you: one per screen (or screen family) in the manual. The groups and IDs are those of
/// `docs/screens.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dest {
    // ---- A: entry and editing ----
    /// A1, A2: frequency (VFO) or channel number entry, started by this digit.
    Entry(u8),
    /// A4: the channel editor (MENU 8 on a channel).
    ChannelEdit,
    /// A5: a setting's bigger editor (MENU-17).
    Value(Item),
    /// A6: RX and TX tones.
    Tones,
    /// A7: multi-tap text entry (KEY-12), for one target: where OK writes the text.
    Text(super::text::TextFor),
    /// A8: latitude, longitude, altitude.
    PositionEntry,
    /// A9: save the VFO as a channel (hold 9, MENU 8 on a VFO).
    SaveChannel,
    /// A10: add a repeater (MENU 0 2 2).
    AddRepeater,
    /// A11: one range preset.
    RangePreset,
    /// A12: one zone's settings.
    ZoneEdit,
    /// A13: SAME location codes (MENU 0 4 5 2).
    LocationCodes,
    /// A16b: APRS auto reply (MENU 0 5 9 9, APRS-57).
    AutoReply,
    /// A14: DTMF dialer (MENU 0 8 5 1).
    DtmfDialer,
    /// A15: DTMF memories (MENU 0 8 5 2).
    DtmfMemories,
    /// A16: APRS message compose.
    Compose,
    /// A16a: the quick messages, from the Messages options (TEXT-7).
    QuickMessages,
    /// C5a: the contacts, from the Messages options (APRS-40).
    Contacts,
    /// A17: PIN entry.
    Pin,
    /// A18: confirm a destructive action (MENU-23); MENU 0 9 7 2 reset.
    Confirm,
    /// A19: the Extend TX warning.
    ExtendTx,
    /// A20-A27: first-run setup, eight pages (MENU 0 9 7 1).
    Setup,

    // ---- B: radio activity ----
    /// B4: the shortcut map while a digit is held (HOME-8).
    ShortcutMap,
    /// B6: transmitting.
    Transmit,
    /// B7: transmit refused, with the fix.
    TxRefused,
    /// B8: "Emergency frequency. PTT again to transmit".
    Emergency,
    /// B9: the scan screen.
    Scan,
    /// B10: band scope (hold 1, MENU 0 3 1 6).
    BandScope,
    /// B12, B13: HF mode (MENU 0 3 3 only).
    HfMode,
    /// B14: tone search (MENU 0 3 5).
    ToneSearch,
    /// B22-B24: satellite mode (MENU 0 3 6).
    Satellites,
    /// B15: frequency counter (MENU 0 3 2).
    Counter,
    /// B17: an alert (SAME, DSC distress).
    Alert,
    /// B18-B20: crossband repeater (MENU 0 3 4).
    Crossband,
    /// B21: squelch from home (SIDE1 + knob); a toast for now.
    Squelch,
    Snake,
    Activation,

    // ---- C: lists and data ----
    /// C1: the event log (MENU 0 4 1).
    EventLog,
    /// C2: one event.
    EventDetail,
    /// C3: APRS heard (MENU 0 5 1).
    AprsHeard,
    /// C4: one APRS station.
    AprsStation,
    /// C3a: our beacons and who repeated them (the APRS list's Sent, APRS-53).
    AprsSent,
    /// C5: APRS messages (MENU 0 5 2).
    AprsMessages,
    /// C6: one APRS message.
    AprsMessage,
    /// C8-C10: channel browser (MENU 0 2 1).
    Browse,
    /// C11: zones (MENU 0 2 3).
    Zones,
    /// C12: range presets (MENU 0 1 4).
    RangePresets,
    /// C13: skip list (MENU 0 1 5).
    SkipList,
    /// C14: waypoints (MENU 0 6 7).
    Waypoints,
    /// C15: go to a waypoint.
    WaypointGo,
    /// C16, C17: GPS position and satellites (MENU 0 6 2).
    GpsPosition,
    /// C18: HF recent frequencies.
    HfRecent,

    // ---- D: system and modes ----
    /// D1: quick settings (MENU).
    Quick,
    /// D2: advanced settings (MENU 0).
    Advanced,
    /// D3: a settings group, MENU 0 n.
    Group(u8),
    /// D3: a third-level list, MENU 0 n m.
    Sub(u8, u8),
    /// D6: the Status page (#, MENU 0 0).
    Status,
    /// D7: About, battery, self-test (MENU 0 9 8).
    About,
    /// D8: calibration, read only (MENU 0 9 9 2).
    Calibration,
    /// D9: key test (MENU 0 9 1 2).
    KeyTest,
    /// D10: advanced squelch per band (MENU 0 9 9 1).
    AdvSquelch,
    /// D15: SSB patch confirm at boot.
    SsbConfirm,
    /// D16: SSB patch (MENU 0 9 6).
    SsbPatch,
    /// D17: link status (MENU 0 9 2 2). (D18-D20, approved devices and the write approval, went with decision 104.)
    LinkStatus,
    /// D21: link write progress.
    WriteProgress,
    /// D22: power off, "Saving...".
    PowerOff,
    /// D23: empty battery.
    EmptyBattery,
    /// D24: mailbox.
    Mailbox,
    /// D25: finish the update in Ferrite Console (UPD-16: the chip holds another layout's data).
    FinishUpdate,
}

impl Dest {
    /// OK on a setting: tones have their own picker (A6), everything else the value editor (A5).
    pub fn value(item: Item) -> Dest {
        if item == Item::Tones { Dest::Tones } else { Dest::Value(item) }
    }

    /// Name, manual chapter and inventory ID (`docs/screens.md`).
    pub fn info(self) -> (&'static str, &'static str, &'static str) {
        use Dest::*;
        match self {
            Entry(_) => ("Direct entry", "9", "A1"),
            ChannelEdit => ("Channel editor", "10", "A4"),
            Value(_) => ("Value editor", "11", "A5"),
            Tones => ("Tone picker", "16", "A6"),
            Text(_) => ("Text entry", "7", "A7"),
            PositionEntry => ("Position entry", "28", "A8"),
            SaveChannel => ("Save as channel", "9", "A9"),
            AddRepeater => ("Add repeater", "10", "A10"),
            RangePreset => ("Range preset", "14", "A11"),
            ZoneEdit => ("Zone editor", "10", "A12"),
            LocationCodes => ("Location codes", "23", "A13"),
            AutoReply => ("Auto reply", "25", "A16b"),
            DtmfDialer => ("DTMF dialer", "17", "A14"),
            DtmfMemories => ("DTMF memories", "17", "A15"),
            Compose => ("Compose message", "25", "A16"),
            QuickMessages => ("Quick messages", "7", "A16a"),
            Contacts => ("Contacts", "25", "C5a"),
            Pin => ("PIN entry", "34", "A17"),
            Confirm => ("Confirm", "11", "A18"),
            ExtendTx => ("Extend TX warning", "15", "A19"),
            Setup => ("Setup", "4", "A20"),
            ShortcutMap => ("Shortcut map", "7", "B4"),
            Transmit => ("Transmit", "15", "B6"),
            TxRefused => ("Transmit refused", "15", "B7"),
            Emergency => ("Emergency frequency", "15", "B8"),
            Scan => ("Scan", "14", "B9"),
            BandScope => ("Band scope", "21", "B10"),
            HfMode => ("HF mode", "20a", "B12"),
            ToneSearch => ("Tone search", "22", "B14"),
            Satellites => ("Satellites", "28a", "B22"),
            Counter => ("Frequency counter", "22", "B15"),
            Alert => ("Alert", "23", "B17"),
            Crossband => ("Crossband repeater", "19", "B18"),
            Squelch => ("Squelch", "12", "B21"),
            Activation => ("SOTA / POTA", "11", "D27"),
            Snake => ("Snake", "11", "D26"),
            EventLog => ("Event log", "24", "C1"),
            EventDetail => ("Event detail", "27", "C2"),
            AprsHeard => ("APRS heard", "25", "C3"),
            AprsStation => ("APRS station", "25", "C4"),
            AprsSent => ("APRS sent", "25", "C3a"),
            AprsMessages => ("APRS messages", "25", "C5"),
            AprsMessage => ("APRS message", "25", "C6"),
            Browse => ("Channel browser", "10", "C8"),
            Zones => ("Zones", "10", "C11"),
            RangePresets => ("Range presets", "14", "C12"),
            SkipList => ("Skip list", "14", "C13"),
            Waypoints => ("Waypoints", "28", "C14"),
            WaypointGo => ("Go to waypoint", "28", "C15"),
            GpsPosition => ("GPS position", "28", "C16"),
            HfRecent => ("HF recent", "20a", "C18"),
            Quick => ("Quick", "11", "D1"),
            Advanced => ("More", "11", "D2"),
            Group(_) => ("Settings group", "11", "D3"),
            Sub(..) => ("Settings list", "11", "D3"),
            Status => ("Status", "8", "D6"),
            About => ("About", "36", "D7"),
            Calibration => ("Calibration", "35", "D8"),
            KeyTest => ("Key test", "7", "D9"),
            AdvSquelch => ("Advanced squelch", "12", "D10"),
            SsbConfirm => ("SSB patch confirm", "30", "D15"),
            SsbPatch => ("SSB patch", "20a", "D16"),
            LinkStatus => ("Link status", "29", "D17"),
            WriteProgress => ("Writing", "29", "D21"),
            PowerOff => ("Power off", "33", "D22"),
            EmptyBattery => ("Empty battery", "33", "D23"),
            Mailbox => ("Mailbox", "38", "D24"),
            FinishUpdate => ("Finish the update", "30", "D25"),
        }
    }

    pub fn name(self) -> &'static str {
        self.info().0
    }

    /// The heading a screen shows for itself: the group's name, the menu path, or the setting being edited.
    pub fn heading(self, b: &mut Buf<32>) {
        b.clear();
        match self {
            Dest::Group(g) => b.set(GROUPS[(g as usize).clamp(1, 9) - 1].0.as_bytes()),
            Dest::Sub(g, i) => {
                let _ = uwrite!(b, "MENU 0 {} {}", g, i);
            }
            Dest::Value(item) => {
                b.set(item.name().as_bytes());
                b.push(b" editor");
            }
            _ => b.set(self.name().as_bytes()),
        }
    }

    /// The menu path this destination is a list for (a group or a third-level list), if it is one.
    pub fn menu_path(self, b: &mut [u8; 3]) -> Option<usize> {
        match self {
            Dest::Group(g) => {
                b[0] = g;
                Some(1)
            }
            Dest::Sub(g, i) => {
                (b[0], b[1]) = (g, i);
                Some(2)
            }
            _ => None,
        }
    }
}

/// Every destination once (groups and lists by one example each), for tests and the screen inventory.
pub const ALL: [Dest; 71] = {
    use Dest::*;
    [
        Entry(1), ChannelEdit, Value(Item::Squelch), Tones, Text(super::text::TextFor::Callsign), PositionEntry, SaveChannel, AddRepeater, RangePreset,
        ZoneEdit, LocationCodes, DtmfDialer, DtmfMemories, Compose, QuickMessages, Contacts, Pin, Confirm, ExtendTx, Setup, ShortcutMap,
        Transmit, TxRefused, Emergency, Scan, BandScope, HfMode, ToneSearch, Counter, Alert,
        Crossband, Squelch, EventLog, EventDetail, AprsHeard, AprsStation, AprsSent, AprsMessages, AprsMessage,
        Browse, Zones, RangePresets, SkipList, Waypoints, WaypointGo, GpsPosition, HfRecent, Quick,
        Advanced, Group(5), Sub(9, 1), Status, About, Calibration, KeyTest, AdvSquelch, SsbConfirm, SsbPatch,
        LinkStatus, WriteProgress, PowerOff, EmptyBattery, Mailbox,
        Group(1), Sub(4, 2), Value(Item::Power), FinishUpdate, Satellites, AutoReply, Snake, Activation,
    ]
};

/// MENU 0 paths that open a screen or a third-level list (manual Appendix A). Every other path
/// is a plain value, edited in its group's list.
pub const MENU0: [(&[u8], Dest); 56] = [
    (&[1, 6], Dest::Sub(1, 6)),
    (&[1, 4], Dest::RangePresets),
    (&[1, 5], Dest::SkipList),
    (&[2, 1], Dest::Browse),
    (&[2, 2], Dest::AddRepeater),
    (&[2, 3], Dest::Zones),
    (&[2, 6], Dest::Sub(2, 6)), // BCR-1 (decision 352): FM broadcast's settings; a slot in the band is the receiver
    (&[3, 1], Dest::Sub(3, 1)),
    (&[3, 1, 6], Dest::BandScope),
    (&[3, 2], Dest::Counter),
    (&[3, 3], Dest::HfMode),
    (&[3, 4], Dest::Crossband),
    (&[3, 5], Dest::ToneSearch),
    (&[3, 6], Dest::Satellites), // SAT-1
    (&[3, 7], Dest::Snake),
    (&[3, 8], Dest::Activation),
    (&[4, 1], Dest::EventLog),
    (&[4, 2], Dest::Sub(4, 2)),
    (&[4, 5], Dest::Sub(4, 5)),
    (&[4, 5, 2], Dest::LocationCodes),
    (&[5, 9, 9], Dest::AutoReply),
    (&[5, 1], Dest::AprsHeard),
    (&[5, 2], Dest::AprsMessages),
    (&[5, 5], Dest::Sub(5, 5)),
    (&[5, 6], Dest::Sub(5, 6)),
    (&[5, 9], Dest::Sub(5, 9)),
    (&[5, 7], Dest::Sub(5, 7)),
    (&[6, 2], Dest::GpsPosition),
    (&[6, 7], Dest::Waypoints),
    (&[6, 9], Dest::PositionEntry),
    (&[5, 5, 5], Dest::PositionEntry), // APRS-31: the same fixed position
    (&[7, 3], Dest::Sub(7, 3)),
    (&[7, 4], Dest::Sub(7, 4)),
    (&[7, 5], Dest::Sub(7, 5)),
    (&[7, 7], Dest::Sub(7, 7)), // Battery Save, Low Power Mode (PWR-24)
    (&[8, 2], Dest::Sub(8, 2)),
    (&[8, 8], Dest::Sub(8, 8)), // Advanced: subtone level, tail, Extend TX, Dry TX (TX-48)
    (&[8, 5], Dest::Sub(8, 5)),
    (&[8, 5, 1], Dest::DtmfDialer),
    (&[8, 5, 2], Dest::DtmfMemories),
    (&[8, 6], Dest::Sub(8, 6)),
    (&[9, 1], Dest::Sub(9, 1)),
    (&[9, 1, 2], Dest::KeyTest),
    (&[9, 1, 3], Dest::ShortcutMap),
    (&[9, 3, 3], Dest::Pin),
    (&[9, 2], Dest::Sub(9, 2)),
    (&[9, 2, 2], Dest::LinkStatus),
    (&[9, 3], Dest::Sub(9, 3)),
    (&[9, 9], Dest::Sub(9, 9)),
    (&[9, 9, 1], Dest::AdvSquelch),
    (&[9, 6], Dest::SsbPatch),
    (&[9, 9, 2], Dest::Calibration),
    (&[9, 7], Dest::Sub(9, 7)),
    (&[9, 7, 1], Dest::Setup),
    (&[9, 7, 2], Dest::Confirm),
    (&[9, 8], Dest::About),
];

/// The screen at a MENU 0 path, if the path opens one.
pub fn menu0(path: &[u8]) -> Option<Dest> {
    MENU0.iter().find(|(p, _)| *p == path).map(|&(_, d)| d)
}

/// A destination's screen: built screens by their own arm, the rest as a placeholder. Adding a screen = one
/// arm here.
pub fn enter(dest: Dest, app: &mut App) -> Screen {
    use super::*;
    let a = app.active;
    let on_ch = app.slots[a as usize].on_channel();
    use super::info::{Info, Page};
    use super::link::{Link, LinkPage};
    match dest {
        Dest::Entry(d) => Screen::Entry(entry::Entry::new(if on_ch { entry::EntryFor::Channel } else { entry::EntryFor::Vfo }, Some(d))),
        Dest::ChannelEdit if on_ch => Screen::ChEdit(chedit::ChEdit { cell: 0 }),
        Dest::ChannelEdit | Dest::SaveChannel => Screen::Save(save::Save::new(false)),
        Dest::AddRepeater => Screen::Save(save::Save::new(true)),
        Dest::Value(item) => Screen::Value(value::Value::new(item, app)),
        Dest::Tones => Screen::Tones(tones::Tones::new(app)),
        Dest::Text(to) => Screen::Text(text::Text::new(to)),
        Dest::Confirm => Screen::Confirm(confirm::Confirm { ask: confirm::Ask::Reset { armed: false }, boot: false, since: 0 }),
        Dest::ShortcutMap => Screen::Shortcut(shortcut::Shortcut { sel: 0 }),
        Dest::Quick => Screen::Quick(Quick { row: 0 }),
        Dest::Advanced => Screen::Advanced(Advanced { row: 0, from_quick: false }),
        Dest::Status => Screen::Status(Status { row: 0, from_menu: false }),
        // Group B, radio activity.
        Dest::Transmit => Screen::Transmit(transmit::Transmit { slot: a, dial: false }),
        Dest::TxRefused => {
            let why = transmit::refusal(app, a).unwrap_or(ferrite_core::txplan::Refusal::OutOfBand);
            Screen::TxRefused(transmit::TxRefused { why, slot: a })
        }
        Dest::Emergency => Screen::Emergency(transmit::Emergency { slot: a, aprs: false }),
        Dest::Scan => Screen::Scan(scan::Scan { slot: a, list: false, sel: 0 }),
        Dest::BandScope => scope::open(app),
        Dest::ToneSearch => probe::open_tone(app),
        Dest::Counter => probe::open_counter(app),
        Dest::Activation => Screen::Activation(super::activation::Activation),
        Dest::Snake => super::snake::open(app),
        Dest::Crossband => Screen::Crossband(xb::Crossband { cell: 0 }),
        Dest::Squelch => Screen::Squelch(squelch::Squelch { floor: 0 }),
        // Group C: lists and data
        Dest::EventLog => Screen::Log(super::log::Log::new()),
        Dest::EventDetail => Screen::Log(super::log::Log { detail: true, ..super::log::Log::new() }),
        Dest::AprsHeard => Screen::Aprs(super::aprs::Aprs { cur: Default::default(), station: false }),
        Dest::AprsStation => Screen::Aprs(super::aprs::Aprs { cur: Default::default(), station: true }),
        Dest::AprsSent => Screen::AprsSent(super::aprs::Sent { cur: Default::default() }),
        Dest::AprsMessages => Screen::Msgs(super::msgs::Msgs { cur: Default::default(), open: false }),
        Dest::AprsMessage => Screen::Msgs(super::msgs::Msgs { cur: Default::default(), open: true }),
        Dest::Compose => {
            app.text.clear(); // the keys of a predicted word (TEXT-2)
            Screen::Compose(super::msgs::Compose::new())
        }
        Dest::QuickMessages => Screen::QuickMsgs(super::msgs::QuickMsgs { cur: Default::default() }),
        Dest::Contacts => Screen::Contacts(super::contacts::Contacts { cur: Default::default() }),
        Dest::GpsPosition => Screen::Gps(super::gps::Gps::new(super::gps::Page::Position)),
        Dest::Waypoints => Screen::Gps(super::gps::Gps::new(super::gps::Page::Waypoints)),
        Dest::WaypointGo => Screen::Gps(super::gps::Gps::new(super::gps::Page::Go)),
        Dest::Browse => Screen::Browse(super::browse::Browse::new()),
        Dest::Zones => Screen::Zones(super::zones::Zones::new()),
        Dest::ZoneEdit => Screen::Zones(super::zones::Zones { mode: 2, ..super::zones::Zones::new() }),
        Dest::RangePresets => Screen::ScanLists(super::scanlists::ScanLists::new(super::scanlists::Page::Presets)),
        Dest::RangePreset => Screen::ScanLists(super::scanlists::ScanLists::new(super::scanlists::Page::Edit(0))),
        Dest::SkipList => Screen::ScanLists(super::scanlists::ScanLists::new(super::scanlists::Page::Skips)),
        Dest::HfRecent => Screen::Recall(super::recall::Recall { cur: Default::default() }),
        // D: system and modes
        // A group opened to a setting (the Status page, a shortcut) is on row 1; plain menu navigation brings
        // back the remembered row afterwards ([`super::groups::restore_row`], MENU-24).
        Dest::Group(g) => Screen::Group(super::groups::Group { g: g.clamp(1, 9), sub: 0, row: 0 }),
        Dest::Sub(g, m) => Screen::Group(super::groups::Group { g, sub: m, row: 0 }),
        Dest::Setup => Screen::Setup(super::setup::Setup::new(false)),
        Dest::ExtendTx => Screen::ExtendTx(super::setup::ExtendTx),
        Dest::Pin if app.locked() => Screen::Pin(super::pin::Pin::unlock()),
        Dest::Pin => Screen::Pin(super::pin::Pin::change(app)), // 0 9 3 3 Change PIN
        Dest::About => Screen::Info(Info { page: Page::About }),
        Dest::Calibration => Screen::Info(Info { page: Page::Calibration }),
        Dest::AdvSquelch => Screen::Info(Info { page: Page::AdvSquelch }),
        Dest::SsbPatch => Screen::Info(Info { page: Page::SsbPatch }),
        Dest::SsbConfirm => Screen::Info(Info { page: Page::SsbConfirm }),
        Dest::PowerOff => Screen::Info(Info { page: Page::PowerOff }),
        Dest::EmptyBattery => Screen::Info(Info { page: Page::EmptyBattery }),
        Dest::Mailbox => Screen::Info(Info { page: Page::Mailbox }),
        Dest::FinishUpdate => Screen::Info(Info { page: Page::FinishUpdate }),
        Dest::KeyTest => Screen::KeyTest(super::keytest::KeyTest::new()),
        Dest::PositionEntry => Screen::PosEntry(super::posentry::PosEntry::new()),
        Dest::LocationCodes => Screen::SameCodes(super::samecodes::SameCodes::new()),
        Dest::AutoReply => Screen::AutoReply(super::autoreply::AutoReply::new()),
        Dest::DtmfDialer => Screen::Dtmf(super::dtmf::DtmfPad::dialer()),
        Dest::DtmfMemories => Screen::Dtmf(super::dtmf::DtmfPad::memories()),
        Dest::LinkStatus => Screen::Link(Link::new(LinkPage::Status)),
        Dest::WriteProgress => Screen::Link(Link::new(LinkPage::Progress)),
        Dest::HfMode => hf::open(app),
        Dest::Satellites => sat::open(app),
        Dest::Alert => Screen::Alert(super::alert::Alert { silenced: false }),
        // Every destination has its own screen now; a new one shows the placeholder until it is built.
        #[allow(unreachable_patterns)]
        _ => Screen::Coming(Coming { dest }),
    }
}

/// A digit held on home: the shortcut runs at the long press, while still held; the release does nothing (HOME-8,
/// KEY-3). No page during the hold: a popup in the middle names what ran.
pub fn hold_digit(app: &mut App, d: u8, store: &impl ChannelStore) {
    let (before, prev) = (app.screen, app.toast.map(|t| t.until));
    shortcut(app, d, store);
    // A popup in the middle names what ran: a toggle's own toast (with its new value) or an error moves there;
    // a shortcut that opens a screen gets just its name.
    match app.toast.filter(|t| !t.center && Some(t.until) != prev) {
        Some(t) => app.show_popup(t.head.as_bytes(), t.body.as_bytes()),
        // a confirm (hold 9's "Copy to VFO?", KEY-23) names itself: no popup over its title
        None if app.screen != before && !matches!(app.screen, Screen::Confirm(_)) => {
            app.show_popup(super::shortcut::name(app, d).as_bytes(), b"")
        }
        None => {}
    }
}

/// The ten fixed shortcuts (KEY-11), run at the long press. 2-9 follow stock's factory defaults
/// (TX power, squelch, dual standby, CTCSS/DCS, step, shift direction, offset, zone); 0 and 1 are Ferrite's.
pub fn shortcut(app: &mut App, d: u8, store: &impl ChannelStore) {
    let on_ch = app.slot(app.active).on_channel();
    // Channel-only mode (PIN-5): shortcuts that change a value are refused; those that open a view still work.
    // Copy (9) is refused too: it saves a channel or leaves the channel for the VFO.
    if app.set.chan_only && matches!(d, 0 | 2..=9) {
        return app.show_toast(b"Channel-only", b"hold # to leave", false);
    }
    match d {
        0 => home_channel(app, store),
        1 => app.open(Dest::BandScope), // SCOPE-1
        // HOME-4.2: where PTT can't transmit, hold 2 says why instead of changing a power that isn't used
        2 => match super::transmit::tx_block(app, app.active) {
            Some(r) => app.show_toast(b"No TX", super::transmit::short(r).as_bytes(), false),
            None => cycle(app, Item::Power),
        },
        3 => app.open(Dest::Value(Item::Squelch)),
        4 => {
            // WATCH-7: off, or back on as the set it was (A+B or A+B+C)
            let v = if app.watch == 0 { app.watch_on.clamp(1, 2) } else { 0 };
            app.change(Item::Watch, v as i32 - app.watch as i32);
            app.show_toast(b"Watch", [b"off" as &[u8], b"A+B", b"A+B+C"][v as usize], false);
        }
        5 => app.open(Dest::Tones),
        6 => cycle(app, Item::Step), // VFO only (VFO-8): on a channel the toast says so
        // VFO-26 (KEY-11): 7 the shift's options (simplex, +, -, split; open until OK or BACK), 8 the offset's
        // amount, typed; on a VFO and a channel alike (a channel through its rules, saved, EDIT-4).
        7 => app.open(Dest::Value(Item::Offset)),
        8 => offset_amount(app),
        // KEY-23: copy, one idea both ways. On a channel "Copy to VFO?" (VFO-23), on a VFO the save form (VFO-9).
        9 if on_ch => super::confirm::ask(app, super::confirm::Ask::CopyToVfo),
        _ => app.open(Dest::SaveChannel),
    }
}

/// Hold 8 (VFO-26): the offset amount typed, from empty (its Now row shows the slot's).
pub(crate) fn offset_amount(app: &mut App) {
    app.open(Dest::Entry(0));
    app.screen = Screen::Entry(super::entry::Entry::new(super::entry::EntryFor::OffsetAmount, None));
}

/// Hold 0: the active slot to the home channel, in channel mode, from VFO or any zone (CHAN-19, CHAN-20).
fn home_channel(app: &mut App, store: &impl ChannelStore) {
    match app.set.home_ch.and_then(|n| store.by_num(n)) {
        Some((c, _)) if c.hf_mode_only() => super::hf::recall(app, &c), // CHAN-26
        Some((c, pos)) => {
            let a = app.active as usize;
            let s = &mut app.slots[a];
            (s.zone, s.pos, s.ch, s.vfo_mode) = (c.zone, pos, Some(c), false);
            app.send_slot(a as u8);
            let mut l = Buf::<32>::new();
            crate::slot::chan_label(c.num, &mut l);
            app.show_toast(b"Home channel", l.as_bytes(), false);
        }
        None => app.show_toast(b"No home channel", b"hold ABC to set", false),
    }
}

/// Hold 2, 6, 7: the next value, shown as a toast (or why it can't change).
fn cycle(app: &mut App, item: Item) {
    let greyed = app.greyed(item).is_some();
    app.change(item, 1);
    if !greyed {
        let mut b = Buf::<16>::new();
        app.item_text(item, true, &mut b);
        app.show_toast(item.name().as_bytes(), b.as_bytes(), false);
    }
}

/// PTT down where the screen has no PTT of its own: home, then transmit on the active slot (KEY-7, TX-20).
pub fn transmit(app: &mut App) {
    // The slot the PTT keys was settled at key down (App::ptt_target, KEY-7).
    super::transmit::ptt(app, app.ptt_slot);
}
