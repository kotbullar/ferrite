//! Radio-wide settings, one value wherever it is changed (setup, the advanced menu's groups, the screens that
//! keep their own: SETUP-5, MENU-7). They live outside the [`crate::App`], which holds only UI state and a
//! reference to them ([`crate::App::new`]): the firmware keeps one [`Settings`] in a static that the Storage
//! task will write to the settings struct in SPI flash (0x02D000, 2 s after the last edit: MENU-21) and HF
//! mode's memory to its own sector (HF-29); the simulator and tests keep one each. [`Settings::DEFAULT`] is
//! what a first boot writes (BOOT-12).
//!
//! The value lists ([`REGION_LIST`], [`TIME_ZONES`], ...) are the menu's tables: their names are the ones the
//! screens show (MENU-7).

use crate::screens::hf::HfMem;
use ferrite_core::freq::Freq10;
use ferrite_core::msg::XbCfg;
use ferrite_core::txplan::Region;
use ferrite_ui::boot::Owner;
use ferrite_ui::text::Buf;
use ufmt::uwrite;

/// Text in a fixed array, zero padded (no length field: smaller than a [`Buf`] in the App).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Text<const N: usize>(pub [u8; N]);

impl<const N: usize> Text<N> {
    pub const EMPTY: Self = Text([0; N]);

    pub fn as_bytes(&self) -> &[u8] {
        &self.0[..self.len()]
    }

    pub fn len(&self) -> usize {
        self.0.iter().position(|&c| c == 0).unwrap_or(N)
    }

    pub fn is_empty(&self) -> bool {
        self.0[0] == 0
    }

    /// Adds a character; false when full.
    pub fn push(&mut self, c: u8) -> bool {
        let n = self.len();
        if n < N {
            self.0[n] = c;
        }
        n < N
    }

    /// The last character, replaced (multi-tap: the next letter on the same key).
    pub fn set_last(&mut self, c: u8) {
        let n = self.len();
        if n > 0 {
            self.0[n - 1] = c;
        }
    }

    pub fn pop(&mut self) {
        let n = self.len();
        if n > 0 {
            self.0[n - 1] = 0;
        }
    }

    pub fn set(&mut self, s: &[u8]) {
        self.0 = [0; N];
        let n = s.len().min(N);
        self.0[..n].copy_from_slice(&s[..n]);
    }
}

/// Regions (0 9 4).
pub const REGION_LIST: [&str; 8] =
    ["United States", "Canada", "United Kingdom", "Europe", "Australia", "New Zealand", "Japan", "Other"];

/// The regions as the lists show them: Canada first, then by name, Other last (Alex, Nov 9, 2026). Each entry is
/// the stored value (1 + its index in [`REGION_LIST`]), so the order changes nothing saved.
pub const REGION_ORDER: [u8; 8] = [2, 5, 4, 7, 6, 3, 1, 8];

/// The stored region shown at `row` of the lists ([`REGION_ORDER`]).
pub fn region_at(row: u8) -> u8 {
    REGION_ORDER[row as usize % REGION_ORDER.len()]
}

/// The list row of a stored region (1-8); 0 for one not set.
pub fn region_row(region: u8) -> u8 {
    REGION_ORDER.iter().position(|&r| r == region).unwrap_or(0) as u8
}

/// Time zones (0 6 5), in quarter hours: UTC-12 to +14 with the 30 and 45 minute offsets in use.
pub const TIME_ZONES: [i8; 38] = [
    -48, -44, -40, -38, -36, -32, -28, -24, -20, -16, -14, -12, -8, -4, 0, 4, 8, 12, 14, 16, 18, 20, 22, 23, 24,
    26, 28, 32, 35, 36, 38, 40, 42, 44, 48, 51, 52, 56,
];
/// UTC until you set it (decided Oct 7, 2026): GPS gives UTC only, so no zone is guessed.
pub const TZ_DEFAULT: u8 = 14;

const _: () = assert!(TIME_ZONES[TZ_DEFAULT as usize] == 0);

