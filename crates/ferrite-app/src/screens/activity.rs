//! What the radio-activity screens (group B: transmit, scan, scope, tone search, counter, FM broadcast, HF
//! mode, weather, crossband) share, held once on the App rather than in each screen's state (the screen and
//! its BACK copies stay a few bytes): the transmit gates, the exclusive mode a screen started (so leaving it,
//! by any route, stops it on the Radio task), the last hits a probe screen lists, each slot's last minute of
//! activity (the watch + activity layout) and HF mode's tuning state. Settings (region, Extend TX, callsign,
//! layout, the crossband, FM and HF memories) are in [`crate::settings`].

use crate::{App, Screen};
use ferrite_core::freq::Freq10;
use ferrite_core::msg::{BroadcastCmd, Device, LiveAll, RadioCmd, SLOTS};
use ferrite_core::txplan::Region;

/// The exclusive mode a screen started on the Radio task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// The band scope (SCOPE-4), or home's scope strip (single + scope layout).
    Scope,
    Strip,
    Tone,
    Counter,
    Fm,
    Hf,
    Xb,
    /// Satellite mode (SAT-1): both chips, the downlink tuned.
    Sat,
}

/// One remembered hit: what (a frequency, or a tone word), when it was last heard and when that stop began
/// (s, wrapping: its length, SCAN-23.1), how strong (dBm), how many times (SCAN-23's count).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Hit {
    pub what: u32,
    pub at_s: u16,
    pub from_s: u16,
    pub dbm: i8,
    pub n: u8,
}

impl Hit {
    pub fn new(what: u32, now: u32, rssi: i16) -> Self {
        let at_s = (now / 1000) as u16;
        Hit { what, at_s, from_s: at_s, dbm: rssi.clamp(-128, 0) as i8, n: 1 }
    }

    /// How long its last stop lasted (SCAN-23.1), in seconds.
    pub fn len_s(&self) -> u32 {
        self.at_s.wrapping_sub(self.from_s) as u32
    }

    pub fn age_ms(&self, now: u32) -> u32 {
        ((now / 1000) as u16).wrapping_sub(self.at_s) as u32 * 1000
    }

    pub fn rssi(&self) -> i16 {
        self.dbm as i16
    }
}

/// The last hits the probe and scan screens show.
pub const HITS: usize = 3;
/// The hits kept: a range scan's hit list (SCAN-23, nicFW's smart scan list), the counter's and tone search's
/// last few. 12 bytes each.
pub const HIT_LIST: usize = 16;

/// Activity buckets per slot for the watch + activity layout: 32 of 1.875 s, the last minute.
pub const ACT_BUCKETS: u32 = 32;
pub const ACT_MS: u32 = 1875;

/// MODE-4: home's scope strip comes back this long after the active frequency last moved (each knob step would
/// otherwise restart the sweep and the knob lagged on the radio, Oct 12, 2026).
pub const STRIP_REST_MS: u32 = 500;

