//! # ferrite-app
//!
//! The UI state machine, the same on the radio and in the simulator. In: key down/up ([`App::key_down`],
//! [`App::key_up`]), knob steps ([`App::knob`]), the time ([`App::tick`]) and the Radio task's snapshot of the
//! three slots ([`LiveAll`]). Out: commands for the Radio task ([`App::pop_cmd`]), channel writes for the store's
//! owner ([`App::pop_write`]) and the screen, drawn through any [`Panel`] by [`App::render`], which redraws only
//! what changed (HOME-7).
//!
//! No heap and fixed-size state (`size_of::<App>()` is checked by a test). Channels come through
//! [`chan::ChannelStore`], read on demand.
//!
//! Screens live in [`screens`], one module each, plugged in through one registry ([`screens::Screen`]); how to
//! add one is in that module's documentation, and how each is reached in [`screens::nav`]. The App itself holds
//! what every screen shares: the three slots, the radio-wide values, the toast, undo, the key tracker and the
//! commands for the Radio task. Rules are cited by their manual IDs.

#![no_std]

#[cfg(test)]
extern crate std;

// `txt!` and `txtc!` (the text pack, build.rs): before the modules, which use them.
include!(concat!(env!("OUT_DIR"), "/txt_macros.rs"));

pub mod aprs;
pub mod aprs_card;
pub mod bcast;
pub mod aprs_live;
pub mod chan;
pub mod chanscan;
pub mod chfield;
pub mod data;
pub mod gpsdata;
pub mod keys;
pub mod lights;
pub mod local;
pub mod lpm;
pub mod link;
pub mod pager;
pub mod persist;
pub mod ports;
pub mod radio_data;
pub mod remote;
pub mod sample;
pub mod sat;
pub mod screens;
pub mod slotctl;
pub mod settings;
pub mod slot;
pub mod text;
pub mod words;
pub mod term;
pub mod uistate;

use crate::chan::ZoneId;
use chan::{ChanWrite, Channel, ChannelStore};
use screens::alert::AlertInfo;
use settings::Settings;
use ferrite_core::freq::{mhz, Freq10};
use ferrite_core::keys::Key;
use ferrite_core::msg::{LiveAll, Param, RadioCmd, SLOTS};
use ferrite_core::rxsleep::Level;
use ferrite_ui::text::Buf;
use keys::{KeyEvent, KeyTracker, Press};
use screens::nav::Dest;
use screens::shared::REGIONS;
use slot::{band_near, Item, SlotCfg, BANDS, DEFAULT_CFG, SQ_GLOBAL};

pub use ferrite_ui::panel::Panel;
pub use screens::Screen;
/// Maidenhead grid squares to and from a position (GPS-21), for Ferrite Console's channel positions (LINK-53).
pub use screens::lists::{grid, grid_pos};

/// Toasts show for 3 s (HOME-9); undo is offered for 5 s (EDIT-4, MENU-22).
pub const TOAST_MS: u32 = 3000;
/// SAFE-14: the "PTT stuck" popup stays this long after the release.
pub const STUCK_AFTER_MS: u32 = 2000;
/// A hold-digit shortcut's popup (HOME-8).
pub const POPUP_MS: u32 = 1200;
/// KEY-13: the Keypad lock's popup offers the wider modes.
pub const LOCK_ASK: &[u8] = "* all · 0 all but PTT".as_bytes();
pub const UNDO_MS: u32 = 5000;
/// [`App`]'s VOX flags (`vox_bits`).
const VOX_REFUSED: u8 = 1;
const VOX_SENT: u8 = 2;
const VOX_SCREEN: u8 = 4;
/// The refusal beep (SND-4, TEXT-5): short and low, unlike a key beep.
const REFUSED_HZ: u16 = 440;
const REFUSED_MS: u16 = 80;
/// A key beep (SND-3): short and high, so it never sounds like a refusal. Bench-confirm against stock's.
pub const KEY_BEEP_HZ: u16 = 1000;
pub const KEY_BEEP_MS: u16 = 30;
/// An alert's tone (SND-6, INBOX-4): 1050 Hz, as the weather service's attention tone, half a second each
/// second until acknowledged.
pub const ALERT_HZ: u16 = 1050;
pub const ALERT_MS: u16 = 500;
/// Edit in place ends after 8 s idle (EDIT-6); menus go home after the menu timeout (MENU-28, 0 7 9: 30 s).
pub const EDIT_IDLE_MS: u32 = 8000;
pub const MENU_TIMEOUT_MS: u32 = 30_000;
/// Watch pause (0 1 6 1, default 1 min).
pub const PAUSE_S: u16 = 60;
/// After a transmission, watch waits this long with the active slot quiet, or until a key (WATCH-16; nicFW's 30 s).
pub const TX_PAUSE_S: u16 = 30;

/// One slot: a VFO and a channel position; which one it is on.
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub vfo_mode: bool,
    pub vfo: Freq10,
    /// The VFO's settings.
    pub cfg: SlotCfg,
    /// The VFO's last frequency on each band (VFO-1).
    pub mem: [Freq10; BANDS.len()],
    pub zone: ZoneId,
    /// Position in the zone.
    pub pos: u16,
    /// A copy of the channel it is on, edits included.
    pub ch: Option<Channel>,
    /// The clarifier (VFO-22): a receive offset in 100 Hz, -25 to 25; never saved, gone when the slot is
    /// tuned again.
    pub clar: i8,
}

impl Slot {
    fn new(f: Freq10) -> Self {
        let mut mem = [0; BANDS.len()];
        for (m, b) in mem.iter_mut().zip(&BANDS) {
            *m = b.start;
        }
        let mut cfg = DEFAULT_CFG;
        let b = &BANDS[band_near(f)];
        cfg.mode = b.mode;
        cfg.step = b.step;
        Slot { vfo_mode: true, vfo: f, cfg, mem, zone: 0, pos: 0, ch: None, clar: 0 }
    }

    pub fn on_channel(&self) -> bool {
        !self.vfo_mode && self.ch.is_some()
    }

    pub fn freq(&self) -> Freq10 {
        match self.ch {
            Some(c) if !self.vfo_mode => c.rx,
            _ => self.vfo,
        }
    }

    pub fn cfg(&self) -> &SlotCfg {
        match &self.ch {
            Some(c) if !self.vfo_mode => &c.cfg,
            _ => &self.cfg,
        }
    }

    fn cfg_mut(&mut self) -> &mut SlotCfg {
        match &mut self.ch {
            Some(c) if !self.vfo_mode => &mut c.cfg,
            _ => &mut self.cfg,
        }
    }
}

#[derive(Clone, Copy)]
struct Toast {
    head: Buf<20>,
    body: Buf<28>,
    icon: bool,
    /// A shortcut's popup in the middle of the screen (HOME-8), not over the hint bar.
    center: bool,
    until: u32,
}

/// What a change replaced, for "* Undo".
#[derive(Clone, Copy)]
struct Undo {
    slot: u8,
    cfg: SlotCfg,
    /// The channel's own fields as they were, when the slot is on one (its zone, number, frequency and flags
    /// are edited too; a name is kept or dropped in the text entry).
    ch: Option<ChUndo>,
    home: Option<u16>,
    watch: u8,
    bsave: u8,
    until: u32,
}

/// A settings-list change, for "* Undo" (MENU-22): the item's path, the way the knob turned, and a hash of the
/// value it showed before the run of changes. Undo steps the other way until that value shows again: one
/// record (12 bytes) covers every radio-wide setting without a copy of the settings.
#[derive(Clone, Copy)]
pub(crate) struct SetUndo {
    pub path: [u8; 3],
    pub len: u8,
    pub dir: i8,
    pub was: u32,
    pub until: u32,
}

#[derive(Clone, Copy)]
struct ChUndo {
    /// The number (0-999, 10 bits) and, above it, the end tone and tail byte (5 bits, decision 86): one u16
    /// keeps the undo record 8 bytes.
    num: u16,
    zone: ZoneId,
    flags: u8,
    rx: Freq10,
}

/// Commands waiting for the Radio task: single commands in [`ferrite_core::cmdq`]'s queue (order kept, a newer
/// setting replacing a waiting one, MODE-6), and whole slots to send, which are made from the slot when popped. A
/// slot's frequency and nine settings are ten commands; sending a slot again before it has gone out costs nothing,
/// so start-up (three slots) and fast channel stepping never overflow the queue.
struct Outbox {
    q: ferrite_core::cmdq::CmdRing<32>,
    /// Slots whose full state is still to send, a bit each.
    sync: u8,
    /// The slot being sent and its next item (0 the frequency, then [`slot::params`], then the end tone and tail).
    cur: u8,
    step: u8,
}

impl Outbox {
    fn push(&mut self, c: RadioCmd) {
        // Full only if the Radio task stopped taking commands for long: the least important waiting one goes.
        let _ = self.q.push(c);
    }

    /// The whole slot goes out (again, from the start if it is being sent).
    fn sync(&mut self, slot: u8) {
        self.sync |= 1 << slot;
        if self.cur == slot {
            self.step = 0;
        }
    }

    fn pop(&mut self) -> Option<RadioCmd> {
        self.q.pop()
    }
}

/// What is on the screen now, so render can skip what hasn't changed.
#[derive(Clone, Copy)]
pub(crate) struct Drawn {
    /// Which screen the content area shows (its variant; a screen whose state changes keeps its page).
    pub screen: Option<core::mem::Discriminant<Screen>>,
    pub sbar: Option<ferrite_ui::widgets::StatusBar>,
    /// One hash per region; 0 = not drawn.
    pub hash: [u32; REGIONS],
    /// Each card's meter: lit segments and whether squelch was open.
    pub meter: [Option<(u8, bool)>; SLOTS],
    /// Each home card's geometry (slot, place, size, APRS) as last drawn: a change of border colour alone then
    /// redraws only the border, not the whole card (DISP-25).
    pub card: [u32; SLOTS],
    /// The scroll cue as last drawn; on the screen while region R_SCROLL says so ([`App::cue`]).
    pub cue: ferrite_ui::widgets::Cue,
    /// A popup just went: clear its box and redraw the screen's regions under it.
    pub popup_gone: bool,
    /// The quick menu's box just went (HOME-17): its rectangle is drawn again from the screen under it.
    pub qmenu_gone: bool,
    /// DISP-31: where a table's rows last ended ([`App::table`]): rows that end higher next time clear what was below.
    /// Two bytes in the padding before `popup`.
    pub table_end: i16,
    /// HOME-8: the shortcut popup on home as last drawn (its text's hash; 0 none): while it is up home waits under
    /// it, and the popup is drawn again only when its text changes (DISP-25).
    pub popup: u32,
}

/// [`App::alert`]'s bits.
pub(crate) const ALERT_UNREAD: u8 = 1;
const ALERT_WAIT: u8 = 2;