pub const CLOCKS: [&str; 2] = ["24 h", "12 h"];
/// 0 2 6 2, FM AGC: 0 is Auto, 1-10 fixed gain (0-36 dB in 4 dB steps).
pub const AGC_MAX: u8 = 10;
pub const UNITS: [&str; 3] = ["Metric", "Imperial", "Nautical"];
pub const LAYOUTS: [&str; 3] = ["Watch", "Single + scope", "Watch + activity"];
pub const THEMES: [&str; 4] = ["Dark", "Daylight", "Night red", "Cyberpunk"];
pub const PIN_MODES: [&str; 3] = ["Off", "At power on", "Power on + unlock"];
pub const ON_OFF: [&str; 2] = ["Off", "On"];
/// Power-on message (0 7 3 2): what the logo screen shows under the version (BOOT-9).
pub const POWER_ON: [&str; 3] = ["Off", "Callsign", "Text"];
/// Boot screen (0 7 3 3).
pub const BOOT_SCREENS: [&str; 2] = ["Logo", "Owner card"];
/// The cable port (0 9 2 3, TERM-2): auto picks from the first bytes; the others force one.
pub const CABLE_PORTS: [&str; 3] = ["Auto", "Link", "Terminal"]; // KISS on the cable is 0 5 8 alone (decision 386)
/// 0 5 8's values (KISS-6): which port is the KISS TNC.
pub const KISS_PORTS: [&str; 3] = ["Off", "Cable", "Bluetooth"];
/// 0 5 9 7 (WATCH-17.3, decision 295): who wins a chip shared with the APRS slot. 0, the default, APRS first.
pub const APRS_SHARE: [&str; 2] = ["APRS first", "A\u{b7}B first"];

/// SAT-22 (decision 341): the satellite options' minimum elevation, whole degrees: a pass whose highest point is
/// lower isn't listed. 10° to start.
pub const SAT_MIN_EL: [u8; 5] = [0, 5, 10, 15, 20];
pub const SAT_MIN_EL_START: u8 = 2;