pub struct RadioUi {
    /// From the last [`Device`] drawn: the build can transmit, the battery is above the cut-off, the SSB patch
    /// is installed (PTT and HF mode's keys arrive without the Device).
    pub tx_built: bool,
    pub batt_cut: bool,
    pub ssb_patch: bool,
    /// The slot keyed (3 = HF), if any.
    pub keyed: Option<u8>,
    pub mode: Mode,
    /// When the timed screen up now began (transmit, emergency prompt, tone search), or ends.
    pub t0: u32,
    /// The scope's centre, kept between openings.
    pub centre: Freq10,
    pub scope_hf: bool,
    pub hits: [Hit; HIT_LIST],
    /// The scan hit count ([`ferrite_core::msg::SlotLive::hits`]) last counted into the list.
    pub hits_seen: u8,
    /// Scan: zones 0-15 added with digits (SCAN-3).
    pub zones: u16,
    /// Bit i of act[s]: slot s heard something in bucket (now / ACT_MS - i).
    pub act: [u32; SLOTS],
    pub act_bucket: u16,
    /// HF mode: when the frequency was tuned and whether it is in the recent list yet (HF-28), and a
    /// frequency being typed, as a number (the screen state holds its length and point).
    pub hf_tuned_at: u32,
    pub hf_counted: bool,
    /// HF-12: when SIDE2 last changed the mode (its row shows a moment); 0 none.
    pub hf_pick_at: u32,
    /// A seek (hold 3 or 1) is running: the receiver's frequency is taken as it lands.
    pub hf_seeking: bool,
    pub hf_entry: u32,
    /// A channel scan's list still going out to the Radio task ([`crate::chanscan`]), the store's channel version
    /// it was last sent for, and the channel-scanning slots whose list goes again (the channels changed).
    pub feed: Option<crate::chanscan::Feed>,
    pub scan_ver: u32,
    pub refresh: u8,
    /// The Radio task's last ask for a channel's frequencies, answered ([`ferrite_core::msg::LiveAll::want`]).
    pub want_seen: u16,
    /// The Radio task's count of dialled sequences sent ([`ferrite_core::msg::TxLive::dtmf_sent`]) as last seen
    /// unkeyed: a dialled transmission ends when it moves (DTMF-1).
    pub dtmf_seen: u8,
    /// MODE-1: the crossband repeater was started with # (XB-1) and not stopped; it runs while its screen is
    /// up (or under one it opened).
    pub xb_on: bool,
    /// XB-20.6: who started the repeater running now ([`super::xb::by_word`]), with [`super::xb::LISTEN`] set while
    /// SIDE1 is held (XB-15; one byte: the App's RAM budget); the start or stop not yet in the cable log (a stop
    /// with who and why is put here before the reconciler's stop).
    pub xb_by: u8,
    pub xb_log: Option<super::xb::XbLog>,
    /// MODE-4: the active slot's frequency as last seen, and when it last changed (home's strip waits for the
    /// knob to rest). None until first seen, which isn't a change.
    pub seen_freq: Option<Freq10>,
    pub freq_at: u32,
    /// MODE-3: when the lease on the running mode was last renewed, and the Radio task's count of modes it
    /// stopped itself as last seen.
    pub lease_at: u32,
    pub lease_drops: u8,
    /// BCR-22: the slot playing broadcast FM, and the slots in the broadcast band as last seen (one coming in
    /// takes the broadcast).
    pub bcast: Option<u8>,
    pub bcast_in: u8,
    /// BCR-5: when the broadcast slot took the broadcast, for the sleep timer.
    pub bcast_since: u32,
    /// BCR-4: the mute mask the Radio task was last sent ([`App::send_mute`]).
    pub mute_sent: u8,
    /// The broadcast plays on the SI4732: in [`Mode::Fm`] (the FM screen, or a broadcast slot), or beside home's
    /// strip while another slot than the active one plays broadcast (MODE-5, decision 115). Started and stopped
    /// by [`App::reconcile_mode`] apart from the exclusive mode, so the strip coming and going never restarts it.
    pub fm_on: bool,
    /// The FM station last sent to the SI4732 (10 kHz), and a seek landing since then.
    pub fm_sent: u16,
    pub fm_seek_at: Option<u32>,
}

impl RadioUi {
    pub const fn new() -> Self {
        RadioUi {
            tx_built: false,
            batt_cut: false,
            ssb_patch: false,
            keyed: None,
            mode: Mode::Off,
            t0: 0,
            centre: 0,
            scope_hf: false,
            hits: [Hit { what: 0, at_s: 0, from_s: 0, dbm: 0, n: 0 }; HIT_LIST],
            hits_seen: 0,
            dtmf_seen: 0,
            xb_on: false,
            xb_by: 0,
            xb_log: None,
            seen_freq: None,
            freq_at: 0,
            lease_at: 0,
            lease_drops: 0,
            zones: 0,
            act: [0; SLOTS],
            act_bucket: 0,
            hf_tuned_at: 0,
            hf_pick_at: 0,
            hf_counted: false,
            hf_seeking: false,
            hf_entry: 0,
            feed: None,
            scan_ver: 0,
            refresh: 0,
            want_seen: 0,
            bcast: None,
            bcast_since: 0,
            mute_sent: 0,
            bcast_in: 0,
            fm_on: false,
            fm_sent: 0,
            fm_seek_at: None,
        }
    }

    /// A hit at the front, the oldest going when the list is full. The newest again only refreshes it, unless
    /// `again` (a new stop there, SCAN-23): then it counts; one further down moves to the front and counts.
    pub fn push_hit(&mut self, h: Hit, again: bool) {
        if h.what == 0 {
            return;
        }
        // the same stop going on keeps its count and its start; a new one starts its length again
        let (n, from_s) = match self.hits.iter().position(|o| o.what == h.what) {
            Some(0) if !again => (self.hits[0].n, self.hits[0].from_s),
            Some(i) => {
                let n = self.hits[i].n.saturating_add(1);
                self.hits.copy_within(0..i, 1);
                (n, h.from_s)
            }
            None => {
                self.hits.copy_within(0..HIT_LIST - 1, 1);
                (1, h.from_s)
            }
        };
        self.hits[0] = Hit { n, from_s, ..h };
    }