pub struct App {
    screen: Screen,
    slots: [Slot; SLOTS],
    rf_gain: u8,
    active: u8,
    /// The slot the PTT being pressed keys ([`App::ptt_target`]).
    pub(crate) ptt_slot: u8,
    /// The quick menu (hold \* on home, HOME-17) while it is up.
    pub(crate) qmenu: Option<screens::qmenu::QuickMenu>,
    /// HOME-17 (safety): PTT A (bit 0) and PTT B (bit 1) went down while the quick menu was up: dead until let go,
    /// whether or not the menu is still up ([`screens::qmenu::ptt_bit`]).
    pub(crate) ptt_block: u8,
    /// The theme in use before night red, for the quick menu's PTT B (0 7 1; RAM only).
    pub(crate) day_theme: u8,
    /// Index into Watch's values: off, A+B, A+B+C.
    watch: u8,
    /// The watch set hold 4 turns back on (WATCH-7, HOME-8): the last one that wasn't off.
    pub(crate) watch_on: u8,
    /// Battery Save: 0 off, 1-3.
    bsave: u8,
    /// The key lock: what it covers (KEY-14), chosen as it locked (KEY-13); `None` unlocked. Not stored (KEY-14.5).
    pub(crate) lock: Option<settings::Lock>,
    /// KEY-13: when the lock came on (the time's low 16 bits, bit 0 set; 0 none): for [`TOAST_MS`] after it * and 0
    /// choose the lock's mode (All, All but PTT) instead of being locked keys; cleared after that
    /// ([`App::lock_asking`]).
    lock_ask: u16,
    /// MENU-24: each group's (and third-level list's) last item, a nibble each ([`screens::groups::memo`]), for
    /// this visit to the menu: going home forgets them, so the menu starts on row 1 again.
    pub(crate) group_rows: [u8; screens::groups::MEMO_BYTES],
    inbox: u8,
    /// The event log's unread entries (INBOX-7): the log's own count, not the mail badge's (INBOX-3.1).
    pub(crate) log_unread: u8,
    /// An unread alert (the bar's alert mark); [`ALERT_WAIT`]: one came in while the crossband repeater ran,
    /// shown as a banner, and its screen opens when the repeater stops (XB-19).
    alert: u8,
    /// VOX keyed the active slot and holds it (VOX-13).
    vox_on: bool,
    /// Dry TX (TX-48, 0 8 8 5): the PA drive held at 0 on every key-up of a `tx` build. Not a setting: off at
    /// every power-on and never stored, changed only on the radio's own menu (never by the link or terminal).
    dry_tx: bool,
    /// Low Power Mode's flags ([`lpm`]: in it, Auto held off, the side keys' hold), the battery % Auto looks at
    /// (PWR-24) and when the side keys' hold began.
    lpm: u8,
    batt: u8,
    lpm_at: u32,
    /// GPS-1.5: 0 6 1's Auto, whether something has the module powered ([`gpsdata::auto_step`]).
    pub(crate) gps_auto: gpsdata::GpsAuto,
    /// VOX's flags: [`VOX_REFUSED`], speech refused on an emergency frequency (TX-35: the toast once per burst);
    /// [`VOX_SENT`] and [`VOX_SCREEN`], whether the screen lets VOX sample as last sent to the Radio task (VOX-3,
    /// [`App::vox_screen`]). One byte: the App's RAM budget (`ram_is_small`).
    vox_bits: u8,
    /// Remote PTT over the link holds the key (LINK-20, [`remote`]); why the last one ended; a VOX or remote PTT
    /// press is going through the PTT path now (not a person's); the local PTTs held, and those whose press ended
    /// a remote transmission (dead until let go), bits from [`remote::bit`].
    pub(crate) remote: Option<remote::Remote>,
    pub(crate) remote_ended: Option<remote::RemoteEnd>,
    /// The PTT release in progress is a cut (LINK-20: a remote key-up lost), sent as [`RadioCmd::TxCut`].
    pub(crate) tx_cut: bool,
    /// SAFE-14: the "PTT stuck" popup has shown this power-on.
    pub(crate) stuck_shown: bool,
    pub(crate) virtual_ptt: bool,
    /// CAL-20: About last drew the calibration as "Changed" (OK asks to use it as it is).
    pub(crate) cal_changed: bool,
    /// LINK-19: the keys the link holds and its long press ([`remote::RemoteKeys`]); a remote key's input is being
    /// handled now (it never transmits: [`App::remote_tx_refused`]).
    pub(crate) rkeys: remote::RemoteKeys,
    pub(crate) remote_in: bool,
    /// Refusal beeps sent since power-on (SND-4), wrapping: the refused screen's readback shows it (`beeps=`), as
    /// the 80 ms beep is too short to catch with `gpio` samples over the link.
    pub(crate) refusal_beeps: u8,
    /// TX-30.2: when the timeout's beep is due (0 none): a moment after the unkey, the transmitter off.
    pub(crate) timeout_beep: u32,
    /// A hash of the callsign the Radio task has for CW ID ([`App::send_tx_tones`]): [`App::tick`] sends it again
    /// whenever the setting's differs, whatever changed it (setup's callsign page typed into the setting and sent
    /// nothing: CW ID kept the old one until a restart, XB-12 on the radio, Oct 15, 2026).
    pub(crate) cw_sent: u32,
    /// Who runs the command line now (LINK-34's `CMD` sets it for its line): [`remote::BY_CABLE`], [`remote::BY_BT`],
    /// else [`remote::BY_TERM`] (the terminal); `xb start` logs it (XB-20.6).
    pub(crate) cmd_by: u8,
    /// Redraws that changed something, and when the last was (the link's UI readback: LINK-19).
    pub(crate) frames: u32,
    pub(crate) drawn_at: u32,
    pub(crate) local_ptt: u8,
    pub(crate) ptt_cut: u8,
    /// The second of the alert tone's last beep, with the top bit set (SND-6, INBOX-4).
    alert_beep: u16,
    toast: Option<Toast>,
    undo: Option<Undo>,
    /// A radio-wide setting changed in a settings list, for "* Undo" (MENU-22).
    pub(crate) set_undo: Option<SetUndo>,
    /// The last settings changed, newest first (MENU-8).
    recent: [Option<Item>; 3],
    keys: KeyTracker,
    now: u32,
    last_input: u32,
    /// HOME-13: the last knob step, while home holds its heavier redraws ([`App::knob_busy`]).
    pub(crate) knob_at: Option<u32>,
    paused_until: Option<u32>,
    /// The pause is the one after a transmission (WATCH-16): a key press ends it, a signal on the active slot
    /// extends it. The # pause (WATCH-6) is neither.
    tx_pause: bool,
    /// The last activity for the backlight: keys, PTT, and receive and transmit as 0 7 4 4 says ([`lights`]).
    seen: u32,
    /// The last activity of any kind, receive and transmit too, for auto power off ([`lights`]); and the last key
    /// or knob step, the waking one too, for the keypad light's "on key press" (DISP-22).
    awake: u32,
    pressed: u32,
    /// The last link frame of a big transfer, for the LED's transfer flash (DISP-30, [`lights`]).
    xfer: u32,
    /// UPD-16.2: the SPI part being written and how many (`DATA_WRITE` `PART`); (0, 0) before any.
    pub(crate) update_part: (u8, u8),
    /// UPD-16.2: the last raw request was the read-back (`DATA_READ` CRC), not a write.
    pub(crate) update_checking: bool,
    /// [`lights`]'s flags: dark, warned of auto off, power off wanted, the waking key swallowed, a transfer seen.
    lit: u8,
    /// The APRS settings from the APRS store ([`aprs`]), and whether they were loaded and need writing back.
    aprs: aprs::AprsCfg,
    aprs_st: u8,
    /// The armed reset's # was held 3 s ([`screens::confirm`]): done at the next tick, which has the store.
    want_reset: bool,
    /// A clock set by hand (GPS-23), UTC seconds, for the clock's owner to take ([`App::take_clock_set`]).
    clock_set: Option<u32>,
    /// From the last snapshot: slots receiving, scanning, skipped by the relay rule.
    rx_mask: u8,
    scan_mask: u8,
    /// UPD-16: the finish-the-update page has been shown this power-on (BACK leaves it for good).
    update_told: bool,
    /// SCAN-6: each slot's scan's last hit (0 none), for Return to "Last hit".
    pub(crate) last_hit: [Freq10; SLOTS],
    relay_mask: u8,
    /// The tone hint per slot (TSRCH-13, TSRCH-16): the tone word the Radio task heard, kept from the last tick
    /// for the quick settings' Tones row.
    pub(crate) heard: [u16; SLOTS],
    /// PWR-24.8: the active slot's last reception: (slot, opened, closed (0 while open or never), peak dBm). RAM
    /// only; Low Power Mode's home shows it.
    pub(crate) last_rx: (u8, u32, u32, i16),
    /// Where each scanning slot is stopped on a hit (held there, through the resume wait); 0 when it isn't, or
    /// only passing a busy step. PTT transmits there (SCAN-19).
    scan_hit: [Freq10; SLOTS],
    /// Slots with monitor latched (SIDE1 tap): MON on the card.
    pub(crate) monitor: u8,
    /// SQL-9.1: the slot (plus 1; 0 none) whose squelch SIDE1 held alone opened, closed when SIDE1 is let go.
    pub(crate) mon_held: u8,
    /// Quick mute (\* on home, HOME-15): slots received but never heard, a bit per slot. RAM only.
    pub(crate) mute: u8,
    /// Slots turned off (\* \* on home, HOME-15.1): not received or watched, a bit per slot. Never with a mute bit.
    /// RAM only, as the mute.
    pub(crate) off: u8,
    /// HOME-15.1: when \* last muted a slot (`now` as u16, low bit set; 0 none): a second \* within
    /// a second (`STAR_OFF_MS` in home.rs) turns it off instead of unmuting.
    pub(crate) star_at: u16,
    out: Outbox,
    /// The channel write for the store's owner ([`App::pop_write`]): one, as every input makes at most one and
    /// the owner drains after every input.
    write: Option<ChanWrite>,
    /// The text being typed (A7): a channel name, a callsign; shared by the text entry and the forms that
    /// open it, as only one text is edited at a time.
    text: Buf<16>,
    /// The contact whose name is being typed (APRS-37), between its callsign's entry and its name's.
    contact_call: Buf<10>,
    /// Where BACK returns from screens opened with [`App::open`], newest last (MENU-24).
    back: [Option<Screen>; BACK_DEPTH],
    pub(crate) drawn: Drawn,
    /// The zones page's Nearby row (CHAN-29): the nearest positioned channel, found again only when the fix moved
    /// about 100 m or the channels changed. 12 bytes.
    pub(crate) near: screens::zones::NearCache,
    /// The Nearby view's rows (CHAN-15).
    pub(crate) near_list: screens::zones::NearList,
    /// The channel browser's header count and what it was counted for (0: none yet).
    pub(crate) view_count: (u32, u16),
    /// The browser picker's band view counts and the records' version they were counted for (0: none yet).
    pub(crate) band_counts: (u32, [u16; 5]),
    /// The Local zone (CHAN-34): its channels, nearest first, found again only when the position moved about 1 km,
    /// the channels or the range changed. 112 bytes.
    pub(crate) local: local::Local,
    /// Radio activity: transmit, the exclusive modes, scan hits, region ([`screens::activity`]).
    pub(crate) radio: screens::activity::RadioUi,
    /// Radio-wide settings: not the App's, only borrowed ([`settings`]).
    set: &'static mut Settings,
    /// The last alert, for the alert screen (WX-5): kept outside the App too, as it outlives the screen.
    pub(crate) alert_info: &'static mut AlertInfo,
    /// The user's answer to a link write approval, for the Link task.
    link_answer: Option<screens::link::LinkAnswer>,
    /// Satellite mode (chapter 28a): the platform's satellites and passes, and the App's own few bytes.
    satd: Option<&'static mut dyn ferrite_core::sat::data::SatData>,
    pub(crate) sat: sat::SatUi,
}

/// Screens remembered for BACK: menu, group, item and one more.
pub(crate) const BACK_DEPTH: usize = 4;

/// [`Outbox::cur`] with no slot being sent.
const NO_SLOT: u8 = 0xFF;