/// FM broadcast step (0 2 6 1) in 10 kHz units, and its names.
pub const FM_STEPS: [(u16, &str); 3] = [(5, "50 kHz"), (10, "100 kHz"), (20, "200 kHz")];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// 0 8 6 1, capitals (SETUP-12).
    pub callsign: Text<10>,
    /// The owner card (SETUP-11): name, phone, e-mail. Entered on the radio only in setup page 4, then edited
    /// in the web app only; shown at boot when [`Settings::boot_card`] is on.
    pub owner: [Text<20>; 3],
    /// 0 7 3 3: the boot screen is the owner card (BOOT_SCREENS[1]), not the logo.
    pub boot_card: bool,
    /// 0 7 3 2: the boot screen's line under the logo, an index into [`POWER_ON`] (BOOT-9), and its text.
    pub pon: u8,
    pub pon_text: Text<16>,
    /// 0 9 4: 0 = not chosen yet, else REGION_LIST[n - 1].
    pub region: u8,
    /// 0 8 8, and whether its warning has been shown (shown once: SETUP-8).
    pub extend_tx: bool,
    pub extend_warned: bool,
    /// 0 6 5, an index into TIME_ZONES; 0 6 6 format and from GPS; 0 6 4.
    pub tz: u8,
    pub clock: u8,
    pub clock_gps: bool,
    pub units: u8,
    /// 0 7 2, 0 7 1.
    pub layout: u8,
    pub theme: u8,
    /// 0 9 3 1: mode (PIN_MODES), the PIN and its length (0 = none).
    pub pin_mode: u8,
    pub pin: u32,
    pub pin_len: u8,
    /// Not settings: wrong PINs so far, and the wait's end in seconds of app time (0 none: PIN-3).
    pub pin_misses: u8,
    pub pin_wait: u16,
    /// 0 9 2 1.
    pub bt: bool,
    /// 0 9 2 3: what the cable carries, an index into [`CABLE_PORTS`] (TERM-2).
    pub cable_port: u8,
    /// 0 5 8: the KISS TNC's port, an index into [`KISS_PORTS`] (KISS-6).
    pub kiss_port: u8,
    /// 0 4 5 1 (US and Canada only).
    pub same: bool,
    /// 0 4 2 (DEC-4, DEC-18): the pager decoder on or off, its frequency, and the one capcode it keeps (0: every
    /// capcode).
    pub pocsag_on: bool,
    pub pocsag_freq: Freq10,
    pub pocsag_cap: u32,
    /// 0 4 4 (DEC-7): a toast for each decoder hit (pages and the other decoders); off, they only go to the
    /// event log and the inbox count.
    pub popups: bool,
    /// 0 7 5 1-3 (SND-3, SND-6): key beep, beep volume (1-5, relative to voice: the knob scales both) and alert
    /// tones.
    pub key_beep: bool,
    pub beep_vol: u8,
    pub alert_tones: bool,
    /// 0 8 2 (VOX-1 to VOX-5, VOX-13): VOX on, its level (1-9, 1 the least sensitive) and its tail (index into
    /// [`VOX_TAILS`]).
    pub vox: bool,
    pub vox_level: u8,
    pub vox_tail: u8,
    /// 0 8 3 (VOX-15): Mic gain 1–12, 5 stock's default REG_7D.
    pub mic_gain: u8,
    /// 0 8 1 (TX-30): the transmit timeout in 30 s steps, 1-20 (30-600 s; stock's TOT counts 30 s too).
    pub tx_timeout: u8,
    /// 0 2 6 4 (BCR-5): the broadcast's sleep timer, an index into [`SLEEP_TIMES`] (0 off).
    pub fm_sleep: u8,
    /// 0 7 4 2 (DISP-16, DISP-23): the screen's brightness, 1-10, and night red's own level (the item sets the one
    /// for the theme in use).
    pub brightness: u8,
    pub brightness_night: u8,
    /// 0 7 4 3 (DISP-26): the backlight stays on through a transmission.
    pub tx_light: bool,
    /// 0 7 4 4 (DISP-15): what wakes the screen ([`WAKE_ON`]): 0 keys, 1 and receive, 2 and transmit.
    pub wake_on: u8,
    /// Unused, kept so the record's length stays and a later field doesn't read an old value: it held the old
    /// MCU temperature offset (SBAR-21) until decision 112 dropped it. Never read; whatever an older record left
    /// there has no effect.
    pub spare_temp: u8,
    /// FM broadcast: frequency in 10 kHz, step index, AGC (0 auto, else gain dB + 1), radio monitor (0 2 6).
    pub fm_freq: u16,
    pub fm_step: u8,
    pub fm_agc: u8,
    pub fm_monitor: bool,
    /// Setup has finished once (the layout header exists).
    pub setup_done: bool,
    /// The one home channel's number (CHAN-17), if set.
    pub home_ch: Option<u16>,
    /// The crossband repeater's settings (chapter 19).
    pub xb: XbCfg,
    /// HF mode's VFO, options and recent list (HF-27 to HF-29): its own SPI sector.
    pub hf: HfMem,
    /// 0 6 3: how positions are written, an index into [`COORD_FORMATS`].
    pub coord_fmt: u8,
    /// 0 6 9: the fixed position (degrees x 1e7) and altitude in metres, used with no fix (GPS-3); off = none.
    pub fixed_on: bool,
    pub fixed: crate::data::Pos,
    pub fixed_alt: i16,
    /// 0 4 5 2: SAME location codes, six digits each (FIPS in the US, CLC in Canada); 0 = an empty place (WX-10).
    pub same_codes: [u32; SAME_CODES],
    /// Band scope (0 3 1): bars (an index into [`SCOPE_BAR_COUNTS`]), step (into the tuning step list), dwell
    /// per point in 0.1 ms (1-40), trigger action ([`SCOPE_TRIGGERS`]), the trigger line (0 none, 1-9: hold a
    /// digit on the scope), the waterfall row, and the scope's mode (FM, NFM, AM: \* on the scope).
    pub scope_bars: u8,
    pub scope_step: u8,
    pub scope_dwell: u8,
    pub scope_trig: u8,
    pub scope_line: u8,
    pub scope_wf: bool,
    pub scope_mode: u8,
    /// 0 9 9 3: the cable terminal in ANSI colour (TERM-18); off, reverse video and bold only.
    pub term_colour: bool,
    /// 0 5 3: APRS mode, slot C as the APRS slot (APRS-15 – APRS-22; the takeover itself comes in milestone 5).
    pub aprs_mode: bool,
    /// Message text is typed predictively (TEXT-10); hold \* switches, and the choice is kept for the next one.
    pub predictive: bool,
    /// 0 7 7 2 (PWR-24): Low Power Mode, [`LPM_MODES`]: 0 off, 1 on, 2 auto; and 0 7 7 3, the battery % Auto
    /// enters below (10, 20, 30 or 40). Two bytes taken from the spare below (decision 234): an older record has
    /// zeros there, Off and (sanitized) 20 %.
    pub lpm: u8,
    pub lpm_below: u8,
    /// Unused, kept so the fields after it stay where older records have them: it held Bluetooth approval tokens
    /// until decision 104 (PIN only). Saved as zeros.
    pub spare_tokens: [u8; 30],
    /// 0 7 4 1: the screen goes dark after this ([`BACKLIGHT_TIMES`] index; 0 never).
    pub backlight: u8,
    /// 0 7 4 5: the keypad light ([`KEYPAD_LIGHTS`]).
    pub keypad_light: u8,
    /// 0 7 6: what the LEDs show ([`LED_MODES`]).
    pub leds: u8,
    /// 0 7 8: auto power off ([`AUTO_OFF_TIMES`] index; 0 off).
    pub auto_off: u8,
    /// 0 9 1 1: auto key lock (KEY-20, [`AUTO_LOCK_TIMES`] index; 0 off).
    pub auto_lock: u8,
    /// 0 8 5 4: hear the DTMF digits sent, at beep volume.
    pub dtmf_sidetone: bool,
    /// 0 7 5 4: a tone on arriving at a waypoint.
    pub arrival_tone: bool,
    /// The channel lists show hidden channels too (their options).
    pub show_hidden: bool,
    /// 0 9 3 2: channel-only mode (PIN-5): no V/M, MENU 0 or editing; hold # leaves it.
    pub chan_only: bool,
    /// Cable log (0 9 9 5, TERM-28): the plain log on the cable in the normal build, until a byte arrives. Its
    /// bit was Follow's (0 1 6 2 then, dropped in decision 81), so the saved flag bits after it keep their places.
    pub cable_log: bool,
    /// 0 5 5 4: APRS beacons send the fixed position even with a GPS fix (APRS-31; the bit was Follow return's,
    /// dropped in decision 81, so an old record reads GPS).
    pub aprs_fixed: bool,
    /// 0 1 6 3: channel scans also listen on each channel's input (its TX frequency) when it has a shift or
    /// split (SCAN-22).
    pub scan_tx: bool,
    /// 0 8 8 1: the CTCSS/DCS level sent, around the calibration's ([`TONE_LEVELS`]; TONE-16).
    pub tone_level: u8,
    /// 0 1 2: the radio's resume rule (SCAN-5), as `scan::Resume::from_code`: 1-10 s, 11 carrier drop, 12 stop
    /// on hit. Zones and range presets can have their own.
    pub scan_resume: u8,
    /// 0 1 3: the priority check's interval, seconds; 0 off (SCAN-12).
    pub scan_prio: u8,
    /// 0 8 8 2: tail elimination ([`TAIL_MODES`]: off, receive, transmit, both, as bits: 1 receive, 2 transmit).
    pub tail_mode: u8,
    /// 0 8 8 3: its phase ([`TAIL_PHASES`]: 55 Hz, 120°, 180°, 240°; the phases on CTCSS only, TONE-5).
    pub tail_phase: u8,
    /// 0 8 4: the end tone ([`END_TONE_NAMES`]; 0 off), a channel's own over it (TONE-10).
    pub end_tone: u8,
    /// 0 8 5 3: DTMF speed, ms each tone and each gap (50-200, DTMF-3).
    pub dtmf_ms: u8,
    /// 0 8 5 5: DTMF first digit delay in 10 ms (0-100: 0-1000 ms, DTMF-3).
    pub dtmf_delay: u8,
    /// 0 8 6 2: CW ID ([`CW_ID_MODES`]: off, every 10 min, on unkey; VOX-12).
    pub cw_id: u8,
    /// 0 6 1: the GPS receiver (GPS-1): [`GPS_OFF`], [`GPS_ON`] or [`GPS_AUTO`]. Stored as the old on/off byte was
    /// (0 off, 1 on), Auto as 2, so a radio keeps the setting it had (GPS-1.4).
    pub gps: u8,
    /// 0 6 8: GPS during RX/TX ([`GPS_RXTX`]: bit 0 off while receiving, bit 1 off while transmitting; GPS-4).
    pub gps_rxtx: u8,
    /// 0 6 7's arrival alarm: an index into [`ARRIVAL_M`] (0 off; GPS-12).
    pub arrival: u8,
    /// 0 7 9: an idle menu goes home after this ([`MENU_TIMEOUTS_S`]; 0 never; MENU-28).
    pub menu_timeout: u8,
    /// 0 1 6 1: how long # on home pauses watch ([`PAUSE_TIMES_S`]; WATCH-6).
    pub pause_time: u8,
    /// Unused, kept so the record's length stays: it held 0 9 1 2's Lock covers until decision 304 made the lock's
    /// mode a choice at each lock (KEY-14.5). Never read; whatever an older record left there has no effect.
    pub spare_lock: u8,
    /// 0 8 7: busy lockout for new channels and the VFOs (TX-21.1).
    pub busy_default: bool,
    /// 0 7 3 1: the cards' S-meter as dBm instead of bars (SQL-12.3).
    pub smeter_dbm: bool,
    /// 0 2 4: what a channel shows on home ([`CHANNEL_DISPLAY`]: name, number, frequency; CHAN-33).
    pub ch_display: u8,
    /// 0 1 6 2: frequency and channel scans start going down (SCAN-26).
    pub scan_down: bool,
    /// 0 1 6 4: a scan stopped by hand leaves the slot on its last hit instead of where it started (SCAN-6).
    pub scan_return_hit: bool,
    /// 0 7 5 6: the message sound's own volume (`tonegen::CHIME_LEVELS`; APRS-6.2).
    pub msg_vol: u8,
    /// 0 9 9 4: the MCU clock (PWR-26.5): false Dynamic (24 MHz at rest, the build's speed while something needs
    /// it), true Fixed (always the build's speed, as before the governor).
    pub mcu_fixed: bool,
    /// 0 9 5: who decides the squelch (SQL-18, decision 269): false the software squelch (the default), true the
    /// chip's own, stock's way.
    pub sq_chip: bool,
    /// 0 5 9 6: C starts muted in APRS mode (APRS-2, APRS-15.2); \* on C in APRS mode flips it.
    pub aprs_mute: bool,
    /// 0 4 3 DTMF decode (DEC-6): the digits heard on a slot's card.
    pub dtmf_rx: bool,
    /// 0 5 9 7 (WATCH-17.3, decision 295): [`APRS_SHARE`]: 0 APRS first (APRS mode's C takes long turns on a chip
    /// it shares, the other slots short looks), 1 A·B first (WATCH-17.2's turns; decision 313 renamed Voice first).
    pub aprs_share: u8,
    /// 0 2 5 Local zone (CHAN-34, decision 326): an index into [`crate::local::RANGES`], 0 Off; 50 km by default.
    pub local: u8,
    /// SAT-22 (decision 341): the pass list's minimum elevation, an index into [`SAT_MIN_EL`]; 10° by default.
    pub sat_min_el: u8,
    /// 0 5 9 8 Decode log (APRS-56): each packet heard but not decoded goes to the event log. Off by default.
    pub decode_log: bool,
}