    pub fn clear_hits(&mut self) {
        self.hits = [Hit::default(); HIT_LIST];
    }
}

impl App {
    /// Region (setup, MENU 0 9 4) and Extend TX (MENU 0 8 8), from outside (the simulator's samples, tests).
    pub fn set_region(&mut self, r: Option<Region>, extend_tx: bool) {
        self.set.set_region(r);
        self.set.extend_tx = extend_tx;
        self.region_steps();
    }

    /// Put a slot on its VFO at a frequency (the simulator's samples).
    pub fn set_vfo(&mut self, slot: u8, f: Freq10) {
        let s = &mut self.slots[slot as usize % SLOTS];
        s.vfo = f;
        s.vfo_mode = true;
        self.send_slot(slot % SLOTS as u8);
    }

    /// Put a slot on a channel that may not be in the store (the simulator's samples: a 16-character name, no name).
    pub fn set_channel_rec(&mut self, slot: u8, c: crate::chan::Channel) {
        let s = &mut self.slots[slot as usize % SLOTS];
        (s.zone, s.ch, s.vfo_mode) = (c.zone, Some(c), false);
        self.send_slot(slot % SLOTS as u8);
    }

    /// The callsign (setup page 2): crossband ID and automatic transmit need one.
    pub fn set_callsign(&mut self, c: &[u8]) {
        self.set.set_callsign(c);
        self.send_tx_tones(); // CW ID sends it (VOX-12)
    }

    /// Home layout (MENU 0 7 2): 0 watch, 1 single + scope, 2 watch + activity (HOME-3).
    pub fn set_layout(&mut self, l: u8) {
        self.set.layout = l % 3;
        self.reconcile_mode(); // MODE-1: the strip comes or goes with the layout
        self.redraw_all();
    }

    /// The radio-wide state that gates transmit, remembered from what is drawn (PTT arrives without it).
    pub(crate) fn note_device(&mut self, dev: &Device) {
        self.radio.tx_built = dev.tx_built;
        self.radio.batt_cut = dev.battery_cutoff;
        self.radio.ssb_patch = dev.selftest.ssb_patch;
    }

    /// MODE-1: the exclusive mode the radio should be in now, from the screen, what BACK returns to and the
    /// settings: the nearest mode screen up or under the screen up (a scope's typed centre, HF mode's recent list or
    /// save keep their mode), else home's scope strip (single + scope layout) once the active frequency has
    /// rested for [`STRIP_REST_MS`] (MODE-4) and the strip can run beside a broadcast (MODE-5:
    /// [`App::strip_blocked`]), else FM broadcast while a slot is in the broadcast band (BCR-22), else none.
    /// The strip with a broadcast slot keeps the broadcast playing beside it ([`RadioUi::fm_on`]). While keyed only HF mode (HF-23, it transmits in place)
    /// and the crossband repeater (XB-14, held through local PTT) keep running; the others stop before the
    /// key-up (BCR-16, SCOPE-12). Screens never start or stop a mode themselves: [`App::reconcile_mode`] does.
    pub(crate) fn wanted_mode(&self) -> Mode {
        let keyed = self.radio.keyed.is_some();
        let under = self.back.iter().rev().flatten().copied();
        for s in core::iter::once(self.screen).chain(under) {
            let m = match s {
                // PWR-24.4: no scope, counter, broadcast, HF mode or crossband in Low Power Mode
                Screen::Scope(_) | Screen::Counter(_) if self.low_power() => continue,
                Screen::Scope(_) => Mode::Scope,
                Screen::ToneSearch(_) => Mode::Tone,
                Screen::Counter(_) => Mode::Counter,
                Screen::Hf(_) | Screen::Crossband(_) if self.low_power() => continue,
                Screen::Hf(_) => Mode::Hf,
                Screen::Sat(_) => Mode::Sat,
                Screen::Crossband(_) if self.radio.xb_on => Mode::Xb,
                _ => continue,
            };
            return if keyed && !matches!(m, Mode::Hf | Mode::Xb | Mode::Sat) { Mode::Off } else { m };
        }
        if keyed || self.low_power() {
            return Mode::Off; // TX-24: the broadcast stops before the key-up and starts again after; PWR-24.4
        }
        let rested = self.radio.seen_freq.is_some() && self.now.wrapping_sub(self.radio.freq_at) >= STRIP_REST_MS;
        if self.screen == Screen::Home && self.set.layout == 1 && rested && !self.strip_blocked() {
            Mode::Strip
        } else if self.radio.bcast.is_some() {
            Mode::Fm
        } else {
            Mode::Off
        }
    }