impl App {
    /// Three slots: A on 146.520, B on 443.475, C on 118.950 (the look review's sample), all VFO. The settings
    /// (stored, or [`Settings::DEFAULT`] on a first boot) and the last alert record ([`AlertInfo::NONE`]) are
    /// the caller's, borrowed for the App's lifetime: the App holds only UI state.
    pub fn new(set: &'static mut Settings, alert: &'static mut AlertInfo) -> Self {
        let mut app = App {
            screen: Screen::Home,
            slots: [Slot::new(mhz(146) + 52_000), Slot::new(mhz(443) + 47_500), Slot::new(mhz(118) + 95_000)],
            active: 0,
            rf_gain: 0,
            ptt_slot: 0,
            qmenu: None,
            ptt_block: 0,
            day_theme: 0,
            watch: 1,
            watch_on: 1,
            bsave: 1,
            lock: None,
            lock_ask: 0,
            group_rows: [0; screens::groups::MEMO_BYTES],
            inbox: 0,
            log_unread: 0,
            alert: 0,
            vox_on: false,
            dry_tx: false,
            lpm: 0,
            batt: lpm::BATT_UNKNOWN,
            lpm_at: 0,
            gps_auto: gpsdata::GpsAuto::new(),
            vox_bits: 0,
            remote: None,
            remote_ended: None,
            tx_cut: false,
            stuck_shown: false,
            virtual_ptt: false,
            cal_changed: false,
            rkeys: remote::RemoteKeys::default(),
            remote_in: false,
            refusal_beeps: 0,
            timeout_beep: 0,
            cw_sent: 0,
            cmd_by: remote::BY_TERM,
            frames: 0,
            drawn_at: 0,
            local_ptt: 0,
            ptt_cut: 0,
            alert_beep: 0,
            toast: None,
            undo: None,
            set_undo: None,
            recent: [None; 3],
            keys: KeyTracker::new(),
            now: 0,
            last_input: 0,
            knob_at: None,
            paused_until: None,
            tx_pause: false,
            seen: 0,
            awake: 0,
            pressed: 0,
            xfer: 0,
            update_part: (0, 0),
            update_checking: false,
            lit: 0,
            aprs: aprs::AprsCfg::DEFAULT,
            aprs_st: 0,
            want_reset: false,
            clock_set: None,
            rx_mask: 0,
            scan_mask: 0,
            update_told: false,
            last_hit: [0; SLOTS],
            relay_mask: 0,
            scan_hit: [0; SLOTS],
            heard: [0; SLOTS],
            last_rx: (0xFF, 0, 0, i16::MIN),
            monitor: 0,
            mon_held: 0,
            mute: ((set.aprs_mode && set.aprs_mute) as u8) << aprs::APRS_SLOT, // APRS-15.2: muted at power on in APRS mode
            off: 0,
            star_at: 0,
            out: Outbox { q: ferrite_core::cmdq::CmdRing::new(), sync: 0, cur: NO_SLOT, step: 0 },
            write: None,
            text: Buf::new(),
            contact_call: Buf::new(),
            back: [None; BACK_DEPTH],
            near: screens::zones::NearCache::NONE,
            near_list: screens::zones::NearList::NONE,
            view_count: (0, 0),
            band_counts: (0, [0; 5]),
            local: local::Local::OFF,
            drawn: Drawn { screen: None, sbar: None, hash: [0; REGIONS], meter: [None; SLOTS], card: [0; SLOTS], cue: ferrite_ui::widgets::Cue { y: 0, h: 0, ty: 0, th: 0 }, popup_gone: false, qmenu_gone: false, table_end: 0, popup: 0 },
            radio: screens::activity::RadioUi::new(),
            set,
            alert_info: alert,
            link_answer: None,
            satd: None,
            sat: sat::SatUi::new(),
        };
        app.region_steps(); // VFO-18: the region's step on 2 m and 1.25 m (saved slots bring their own)
        for s in 0..SLOTS as u8 {
            app.send_slot(s);
        }
        app.out.push(RadioCmd::Active(0));
        app.out.push(RadioCmd::Watch(app.watch_mask()));
        app.send_radio_settings();
        app.out.push(RadioCmd::BatterySave(level(app.bsave)));
        app
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// The page the content area shows, as a hash that changes when a screen turns its page (HF's loading page,
    /// setup): the simulator's flicker survey counts that as a change of screen, not a live update (DISP-25).
    pub fn page_shown(&self) -> u32 {
        self.drawn.hash[screens::shared::R_PAGE]
    }

    pub fn active(&self) -> u8 {
        self.active
    }

    pub fn slot(&self, i: u8) -> &Slot {
        &self.slots[i as usize % SLOTS]
    }

    /// A toast is showing.
    pub fn toast_up(&self) -> bool {
        self.toast.is_some()
    }

    pub fn locked(&self) -> bool {
        self.lock.is_some()
    }

    /// What the lock covers while locked (KEY-14), else `None`.
    pub fn lock_mode(&self) -> Option<settings::Lock> {
        self.lock
    }

    /// Locks the keys in Keypad mode (KEY-13; auto lock, KEY-20); `ask` keeps * and 0 for choosing a wider mode for
    /// the popup's time.
    pub(crate) fn lock_keys(&mut self, ask: bool) {
        self.lock = Some(settings::Lock::Keypad);
        self.lock_ask = if ask { self.now as u16 | 1 } else { 0 };
    }

    /// For the lock popup's time * and 0 choose the lock's mode (KEY-13).
    fn lock_asking(&self) -> bool {
        self.lock_ask != 0 && ((self.now as u16).wrapping_sub(self.lock_ask & !1) as u32) < TOAST_MS
    }

    /// Unlocks the keys (KEY-13, KEY-16).
    pub(crate) fn unlock(&mut self) {
        self.lock = None;
        self.lock_ask = 0;
    }

    /// Off, A+B, A+B+C.
    pub fn watch(&self) -> u8 {
        self.watch
    }

    /// The next command for the Radio task: single commands first, then whole slots, made from the slot now
    /// (so they always carry its latest state).
    pub fn pop_cmd(&mut self) -> Option<RadioCmd> {
        if let Some(c) = self.out.pop() {
            return Some(c);
        }
        if self.out.sync == 0 {
            return None;
        }
        if self.out.cur == NO_SLOT {
            (self.out.cur, self.out.step) = (self.out.sync.trailing_zeros() as u8, 0);
        }
        let slot = self.out.cur;
        let c = match self.out.step {
            0 => RadioCmd::TuneSlot { slot, freq: self.tuned_freq(slot) },
            // After the slot's settings: its end tone and tail (the channel's over the radio's), and whether its
            // channel is receive-only (CHAN-4, for the Radio task's own refusal: TX audit F11).
            k if k as usize > slot::params(&DEFAULT_CFG).len() => {
                let (end, tail) = self.slot_tones(slot);
                match k as usize - slot::params(&DEFAULT_CFG).len() {
                    1 => RadioCmd::SlotParam { slot, param: Param::EndTone, value: end as i32 },
                    2 => RadioCmd::SlotParam { slot, param: Param::Tail, value: tail as i32 },
                    _ => RadioCmd::SlotParam { slot, param: Param::RxOnly, value: self.rx_only(slot) as i32 },
                }
            }
            k => {
                let (param, value) = slot::params(&self.tuned_cfg(slot))[k as usize - 1];
                let value = if param == Param::Squelch { self.sq_level(slot) as i32 } else { value };
                RadioCmd::SlotParam { slot, param, value }
            }
        };
        self.out.step += 1;
        if self.out.step as usize > slot::params(&DEFAULT_CFG).len() + 3 {
            self.out.sync &= !(1 << slot);
            self.out.cur = NO_SLOT;
        }
        Some(c)
    }

    /// The next channel write for the store's owner (the Storage task; the simulator's store). Drained, like
    /// [`App::pop_cmd`], after every input.
    pub fn pop_write(&mut self) -> Option<ChanWrite> {
        self.write.take()
    }

    /// Queues a write; a later write of the same channel replaces it (keeping where it came from).
    pub(crate) fn write(&mut self, w: ChanWrite) {
        if let (Some(ChanWrite::Put { ch: c, from: f }), ChanWrite::Put { ch, from }) = (self.write, w) {
            if c.num == ch.num || Some(c.num) == from {
                self.write = Some(ChanWrite::Put { ch, from: f.or(from).filter(|&n| n != ch.num) });
                return;
            }
        }
        debug_assert!(self.write.is_none(), "one channel write per input: {:?} then {:?}", self.write, w);
        self.write = Some(w);
    }

    /// A channel changed outside the radio's own editor (the terminal: TERM-9): saved as an edit there is, and
    /// every slot on it follows at once.
    pub(crate) fn put_channel(&mut self, c: chan::Channel) {
        self.put_channel_as(c, ChanWrite::Put { ch: c, from: None });
    }

    /// [`App::put_channel`] with its position set or cleared (the terminal's `ch N pos=`, CHAN-2).
    pub(crate) fn put_channel_pos(&mut self, c: chan::Channel, pos: Option<data::Pos>) {
        self.put_channel_as(c, ChanWrite::PutPos { ch: c, pos });
    }

    /// LINK-41.1: [`App::put_channel`] for a link's `WRITE_MANY`: straight into the store's queue when it takes it.
    pub(crate) fn put_channel_queued(&mut self, c: chan::Channel, st: &impl chan::ChannelStore) {
        if st.queue_write(ChanWrite::Put { ch: c, from: None }) {
            self.channel_put(c);
        } else {
            self.put_channel(c);
        }
    }

    /// LINK-53: a channel's position set or cleared from the link: into the store's queue when it takes it.
    pub(crate) fn put_pos_queued(&mut self, num: u16, pos: Option<data::Pos>, st: &impl chan::ChannelStore) {
        let w = ChanWrite::Pos { num, pos };
        if !st.queue_write(w) {
            self.write(w);
        }
    }

    fn put_channel_as(&mut self, c: chan::Channel, w: ChanWrite) {
        self.write(w);
        self.channel_put(c);
    }

    /// Slots on a channel just written take its new settings.
    fn channel_put(&mut self, c: chan::Channel) {
        for i in 0..SLOTS {
            let s = &mut self.slots[i];
            if s.ch.is_some_and(|o| o.num == c.num) {
                let moved = s.ch.is_some_and(|o| o.rx != c.rx || o.cfg != c.cfg);
                (s.ch, s.zone) = (Some(c), c.zone);
                if moved && s.on_channel() {
                    self.send_slot(i as u8);
                }
            }
        }
    }

    /// A channel given another number by the terminal (TERM-14): the record moves, and slots on it and the home
    /// channel go with it (CHAN-18), as the radio's editor does (EDIT-5).
    #[cfg_attr(not(feature = "vt100"), allow(dead_code))] // the terminal pages' renumbering
    pub(crate) fn renumber_channel(&mut self, c: chan::Channel, to: u16) {
        let n = chan::Channel { num: to, ..c };
        self.write(ChanWrite::Put { ch: n, from: Some(c.num) });
        for s in self.slots.iter_mut() {
            if s.ch.is_some_and(|o| o.num == c.num) {
                s.ch = Some(n);
            }
        }
        if self.set.home_ch == Some(c.num) {
            self.set.home_ch = Some(to);
        }
    }

    /// A channel write is waiting for [`App::pop_write`] (the terminal runs its next line only after it).
    pub fn write_pending(&self) -> bool {
        self.write.is_some()
    }

    /// Deletes a channel (CHAN-13): slots on it go to VFO, and it stops being the home channel (CHAN-18).
    pub(crate) fn delete_channel(&mut self, num: u16, store: &impl chan::ChannelStore) {
        if !store.data().request(data::Op::DeleteChannel(num)) {
            self.write(ChanWrite::Delete(num)); // a store whose records have no Data owner yet
        }
        self.channel_gone(num);
    }

    /// LINK-41.2: [`App::delete_channel`] for a link's `WRITE_MANY`: straight into the store's queue, so deletes
    /// in one sector share its rewrite (decision 227).
    pub(crate) fn delete_channel_queued(&mut self, num: u16, store: &impl chan::ChannelStore) {
        if store.queue_write(ChanWrite::Delete(num)) {
            self.channel_gone(num);
        } else {
            self.delete_channel(num, store);
        }
    }

    /// Slots on a deleted channel go to VFO, and it stops being the home channel (CHAN-13, CHAN-18).
    fn channel_gone(&mut self, num: u16) {
        for s in 0..SLOTS {
            if self.slots[s].ch.is_some_and(|c| c.num == num) {
                let sl = &mut self.slots[s];
                (sl.ch, sl.vfo_mode) = (None, true);
                self.send_slot(s as u8);
            }
        }
        if self.set.home_ch == Some(num) {
            self.set.home_ch = None;
        }
    }

    /// CHAN-12.1: zone `z` was deleted (on the radio or over the link): the slots in it, and their copies of its
    /// channels, are in no zone now, so a later edit of the channel doesn't write the old zone back.
    pub(crate) fn zone_deleted(&mut self, z: ZoneId) {
        for s in self.slots.iter_mut().filter(|s| s.zone == z) {
            s.zone = chan::NO_ZONE;
            if let Some(c) = &mut s.ch {
                c.zone = chan::NO_ZONE;
            }
        }
    }

    /// Deletes every channel (the reset's second step, PIN-6; the terminal's `clear channels`): every slot to VFO.
    pub(crate) fn erase_channels(&mut self) {
        self.write(ChanWrite::EraseAll);
        for s in self.slots.iter_mut() {
            (s.ch, s.vfo_mode, s.zone, s.pos) = (None, true, 0, 0);
        }
        for s in 0..SLOTS as u8 {
            self.send_slot(s);
        }
        self.set.home_ch = None;
    }

    /// The channel the active slot is on was changed (a setting, its zone, number, name): save it (EDIT-4).
    pub(crate) fn save_channel(&mut self, slot: u8, from: Option<u16>) {
        if let Some(ch) = self.slots[slot as usize].ch.filter(|_| self.slots[slot as usize].on_channel()) {
            self.write(ChanWrite::Put { ch, from });
        }
    }

    /// SCAN-19: PTT on a scanning slot ends the scan; stopped on a hit, the slot moves there (its channel, or its
    /// VFO), so the transmission goes out on the hit; otherwise on the frequency the scan started from.
    /// The slot a PTT keys (KEY-7): PTT B keys slot B; PTT A and the side connector's key the active slot.
    pub(crate) fn ptt_target(&self, k: u8) -> u8 {
        if k == ferrite_core::keys::PTT_B { 1 } else { self.active }
    }

    fn scan_ptt(&mut self, slot: u8, store: &impl ChannelStore) {
        if self.scan_mask & 1 << slot == 0 {
            return;
        }
        self.out.push(RadioCmd::Scan { slot, on: false });
        self.scan_mask &= !(1 << slot);
        let f = core::mem::take(&mut self.scan_hit[slot as usize]);
        self.scan_move(slot, f, store);
    }

    /// SCAN-6: SIDE2 stops a scan; with Return to "Last hit" (0 1 6 4) the slot moves to the scan's last hit,
    /// as a PTT on a hit does (SCAN-19); else it stays where the scan started.
    pub(crate) fn stop_scan(&mut self, slot: u8, store: &impl ChannelStore) {
        self.out.push(RadioCmd::Scan { slot, on: false });
        let f = core::mem::take(&mut self.last_hit[slot as usize % SLOTS]);
        if self.set.scan_return_hit {
            self.scan_move(slot, f, store);
        }
    }

    /// The slot to a scan's hit `f` (none: 0): its channel on a channel scan, else its VFO.
    fn scan_move(&mut self, slot: u8, f: Freq10, store: &impl ChannelStore) {
        let s = self.slots[slot as usize];
        if f == 0 || s.on_channel() && s.ch.is_some_and(|c| c.rx == f) {
            return;
        }
        if s.on_channel() {
            if let Some((z, pos)) = self.scan_chan(slot, f, store) {
                self.set_channel(slot, z, pos, store);
            }
            return;
        }
        let from = core::mem::replace(&mut self.slots[slot as usize].vfo, f);
        self.hf_starts_low(slot, from); // TX-36
        self.send_slot(slot);
    }

    /// A channel scan's channel on `f` as (zone, position): the slot's zone first, then the zones added to the
    /// scan (SCAN-3).
    pub(crate) fn scan_chan(&self, slot: u8, f: Freq10, store: &impl ChannelStore) -> Option<(ZoneId, u16)> {
        let s = self.slots[slot as usize % SLOTS];
        let zones = core::iter::once(s.zone).chain((0..10 as ZoneId).filter(|z| self.radio.zones & 1 << z != 0));
        for z in zones {
            if let Some(pos) = (0..self.zlen(store, z)).find(|&i| self.zat(store, z, i).is_some_and(|c| c.rx == f)) {
                return Some((z, pos));
            }
        }
        None
    }

    /// Put a slot on a channel (setup, or the simulator's sample): channel mode at that zone position.
    pub fn set_channel(&mut self, slot: u8, zone: ZoneId, pos: u16, store: &impl ChannelStore) {
        match self.zat(store, zone, pos) {
            Some(c) if c.hf_mode_only() => screens::hf::recall(self, &c), // CHAN-26: HF mode's, not a slot's
            Some(c) => {
                let s = &mut self.slots[slot as usize % SLOTS];
                (s.zone, s.pos, s.ch, s.vfo_mode) = (zone, pos, Some(c), false);
                self.send_slot(slot);
            }
            None => {}
        }
    }

    /// Watch: 0 off, 1 A+B, 2 A+B+C (setup, or the simulator's sample).
    pub fn set_watch(&mut self, v: u8) {
        self.put_watch(v % 3);
    }

    /// Watch to v (0-2), told to the Radio task; a set that isn't off is the one hold 4 turns back on (WATCH-7).
    fn put_watch(&mut self, v: u8) {
        self.watch = v;
        if v != 0 {
            self.watch_on = v;
        }
        self.out.push(RadioCmd::Watch(self.watch_mask()));
    }

    /// A slot's VFO settings (setup, or the simulator's sample).
    pub fn set_vfo_cfg(&mut self, slot: u8, cfg: SlotCfg) {
        self.slots[slot as usize % SLOTS].cfg = cfg;
        self.send_slot(slot % SLOTS as u8);
    }

    /// Everything is drawn again on the next render.
    pub fn redraw_all(&mut self) {
        self.drawn.screen = None;
        self.drawn.sbar = None;
    }

    /// The clock set by hand at 0 6 6 (GPS-23): UTC seconds for the clock's owner (the GPS loop on the radio,
    /// the simulator) to take; GPS time takes over again when a fix comes, while 0 6 6 says "GPS" (GPS-7).
    pub(crate) fn set_clock_by_hand(&mut self, utc: u32) {
        self.clock_set = Some(utc);
    }

    /// A clock set by hand since the last call ([`App::set_clock_by_hand`]).
    pub fn take_clock_set(&mut self) -> Option<u32> {
        self.clock_set.take()
    }

    /// A decoder's hit (DEC-7): the inbox count, and a toast only while Popups (0 4 4) is on.
    pub fn notify_decode(&mut self, head: &[u8], body: &[u8]) {
        if self.set.popups {
            self.notify(head, body, false);
        } else {
            self.log_unread = self.log_unread.saturating_add(1);
        }
    }

    /// A message to us arrived (APRS-32): the mail badge's count (INBOX-3.1) and its log entry's, with a toast.
    pub fn notify_message(&mut self, head: &[u8], body: &[u8]) {
        self.inbox = self.inbox.saturating_add(1);
        self.notify(head, body, false);
        // DISP-15.7: the APRS slot's packets don't wake the screen; a message to us does, as receive would.
        if self.set.wake_on >= 1 {
            self.seen = self.now;
        }
        if self.aprs.sound != 0 {
            self.out.push(RadioCmd::Chime { sound: self.aprs.sound, level: self.set.msg_level() }); // APRS-6.1, APRS-6.2
        }
    }

    /// A decode, arrival or alert was logged: a toast and the log's unread count (SBAR-6); an alert also opens the
    /// alert screen, with `body` as its event and `head` as its originator.
    pub fn notify(&mut self, head: &[u8], body: &[u8], alert: bool) {
        self.log_unread = self.log_unread.saturating_add(1);
        self.alert |= alert as u8 * ALERT_UNREAD;
        // a message's or an alert's own text, cut to the toast's line on purpose: the inbox, the log and the alert
        // screen have it whole
        use ferrite_ui::widgets as w;
        self.show_toast(head, w::fit(body, &ferrite_ui::assets::UI_12, w::toast_room(head, true)), true);
        if alert {
            let mut a = screens::alert::AlertInfo::NONE;
            a.event.set(body);
            a.from.set(head);
            *self.alert_info = a;
            self.open_alert(); // WX-4: over any screen
        }
    }

    /// The alert screen over any screen (WX-4), except while the crossband repeater runs: then a banner, and the
    /// screen once the repeater stops; the repeater keeps running (XB-19).
    fn open_alert(&mut self) {
        if screens::xb::running(self) {
            self.alert |= ALERT_WAIT;
            let event = self.alert_info.event;
            let room = ferrite_ui::widgets::toast_room(b"ALERT", true); // the event cut on purpose, as in notify
            self.show_toast(b"ALERT", ferrite_ui::widgets::fit(event.as_bytes(), &ferrite_ui::assets::UI_12, room), true);
        } else {
            self.alert &= !ALERT_WAIT;
            self.open(Dest::Alert);
        }
    }

    /// An alert with everything the decoder knows (WX-5): over any screen, flashing, until OK (WX-4, WX-6).
    pub fn raise_alert(&mut self, a: &screens::alert::AlertInfo) {
        self.log_unread = self.log_unread.saturating_add(1);
        self.alert |= ALERT_UNREAD;
        *self.alert_info = *a;
        self.open_alert();
    }

    /// The counts at power on: unread messages to us (the mail badge, INBOX-3.1) and the log's unread entries
    /// (INBOX-7).
    pub fn set_inbox(&mut self, messages: u16, log: u16) {
        self.inbox = messages.min(255) as u8;
        self.log_unread = log.min(255) as u8;
    }

    /// A SAME header decoded on a slot (WX-4, WX-5): the alert screen over any screen, with its event, areas,
    /// issue and expiry (local time), originator, slot and frequency. Tests (RWT, RMT) only toast and count in
    /// the inbox. Nothing happens with SAME alerts off or outside the US and Canada (WX-3). Returns whether the
    /// full-screen alert went up. The decoder (DSP task) hands headers to the UI task, which calls this.
    pub fn same_alert(&mut self, h: &ferrite_core::same::SameHeader, slot: u8, freq: Freq10) -> bool {
        use ferrite_core::same;
        if !self.set.same_on() {
            return false;
        }
        let mut name = Buf::<30>::new();
        match same::event_name(&h.event) {
            Some(n) => name.set(n.as_bytes()),
            None => {
                name.set(b"Alert ");
                name.push(&h.event);
            }
        }
        if same::is_test(&h.event) {
            self.log_unread = self.log_unread.saturating_add(1);
            self.show_toast(b"SAME test", name.as_bytes(), true);
            return false;
        }
        // WX-10: with location codes set, a warning counts only when one of its areas is in the list; with
        // none set, every warning does.
        let codes = &self.set.same_codes;
        if codes.iter().any(|&c| c != 0) && !h.areas[..h.n_areas as usize].iter().any(|a| codes.contains(a)) {
            return false;
        }
        let mut a = AlertInfo::NONE;
        a.event.set(name.as_bytes());
        let mut areas = Buf::<20>::new();
        let _ = ufmt::uwrite!(areas, "{}", ferrite_ui::text::Zero(h.areas[0], 6));
        if h.n_areas > 1 {
            let _ = ufmt::uwrite!(areas, " +{}", h.n_areas - 1);
        }
        a.areas.set(areas.as_bytes());
        let sender = h.sender.iter().rposition(|&c| c != b' ').map_or(&h.sender[..0], |n| &h.sender[..=n]);
        a.from.set(if sender.is_empty() { same::org_name(&h.org).as_bytes() } else { sender });
        let tz = settings::TIME_ZONES[self.set.tz as usize % settings::TIME_ZONES.len()] as i32 * 15;
        a.issued = (h.issued_min as i32 + tz).rem_euclid(1440) as u16;
        a.until = (a.issued as u32 + h.purge_min as u32).rem_euclid(1440) as u16;
        (a.slot, a.freq) = (slot, freq);
        self.raise_alert(&a);
        true
    }

    /// First boot (no layout header: BOOT-12): setup runs, and BACK on its first page has nowhere to go.
    pub fn first_boot(&mut self) {
        self.go_home();
        self.screen = Screen::Setup(screens::setup::Setup::new(true));
    }

    /// At power on, after the self-test: the PIN, if one is set (PIN-1, BOOT-13).
    pub fn ask_pin(&mut self) {
        if self.set.pin_len > 0 && self.set.pin_mode > 0 {
            self.go_home();
            self.screen = Screen::Pin(screens::pin::Pin::power_on());
        }
    }

    // ---- events the firmware (and the simulator) report: each opens its screen ----

    /// The volume knob turned to off (the power switch, PWR-17): "Saving...", modal, while the writes finish;
    /// then the firmware cuts the power. Nothing else happens on screen after this.
    pub fn power_off(&mut self) {
        self.go_home();
        self.screen = Screen::Info(screens::info::Info { page: screens::info::Page::PowerOff });
    }

    /// The battery reached empty (PWR-16): the empty-battery screen, modal, before the firmware switches off.
    pub fn battery_empty(&mut self) {
        self.go_home();
        self.screen = Screen::Info(screens::info::Info { page: screens::info::Page::EmptyBattery });
    }

    /// At boot, an SSB patch in flash that matches no known stock version (D15): ask whether to use it,
    /// over whatever is up (after the PIN, if any).
    pub fn ssb_patch_found(&mut self) {
        self.open(Dest::SsbConfirm);
    }

    /// The Link task: a write session is running (LINK-9): its progress screen (the percentage is the
    /// Device's `link.write_pct`), keys ignored but BACK, which cancels.
    pub fn link_write_progress(&mut self) {
        if !matches!(self.screen, Screen::Link(l) if l.page == screens::link::LinkPage::Progress) {
            self.open(Dest::WriteProgress);
        }
    }

    /// The Link task: the write session ended: back to where it was.
    pub fn link_write_done(&mut self) {
        if matches!(self.screen, Screen::Link(l) if l.page == screens::link::LinkPage::Progress) {
            self.back();
            self.show_toast(b"Write done", b"", false);
        }
    }

    /// MENU held at power on (PIN-6, BOOT-14), after the self-test: the settings reset, asked twice (settings,
    /// then channels too). It clears the PIN, so it comes before it; BACK on its first question goes on to the
    /// PIN if one is set (else home). Calibration, stock data and the SSB patch are never touched.
    pub fn power_on_reset(&mut self) {
        self.go_home();
        if self.set.pin_len > 0 && self.set.pin_mode > 0 {
            self.screen = Screen::Pin(screens::pin::Pin::power_on());
            self.push_back();
        }
        let ask = screens::confirm::Ask::Reset { armed: false };
        self.screen = Screen::Confirm(screens::confirm::Confirm { ask, boot: true, since: 0 });
    }

    /// A matrix code went down or up, mapped to a key or not: the key test shows it (KEY-2). The firmware calls
    /// this for every code, before [`App::key_down`] / [`App::key_up`] for the mapped ones.
    pub fn raw_key(&mut self, code: u8, down: bool) {
        self.screen.raw(self, ferrite_core::keys::key(code), Some(code), down);
    }

    /// The answer to the last write approval or write session (allowed, refused, cancelled), once.
    pub fn take_link_answer(&mut self) -> Option<screens::link::LinkAnswer> {
        self.link_answer.take()
    }

    /// The callsign, for the boot screen (empty until set).
    pub fn callsign(&self) -> &[u8] {
        self.set.callsign.as_bytes()
    }

    /// Dry TX is on (TX-48): the firmware holds the PA drive at 0 while it is.
    pub fn dry_tx(&self) -> bool {
        self.dry_tx
    }

    /// Dry TX on or off: only the radio's menu calls this (TX-48).
    pub(crate) fn set_dry_tx(&mut self, on: bool) {
        self.dry_tx = on;
    }

    /// TX-48.1: the tx-bench build starts every power-on with Dry TX on (the firmware calls this at boot).
    pub fn dry_at_boot(&mut self) {
        self.dry_tx = true;
    }

    /// The radio-wide settings.
    pub fn settings(&self) -> &Settings {
        self.set
    }

    /// The radio-wide settings, to change from outside (the simulator's sample, tests; the link once it
    /// writes them). The radio state that follows from them is sent again.
    pub fn settings_mut(&mut self) -> &mut Settings {
        self.set
    }

    // ---- inputs ----

    pub fn key_down(&mut self, key: Key, now: u32, store: &impl ChannelStore) {
        self.tick_keys(now, store);
        // XB-20.5: a key (not PTT or SIDE1) stops a running crossband repeater, lit or dark, locked or not, and does
        // nothing else: it wakes the screen and its release and holds are ignored, as a waking key's.
        if screens::xb::key_stops(self, key) {
            self.wake(now);
            self.swallow(true);
            self.keys.down(key, now);
            self.last_input = now;
            return;
        }
        // A dark screen: the key only wakes it, and its release and holds are ignored; PTT acts at once. A remote
        // key (LINK-19, DISP-*) wakes it and acts too: the PC or phone can't see that the screen is dark.
        if self.wake(now) && !matches!(key, Key::Ptt(_)) && !self.remote_in {
            self.swallow(true);
            self.keys.down(key, now);
            self.last_input = now; // PWR-26.1: the wake's redraw at full speed
            return;
        }
        // PWR-24.4: SIDE1 + SIDE2 held in Low Power Mode leaves it; neither does its own thing meanwhile.
        if self.lpm_combo_down(key, now) {
            self.last_input = now;
            return;
        }
        // LINK-20: a person's PTT during a remote transmission ends it, and does nothing else until let go.
        if self.local_ptt_down(key, now, store) {
            return;
        }
        self.keys.down(key, now);
        self.last_input = now;
        self.pressed = now;
        self.out.push(RadioCmd::Activity);
        self.sweep_hold();
        if self.set.key_beep && !matches!(key, Key::Ptt(_)) {
            self.out.push(RadioCmd::Beep { hz: KEY_BEEP_HZ, ms: KEY_BEEP_MS }); // SND-3
        }
        if self.tx_pause && !matches!(key, Key::Ptt(_)) {
            // WATCH-16: a key press ends the pause after a transmission
            (self.paused_until, self.tx_pause) = (None, false);
            self.out.push(RadioCmd::PauseWatch(0));
        }
        // HOME-17: while the quick menu is up it takes every key but the side connector's PTT (which closes it):
        // PTT A and PTT B run its features and never get further.
        if screens::qmenu::key_down(self, key) {
            return;
        }
        self.screen.raw(self, Some(key), None, true);
        if let Key::Ptt(k) = key {
            // HOME-17 (safety): a PTT pressed while the quick menu was up never keys, even after the menu closed,
            // until it has been let go (a second down without an up, a bounce, is the same press).
            if self.ptt_block & screens::qmenu::ptt_bit(k) != 0 {
                return;
            }
            // KEY-14.3: locked in "All" a radio doesn't key either
            if self.lock_mode() == Some(settings::Lock::All) {
                return self.locked_toast();
            }
            // KEY-7: from any screen; works locked too (KEY-14.1, KEY-14.2). Each screen says what PTT does there. PTT B keys
            // slot B; PTT A and the side connector's key the active slot (decision 77, as nicFW).
            let s = self.ptt_target(k);
            // APRS-18: in compose any PTT sends; a PTT that would key the APRS slot opens compose from any screen.
            // Neither keys voice.
            if matches!(self.screen, Screen::Compose(c) if !c.quick_edit_on()) {
                return screens::msgs::send(self, store);
            }
            if self.aprs_slot(s) {
                return self.open(Dest::Compose);
            }
            self.ptt_slot = s;
            self.scan_ptt(s, store);
            self.screen.ptt(self, true);
        }
    }

    pub fn key_up(&mut self, key: Key, now: u32, store: &impl ChannelStore) {
        self.tick_keys(now, store);
        self.lpm_combo_up(key);
        if self.local_ptt_up(key) {
            return; // LINK-20: the press that ended a remote transmission
        }
        if !matches!(key, Key::Ptt(_)) && self.swallow(false) {
            self.keys.release(key, now);
            return;
        }
        self.screen.raw(self, Some(key), None, false);
        if let Key::Ptt(k) = key {
            // HOME-17: a PTT the quick menu took comes up: it never went down for the screen, so nor does it come up
            let bit = screens::qmenu::ptt_bit(k);
            if self.ptt_block & bit == 0 {
                self.screen.ptt(self, false);
            }
            self.ptt_block &= !bit;
            self.ptt_released(); // TX-25: whatever the screen did, PTT up unkeys
        }
        match self.keys.release(key, now) {
            Some(e) if e.press == Press::Short => self.on_key(e, store),
            // The release after a long press: only the screen hears it (the shortcut map runs its shortcut).
            Some(e) if !self.locked() || self.lock_sides() && matches!(e.key, Key::Side(_)) => self.screen.key(self, e, store),
            _ => {}
        }
        self.reconcile_mode(); // MODE-2: PTT up, or what the key did
    }

    /// Knob: +1 per click clockwise.
    pub fn knob(&mut self, d: i8, now: u32, store: &impl ChannelStore) {
        self.tick_keys(now, store);
        if self.wake(now) && !self.remote_in {
            return; // a remote knob step (LINK-19) wakes and acts, as a remote key
        }
        self.last_input = now;
        self.pressed = now;
        self.out.push(RadioCmd::Activity);
        self.sweep_hold();
        if self.locked() && !(self.lock_sides() && self.side1_squelch()) {
            return self.locked_toast(); // KEY-14; with the keypad's lock SIDE1 + knob, the squelch, works (SQL-11)
        }
        if screens::qmenu::knob(self, d as i32) {
            return; // HOME-17: brightness while the quick menu is up
        }
        self.screen.knob(self, d as i32, store);
        self.knob_at = Some(now); // HOME-13: home holds its heavier redraws while the knob turns
        self.reconcile_mode(); // MODE-2
    }

    /// Time passes: long presses and repeats, toasts, undo, timeouts; and the latest snapshot.
    pub fn tick(&mut self, now: u32, live: &LiveAll, store: &impl ChannelStore) {
        self.tick_keys(now, store);
        // Decision 333: the store keeps the member lists of the slots' zones (CHAN-36)
        store.slot_zones([self.slots[0].zone, self.slots[1].zone, self.slots[2].zone]);
        // UPD-16, LINK-61: the chip's data waits for Console: said once, over whatever is up
        if store.data().layout_found() != 0 && !self.update_told {
            self.update_told = true;
            self.open(Dest::FinishUpdate);
        }
        local::tick(self, store); // CHAN-34: before anything steps through a slot's zone
        // Channel scans in the Radio task: their set going out, and the frequencies it asks for.
        chanscan::feed(self, store);
        chanscan::answer(self, live, store);
        self.lights_tick(now, live);
        self.lpm_tick(); // PWR-24
        let mut rx = 0;
        let mut scan = 0;
        let mut relay = 0;
        for (i, s) in live.slots.iter().enumerate() {
            rx |= (s.squelch_open as u8) << i;
            scan |= (s.scanning as u8) << i;
            relay |= (s.relay as u8) << i;
            self.heard[i] = s.heard_tone;
            self.scan_hit[i] = if s.scanning && s.held { s.freq } else { 0 };
            if self.scan_hit[i] != 0 {
                self.last_hit[i] = self.scan_hit[i]; // SCAN-6
            }
        }
        // PWR-24.8: the active slot's last reception, for Low Power Mode's home.
        let a = self.active as usize;
        if rx & 1 << a != 0 {
            let r = live.slots[a].rssi;
            if self.rx_mask & 1 << a == 0 || self.last_rx.0 != a as u8 || self.last_rx.2 != 0 {
                self.last_rx = (a as u8, now, 0, r);
            } else {
                self.last_rx.3 = self.last_rx.3.max(r);
            }
        } else if self.last_rx.0 == a as u8 && self.last_rx.2 == 0 && self.last_rx.1 != 0 {
            self.last_rx.2 = now.max(1);
        }
        // CHAN-25: Recent gets a channel when you tune to it (the active slot's), transmit on it, or a slot on it
        // opens squelch.
        let opened = rx & !self.rx_mask;
        for i in 0..SLOTS {
            let sl = &self.slots[i];
            let Some(c) = sl.ch.filter(|_| sl.on_channel()) else { continue };
            if i == self.active as usize {
                store.tuned(c.num);
            }
            if opened & 1 << i != 0 || self.radio.keyed == Some(i as u8) && store.recent(0) != Some(c.num) {
                store.used(c.num);
            }
        }
        if relay != 0 && self.relay_mask == 0 {
            self.show_toast(b"Watch off", b"HF needs the relay", false); // WATCH-11.2
        }
        (self.rx_mask, self.scan_mask, self.relay_mask) = (rx, scan, relay);
        if let Some(t) = self.toast.filter(|t| after(now, t.until)) {
            self.toast = None;
            self.drawn.popup_gone |= t.center;
        }
        if !self.lock_asking() {
            self.lock_ask = 0; // KEY-13: before the 16-bit time comes round again
        }
        self.ptt_stuck_notice(live.tx.ptt_stuck);
        if self.alert & ALERT_WAIT != 0 && !screens::xb::running(self) && self.radio.keyed.is_none() {
            self.open_alert();
        }
        if self.undo.is_some_and(|u| after(now, u.until)) {
            self.undo = None;
        }
        if self.set_undo.is_some_and(|u| after(now, u.until)) {
            self.set_undo = None;
        }
        self.alert_tone();
        if screens::shared::Mix::new().h(&self.set.callsign).get() != self.cw_sent {
            self.send_tx_tones(); // VOX-12, XB-12: CW ID sends the callsign as it is now
        }
        self.vox_tick(live, now, store);
        if self.paused_until.is_some_and(|t| after(now, t)) {
            (self.paused_until, self.tx_pause) = (None, false);
        }
        // WATCH-16: after a transmission, 30 s of quiet on the active slot before watch resumes.
        if self.tx_pause && self.rx_mask & 1 << self.active != 0 && self.paused_until.is_some_and(|t| t.wrapping_sub(now) < (TX_PAUSE_S as u32 - 5) * 1000) {
            self.paused_until = Some(now.wrapping_add(TX_PAUSE_S as u32 * 1000));
            self.out.push(RadioCmd::PauseWatch(TX_PAUSE_S));
        }
        // TX-30.2: the timeout's beep, once the transmitter is off
        if self.timeout_beep != 0 && after(now, self.timeout_beep) {
            self.timeout_beep = 0;
            self.out.push(RadioCmd::Beep { hz: ALERT_HZ, ms: ALERT_MS });
        }
        // MENU-28: a menu's timeout is 0 7 9's (Off never); other screens keep their own (EDIT-6)
        let timeout = self.screen.timeout().and_then(|t| if t == MENU_TIMEOUT_MS { self.set.menu_timeout_ms() } else { Some(t) });
        if timeout.is_some_and(|t| now.wrapping_sub(self.last_input) >= t) {
            self.go_home();
        }
        self.aprs_tick(store.data());
        self.radio_tick(live);
        self.sat_tick(); // SAT-6, SAT-8, SAT-12
        self.screen.tick(self, live);
        self.remote_tick(now, store); // LINK-20: after the screen's own unkeys (TX-30, the Radio task's stops)
        self.remote_keys_tick(now, store); // LINK-19: a remote long press let go on time
        if core::mem::take(&mut self.want_reset) {
            screens::confirm::reset_all(self, store);
        }
        if self.knob_at.is_some() && !self.knob_busy() {
            self.knob_at = None; // HOME-13: rested; home draws what it held
        }
        screens::qmenu::expire(self); // HOME-17: 5 s idle, dark, locked or away from home
        self.reconcile_mode(); // MODE-2: however the screen or a setting changed
    }

    fn tick_keys(&mut self, now: u32, store: &impl ChannelStore) {
        self.now = now;
        screens::qmenu::expire(self); // HOME-17: closed by time before the input is looked at
        let mut evs = [None; 8];
        let mut n = 0;
        self.keys.tick(now, |e| {
            if n < evs.len() {
                evs[n] = Some(e);
                n += 1;
            }
        });
        if self.swallowing() {
            return; // the key that woke the screen: no long press or repeat either
        }
        for e in evs.into_iter().flatten() {
            if self.qmenu.is_some() {
                break; // HOME-17: the quick menu just opened and took every key held
            }
            self.last_input = now;
            // LINK-19: a long press or repeat of a key the link holds is a remote key's (it never transmits).
            let was = self.remote_in;
            self.remote_in |= self.remote_held(e.key);
            self.on_key(e, store);
            self.remote_in = was;
            self.reconcile_mode(); // MODE-2
        }
        // SQL-9.1: SIDE1 let go (or used as a modifier and let go): the held monitor closes, unless latched since.
        if self.mon_held != 0 && !self.keys.is_held(Key::Side(1)) {
            let slot = self.mon_held - 1;
            self.mon_held = 0;
            if self.monitor & 1 << slot == 0 {
                self.out.push(RadioCmd::Monitor { slot, on: false });
            }
        }
    }

    // ---- what keys do ----

    fn on_key(&mut self, e: KeyEvent, store: &impl ChannelStore) {
        use Press::*;
        if self.screen.modal() {
            return self.screen.key(self, e, store); // the PIN at power on, power off, a link write
        }
        // One BACK rule (KEY-5): short BACK is the screen's, and goes back one level ([`App::back`]); hold BACK
        // goes home from anywhere but the modal screens. Hold # toggles the lock (KEY-13).
        match (e.key, e.press) {
            (Key::Exit, Long) => return self.go_home(),
            // BOOT-14: the armed reset's hold # is the reset's, not the key lock
            (Key::Hash, Long) if matches!(self.screen, Screen::Confirm(c) if c.ask == screens::confirm::Ask::Reset { armed: true }) => return,
            (Key::Hash, Long) if self.set.chan_only => return self.leave_chan_only(), // PIN-5
            (Key::Hash, Long) if self.locked() && self.set.pin_mode == 2 => {
                // KEY-16: unlocking asks for the PIN
                return self.open(Dest::Pin);
            }
            (Key::Hash, Long) if self.locked() => {
                self.unlock();
                return self.show_toast(b"Keys unlocked", b"", false);
            }
            (Key::Hash, Long) => {
                self.lock_keys(true);
                return self.lock_toast();
            }
            _ => {}
        }
        if self.locked() {
            // KEY-14, SQL-11: PTT goes through key_down/key_up. The keypad's lock (KEY-14.1) lets the side keys
            // through (SIDE1 monitor, SIDE2 scan and counter: the screen's own) and SIDE1 + ▲▼ (squelch, as SIDE1 +
            // knob: [`App::knob`]); SIDE1 + ◀▶ (the clarifier, a retune) and SIDE2 + ▲▼ (a tuning step) stay locked.
            let sides = self.lock_sides();
            let asking = self.lock_asking();
            match (e.key, e.press) {
                (Key::Ptt(_), _) => {}
                // KEY-13: for the lock popup's time * locks All, 0 All but PTT; the key does nothing else
                (Key::Star | Key::Digit(0), _) if asking => {
                    self.lock = Some(if e.key == Key::Star { settings::Lock::All } else { settings::Lock::AllButPtt });
                    self.lock_ask = 0;
                    self.lock_toast();
                }
                // SQL-11, SQL-9.3: SIDE1 is the monitor key while locked too, wherever it has no job of its own
                (Key::Side(1), Short | Long) if sides && !self.screen.own_side1() => self.monitor_key(e.press),
                (Key::Side(_), _) if sides => self.screen.key(self, e, store),
                (Key::Up | Key::Down, Short | Repeat) if sides && self.side1_squelch() => self.screen.key(self, e, store),
                _ => self.locked_toast(),
            }
            return;
        }
        // SAT-12: OK on the pass alarm's toast opens the pass page.
        if (e.key, e.press) == (Key::Menu, Short) && self.sat_toast_ok() {
            return;
        }
        // MENU-25: hold MENU is this screen's options, on every screen the same way.
        if (e.key, e.press) == (Key::Menu, Long) && !self.screen.own_hold_menu() {
            if !self.screen.options(self) {
                self.show_toast(b"No options here", b"", false);
            }
            return;
        }
        // SQL-9.3: SIDE1 is the monitor key on every screen that doesn't give it a job of its own.
        if matches!((e.key, e.press), (Key::Side(1), Short | Long)) && !self.screen.own_side1() {
            return self.monitor_key(e.press);
        }
        // KEY-19: ◀▶ are the knob, unless the screen gives them a job of its own.
        if matches!((e.key, e.press), (Key::Left | Key::Right, Short | Repeat)) && !self.screen.own_arrows() {
            return self.screen.knob(self, if e.key == Key::Right { 1 } else { -1 }, store);
        }
        self.screen.key(self, e, store);
    }

    // ---- actions ----

    /// Home, forgetting where BACK would return (HOME-2).
    pub fn go_home(&mut self) {
        self.screen = Screen::Home;
        self.back = [None; BACK_DEPTH];
        self.group_rows = [0; screens::groups::MEMO_BYTES]; // MENU-24: remembered for one visit to the menu
        self.reconcile_mode(); // MODE-1: a scope, search or repeater stops with its screen, before what follows
    }

    /// Opens a screen by where it is in the manual ([`Dest`]), remembering this one for BACK. Screens not built
    /// yet open as a "coming" placeholder ([`screens::nav::enter`]).
    pub fn open(&mut self, dest: Dest) {
        // PWR-24.4: Low Power Mode has no scan, band scope, counter, broadcast, HF mode or crossband (two VFOs):
        // nothing that keeps a receiver sweeping or listening; menus, editing and tone search stay.
        if self.low_power()
            && matches!(dest, Dest::Scan | Dest::BandScope | Dest::Counter | Dest::HfMode | Dest::Crossband)
        {
            return self.lpm_refuse();
        }
        // PIN-5: channel-only mode has no MENU 0 and no editing
        if matches!(dest, Dest::Advanced | Dest::Group(_) | Dest::Sub(..) | Dest::ChannelEdit | Dest::SaveChannel | Dest::AddRepeater | Dest::Quick)
            && self.chan_only_refuse()
        {
            return;
        }
        self.push_back();
        self.screen = screens::nav::enter(dest, self);
        self.reconcile_mode(); // MODE-2: a mode screen's mode starts as it opens
    }

    /// Remembers the screen up now for BACK (MENU-24); full, the oldest goes.
    pub(crate) fn push_back(&mut self) {
        if self.back[BACK_DEPTH - 1].is_some() {
            self.back.copy_within(1.., 0);
            self.back[BACK_DEPTH - 1] = None;
        }
        if let Some(b) = self.back.iter_mut().find(|b| b.is_none()) {
            *b = Some(self.screen);
        }
    }

    /// Short BACK, on every screen (KEY-5, MENU-24): back one level, to the screen this one was opened from
    /// ([`App::open`] or [`App::push_back`] remembered it), or home when it was opened from home. Screens with
    /// steps of their own (an edit, a sub-page, typed digits) take one step back first.
    fn back(&mut self) {
        match self.back.iter_mut().rev().find(|b| b.is_some()).and_then(|b| b.take()) {
            Some(s) if s != Screen::Home => {
                self.screen = s;
                self.reconcile_mode(); // MODE-1
            }
            _ => self.go_home(), // home by BACK too stops what the screens started
        }
    }

    /// SIDE1 held on home or the squelch screen: the knob and ▲▼ set the squelch (SQL-10), locked too (SQL-11).
    fn side1_squelch(&self) -> bool {
        self.keys.is_held(Key::Side(1)) && matches!(self.screen, Screen::Home | Screen::Squelch(_))
    }

    /// KEY-13: the lock's popup in the middle of the screen names what it covers: Keypad's offers the wider modes for
    /// as long as * and 0 choose them ([`App::lock_keys`]), the others say whether PTT still works.
    fn lock_toast(&mut self) {
        match self.lock.unwrap_or(settings::Lock::Keypad) {
            settings::Lock::Keypad => {
                self.show_popup(b"Keypad locked", LOCK_ASK);
                if let Some(t) = &mut self.toast {
                    t.until = self.now.wrapping_add(TOAST_MS);
                }
            }
            settings::Lock::AllButPtt => self.show_popup(b"All keys locked", b"PTT works"),
            settings::Lock::All => self.show_popup(b"All keys locked", b"PTT too"),
        }
    }

    /// Locked with the keypad's lock (KEY-14.1): the side keys still work.
    fn lock_sides(&self) -> bool {
        self.lock == Some(settings::Lock::Keypad)
    }

    fn locked_toast(&mut self) {
        // KEY-14.4: a side key held through a refused key or knob step gives nothing at its release either (SIDE2's
        // scan, SIDE1's monitor), as a modifier that was used
        self.keys.used(Key::Side(1));
        self.keys.used(Key::Side(2));
        self.show_toast(b"Locked:", b"hold # to unlock", false); // KEY-14
    }

    /// A toast from outside the app (the firmware's bring-up notice at boot).
    pub fn notice(&mut self, head: &[u8], body: &[u8]) {
        self.show_toast(head, body, false);
    }

    /// The short refusal beep (SND-4, TEXT-5), from the BK4829's tone generator (SND-11).
    /// TX-21.1: 0 8 7 changed: the three VFOs take the new busy lockout (channels keep their own).
    pub(crate) fn set_busy_default(&mut self) {
        let busy = self.set.busy_default as u8;
        for a in 0..SLOTS as u8 {
            self.slots[a as usize].cfg.busy = busy;
            if !self.slots[a as usize].on_channel() {
                self.send_slot(a);
            }
        }
    }

    pub(crate) fn beep_refused(&mut self) {
        self.refusal_beeps = self.refusal_beeps.wrapping_add(1);
        self.out.push(RadioCmd::Beep { hz: REFUSED_HZ, ms: REFUSED_MS });
    }

    fn show_toast(&mut self, head: &[u8], body: &[u8], icon: bool) {
        // DISP-31: replacing a centred popup (the lock's, say) with a hint-bar toast: the popup's box is cleared and
        // the screen under it drawn again (it stayed on the screen: the knob while locked)
        self.drawn.popup_gone |= self.toast.is_some_and(|t| t.center);
        self.toast = Some(Toast { head: Buf::from(head), body: Buf::from(body), icon, center: false, until: self.now.wrapping_add(TOAST_MS) });
    }

    /// SAFE-14: "PTT stuck" and the inputs held since boot (`stuck`: [`ferrite_core::stuck`]'s bits, held now in the
    /// low nibble, stuck since boot in the high), as a popup in the middle of the screen over whatever is up (the
    /// hint bar's toast was missed on the radio, Oct 13, 2026): kept up while held, over any other toast, and
    /// [`STUCK_AFTER_MS`] after the release; inputs let go before the UI first looked are named once for as long.
    fn ptt_stuck_notice(&mut self, stuck: u8) {
        use crate::screens::STUCK_HEAD;
        use ferrite_core::stuck::{names, EVER_SHIFT, NOW};
        let held = stuck & NOW;
        let mask = if held != 0 {
            held
        } else if !self.stuck_shown && stuck >> EVER_SHIFT != 0 {
            stuck >> EVER_SHIFT
        } else {
            return; // released: the popup goes at its time (the toast expiry above)
        };
        self.stuck_shown = true;
        let mut b = [0u8; 28];
        let n = names(mask, &mut b);
        let ours = self.toast.is_some_and(|t| t.center && t.head.as_bytes() == STUCK_HEAD && t.body.as_bytes() == &b[..n]);
        if !ours {
            self.show_popup(STUCK_HEAD, &b[..n]);
        }
        if let Some(t) = &mut self.toast {
            t.until = self.now.wrapping_add(STUCK_AFTER_MS);
        }
    }

    /// A hold-digit shortcut's popup in the middle of the screen: what ran, and its new value if it has one (HOME-8).
    pub(crate) fn show_popup(&mut self, head: &[u8], body: &[u8]) {
        self.drawn.popup_gone |= self.toast.is_some_and(|t| t.center);
        self.toast = Some(Toast { head: Buf::from(head), body: Buf::from(body), icon: false, center: true, until: self.now.wrapping_add(POPUP_MS) });
    }

    /// A watched slot (not the active one) is receiving.
    fn watched_rx(&self) -> bool {
        self.rx_mask & self.watch_mask() & !(1 << self.active) & !self.relay_mask != 0
    }

    /// SCOPE-14 (UI backlog 7): a key or knob step while the scope or home's strip sweeps holds the sweep and
    /// starts its pass again, so the input is handled and drawn first; the bars catch up after.
    fn sweep_hold(&mut self) {
        if matches!(self.radio.mode, screens::activity::Mode::Scope | screens::activity::Mode::Strip) {
            self.out.push(RadioCmd::SweepHold);
        }
    }

    /// The watched slots: Watch's, and C while it is the APRS slot (APRS-16, KISS-9).
    pub(crate) fn watch_mask(&self) -> u8 {
        if self.low_power() {
            return 0; // PWR-24.4: the active slot alone
        }
        ([0, 0b011, 0b111][self.watch as usize % 3] | (self.aprs_c() as u8) << aprs::APRS_SLOT) & !self.off // WATCH-2.1
    }

    /// HOME-15.1: slot `s` is turned off.
    pub(crate) fn slot_off(&self, s: u8) -> bool {
        self.off & 1 << s != 0
    }

    /// HOME-15.4: what an off active slot can't do (PTT, a scan, monitor) is refused with a toast and the refusal
    /// beep. True when refused.
    /// SQL-9: SIDE1 on the active slot. A tap latches monitor until tapped again (its card says MON); held alone,
    /// the squelch is open while it is held ([`App::tick_keys`] closes it at the release, SQL-9.1). The same on
    /// every screen that doesn't give SIDE1 a job of its own (SQL-9.3).
    pub(crate) fn monitor_key(&mut self, press: keys::Press) {
        let a = self.active;
        match press {
            keys::Press::Short if self.off_refuse(a) => {} // HOME-15.4
            keys::Press::Short => {
                self.monitor ^= 1 << a;
                let on = self.monitor & 1 << a != 0;
                self.out.push(RadioCmd::Monitor { slot: a, on });
                self.show_toast(b"Monitor", if on { b"on: tap SIDE1 again" } else { b"off" }, false);
            }
            keys::Press::Long if self.monitor & 1 << a == 0 => {
                self.mon_held = a + 1;
                self.out.push(RadioCmd::Monitor { slot: a, on: true });
            }
            _ => {}
        }
    }

    pub(crate) fn off_refuse(&mut self, s: u8) -> bool {
        if !self.slot_off(s) {
            return false;
        }
        self.beep_refused();
        let mut t = Buf::<16>::new();
        t.set(b"Slot ");
        t.push(&[b'A' + s]);
        t.push(b" is off");
        self.show_toast(t.as_bytes(), b"* turns it back on", false);
        true
    }

    /// The radio-wide settings the Radio task keeps: scan inputs (0 1 6 3, SCAN-22) and the subtone level
    /// (0 8 8 1, TONE-16).
    pub(crate) fn send_radio_settings(&mut self) {
        self.out.push(RadioCmd::ScanTx(self.set.scan_tx));
        self.out.push(RadioCmd::ToneLevel(self.set.tone_level));
        self.out.push(RadioCmd::BeepVol(self.set.beep_vol.clamp(1, 3)));
        self.out.push(RadioCmd::MicGain(self.set.mic_gain.clamp(1, 9)));
        self.out.push(RadioCmd::TxTimeout(self.set.tx_timeout_s() as u16));
        self.send_vox();
        self.send_tx_tones();
        self.send_si_opts();
    }

    /// BCR-32, HF-35 (decision 292): the SI4732's receive settings to the Radio task: FM's AGC (0 2 6 2) and
    /// seek step, HF mode's AGC and AM bandwidth, and what the region decides (FM de-emphasis and band, MW spacing).
    pub(crate) fn send_si_opts(&mut self) {
        use ferrite_core::msg::SiOpts;
        use ferrite_core::txplan::Region;
        let r = self.set.region();
        let americas = matches!(r, Some(Region::Us | Region::Canada));
        let mut flags = (self.set.fm_step.min(2)) << 4;
        if !americas {
            flags |= SiOpts::DEEMPH_50 | SiOpts::MW_9K;
        }
        if r == Some(Region::Japan) {
            flags |= SiOpts::BAND_JAPAN;
        }
        let h = &self.set.hf;
        let o = SiOpts { fm_agc: self.set.fm_agc, am_agc: h.agc, am_filter: h.am_filter(), flags };
        self.out.push(RadioCmd::SiOpts(o));
    }

    /// VOX's level and tail to the Radio task (VOX-2, VOX-5, VOX-13; 0 8 2): level 0 when it is off.
    pub(crate) fn send_vox(&mut self) {
        let s = &self.set;
        let level = if s.vox { s.vox_level.clamp(1, 9) } else { 0 };
        let tail = settings::VOX_TAILS[s.vox_tail as usize % settings::VOX_TAILS.len()];
        self.out.push(RadioCmd::Vox { level, tail });
    }

    /// The Transmit group's tone settings to the Radio task (TONE-5 to TONE-10, VOX-12, DTMF-3, DTMF-8): the
    /// radio-wide ones and the callsign for CW ID (each slot's end tone and tail go with the slot).
    pub(crate) fn send_tx_tones(&mut self) {
        let s = &self.set;
        self.out.push(RadioCmd::TxTones { phase: s.tail_phase % 4, cw: s.cw_id % 3, sidetone: s.dtmf_sidetone });
        self.cw_sent = screens::shared::Mix::new().h(&s.callsign).get();
        let call = s.callsign.as_bytes();
        for chunk in 0..2u8 {
            let mut text = [0u8; 5];
            for (k, t) in text.iter_mut().enumerate() {
                *t = call.get(chunk as usize * 5 + k).copied().unwrap_or(0);
            }
            self.out.push(RadioCmd::CwCall { chunk, text });
        }
        self.out.push(RadioCmd::DtmfTiming { ms: s.dtmf_ms.clamp(50, 200), delay_10ms: s.dtmf_delay.min(100) });
    }

    /// Each slot's end tone and tail again (0 8 8 2 or 0 8 4 changed, or a channel's own; a slot's full sync
    /// carries them too).
    pub(crate) fn send_slot_tones(&mut self) {
        for slot in 0..SLOTS as u8 {
            let (end, tail) = self.slot_tones(slot);
            self.out.push(RadioCmd::SlotParam { slot, param: Param::EndTone, value: end as i32 });
            self.out.push(RadioCmd::SlotParam { slot, param: Param::Tail, value: tail as i32 });
        }
    }

    /// A slot's end tone (an index into `tonegen::END_TONES`, 0 off) and tail elimination (bit 0 receive, bit 1
    /// transmit), its channel's own over the radio's (TONE-7, TONE-10, decision 86); none on the APRS slot.
    pub(crate) fn slot_tones(&self, slot: u8) -> (u8, u8) {
        if self.aprs_slot(slot) {
            return (0, 0);
        }
        let ch = self.slots[slot as usize % SLOTS].ch.filter(|_| !self.slots[slot as usize % SLOTS].vfo_mode);
        let end = match ch.map_or(0, |c| c.end_tone()) {
            0 => self.set.end_tone % 6,
            e => e - 1, // Off, then the presets in 0 8 4's order
        };
        let tail = match ch.map_or(0, |c| c.tail()) {
            0 => self.set.tail_mode & 3,
            1 => 0,
            _ => 3,
        };
        (end, tail)
    }

    /// CHAN-4: the slot transmits from a receive-only channel (the APRS slot keeps its own frequency).
    pub(crate) fn rx_only(&self, slot: u8) -> bool {
        slot < SLOTS as u8
            && !self.aprs_slot(slot)
            && self.slot(slot).on_channel()
            && self.slot(slot).ch.is_some_and(|c| c.flags & crate::chan::FLAG_RX_ONLY != 0)
    }

    /// The slot's frequency and every setting, to the Radio task (made when popped: [`App::pop_cmd`]).
    /// TX-36: a slot's VFO tuned onto HF (18-64 MHz) from above it starts transmit on Low, whatever the power was
    /// (nothing on the radio measures the antenna match; the PA is most at risk there). Retuning within HF keeps
    /// what the user raised it to. Call after the VFO moved from `from`; true when the power dropped (the caller
    /// sends the slot).
    pub(crate) fn hf_starts_low(&mut self, slot: u8, from: Freq10) -> bool {
        let s = &mut self.slots[slot as usize % SLOTS];
        let hf = |f: Freq10| f < crate::chan::HF_TOP;
        let drop = s.vfo_mode && hf(s.vfo) && !hf(from) && s.cfg.power != 0;
        if drop {
            s.cfg.power = 0;
        }
        drop
    }

    pub(crate) fn send_slot(&mut self, slot: u8) {
        self.slots[slot as usize % SLOTS].clar = 0; // VFO-22: a retune clears the clarifier (the Radio task's too)
        self.out.sync(slot);
    }

    /// VFO-22: SIDE1 + ◀▶ on home move the active slot's clarifier 100 Hz, within ±2.5 kHz.
    pub(crate) fn step_clarifier(&mut self, d: i32) {
        let a = self.active;
        let s = &mut self.slots[a as usize];
        s.clar = (s.clar as i32 + d).clamp(-25, 25) as i8;
        let c = s.clar;
        self.out.push(RadioCmd::SlotParam { slot: a, param: Param::Clarifier, value: c as i32 * 100 });
        let mut b = Buf::<16>::new();
        if c == 0 {
            b.set(b"off");
        } else {
            slot::clar_text(c, &mut b);
        }
        self.show_toast(b"Clarifier", b.as_bytes(), false);
    }

    /// Split on the active slot: transmit on `tx` whatever RX is tuned to (MENU 5's 4th choice). Refused (false,
    /// with a toast) when TX needs the band relay in the other state from RX (TX-37).
    pub(crate) fn set_split(&mut self, tx: Freq10) -> bool {
        let a = self.active;
        let s = &self.slots[a as usize];
        if !ferrite_core::txplan::relay_ok(s.cfg().rx_of(s.freq()), tx) {
            self.relay_toast();
            return false;
        }
        let mut c = *s.cfg();
        (c.split, c.offset) = (true, tx as i32);
        if let Err(r) = self.channel_ok(a, c) {
            self.refusal_toast(r);
            return false;
        }
        self.keep_undo();
        let c = self.slots[a as usize].cfg_mut();
        (c.split, c.offset) = (true, tx as i32);
        self.out.push(RadioCmd::SlotParam { slot: a, param: Param::Split, value: tx as i32 });
        self.save_channel(a, None);
        true
    }

    /// A repeater shift on the active slot (`shift` 10 Hz, signed; 0 simplex), as MENU 5's knob sets one (its
    /// [`Item::Offset`] path, with the amount given instead of the band's): split ends, the Radio task is told, a
    /// channel slot's channel saved (EDIT-4). Refused (false, with a toast) when TX would need the band relay in the
    /// other state from RX (TX-37). The PC's `slot X offset` (TERM-35, LINK-33).
    pub(crate) fn set_shift(&mut self, shift: i32) -> bool {
        let a = self.active;
        let rx = self.slots[a as usize].freq();
        let mut c = *self.slots[a as usize].cfg();
        let was_split = c.split;
        (c.offset, c.split) = (0, false); // the amount given, not the one kept (VFO-26)
        let Some((param, value)) = Item::Offset.set(&mut c, (shift.signum() + 3) % 3, shift.abs()) else { return false };
        if !ferrite_core::txplan::relay_ok(c.rx_of(rx), c.tx_of(rx)) {
            self.relay_toast();
            return false;
        }
        if let Err(r) = self.channel_ok(a, c) {
            self.refusal_toast(r);
            return false;
        }
        self.keep_undo();
        *self.slots[a as usize].cfg_mut() = c;
        self.out.push(RadioCmd::SlotParam { slot: a, param, value });
        if was_split {
            self.out.push(RadioCmd::SlotParam { slot: a, param: Param::Split, value: 0 });
        }
        self.save_channel(a, None);
        true
    }

    /// A slot on a channel keeps that channel's rules ([`chfield::check_edit`]) with config `c`; a VFO has none.
    fn channel_ok(&self, slot: u8, c: SlotCfg) -> Result<(), chfield::Refusal> {
        let s = &self.slots[slot as usize];
        match s.ch.filter(|_| s.on_channel()) {
            Some(ch) => chfield::check_edit(&chan::Channel { cfg: c, ..ch }),
            None => Ok(()),
        }
    }

    /// A channel field's refusal as a toast: its label and why ("Offset", "TX outside 64 to 1000 MHz"); TX-37's
    /// as [`App::relay_toast`].
    pub(crate) fn refusal_toast(&mut self, r: chfield::Refusal) {
        match r.field() {
            Some(f) => self.show_toast(f.label(false).as_bytes(), r.why().as_bytes(), false),
            None => self.relay_toast(),
        }
    }

    /// TX-37's refusal when a shift or split is set (the full sentence is on the TX refused screen).
    fn relay_toast(&mut self) {
        self.show_toast(b"No split TX", b"needs other relay", false);
    }

    /// Why an item can't be changed on the active slot right now (MENU-14, EDIT-7), if it can't.
    /// APRS-31: the position a beacon sends: the fixed one (0 6 9) when 0 5 5 4 says so and one is set, else `gps`
    /// (the receiver's fix, which already falls back to the fixed position without one, GPS-3).
    pub fn beacon_fix(&self, gps: Option<data::Fix>) -> Option<data::Fix> {
        // GPS-3.1 (decision 388): the phone's position is never beaconed: the fixed position, or none
        if self.set.aprs_fixed && self.set.fixed_on || gps.is_some_and(|f| f.phone) && self.set.fixed_on {
            let s = &self.set;
            return Some(data::Fix { pos: s.fixed, three_d: false, alt_m: s.fixed_alt as i32, speed_kmh: 0, heading: 0, hdop_x10: 0, used: 0, age_s: gpsdata::FIXED_AGE, utc_s: 0, phone: false });
        }
        gps.filter(|f| !f.phone)
    }

    pub fn greyed(&self, item: Item) -> Option<&'static str> {
        let s = &self.slots[self.active as usize];
        match item {
            // APRS-19: nothing on the APRS slot changes but its power, which is the APRS power (APRS-19.1)
            Item::Watch | Item::BatterySave | Item::Channel | Item::Advanced => None,
            Item::Power if self.aprs_slot(self.active) => None,
            _ if self.aprs_slot(self.active) => Some(self.aprs_holds()),
            Item::Step if s.on_channel() => Some("VFO only"), // VFO-8
            Item::Power if self.rx_only(self.active) => Some(chfield::Refusal::RxOnly.why()), // CHAN-4, EDIT-7
            _ => None,
        }
    }