/// 0 7 9's choices, seconds (0 never; MENU-28) and names. 30 s by default.
pub const MENU_TIMEOUTS_S: [u8; 6] = [0, 10, 20, 30, 45, 60];
pub const MENU_TIMEOUT_NAMES: [&str; 6] = ["Off", "10 s", "20 s", "30 s", "45 s", "60 s"];
/// 0 1 6 1's choices, seconds (WATCH-6) and names. 1 min by default.
pub const PAUSE_TIMES_S: [u16; 3] = [30, 60, 300];
pub const PAUSE_TIME_NAMES: [&str; 3] = ["30 s", "1 min", "5 min"];
/// 0 2 4's choices (CHAN-33).
pub const CHANNEL_DISPLAY: [&str; 3] = ["Name", "Number", "Frequency"];

/// 0 6 8's values (GPS-4, GPS-14).
pub const GPS_RXTX: [&str; 4] = ["Never", "Receive", "Transmit", "Both"];
/// The arrival alarm's radii in metres (0 6 7, GPS-12): off, then 50 m to 1 km.
pub const ARRIVAL_M: [u16; 6] = [0, 50, 100, 200, 500, 1000];

/// 0 8 8 2's values: bit 0 receive, bit 1 transmit.
pub const TAIL_MODES: [&str; 4] = ["Off", "Receive", "Transmit", "Both"];
/// 0 8 8 3's values (TONE-5).
pub const TAIL_PHASES: [&str; 4] = ["55 Hz", "120\u{b0}", "180\u{b0}", "240\u{b0}"];
/// 0 8 4's values (TONE-10), as `tonegen::END_TONES`.
pub const END_TONE_NAMES: [&str; 6] = ["Off", "Beep", "Two-tone up", "Two-tone down", "Chirp", "Quindar"];
/// 0 8 6 2's values (VOX-12).
pub const CW_ID_MODES: [&str; 3] = ["Off", "Every 10 min", "On unkey"];