    /// MODE-2: bring the radio to [`App::wanted_mode`]: the running mode's stop, then the wanted one's start,
    /// in that order; and while one runs, the lease renewed (MODE-3). Called after every key, knob step, screen
    /// change and tick, so every way of leaving a mode (BACK, hold BACK, a timeout, PTT, a setting, a reset) ends
    /// it, and nothing else ever has to remember to.
    pub(crate) fn reconcile_mode(&mut self) {
        self.bcast_update(); // BCR-22: which slot (if any) has the broadcast
        let f = self.slot(self.active).freq();
        if self.radio.seen_freq != Some(f) {
            // MODE-4: the knob (or a slot change) moved it; the first sight counts as long rested
            self.radio.freq_at = if self.radio.seen_freq.is_some() { self.now } else { self.now.wrapping_sub(STRIP_REST_MS) };
            self.radio.seen_freq = Some(f);
        }
        if !self.screen_or_under(|s| matches!(s, Screen::Crossband(_))) {
            self.radio.xb_on = false; // left the repeater's screen: it doesn't start again on return
        }
        let want = self.wanted_mode();
        // The broadcast: in its own mode, or beside the strip (MODE-5). Stopped before, started after the mode's
        // change (MODE-2's order), and left playing while the strip comes and goes.
        let fm = want == Mode::Fm || want == Mode::Strip && self.radio.bcast.is_some();
        if !fm && self.radio.fm_on {
            self.radio.fm_on = false;
            self.out.push(RadioCmd::Broadcast(BroadcastCmd::Off));
        }
        if want != self.radio.mode {
            self.stop_mode(want);
            self.radio.mode = want;
            self.start_cmds(want);
            self.radio.lease_at = self.now;
        } else if want != Mode::Off && self.now.wrapping_sub(self.radio.lease_at) >= ferrite_core::lease::RENEW_MS {
            if self.out.q.is_empty() {
                self.out.push(RadioCmd::Lease); // a queued command renews it as well (MODE-3)
            }
            self.radio.lease_at = self.now;
        }
        if fm && !self.radio.fm_on {
            let f = self.fm_freq();
            (self.radio.fm_on, self.radio.fm_sent, self.radio.fm_seek_at) = (true, f, None);
            self.out.push(RadioCmd::Broadcast(BroadcastCmd::Fm(f)));
            self.radio.lease_at = self.now;
        }
        self.fm_retune();
    }

    /// The screen up, or one BACK returns to, matches.
    fn screen_or_under(&self, f: impl Fn(&Screen) -> bool) -> bool {
        f(&self.screen) || self.back.iter().flatten().any(f)
    }

    /// A mode's start, from what it keeps (MODE-2).
    fn start_cmds(&mut self, m: Mode) {
        match m {
            Mode::Off => {}
            Mode::Scope => {
                let c = self.radio.centre;
                super::scope::send_sweep(self, c);
            }
            Mode::Strip => {
                let f = self.slot(self.active).freq();
                super::layouts::send_strip(self, f);
            }
            Mode::Tone => {
                let slot = match self.screen_or_under_tone() {
                    Some(s) => s,
                    None => self.active,
                };
                self.out.push(RadioCmd::ToneSearch { slot, on: true });
            }
            Mode::Counter => self.out.push(RadioCmd::Counter(true)),
            Mode::Fm => {} // the broadcast's start is the reconciler's (MODE-5)
            Mode::Hf => super::hf::send(self),
            Mode::Sat => self.sat_start(),
            Mode::Xb => {
                super::xb::send_start(self);
                self.radio.xb_log = Some(super::xb::XbLog { on: true, by: self.radio.xb_by, why: 0 }); // XB-20.6
            }
        }
    }

    /// The slot the tone search up (or under the screen up) searches.
    fn screen_or_under_tone(&self) -> Option<u8> {
        core::iter::once(&self.screen).chain(self.back.iter().flatten()).find_map(|s| match s {
            Screen::ToneSearch(t) => Some(t.slot),
            _ => None,
        })
    }