    /// The knob or ◀▶ on a setting: change it live (MENU-16, MENU-19), remember it for undo and Recent.
    fn change(&mut self, item: Item, d: i32) {
        if d == 0 || self.greyed(item).is_some() {
            if let Some(why) = self.greyed(item) {
                self.show_toast(item.label().as_bytes(), why.as_bytes(), false);
            }
            return;
        }
        if matches!(item, Item::Channel | Item::Advanced) {
            return;
        }
        let a = self.active;
        self.keep_undo();
        match item {
            Item::Power if self.aprs_slot(a) => {
                let power = (self.aprs.power as i32 + d).rem_euclid(3) as u8; // APRS-19.1: 0 5 6 3
                self.set_aprs(aprs::AprsCfg { power, ..self.aprs });
            }
            Item::Watch => self.put_watch((self.watch as i32 + d).rem_euclid(3) as u8),
            Item::BatterySave => {
                self.bsave = (self.bsave as i32 + d).rem_euclid(4) as u8;
                self.out.push(RadioCmd::BatterySave(level(self.bsave)));
            }
            _ => {
                let rx = self.slots[a as usize].freq();
                let shift = crate::slot::band_shift(band_near(rx), self.set.region());
                let on_ch = self.slots[a as usize].on_channel();
                let c = self.slots[a as usize].cfg_mut();
                let mut v = item.get(c).0 + d;
                if item == Item::Squelch && !on_ch && v.rem_euclid(11) == 0 {
                    v += d.signum(); // a VFO has no "Global": it is the slot's level (SQL-6)
                }
                if item == Item::Offset && v.rem_euclid(4) == 3 {
                    // Turning onto Split keeps today's TX as the split's; OK on it sets another (VFO-16), so the
                    // knob can go on past it.
                    let tx = if c.split { c.offset.max(0) as Freq10 } else { (rx as i64 + c.offset as i64).max(0) as Freq10 };
                    if self.set_split(tx) {
                        self.show_toast(b"Split:", b"OK sets the TX frequency", false);
                    }
                    return;
                }
                let (was_split, old) = (c.split, *c);
                if let Some((param, value)) = item.set(c, v, shift) {
                    let narrow = c.narrow;
                    if item == Item::Offset && !ferrite_core::txplan::relay_ok(c.rx_of(rx), c.tx_of(rx)) {
                        *c = old; // TX-37: a shift that crosses the relay boundary isn't kept
                        self.relay_toast();
                        return;
                    }
                    let new = *c;
                    if let Err(r) = self.channel_ok(a, new) {
                        *self.slots[a as usize].cfg_mut() = old; // a channel's own rules (CHAN-22)
                        return self.refusal_toast(r);
                    }
                    let value = if param == Param::Squelch { self.sq_level(a) as i32 } else { value };
                    self.out.push(RadioCmd::SlotParam { slot: a, param, value });
                    if param == Param::Mode {
                        // VFO-20: the mode is the bandwidth too
                        self.out.push(RadioCmd::SlotParam { slot: a, param: Param::Bandwidth, value: narrow as i32 });
                    }
                    if was_split && param == Param::Offset {
                        self.out.push(RadioCmd::SlotParam { slot: a, param: Param::Split, value: 0 });
                    }
                }
                self.save_channel(a, None);
            }
        }
        if self.recent[0] != Some(item) {
            let keep = self.recent.iter().position(|&r| r == Some(item)).unwrap_or(self.recent.len() - 1);
            self.recent.copy_within(0..keep, 1);
            self.recent[0] = Some(item);
        }
    }