/// 0 8 8 1's values (TONE-16).
/// 0 8 2 3's VOX tails, in 100 ms (VOX-5: 2 s by default, as nicFW's VOX Tail).
pub const VOX_TAILS: [u8; 6] = [5, 10, 15, 20, 30, 50];
/// 0 2 6 4's sleep times in minutes (BCR-5: off, 15-120 min).
pub const SLEEP_TIMES: [u8; 7] = [0, 15, 30, 45, 60, 90, 120];
pub const TONE_LEVELS: [&str; 3] = ["Low", "Normal", "High"];

/// 0 7 4 1's values, seconds (0 never).
pub const BACKLIGHT_TIMES: [u16; 9] = [0, 5, 10, 15, 30, 60, 120, 300, 600];
/// 0 7 4 4's values (DISP-15).
pub const WAKE_ON: [&str; 3] = ["Keys", "Keys and RX", "Keys, RX and TX"];
/// The night red theme's index in [`THEMES`] (DISP-23: its own brightness).
pub const NIGHT_RED: u8 = 2;
/// 0 7 4 5's values.
/// 0 6 1 (GPS-1): the stored values. Off and On are the old on/off byte's 0 and 1 (GPS-1.4: a radio keeps its
/// setting); Auto (GPS-1.5) is new.
pub const GPS_OFF: u8 = 0;
pub const GPS_ON: u8 = 1;
pub const GPS_AUTO: u8 = 2;
/// 0 6 1's values in menu order (Off, Auto, On), and the stored value of each.
pub const GPS_MODES: [&str; 3] = ["Off", "Auto", "On"];
pub const GPS_ORDER: [u8; 3] = [GPS_OFF, GPS_AUTO, GPS_ON];
/// 0 7 7 2 (PWR-24): Low Power Mode.
pub const LPM_MODES: [&str; 3] = ["Off", "On", "Auto"];
/// 0 7 7 3 (PWR-24.3): the battery % Auto enters below.
pub const LPM_BELOW: [u8; 4] = [10, 20, 30, 40];
pub const KEYPAD_LIGHTS: [&str; 4] = ["Off", "With screen", "On key press", "Always"];
/// 0 7 6's values: the heartbeat is the dark-screen flash in the battery's colour. Short enough for the menu's
/// value column and the terminal's `set` (24 characters).
pub const LED_MODES: [&str; 3] = ["Off", "RX and TX", "RX, TX, heartbeat"];
/// 0 7 8's values, minutes (0 off).
pub const AUTO_OFF_TIMES: [u16; 5] = [0, 30, 60, 120, 240];
/// 0 9 1 1's values, seconds (0 off).
pub const AUTO_LOCK_TIMES: [u16; 5] = [0, 30, 60, 120, 300];
/// The lock's modes' names (KEY-14; the Status page's Key lock row), [`Lock`]'s order.
pub const LOCK_MODES: [&str; 3] = ["Keypad", "All but PTT", "All"];