    /// The running mode's stop (MODE-2; only [`App::reconcile_mode`] calls it).
    fn stop_mode(&mut self, next: Mode) {
        let a = self.active;
        match core::mem::replace(&mut self.radio.mode, Mode::Off) {
            Mode::Off => {}
            Mode::Scope | Mode::Strip => {
                self.out.push(RadioCmd::Sweep { centre: 0, step: 0, on: false });
                if core::mem::take(&mut self.radio.scope_hf) && next != Mode::Hf {
                    self.out.push(RadioCmd::Active(a));
                }
            }
            Mode::Tone => self.out.push(RadioCmd::ToneSearch { slot: a, on: false }),
            Mode::Counter => self.out.push(RadioCmd::Counter(false)),
            Mode::Fm => {} // the broadcast's stop is the reconciler's (MODE-5)
            Mode::Hf => self.out.push(RadioCmd::Hf { freq: self.set.hf.freq, mode: self.set.hf.mode, on: false, keep_rx: next == Mode::Scope && super::scope::from_hf(self) }),
            Mode::Sat => self.out.push(RadioCmd::SatRx(0)),
            Mode::Xb => {
                self.out.push(RadioCmd::Crossband(None));
                if self.radio.xb_by & super::xb::LISTEN != 0 {
                    super::xb::listen(self, false); // XB-15: no monitor left on
                }
                // XB-20.6: who stopped it and why, if said; else its screen closed (hold BACK, a reset, ...)
                if !self.radio.xb_log.is_some_and(|l| !l.on) {
                    self.radio.xb_log = Some(super::xb::XbLog { on: false, by: super::xb::BY_KEYS, why: super::xb::XB_SCREEN });
                }
            }
        }
    }

    /// PTT released: unkey whatever is keyed (TX-25), whichever screen is up.
    pub(crate) fn ptt_released(&mut self) {
        if let Some(slot) = self.radio.keyed.take() {
            // LINK-20, TX-47: a remote key-up lost (keepalive, session) unkeys as a cut, the carrier dropped at once.
            self.out.push(if self.tx_cut { RadioCmd::TxCut { slot } } else { RadioCmd::Transmit { slot, on: false } });
            self.voice_ended(); // APRS-27
            // WATCH-16: watch waits 30 s of quiet (or a key) so the reply on this slot isn't missed; a longer #
            // pause (WATCH-6) is left as it is.
            let until = self.now.wrapping_add(crate::TX_PAUSE_S as u32 * 1000);
            if self.tx_pause || self.paused_until.is_none_or(|t| crate::after(until, t)) {
                (self.paused_until, self.tx_pause) = (Some(until), true);
                self.out.push(RadioCmd::PauseWatch(crate::TX_PAUSE_S));
            }
        }
    }

    /// Every tick: each slot's activity for the watch + activity layout.
    pub(crate) fn radio_tick(&mut self, live: &LiveAll) {
        // XB-2 on the radio (decision 93): it refused to start the repeater; say why and stop.
        if let (Mode::Xb, ferrite_core::msg::ModeLive::Xb(x)) = (self.radio.mode, live.mode) {
            if let Some(why) = ferrite_core::xb::Stop::from_code(x.stopped) {
                self.radio.xb_on = false;
                self.radio.xb_log = Some(super::xb::XbLog { on: false, by: super::xb::BY_KEYS, why: super::xb::XB_REFUSED | x.stopped });
                self.reconcile_mode();
                // the reason alone, as the page's own refusals (`xb::try_start`): after "Can't start:" it was cut
                self.show_toast(b"", why.text().as_bytes(), false);
            }
        }
        // MODE-3: the Radio task stopped the mode itself (the lease ran out): nothing runs; the reconciler starts
        // again whatever is still wanted.
        if live.lease_drops != self.radio.lease_drops {
            self.radio.lease_drops = live.lease_drops;
            (self.radio.mode, self.radio.fm_on) = (Mode::Off, false);
        }
        if self.radio.keyed.is_none() {
            self.radio.dtmf_seen = live.tx.dtmf_sent;
        }
        self.bcast_tick(live);
        let b = (self.now / ACT_MS) as u16;
        let shift = b.wrapping_sub(self.radio.act_bucket).min(32) as u32;
        if shift > 0 {
            self.radio.act_bucket = b;
            for a in &mut self.radio.act {
                *a = a.checked_shl(shift).unwrap_or(0);
            }
        }
        for (i, s) in live.slots.iter().enumerate() {
            if s.squelch_open {
                self.radio.act[i] |= 1;
            }
        }
        if live.tx.keyed {
            self.radio.act[(live.tx.slot as usize).min(SLOTS - 1)] |= 1;
        }
    }
}

/// Text bytes as &str for ufmt (the UI's text is always UTF-8).
pub fn txt(b: &[u8]) -> &str {
    core::str::from_utf8(b).unwrap_or("")
}

/// "2:47" (minutes and seconds).
pub fn mmss<const N: usize>(s: u32, b: &mut ferrite_ui::text::Buf<N>) {
    let _ = ufmt::uwrite!(b, "{}:{}{}", s / 60, (s % 60) / 10, s % 10);
}