    /// # on a setting: back to its default (MENU-18).
    fn reset(&mut self, item: Item) {
        let cur = match item {
            Item::Watch => self.watch as i32,
            Item::BatterySave => self.bsave as i32,
            _ => item.get(self.slots[self.active as usize].cfg()).0,
        };
        let def = match item {
            Item::Watch | Item::BatterySave => 1,
            Item::Squelch if self.slots[self.active as usize].on_channel() => 0, // a channel's default: Global
            _ => item.default_of(),
        };
        self.change(item, def - cur);
    }

    /// The squelch level slot `s` listens with: its own, or the slot's (VFO) level when its channel says Global
    /// (SQL-6).
    pub fn sq_level(&self, s: u8) -> u8 {
        let slot = &self.slots[s as usize];
        match slot.cfg().squelch {
            SQ_GLOBAL.. => slot.cfg.squelch.min(9),
            l => l,
        }
    }

    pub(crate) fn rf_gain_text(&self) -> &'static [u8] {
        [b"Auto".as_slice(), b"1 / 4", b"2 / 4", b"3 / 4", b"4 / 4"][self.rf_gain as usize]
    }

    pub(crate) fn step_rf_gain(&mut self, d: i32) {
        let gain = i32::from(self.rf_gain).saturating_add(d).clamp(0, 4) as u8;
        if gain != self.rf_gain {
            self.rf_gain = gain;
            self.out.push(RadioCmd::RfGain(gain));
        }
    }

    /// SIDE1 + knob / ▲▼: the live level one step up or down, stopping at 0 and 9 (SQL-3). A channel on Global
    /// moves the slot's level and stays Global.
    pub(crate) fn step_squelch(&mut self, d: i32) {
        let a = self.active;
        let cur = self.sq_level(a) as i32;
        let to = (cur + d).clamp(0, 9);
        if to == cur {
            return;
        }
        if self.slots[a as usize].cfg().squelch >= SQ_GLOBAL {
            self.keep_undo();
            self.slots[a as usize].cfg.squelch = to as u8;
            self.out.push(RadioCmd::SlotParam { slot: a, param: Param::Squelch, value: to });
        } else {
            self.change(Item::Squelch, to - cur);
        }
    }

    /// Remembers the active slot's state before a change, for "* Undo" (EDIT-4, MENU-22): the first change
    /// of a run keeps it, every change restarts the 5 s.
    pub(crate) fn keep_undo(&mut self) {
        let a = self.active;
        if self.undo.is_none() {
            let s = &self.slots[a as usize];
            let ch = s.ch.filter(|_| s.on_channel()).map(|c| ChUndo { num: c.num | (c.tones as u16) << 10, zone: c.zone, flags: c.flags, rx: c.rx });
            self.undo = Some(Undo { slot: a, cfg: *s.cfg(), ch, home: self.set.home_ch, watch: self.watch, bsave: self.bsave, until: 0 });
        }
        if let Some(u) = &mut self.undo {
            u.until = self.now.wrapping_add(UNDO_MS);
        }
    }

    /// SND-6, INBOX-4: the alert screen sounds its tone each second until OK, when alert tones are on.
    fn alert_tone(&mut self) {
        let sounding = matches!(self.screen, Screen::Alert(a) if !a.silenced) && self.set.alert_tones;
        let sec = (self.now / 1000) as u16 | 0x8000;
        if sounding && sec != self.alert_beep {
            self.alert_beep = sec;
            self.out.push(RadioCmd::Beep { hz: ALERT_HZ, ms: ALERT_MS });
        }
    }

    /// VOX-3, as stock's UI mode 0 (0x08025994: any other screen holds VOX): VOX samples only on home, with no
    /// quick menu up; its own Transmit screen counts as home while VOX holds the key. Every other screen (menus,
    /// settings, editors, entry, popups, prompts, alerts, setup, the link's and the terminal's screens) blocks it,
    /// so MENU 0 8 2 can always be reached to turn VOX off.
    pub fn vox_screen(&self) -> bool {
        self.qmenu.is_none()
            && match self.screen {
                Screen::Home => true,
                Screen::Transmit(_) => self.vox_on,
                _ => false,
            }
    }

    /// VOX-13, VOX-4: speech from the Radio task keys the active slot as PTT A does, with every PTT check;
    /// its end (after the tail) releases it. Not on the APRS slot, whose PTT opens compose.
    fn vox_tick(&mut self, live: &LiveAll, now: u32, store: &impl ChannelStore) {
        let screen_ok = self.vox_screen();
        let bits = VOX_SENT | if screen_ok { VOX_SCREEN } else { 0 };
        if self.vox_bits & (VOX_SENT | VOX_SCREEN) != bits {
            self.vox_bits = self.vox_bits & VOX_REFUSED | bits;
            self.out.push(RadioCmd::VoxScreen(screen_ok));
        }
        // VOX-3: speech keys only from home; a release always goes through.
        let voice = live.tx.voice && self.set.vox && !self.aprs_slot(self.active) && (screen_ok || self.vox_on);
        // TX-35 (TX audit F11): VOX is automatic, so never on an emergency frequency: a toast, not the prompt.
        // APRS-21.3: nor on the APRS frequency while C is the APRS slot (data only).
        let emergency = ferrite_core::txplan::emergency(screens::transmit::tx_freq(self, self.active));
        if voice && !self.vox_on && self.radio.keyed.is_none() && (emergency || self.aprs_voice_gate(self.active)) {
            if self.vox_bits & VOX_REFUSED == 0 {
                self.vox_bits |= VOX_REFUSED;
                self.show_toast(b"No VOX", if emergency { b"emergency frequency" as &[u8] } else { b"APRS frequency" }, false);
            }
            return;
        }
        if !voice {
            self.vox_bits &= !VOX_REFUSED;
        }
        if voice && !self.vox_on && self.radio.keyed.is_none() {
            self.vox_on = true;
            self.virtual_ptt = true;
            self.key_down(Key::PTT, now, store);
            self.virtual_ptt = false;
        } else if !voice && self.vox_on {
            self.vox_on = false;
            self.virtual_ptt = true;
            self.key_up(Key::PTT, now, store);
            self.virtual_ptt = false;
        }
    }

    /// Before a settings-list change (MENU-22): keeps the value shown, unless a run on the same item is open;
    /// every change restarts the 5 s.
    pub(crate) fn keep_set_undo(&mut self, p: &[u8], d: i32, was: u32) {
        let mut path = [0u8; 3];
        let len = p.len().min(3);
        path[..len].copy_from_slice(&p[..len]);
        let same = self.set_undo.is_some_and(|u| u.path == path && u.len as usize == len);
        if !same {
            self.set_undo = Some(SetUndo { path, len: len as u8, dir: d.signum() as i8, was, until: 0 });
        }
        if let Some(u) = &mut self.set_undo {
            u.until = self.now.wrapping_add(UNDO_MS);
        }
    }

    pub(crate) fn do_undo(&mut self) {
        if let Some(u) = self.set_undo.take() {
            screens::groups::undo_setting(self, &u);
            return self.show_toast(b"Undone", b"", false);
        }
        self.undo_slot();
    }

    fn undo_slot(&mut self) {
        if let Some(u) = self.undo.take() {
            let s = &mut self.slots[u.slot as usize];
            match u.ch {
                Some(old) if s.on_channel() => {
                    let from = s.ch.map(|c| c.num).filter(|&n| n != old.num & 0x3FF);
                    if let Some(c) = &mut s.ch {
                        (c.num, c.zone, c.flags, c.tones, c.rx, c.cfg) = (old.num & 0x3FF, old.zone, old.flags, (old.num >> 10) as u8, old.rx, u.cfg);
                        s.zone = old.zone;
                    }
                    self.set.home_ch = u.home;
                    self.save_channel(u.slot, from);
                }
                _ => *s.cfg_mut() = u.cfg,
            }
            self.send_slot(u.slot);
            if u.watch != self.watch {
                self.put_watch(u.watch);
            }
            if u.bsave != self.bsave {
                self.bsave = u.bsave;
                self.out.push(RadioCmd::BatterySave(level(self.bsave)));
            }
            self.show_toast(b"Undone", b"", false);
        }
    }

    /// Value text for quick rows that aren't slot settings.
    fn item_text(&self, item: Item, long: bool, b: &mut Buf<16>) {
        let s = &self.slots[self.active as usize];
        match item {
            Item::Watch => b.set(["Off", "A+B", "A+B+C"][self.watch as usize % 3].as_bytes()),
            Item::BatterySave => {
                b.clear();
                if self.bsave == 0 {
                    b.set(b"Off");
                } else {
                    let _ = ufmt::uwrite!(b, "Level {}", self.bsave);
                }
            }
            Item::Channel => b.set(if s.on_channel() { b"Edit" } else { b"Save as channel" }),
            Item::Advanced => b.set(b">"),
            Item::Power if self.aprs_slot(self.active) => b.set(["Low", "Mid", "High"][self.aprs.power as usize % 3].as_bytes()),
            // A channel on Global: the level in use; "Global (4)" where there is room.
            Item::Squelch if s.cfg().squelch >= SQ_GLOBAL => {
                b.clear();
                let l = self.sq_level(self.active);
                let _ = if long { ufmt::uwrite!(b, "Global ({})", l) } else { ufmt::uwrite!(b, "{}", l) };
            }
            _ => item.text(s.cfg(), long, b),
        }
    }

    fn is_default(&self, item: Item) -> bool {
        match item {
            Item::Watch => self.watch == 1,
            Item::BatterySave => self.bsave == 1,
            Item::Channel | Item::Advanced => true,
            Item::Power if self.aprs_slot(self.active) => self.aprs.power == aprs::AprsCfg::DEFAULT.power,
            Item::Squelch if self.slots[self.active as usize].on_channel() => {
                self.slots[self.active as usize].cfg().squelch >= SQ_GLOBAL
            }
            _ => item.get(self.slots[self.active as usize].cfg()).0 == item.default_of(),
        }
    }
}

fn level(b: u8) -> Level {
    [Level::Off, Level::L1, Level::L2, Level::L3][b as usize % 4]
}

/// `now` has reached `t` (wrapping ms).
pub(crate) fn after(now: u32, t: u32) -> bool {
    now.wrapping_sub(t) as i32 >= 0
}

#[cfg(test)]
mod tests;