/// What the key lock covers (KEY-14), chosen as it locks (KEY-13).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lock {
    /// KEY-14.1: the front keys and the tuning knob; PTT and the side keys work.
    Keypad,
    /// KEY-14.2: every key and the knob, the side keys too; PTT works.
    AllButPtt,
    /// KEY-14.3: PTT too.
    All,
}

impl Lock {
    /// Its name ([`LOCK_MODES`]).
    pub fn name(self) -> &'static str {
        LOCK_MODES[self as usize]
    }
}


/// 0 3 1 1's values (SCOPE-9).
pub const SCOPE_BAR_COUNTS: [u8; 4] = [30, 60, 120, 240];
/// 0 3 1 4's values (SCOPE-10).
pub const SCOPE_TRIGGERS: [&str; 3] = ["Off", "Monitor", "Log"];

/// 0 6 3's values.
pub const COORD_FORMATS: [&str; 4] = ["Decimal", "DDM", "DMS", "Grid square"];
/// Up to 8 SAME location codes (WX-10).
pub const SAME_CODES: usize = 8;

impl Settings {
    /// KISS-6: the cable carries raw KISS (0 5 8 on Cable).
    pub fn kiss_on_cable(&self) -> bool {
        self.kiss_port == 1
    }

    /// KISS-5, KISS-6: Bluetooth carries raw KISS (0 5 8 on Bluetooth).
    pub fn kiss_on_bt(&self) -> bool {
        self.kiss_port == 2
    }

    /// KISS is on, on the cable or on Bluetooth (with Bluetooth on): the KISS task's test (KISS-6), and what makes
    /// slot C the APRS slot (KISS-9).
    pub fn kiss_on(&self) -> bool {
        self.kiss_on_cable() || self.kiss_on_bt() && self.bt
    }

    pub const DEFAULT: Settings = Settings {
        callsign: Text::EMPTY,
        owner: [Text::EMPTY; 3],
        boot_card: false,
        pon: 1,
        pon_text: Text::EMPTY,
        region: 0,
        extend_tx: false,
        extend_warned: false,
        tz: TZ_DEFAULT,
        clock: 0,
        clock_gps: true,
        units: 0,
        layout: 0,
        theme: 0,
        pin_mode: 0,
        pin: 0,
        pin_len: 0,
        pin_misses: 0,
        pin_wait: 0,
        bt: false,
        cable_port: 0,
        kiss_port: 0,
        same: true,
        pocsag_on: false,
        pocsag_freq: 43_998_750,
        pocsag_cap: 0,
        popups: true,
        key_beep: true,
        beep_vol: 2,
        alert_tones: true,
        vox: false,
        vox_level: 5,
        vox_tail: 3,
        mic_gain: 5,
        tx_timeout: 4,
        fm_sleep: 0,
        brightness: 7,
        brightness_night: 3,
        tx_light: true,
        wake_on: 1,
        spare_temp: 0,
        fm_freq: 10_110,
        fm_step: 1,
        fm_agc: 0,
        fm_monitor: true,
        setup_done: false,
        home_ch: None,
        xb: XbCfg::DEFAULT,
        hf: HfMem::DEFAULT,
        coord_fmt: 1,
        fixed_on: false,
        fixed: crate::data::Pos { lat: 0, lon: 0 },
        fixed_alt: 0,
        same_codes: [0; SAME_CODES],
        scope_bars: 1,
        scope_step: 11, // 25 kHz
        scope_dwell: 100, // 10 ms a point, stock's (V0.27 0x08023808: one 10 ms tick between tune and RSSI)
        scope_trig: 0,
        scope_line: 0,
        scope_wf: false,
        scope_mode: 0,
        term_colour: false,
        aprs_mode: false,
        predictive: true,
        lpm: 2, // PWR-24.1: Auto by default (Alex, Oct 21, 2026)
        lpm_below: 20,
        spare_tokens: [0; 30],
        backlight: 5, // 60 s (15 s felt short on the radio, Oct 12, 2026)
        keypad_light: 1,
        leds: 2,
        auto_off: 0,
        auto_lock: 0,
        dtmf_sidetone: true,
        arrival_tone: true,
        show_hidden: false,
        chan_only: false,
        cable_log: false,
        aprs_fixed: false,
        scan_tx: false,
        tone_level: 1,
        scan_resume: 5,
        scan_prio: 2,
        tail_mode: 0,
        tail_phase: 0,
        end_tone: 0,
        dtmf_ms: 100,
        dtmf_delay: 30,
        cw_id: 0,
        gps: GPS_AUTO, // GPS-1.4: Auto by default (decision 268)
        gps_rxtx: 0,
        arrival: 0,
        menu_timeout: 3,
        pause_time: 1,
        spare_lock: 0,
        busy_default: false,
        smeter_dbm: false,
        ch_display: 0,
        scan_down: false,
        scan_return_hit: false,
        msg_vol: 3,
        mcu_fixed: false,
        sq_chip: false,
        aprs_mute: true,
        dtmf_rx: true,
        aprs_share: 0,
        local: crate::local::DEFAULT_RANGE,
        sat_min_el: SAT_MIN_EL_START,
        decode_log: false,
    };

    /// Values a stored or linked record may hold out of range, brought back in (load_settings, the link's
    /// settings write). FM AGC (0 2 6 2) is 0-10 and the clock (0 6 6) 0-1; the menu multiplies both in a u8,
    /// which overflowed on a bad record (found in the settings table refactor). Most other list
    /// settings are read modulo their list and need nothing here.
    pub fn sanitize(&mut self) {
        // Decision 386: 0 9 2 3's KISS (3) did what 0 5 8 on Cable does: a radio saved with it keeps KISS on the cable
        if self.cable_port == 3 {
            (self.cable_port, self.kiss_port) = (0, 1);
        }
        if self.cable_port as usize >= CABLE_PORTS.len() {
            self.cable_port = Self::DEFAULT.cable_port;
        }
        if self.fm_agc > AGC_MAX {
            self.fm_agc = Self::DEFAULT.fm_agc;
        }
        if self.clock as usize >= CLOCKS.len() {
            self.clock = Self::DEFAULT.clock;
        }
        if !(1..=ferrite_core::vox::MIC_GAIN_MAX).contains(&self.mic_gain) {
            self.mic_gain = Self::DEFAULT.mic_gain;
        }
        if !(1..=20).contains(&self.tx_timeout) {
            self.tx_timeout = Self::DEFAULT.tx_timeout;
        }
        if self.lpm as usize >= LPM_MODES.len() {
            self.lpm = 0;
        }
        if self.gps > GPS_AUTO {
            self.gps = GPS_AUTO;
        }
        if self.local as usize >= crate::local::RANGES.len() {
            self.local = Self::DEFAULT.local; // CHAN-34
        }
        if self.sat_min_el as usize >= SAT_MIN_EL.len() {
            self.sat_min_el = SAT_MIN_EL_START; // SAT-22
        }
        if !LPM_BELOW.contains(&self.lpm_below) {
            self.lpm_below = Self::DEFAULT.lpm_below; // PWR-24.1: an older record's zero
        }
    }

    /// TX-30: 0 8 1's timeout in seconds, 30-600.
    pub fn tx_timeout_s(&self) -> u32 {
        self.tx_timeout.clamp(1, 20) as u32 * 30
    }

    /// 0 7 4 2: the brightness for the theme in use, 1-10 (DISP-23: night red keeps its own).
    /// MENU-28: the menu timeout in ms, or none (0 7 9 Off).
    pub fn menu_timeout_ms(&self) -> Option<u32> {
        let s = MENU_TIMEOUTS_S[(self.menu_timeout as usize).min(MENU_TIMEOUTS_S.len() - 1)];
        (s != 0).then_some(s as u32 * 1000)
    }

    /// WATCH-6: how long # pauses watch, seconds (0 1 6 1).
    pub fn pause_s(&self) -> u16 {
        PAUSE_TIMES_S[(self.pause_time as usize).min(PAUSE_TIMES_S.len() - 1)]
    }

    /// APRS-6.2: the message sound's DAC level.
    pub fn msg_level(&self) -> u8 {
        ferrite_core::tonegen::CHIME_LEVELS[(self.msg_vol as usize).min(3)]
    }

    pub fn brightness(&self) -> u8 {
        (if self.theme == NIGHT_RED { self.brightness_night } else { self.brightness }).clamp(1, 10)
    }

    /// 0 7 4 2 for the theme in use.
    pub fn brightness_mut(&mut self) -> &mut u8 {
        if self.theme == NIGHT_RED { &mut self.brightness_night } else { &mut self.brightness }
    }

    /// The region's name, or "Not set".
    pub fn region_name(&self) -> &'static str {
        match self.region {
            1..=8 => REGION_LIST[self.region as usize - 1],
            _ => "Not set",
        }
    }

    /// The region, None until setup (or 0 9 4) sets it.
    pub fn region(&self) -> Option<Region> {
        Region::ALL.get((self.region as usize).wrapping_sub(1)).copied()
    }

    /// The region for the transmit plan: until one is set, the ITU common plan ("Other").
    pub fn tx_region(&self) -> Region {
        self.region().unwrap_or(Region::Other)
    }

    pub fn set_region(&mut self, r: Option<Region>) {
        self.region = r.and_then(|r| Region::ALL.iter().position(|&x| x == r)).map_or(0, |i| i as u8 + 1);
    }

    /// Weather radio and SAME exist only in the US and Canada (WX-3).
    pub fn wx_region(&self) -> bool {
        self.region().is_some_and(|r| r.has_weather())
    }

    /// Weather radio may open: in the US and Canada, and until a region is set (WX-1).
    pub fn wx_allowed(&self) -> bool {
        self.region().is_none_or(|r| r.has_weather())
    }

    /// The callsign, capitals (SETUP-12).
    pub fn set_callsign(&mut self, c: &[u8]) {
        self.callsign = Text::EMPTY;
        for &b in c {
            self.callsign.push(b.to_ascii_uppercase());
        }
    }

    /// The boot screen's callsign (BOOT-9): None until one is set.
    pub fn boot_callsign(&self) -> Option<&[u8]> {
        (!self.callsign.is_empty()).then(|| self.callsign.as_bytes())
    }

    /// The logo screen's line (BOOT-9), as Power-on message (0 7 3 2) says: the callsign, the text, or
    /// nothing (also when the one picked is empty).
    pub fn boot_line(&self) -> Option<&[u8]> {
        let t = match self.pon {
            1 => &self.callsign.as_bytes(),
            2 => &self.pon_text.as_bytes(),
            _ => return None,
        };
        (!t.is_empty()).then_some(*t)
    }

    /// The owner card for the boot screen: Some when 0 7 3 3 is "Owner card" and the card has something on it
    /// (a callsign, name, phone or e-mail); else None, and the boot screen shows the logo.
    pub fn boot_owner(&self) -> Option<Owner<'_>> {
        let [name, phone, email] = &self.owner;
        let any = !self.callsign.is_empty() || self.owner.iter().any(|t| !t.is_empty());
        (self.boot_card && any).then(|| Owner { name: name.as_bytes(), phone: phone.as_bytes(), email: email.as_bytes() })
    }

    /// SAME alerts are on: the setting, in a region that has them.
    pub fn same_on(&self) -> bool {
        self.same && self.wx_region()
    }

    /// "UTC-8", "UTC+5:30", "UTC".
    pub fn tz_text(&self, b: &mut Buf<16>) {
        tz_text(TIME_ZONES[self.tz as usize % TIME_ZONES.len()], b);
    }
}

pub fn tz_text(q: i8, b: &mut Buf<16>) {
    b.set(b"UTC");
    if q != 0 {
        let a = q.unsigned_abs();
        let _ = uwrite!(b, "{}{}", if q < 0 { "-" } else { "+" }, a / 4);
        if !a.is_multiple_of(4) {
            let _ = uwrite!(b, ":{}", (a % 4) * 15);
        }
    }
}
