//! The simulator's stand-ins: a fake three-slot radio that obeys the app's commands, the sample records
//! (channels, event log, APRS, GPS: `ferrite_app::sample`), and the radio-wide state for the status bar.
//!
//! The fake radio runs the firmware's own engines (ferrite_core's `watch`, `scan`, `txseq` and `path`): only the
//! carriers are made up, so watch turns, scan stops and transmit refusals look as they will on the radio.

use ferrite_app::chan::ChannelStore;
use ferrite_app::sample::Sample;
use ferrite_app::slot::{band_of, BANDS, DEFAULT_CFG};
use ferrite_core::freq::{mhz, Freq10};
use ferrite_core::path::{Cause, Path, Rx, Sel, Steps};
use ferrite_core::scan::{Range, Resume, Scan};
use ferrite_core::txseq::{self, Kind, Req, TxSeq};
use ferrite_core::watch::{self, BcastTurn, Watch};
use ferrite_core::msg::{BroadcastCmd, Device, GpsState, LiveAll, ModeLive, RadioCmd, SlotLive, TxLive, XbLive, SCOPE_BARS, XB_COLS};
use ferrite_core::txplan::Region;
use ferrite_core::msg::{CalState, LinkLive, SameDecoded, SelfTest};

const fn cfg(tone: u16, offset: i32) -> ferrite_app::slot::SlotCfg {
    let mut c = DEFAULT_CFG;
    c.tx_tone = tone;
    c.offset = offset;
    c
}

/// The look review's sample: A a VFO with tone and shift, B on "Club repeater", C scanning airband, watch
/// A+B+C. The app's start-up commands go to the radio first (they tune C, which would stop its scan).
pub fn sample(app: &mut ferrite_app::App, store: &impl ChannelStore, radio: &mut FakeRadio) {
    sample_app(app, store);
    while let Some(c) = app.pop_cmd() {
        radio.cmd(c);
    }
    radio.cmd(RadioCmd::Scan { slot: 2, on: true });
    radio.live.slots[2].hits = 3;
}

/// The sample's app side alone (the slots, watch, region and callsign), to put back between screenshots.
pub fn sample_app(app: &mut ferrite_app::App, store: &impl ChannelStore) {
    app.set_vfo_cfg(0, cfg(1000, -60_000));
    app.set_channel(1, 1, 1, store);
    app.set_watch(2);
    app.set_region(Some(Region::Canada), false); // as the mock-ups have it
}

/// An app on the sample settings ([`ferrite_app::sample::settings`]: callsign, Canada, HF recent), kept for
/// the program's life as the firmware keeps its own.
pub fn app() -> ferrite_app::App {
    let set = Box::leak(Box::new(ferrite_app::sample::settings()));
    let alert = Box::leak(Box::new(ferrite_app::screens::alert::AlertInfo::NONE));
    ferrite_app::App::new(set, alert)
}

/// Channels in zones, an event log, APRS, GPS, waypoints and the scan lists: [`Sample`].
pub fn store() -> Sample {
    Sample::new()
}

/// What the fake radio's exclusive mode is doing.
#[derive(Clone, Copy, PartialEq)]
enum Fake {
    Off,
    /// The band scope: listening on `listen` (0 = sweeping) since `tl`, the bars held still meanwhile.
    Scope { centre: Freq10, step: u16, n: u8, dwell: u8, t0: f32, listen: Freq10, tl: f32 },
    Tone { t0: f32 },
    Counter { t0: f32 },
    /// FM broadcast, tuned (or a seek landed) at t0 s: the station name comes 0.4 s in, the radio text once
    /// complete, 1.5 s in (BCR-30).
    Fm { t0: f32 },
    Hf { t0: f32, freq: Freq10, mode: u8 },
    /// The crossband repeater: the real engine, fed made-up signals (decision 93); or the radio's refusal.
    Xb { t0: f32, rep: ferrite_core::xb::Repeater },
    XbStopped(u8),
    /// Satellite mode (SAT-1): the downlink heard, and the uplink a key-up goes out on.
    Sat { up: Freq10 },
}

/// Carriers the scope and counter "hear": frequency and level (dBm).
const CARRIERS: [(Freq10, f32); 5] =
    [(mhz(146) + 83_500, -60.0), (mhz(146) + 61_000, -88.0), (mhz(146) + 88_000, -92.0), (mhz(146) + 52_000, -100.0), (mhz(443) + 47_500, -80.0)];
/// FM stations for seek: frequency (10 kHz), RDS name and text.
/// AM broadcasters an HF seek stops on (10 Hz units): LW, MW and shortwave.
const HF_STATIONS: [Freq10; 9] = [19_800, 90_900, 108_900, 502_500, 607_000, 733_500, 958_000, 1_173_500, 1_540_000];

/// The fake's FM stations: frequency (10 kHz), RDS station name, radio texts (the station swaps to the next
/// every 20 s, flipping the A/B flag: BCR-30), signal (dBuV), SNR (dB), stereo and the programme type (RBDS numbers: BCR-26.8; 100.5 sends none). 99.1 sends no radio text, so its
/// card falls back to the station name (BCR-26); 100.5 sends the full 64 characters.
pub type Station = (u16, &'static str, [&'static str; 2], u8, u8, bool, u8);

pub const STATIONS: [Station; 6] = [
    (8_890, "CBC R2", ["Tempo with Julie Nesrallah", "Tempo with Julie Nesrallah"], 38, 16, true, 15),
    (9_150, "CBC R1", ["Ottawa Morning: traffic and weather on the hour", "Ottawa Morning: traffic and weather on the hour"], 41, 18, true, 2),
    (9_810, "JAZZ FM", ["Now playing: Take Five, Dave Brubeck Quartet", "Up next: So What, Miles Davis"], 36, 15, true, 14),
    (9_910, "CHEZ 99", ["", ""], 31, 11, false, 6),
    (10_050, "RDS TEST", [
        "Now playing: Harvest Moon by Neil Young, live at Massey Hall 71",
        "Up next: the 9 o'clock news, then Ottawa traffic and weather now",
    ], 48, 24, true, 0),
    (10_650, "KISS", ["The Morning Show", "The Morning Show"], 44, 20, true, 9),
];

/// Three slots: A has a signal 3 s in every 10, B one for 2 s in every 25 (a watched slot opening), C scans.
pub struct FakeRadio {
    pub live: LiveAll,
    /// No stations on the air (the simulators' LED demo: a dark screen stays dark).
    pub quiet: bool,
    /// The tone hint the fake reports (TSRCH-13): a slot and the tone word heard there, until that slot retunes
    /// or gets a new receive tone.
    pub hint: Option<(u8, u16)>,
    /// Beeps asked for (SND-3, SND-6): how many, the last one's tone and length, and the volume (0 7 5 2).
    pub beeps: u32,
    pub last_beep: (u16, u16),
    /// The last message sound asked for (APRS-6.1).
    pub last_chime: u8,
    pub beep_vol: u8,
    /// VOX's level and tail as the UI sent them (0 8 2); the sims have no microphone, so `voice` is set by
    /// tests.
    pub vox: (u8, u8),
    watch: u8,
    /// The engines, as the Radio task runs them.
    watcher: Watch,
    scans: [Scan; 3],
    /// Channel scans: the sets the UI sends and the frequencies it answers with, as the Radio task keeps them.
    chans: ferrite_core::scan::ChanScans,
    seq: TxSeq,
    path: Path,
    /// The slot each chip listened to on the last step.
    on_chip: [Option<u8>; 2],
    active: u8,
    last_step: f32,
    mode: Fake,
    /// The scope's bars and dwell ([`RadioCmd::ScopeCfg`]).
    scope_cfg: (u8, u8),
    /// Each slot's power level (0 Low, 1 Mid, 2 High; 3 = HF mode), for the duty limit's weighting (TX-38).
    power: [u8; 4],
    /// Slots at squelch 0: the noise floor holds them open, as on the radio.
    sq0: u8,
    #[cfg(feature = "bench")]
    /// The two BK4829s' registers, for the terminal's `bk` (TERM-21).
    bk: [[u16; 128]; 2],
    #[cfg(feature = "bench")]
    /// The BK4829 settle waits the terminal sets (TERM-31); the fake has no chip power to wait on.
    settle: ferrite_core::settle::Settle,
    /// The transmit test armed from the terminal (TERM-33): kept and shown; the simulator sends no audio.
    #[cfg(feature = "bench")]
    txtest: Option<ferrite_core::txtest::Arm>,
    /// Slots with monitor latched: squelch held open.
    monitor: u8,
    /// Slots with receive priority (APRS-21.1, [`RadioCmd::Awake`]).
    prio: u8,
    /// The APRS slot ([`RadioCmd::AprsSlot`]): other slots on its frequency defer to it (APRS-21.2).
    aprs_slot: u8,
    /// Quick mute (HOME-15): slots still received, never heard.
    mute: u8,
    /// Slots turned off (HOME-15.1): no chip, never heard.
    slot_off: u8,
    fm: u16,
    /// The broadcast plays, tuned (or a seek landed) at this time (s): as [`Fake::Fm`], or beside home's strip
    /// (MODE-5), which then has the mode report.
    fm_t0: Option<f32>,
    /// The slot playing broadcast (BCR-23: no chip listens to it while FM plays) and the broadcast's turn on the
    /// speaker (BCR-24), as the Radio task keeps them.
    bcast: Option<u8>,
    bturn: BcastTurn,
    now: f32,
    /// A slot on a NOAA / Weatheradio channel since this time (s): the fake decoder hears a SAME warning there.
    wx_since: Option<f32>,
    same: Option<SameDecoded>,
    /// When the fake pager system last sent (seconds of fake time).
    paged_at: f32,
    /// An APRS packet going out on this slot until this time (s): the TX LED and keyed state for a moment.
    burst: Option<(u8, f32)>,
    /// DTMF-3's timing ([`RadioCmd::DtmfTiming`]: ms, delay in 10 ms) and a dialled sequence being sent until this
    /// time (s), as the Radio task plays it.
    dtmf: (u8, u8),
    dialling: Option<f32>,
}

/// The fake decoder's warning, heard 8 s after a slot settles on a weather channel (162.400-162.550 MHz).
pub const SAME_HEADER: &[u8] = b"ZCZC-WXR-SVR-024031-024033+0045-2761828-KLWX/NWS-";

impl FakeRadio {
    pub fn new() -> Self {
        let mut live = LiveAll { chips: 3, ..Default::default() };
        live.slots[0] = SlotLive { freq: mhz(146) + 52_000, rssi: -121, ..Default::default() };
        live.slots[1] = SlotLive { freq: mhz(443) + 47_500, rssi: -121, ..Default::default() };
        live.slots[2] = SlotLive { freq: mhz(118) + 95_000, rssi: -121, scanning: true, hits: 3, last_hit: mhz(121) + 50_000, ..Default::default() };
        let mut scans = [Scan::new(); 3];
        for sc in &mut scans {
            sc.resume = Resume::Time(2);
            sc.fast = true;
        }
        scans[2].start(scan_pos(live.slots[2].freq), 0);
        let mut path = Path::new();
        path.rx(Rx::Bk(Sel::rx(live.slots[0].freq)), Cause::ActiveSlot, 0, &mut Steps::default()).ok();
        FakeRadio {
            live,
            paged_at: 0.0,
            quiet: false,
            hint: None,
            beeps: 0,
            last_beep: (0, 0),
            last_chime: 0,
            beep_vol: 2,
            vox: (0, 0),
            watch: 0b111,
            watcher: Watch::new(),
            scans,
            chans: ferrite_core::scan::ChanScans::new(),
            seq: TxSeq::new(),
            path,
            on_chip: [None; 2],
            active: 0,
            last_step: 0.0,
            mode: Fake::Off,
            scope_cfg: (60, 10),
            power: [2; 4],
            sq0: 0,
            #[cfg(feature = "bench")]
            bk: fake_bk(),
            #[cfg(feature = "bench")]
            settle: ferrite_core::settle::Settle::DEFAULT,
            #[cfg(feature = "bench")]
            txtest: None,
            monitor: 0,
            prio: 0,
            aprs_slot: 0,
            mute: 0,
            slot_off: 0,
            fm: 9_810,
            fm_t0: None,
            bcast: None,
            bturn: BcastTurn::new(),
            now: 0.0,
            wx_since: None,
            same: None,
            burst: None,
            dtmf: (100, 30),
            dialling: None,
        }
    }

    fn ms(&self) -> u32 {
        (self.now * 1000.0) as u32
    }

    /// The active slot's receive path (the relay follows it, WATCH-10).
    fn rx_path(&mut self) {
        let f = self.live.slots[self.active as usize % 3].freq;
        let now = self.ms();
        self.path.rx(Rx::Bk(Sel::rx(f)), Cause::ActiveSlot, now, &mut Steps::default()).ok();
    }

    /// Key up through the sequencer (TX-17): the app has checked already, so the fake keys wherever the
    /// hardware could (Extend TX, calibrated), with the sequencer's own refusals (the relay, the duty limit).
    fn key(&mut self, slot: u8, kind: Kind) {
        let f = match self.mode {
            Fake::Hf { freq, .. } if slot == 3 => freq,
            Fake::Sat { up } if slot == 4 => up,
            _ => self.live.slots[slot as usize % 3].freq,
        };
        let req = Req {
            tx: f,
            rx: f,
            kind,
            person: kind == Kind::Voice,
            confirmed: true,
            am: false,
            rx_only: false,
            busy: false,
            hf_si: false,
            si_on: false,
            region: Region::Us,
            extend: true,
            cal_ok: true,
            cal_window: true,
            batt_ok: true,
            power: self.power[slot as usize % 4],
        };
        let now = self.ms();
        let r = self.seq.key(&req, &mut self.path, now, &mut Steps::default());
        self.live.tx = TxLive { keyed: r.is_ok(), slot, mic: 0, refused: r.err(), dtmf_sent: self.live.tx.dtmf_sent, voice: self.live.tx.voice, ptt_stuck: self.live.tx.ptt_stuck };
    }

    fn unkey(&mut self) {
        let now = self.ms();
        self.seq.release(&TX_CFG, &mut self.path, now, &mut Steps::default());
        self.live.tx.keyed = false;
    }

    pub fn cmd(&mut self, c: RadioCmd) {
        // Channel scans (SCAN-3, SCAN-12, SCAN-22, SCAN-24): the set, the start, the frequencies asked for.
        let now = self.ms();
        if let Some((i, f)) = self.chans.cmd(&c, &mut self.scans, now) {
            self.live.slots[i].freq = f;
        }
        match c {
            RadioCmd::ScanChans { slot, .. } => {
                let s = &mut self.live.slots[slot as usize % 3];
                (s.scanning, s.hits, s.skips) = (true, 0, 0);
            }
            RadioCmd::TuneSlot { slot, freq } => {
                self.hint = self.hint.filter(|h| h.0 != slot); // TSRCH-14
                let s = &mut self.live.slots[slot as usize % 3];
                s.freq = freq;
                s.scanning = false;
                self.chans.off(slot as usize);
                self.scans[slot as usize % 3].stop();
                self.rx_path();
            }
            RadioCmd::Active(a) => {
                self.active = a % 3;
                self.rx_path();
            }
            RadioCmd::Scan { slot, on } => {
                let now = self.ms();
                let i = slot as usize % 3;
                let s = &mut self.live.slots[i];
                s.scanning = on;
                self.chans.off(i); // a band scan, or stopped
                if on {
                    s.hits = 0;
                    s.skips = 0;
                    // The fake scans its band (a range): the fast pass (SCAN-21).
                    self.scans[i].fast = true;
                    self.scans[i].start(scan_pos(s.freq), now);
                } else {
                    self.scans[i].stop();
                }
            }
            RadioCmd::Watch(m) => self.watch = m,
            RadioCmd::Awake(m) => self.prio = m,
            RadioCmd::AprsSlot(m) => self.aprs_slot = m,
            RadioCmd::SlotParam { slot, param: ferrite_core::msg::Param::RxTone, .. } => self.hint = self.hint.filter(|h| h.0 != slot),
            RadioCmd::SlotParam { slot, param: ferrite_core::msg::Param::Squelch, value } if slot < 3 => {
                self.sq0 = self.sq0 & !(1 << slot) | u8::from(value == 0) << slot;
            }
            RadioCmd::SlotParam { slot, param: ferrite_core::msg::Param::Power, value } => self.power[slot as usize % 4] = value.clamp(0, 2) as u8,
            RadioCmd::PauseWatch(secs) => {
                let now = self.ms();
                self.watcher.pause(now, secs);
            }
            RadioCmd::Transmit { slot, on: true } => self.key(slot, Kind::Voice),
            RadioCmd::Transmit { on: false, .. } | RadioCmd::TxCut { .. } => self.unkey(),
            RadioCmd::TransmitData { slot, on: true, .. } => self.key(slot, Kind::Data),
            RadioCmd::TransmitData { on: false, .. } => self.unkey(),
            RadioCmd::ScanSkip { slot } => {
                let now = self.ms();
                let i = slot as usize % 3;
                let o = if self.chans.is_on(i) {
                    self.chans.skip_once(i, &mut self.scans[i], now)
                } else {
                    self.scans[i].skip_once(&scan_range(self.live.slots[i].freq), now)
                };
                self.scan_out(i, o);
            }
            RadioCmd::Monitor { slot, on } => self.monitor = self.monitor & !(1 << slot) | (on as u8) << slot,
            RadioCmd::Mute(m) => self.mute = m,
            RadioCmd::SlotOff(m) => self.slot_off = m,
            RadioCmd::ScanDir { slot, up, .. } => {
                // SCAN-16: on a hit (or in its resume wait) the engine leaves it at once.
                let now = self.ms();
                let i = slot as usize % 3;
                let o = if self.chans.is_on(i) {
                    self.chans.dir(i, &mut self.scans[i], up, now)
                } else {
                    self.scans[i].dir(up, &scan_range(self.live.slots[i].freq), now)
                };
                self.scan_out(i, o);
            }
            RadioCmd::ScopeCfg { bars, dwell, .. } => self.scope_cfg = (bars, dwell),
            RadioCmd::Sweep { centre, step, on } => {
                let (n, dwell) = self.scope_cfg;
                let off = self.fm_t0.map_or(Fake::Off, |t0| Fake::Fm { t0 }); // the broadcast beside it plays on
                self.set(if on { Fake::Scope { centre, step, n, dwell, t0: self.now, listen: 0, tl: 0.0 } } else { off })
            }
            RadioCmd::ScopeListen { freq, .. } => {
                if let Fake::Scope { listen, tl, .. } = &mut self.mode {
                    (*listen, *tl) = (freq, self.now);
                }
            }
            RadioCmd::ToneSearch { on, .. } => self.set(if on { Fake::Tone { t0: self.now } } else { Fake::Off }),
            RadioCmd::Counter(on) => self.set(if on { Fake::Counter { t0: self.now } } else { Fake::Off }),
            RadioCmd::Broadcast(BroadcastCmd::Fm(f)) => {
                self.fm = f;
                self.fm_on();
            }
            RadioCmd::Broadcast(BroadcastCmd::Seek { up }) => {
                let next = if up {
                    STATIONS.iter().find(|s| s.0 > self.fm).unwrap_or(&STATIONS[0])
                } else {
                    STATIONS.iter().rev().find(|s| s.0 < self.fm).unwrap_or(&STATIONS[STATIONS.len() - 1])
                };
                self.fm = next.0;
                self.fm_on(); // a new station: its RDS starts over
            }
            RadioCmd::BcastSlot(s) => self.bcast = s,
            RadioCmd::Broadcast(_) => {
                self.fm_t0 = None;
                if matches!(self.mode, Fake::Fm { .. }) {
                    self.set(Fake::Off);
                }
            }
            RadioCmd::Hf { freq, mode, on, .. } => {
                let t0 = match self.mode {
                    Fake::Hf { t0, .. } => t0,
                    _ => self.now,
                };
                self.set(if on { Fake::Hf { t0, freq, mode } } else { Fake::Off })
            }
            RadioCmd::HfSeek { up } => {
                if let Fake::Hf { freq, .. } = &mut self.mode {
                    let f = *freq;
                    let next = if up { HF_STATIONS.iter().find(|&&s| s > f) } else { HF_STATIONS.iter().rev().find(|&&s| s < f) };
                    *freq = *next.unwrap_or(if up { &HF_STATIONS[0] } else { &HF_STATIONS[HF_STATIONS.len() - 1] });
                }
            }
            RadioCmd::Crossband(Some(cfg)) => {
                let f = [cfg.side1, cfg.side2].map(|s| self.live.slots[s as usize % 3].freq);
                match ferrite_core::xb::sides_ok(f, self.path.relay()) {
                    Ok(()) => {
                        let mut rep = ferrite_core::xb::Repeater::new(cfg, self.ms());
                        rep.demo_past(XB_PAST, 72 * 60, 23, 9 * 60);
                        self.set(Fake::Xb { t0: self.now, rep });
                    }
                    Err(why) => self.set(Fake::XbStopped(why as u8)),
                }
            }
            RadioCmd::Crossband(None) => self.set(Fake::Off),
            RadioCmd::SatRx(0) => self.set(Fake::Off),
            RadioCmd::SatRx(_) if !matches!(self.mode, Fake::Sat { .. }) => self.set(Fake::Sat { up: 0 }),
            RadioCmd::SatUp(f) => {
                if let Fake::Sat { up } = &mut self.mode {
                    *up = f;
                }
            }
            RadioCmd::DtmfTiming { ms, delay_10ms } => self.dtmf = (ms, delay_10ms),
            RadioCmd::Beep { hz, ms } if !self.live.tx.keyed => {
                self.beeps += 1;
                self.last_beep = (hz, ms);
            }
            RadioCmd::Chime { sound, .. } if !self.live.tx.keyed => self.last_chime = sound,
            RadioCmd::BeepVol(v) => self.beep_vol = v,
            RadioCmd::Vox { level, tail } => self.vox = (level, tail),
            RadioCmd::VoxScreen(_) => {}
            RadioCmd::DtmfSend { len } if self.live.tx.keyed => {
                let ms = ferrite_core::tonegen::dtmf_seq_ms(len as usize, self.dtmf.0 as u16, self.dtmf.1 as u16 * 10);
                self.dialling = Some(self.now + ms as f32 / 1000.0);
            }
            _ => {}
        }
    }

    /// A SAME header the fake decoder heard since the last call, for the app's `same_alert` (the firmware's
    /// UI task gets the same from the DSP task over a channel).
    pub fn take_same(&mut self) -> Option<SameDecoded> {
        self.same.take()
    }

    /// The fake pager system (DEC-4): while the pager decoder is wanted, a page every 20 s, in turn from a
    /// short list (text, numeric, tone only), as the DSP task's decoder would hand them to the UI task.
    pub fn take_page(&mut self, wanted: bool) -> Option<ferrite_core::decoders::Page> {
        use ferrite_core::decoders::{Page, PAGE_TEXT};
        const PAGES: [(u32, u16, u8, &[u8]); 3] =
            [(1_234_567, 1200, 3, b"STN 4 RESPOND MVC HWY 417 EB AT KM 120"), (1_234_570, 512, 0, b"5551234"), (1_234_571, 2400, 2, b"")];
        if !wanted {
            self.paged_at = self.now;
            return None;
        }
        if self.now - self.paged_at < 20.0 {
            return None;
        }
        self.paged_at = self.now;
        let (capcode, baud, function, text) = PAGES[(self.now / 20.0) as usize % PAGES.len()];
        let mut p = Page { capcode, baud, function, numeric: function == 0, len: text.len() as u8, text: [0; PAGE_TEXT] };
        p.text[..text.len()].copy_from_slice(text);
        Some(p)
    }

    /// While a slot sits on a weather channel, a warning comes 8 s in (once per visit).
    fn same_step(&mut self, t: f32) {
        let wx = self.live.slots.iter().position(|s| (mhz(162) + 40_000..=mhz(162) + 55_000).contains(&s.freq));
        match (wx, self.wx_since) {
            (Some(_), None) => self.wx_since = Some(t),
            (Some(i), Some(t0)) if t - t0 >= 8.0 && t0 >= 0.0 => {
                if let Some(header) = ferrite_core::same::parse(SAME_HEADER) {
                    self.same = Some(SameDecoded { header, slot: i as u8, freq: self.live.slots[i].freq });
                }
                self.wx_since = Some(-1.0); // heard; not again until the slot leaves
            }
            (None, _) => self.wx_since = None,
            _ => {}
        }
    }

    /// The broadcast (re)tuned now: the mode, unless home's strip sweeps beside it (MODE-5).
    fn fm_on(&mut self) {
        self.fm_t0 = Some(self.now);
        if !matches!(self.mode, Fake::Scope { .. }) {
            self.set(Fake::Fm { t0: self.now });
        }
    }

    fn set(&mut self, m: Fake) {
        if !matches!((self.mode, m), (Fake::Hf { .. }, Fake::Hf { .. })) {
            self.live.mode = ModeLive::Off;
        }
        self.mode = m;
    }

    /// The exclusive mode's report at t seconds.
    fn mode_step(&mut self, t: f32) {
        let noise = |x: f32| -122.0 + (x * 12.9).sin() * 3.0 + (x * 3.3).cos() * 2.0;
        self.live.mode = match self.mode {
            Fake::Off => ModeLive::Off,
            Fake::Scope { centre, step, n, dwell, t0, listen, tl } => {
                // Which chip (and relay state) a frequency needs: HF on chip 1 with the relay, VHF chip 0, UHF
                // chip 1; points needing another than the centre's are blank (SCOPE-5).
                let chip = |f: Freq10| if f < mhz(18) { 3 } else if f < mhz(64) { 2 } else if f < mhz(300) { 0 } else { 1 };
                let tt = if listen != 0 { tl } else { t };
                let n = (n as usize).clamp(1, SCOPE_BARS);
                let mut bars = [0u8; SCOPE_BARS];
                for (i, b) in bars[..n].iter_mut().enumerate() {
                    let fi = centre as i64 + (i as i64 - n as i64 / 2) * step as i64;
                    if fi <= 0 || chip(fi as Freq10) != chip(centre) {
                        continue;
                    }
                    let f = fi as f32;
                    let mut lv = noise(f / 1000.0 + tt * 0.7);
                    for (cf, l) in CARRIERS {
                        let d = (f - cf as f32).abs() / step.max(1) as f32;
                        if d < 1.5 {
                            lv = lv.max(l - d * 14.0 + (tt * 2.1 + cf as f32).sin() * 3.0);
                        }
                    }
                    *b = (lv + 160.0).clamp(1.0, 255.0) as u8;
                }
                // Sweeps a second: n points at the dwell plus about 0.3 ms each to retune.
                let rate = 1000.0 / (n as f32 * (dwell.max(1) as f32 / 10.0 + 0.3)).max(50.0);
                let open = listen != 0 && CARRIERS.iter().any(|&(cf, _)| (cf as i64 - listen as i64).abs() < 1_250);
                ModeLive::Scope { centre, bars, n: n as u8, sweeps: ((tt - t0) * rate) as u8, listen, open }
            }
            Fake::Tone { t0 } => ModeLive::Probe { freq: 0, rssi: -84, tone: if t - t0 > 2.5 { 1000 } else { 0 } },
            Fake::Counter { t0 } => {
                let p = (t - t0) % 6.0;
                let (freq, rssi) = if p < 1.0 { (0, -127) } else if p < 3.0 { (mhz(147) + 15_000, -42) } else if p < 4.5 { (mhz(446) + 6_250, -51) } else { (mhz(146) + 94_000, -58) };
                ModeLive::Probe { freq, rssi, tone: 0 }
            }
            Fake::Fm { t0 } => {
                let st = STATIONS.iter().find(|s| s.0 == self.fm);
                let mut ps = [b' '; 8];
                let mut rt = [0u8; 64];
                let heard = matches!(self.live.mode, ModeLive::Fm { heard: true, .. });
                if let Some(s) = st {
                    let age = t - t0;
                    if age >= 0.4 {
                        ps[..s.1.len()].copy_from_slice(s.1.as_bytes());
                    }
                    // The radio text once complete; each new one (A/B) replaces the last when it is in.
                    if age >= 1.5 {
                        let text = s.2[((age - 1.5) / 20.0) as usize % 2].as_bytes();
                        let n = text.len().min(64);
                        rt[..n].copy_from_slice(&text[..n]);
                    }
                }
                let (rssi, snr, stereo) = st.map_or((8, 2, false), |s| (s.3 + ((t * 1.3).sin() * 2.0) as u8, s.4, s.5));
                let pty = st.filter(|_| t - t0 >= 0.4).map_or(0, |s| s.6); // with the station name
                ModeLive::Fm { freq: self.fm, ps, rt, rssi, snr, stereo, pty, heard }
            }
            Fake::Hf { t0, freq, mode } => {
                let p = ((t - t0) / 2.0 * 100.0) as u32;
                let si4732 = freq < mhz(18) || matches!(mode, 3..=5); // USB, LSB, CW as Param::Mode
                ModeLive::Hf { freq, rssi: -95 + ((t * 1.3).sin() * 8.0) as i16, snr: 12, si4732, loading: if p > 100 { 255 } else { p as u8 } }
            }
            Fake::Xb { t0, mut rep } => {
                // Side 1 heard 12 s of every 30, side 2 8 s of every 45 (from 20 s), as the mock-ups show; the
                // engine makes the passes, hangs and strip.
                let s = t - t0;
                let heard = [Some(s % 30.0 < 12.0), Some(s % 45.0 > 20.0 && s % 45.0 < 28.0)];
                rep.set_rssi(-79);
                let _ = rep.tick(self.ms(), heard);
                self.mode = Fake::Xb { t0, rep };
                ModeLive::Xb(rep.live)
            }
            Fake::XbStopped(stopped) => ModeLive::Xb(XbLive { stopped, ..Default::default() }),
            Fake::Sat { .. } => {
                let rssi = -97 + ((t * 0.9).sin() * 9.0) as i16;
                ModeLive::Sat { rssi, open: rssi > -100 }
            }
        };
        // Transmit: speech, the mic level wandering 30-85%.
        if self.live.tx.keyed {
            self.live.tx.mic = (58.0 + (t * 5.1).sin() * 18.0 + (t * 13.7).sin() * 9.0) as u8;
        }
    }

    /// A scan engine's answer: the step it tuned, a hit, the end.
    fn scan_out(&mut self, i: usize, o: ferrite_core::scan::Out) {
        let s = &mut self.live.slots[i];
        if let Some(f) = o.tune {
            s.freq = f;
        }
        s.held = self.scans[i].on_hit();
        s.chan = if self.chans.is_on(i) { self.chans.chan(&self.scans[i]) + 1 } else { 0 };
        if o.hit.is_some() {
            s.hits = s.hits.saturating_add(1);
            s.last_hit = s.freq;
            s.hit_chan = s.chan;
        }
        s.skips = self.scans[i].skips.max(s.skips);
        if o.ended {
            s.scanning = false;
            self.chans.off(i);
        }
    }

    /// Advance to t seconds; `active` is always heard.
    /// An APRS packet (beacon, message, ack) on `slot`: keyed for 0.4 s, as the APRS task's transmit is.
    pub fn burst(&mut self, slot: u8) {
        if !self.live.tx.keyed {
            self.key(slot, Kind::Data);
            if self.live.tx.keyed {
                self.burst = Some((slot, self.now + 0.4));
            }
        }
    }

    pub fn step(&mut self, t: f32, active: u8) {
        self.last_step = t;
        self.same_step(t);
        self.now = t;
        let now = self.ms();
        if active % 3 != self.active {
            self.active = active % 3;
            self.rx_path();
        }
        if self.burst.is_some_and(|(_, end)| t >= end) {
            self.burst = None;
            self.unkey();
        }
        // DTMF-1: the dialled sequence is out; the UI unkeys.
        if self.dialling.is_some_and(|end| t >= end) {
            self.dialling = None;
            self.live.tx.dtmf_sent = self.live.tx.dtmf_sent.wrapping_add(1);
        }
        // The sequencer's timeout and duty limit (the app keeps its own timeout screen).
        if self.seq.tick(&TX_CFG, true, &mut self.path, now, &mut Steps::default()).is_some_and(|e| e != txseq::Event::PreAlert) {
            self.live.tx.keyed = false;
        }
        self.mode_step(t);
        // XB-13, WATCH-9, SCAN-8: while the crossband repeater runs, watch and scans pause where they are (the
        // Radio task gives both chips to the repeater).
        if matches!(self.mode, Fake::Xb { .. }) {
            return;
        }
        // Carriers where each slot is tuned now.
        let quiet = self.quiet;
        let sq0 = self.sq0;
        let carrier = |i: usize, s: &SlotLive| -> (bool, f32) {
            if quiet {
                return (false, 0.0);
            }
            if s.scanning {
                return (scan_carrier(s.freq), -79.0 + (t * 3.1).sin() * 5.0);
            }
            if sq0 & 1 << i != 0 {
                return (true, -121.0 + (t * 0.9 + i as f32).sin() * 2.0);
            }
            match i {
                0 => ((t % 10.0) > 6.0 && (t % 10.0) < 9.0, -85.0 + (t * 1.7).sin() * 6.0),
                1 => ((t % 25.0) > 20.0 && (t % 25.0) < 22.0, -97.0 + (t * 2.3).sin() * 4.0),
                _ => (false, 0.0),
            }
        };
        // Watch (WATCH-2): each chip's squelch for the slot it was on, then the engine's next turn.
        let sq = self.on_chip.map(|o| o.is_some_and(|i| carrier(i as usize, &self.live.slots[i as usize]).0));
        // BCR-23: while FM plays, no chip on the broadcast slot or on a slot needing the relay.
        let off = match self.fm_t0 {
            Some(_) => {
                let relay = (0..3).filter(|&i| Sel::rx(self.live.slots[i].freq).relay()).fold(0u8, |m, i| m | 1 << i);
                relay | self.bcast.map_or(0, |s| 1 << s)
            }
            _ => 0,
        };
        let cfg = watch::Cfg { set: self.watch, active: self.active, freq: self.live.slots.map(|s| s.freq), muted: self.mute, off: off | self.slot_off, held: self.monitor, prio: self.prio, aprs: self.aprs_slot, scan: (0..3).filter(|&i| self.scans[i].running() && self.scans[i].fast).fold(0, |m, i| m | 1 << i) , first: watch::aprs_first(), swap: 0 };
        let out = self.watcher.tick(now, &cfg, sq, self.live.tx.keyed);
        // BCR-24: the broadcast plays while no slot is heard, and 2 s after one goes quiet; a muted broadcast slot
        // never plays (HOME-15).
        let turn = self.bturn.tick(now, out.audio.is_some() || self.monitor & !(self.mute | self.slot_off) & 1 << self.active != 0);
        let bmuted = self.bcast.is_some_and(|b| (self.mute | self.slot_off) & 1 << b != 0);
        if let ModeLive::Fm { heard, .. } = &mut self.live.mode {
            *heard = turn && !bmuted;
        }
        self.live.bcast_heard = self.fm_t0.is_some() && turn && !bmuted;
        // Scans step only while their slot has a chip.
        for i in 0..3 {
            if self.live.slots[i].scanning && out.chip.contains(&Some(i as u8)) {
                let open = self.on_chip.contains(&Some(i as u8)) && carrier(i, &self.live.slots[i]).0;
                let o = if self.chans.is_on(i) {
                    self.chans.tick(i, &mut self.scans[i], now, open)
                } else {
                    self.scans[i].tick(&scan_range(self.live.slots[i].freq), now, open)
                };
                self.scan_out(i, o);
            }
        }
        self.live.set_want(self.chans.want(&self.scans));
        self.on_chip = out.chip;
        let relay = |f: Freq10| Sel::rx(f).relay();
        let active_relay = relay(self.live.slots[self.active as usize].freq);
        for (i, s) in self.live.slots.iter_mut().enumerate() {
            let (_, level) = carrier(i, s);
            s.squelch_open = self.watcher.open(i as u8) || self.monitor & 1 << i != 0;
            s.rssi = if s.squelch_open { level as i16 } else if quiet { -121 } else { -121 + ((t * 0.9 + i as f32).sin() * 2.0) as i16 };
            s.relay = out.cancelled && self.watch & 1 << i != 0 && relay(s.freq) != active_relay; // WATCH-11
            s.heard_tone = self.hint.filter(|h| h.0 as usize == i).map_or(0, |h| h.1);
        }
    }
}

/// The fake's transmit settings: the longest timeout (the app shows its own).
const TX_CFG: txseq::Cfg = txseq::Cfg { timeout_s: 600, prealert_s: 0, end_tone: false, cw_id: false, cw_each: false, tail: false };

/// The step the fake scans at.
const SCAN_STEP: Freq10 = 2_500;

/// A scanning slot's range: its band at 2.5 kHz.
fn scan_range(f: Freq10) -> Range<'static> {
    let b = &BANDS[band_of(f).unwrap_or(2)];
    Range { lo: b.lo, hi: b.hi - SCAN_STEP, step: SCAN_STEP, skipped: &[] }
}

/// Where `f` is in its scan range.
fn scan_pos(f: Freq10) -> u16 {
    let r = scan_range(f);
    (f.saturating_sub(r.lo) / SCAN_STEP) as u16
}

/// Scanned carriers: one every 40 steps of a band (a stop every few seconds of scanning).
fn scan_carrier(f: Freq10) -> bool {
    scan_pos(f) % 40 == 20
}

/// The crossband repeater's past in the simulators (shown as 1 h 12 m in): the strip's earlier columns.
const XB_PAST: [i8; XB_COLS] = [
    0, 0, 0, 0, 0, 12, 22, 25, 20, 0, -8, -14, 0, 0, 0, 0, 16, 24, 22, 18, 12, 0, -20, -26, -18, -10, 0, 0, 0, 0, 0, 0,
    14, 20, 0, 0, 0, 0, 18, 22, 25, 24, 20, 16, 0, 0, 0, -12, -22, -16, 0, 0, 0, 0, 0, 0, 10, 0, 0, 0,
];

/// GPS fix with 9 satellites, 7.7 V (what the radio reads, Alex Oct 27), the MCU at 24 C; the clock from the PC's UTC time.
#[cfg(not(target_arch = "wasm32"))]
pub fn device() -> Device {
    device_at(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

/// [`device`] with the clock at `secs` UTC (seconds since 1970).
pub fn device_at(secs: u64) -> Device {
    Device {
        clock_min: Some(((secs % 86_400) / 60) as u16),
        utc: true,
        gps: GpsState::Fix,
        sats: 9,
        gps_phase: ferrite_core::gpspower::GpsPhase::Off,
        bt: false,
        battery_mv: 7700,
        battery_pct: 82,
        battery_low: false,
        battery_cutoff: false,
        mcu_temp: Some(24),
        // SBAR-23, SELF-9.1, SELF-9.2: sample numbers, as the radio's (the simulator has no stack to paint or idle to count)
        stack_free: 52_000,
        ram_statics: 30_272,
        cpu_pct: Some(6),
        cpu_speeds: [None; 3],
        audio: [0; 6],
        decode: [0; 18],
        restart: [0; 2],
        spkmic: false,
        tx_built: true, // the fake radio transmits (into nothing)
        bench: cfg!(feature = "bench"),
        selftest: SelfTest {
            uid: [0x3F2A_91C0, 0x7D44_0E18, 0xA2B6_5C93], // SELF-8: a sample
            idcode: ferrite_core::chipid::EXPECTED_PID,
            mcu_ok: true,
            flash_id: 0xEF_4016,
            flash_mb: 4,
            cal: CalState::Ok,
            cal_fault: Default::default(),
            cal_at: [0; 3],
            chips: 3,
            si4732: true,
            si_rev: *b"6000D", // SELF-5: a sample
            ssb_patch: true,
            battery_trim: 0,
            store: 1, // LINK-19.6: mounted
        },
        link: LinkLive { cable: true, baud_100: 9216, write_pct: None },
        mcu: Default::default(), // PWR-26.6: no MCU clock in the simulator ("Not measured", `mcu=0`)
    }
}


#[cfg(feature = "bench")]
/// The fake BK4829s' registers: the ID in REG_00 and stock's receive-mode REG_30, the rest 0.
fn fake_bk() -> [[u16; 128]; 2] {
    let mut r = [[0u16; 128]; 2];
    for c in r.iter_mut() {
        c[0x00] = 0x4829;
        c[0x30] = 0xBFF1;
        c[0x47] = 0x6142;
    }
    r
}

impl FakeRadio {
    /// The terminal's debug requests (TERM-21), answered with made-up memory: flash and RAM read as the low byte
    /// of each address, peripherals as 0, the GPIO input registers as a pattern; BK4829 registers from the fake
    /// register file (the same refusals as the radio); the SI4732 answers its status byte 0x80 and zeros.
    pub fn debug(&mut self, r: ferrite_core::msg::DebugReq) -> ferrite_core::msg::DebugAns {
        use ferrite_core::msg::{DebugAns, DebugReq};
        #[cfg(feature = "bench")]
        use ferrite_core::msg::{bk_write_refused, DEBUG_MEM};
        match r {
            #[cfg(feature = "bench")]
            DebugReq::Mem { addr, n, words } => {
                let mut data = [0u8; DEBUG_MEM];
                let n = (n as usize).min(DEBUG_MEM);
                let periph = (0x4000_0000..0x4003_0000).contains(&addr) || addr >= 0xE000_0000;
                if periph && !words {
                    return DebugAns::Refused("peripheral registers: use mem32 (word reads)");
                }
                for (i, b) in data[..n].iter_mut().enumerate() {
                    let a = addr + i as u32;
                    *b = if (0x4001_0808..0x4001_1C00).contains(&(a & !3)) && (a & 0x3FF) == 0x008 { 0xA5 } else if periph { 0 } else { a as u8 };
                }
                DebugAns::Mem { addr, data, n: n as u8 }
            }
            #[cfg(feature = "bench")]
            DebugReq::BkRead { chip, reg } => DebugAns::Reg { chip, reg, val: self.bk[chip as usize % 2][reg as usize % 128] },
            #[cfg(feature = "bench")]
            DebugReq::BkWrite { chip, reg, val } => {
                if let Some(why) = bk_write_refused(reg, val) {
                    return DebugAns::Refused(why);
                }
                self.bk[chip as usize % 2][reg as usize % 128] = val;
                DebugAns::Reg { chip, reg, val }
            }
            #[cfg(feature = "bench")]
            DebugReq::Si { reply, .. } => {
                let mut data = [0u8; 16];
                data[0] = 0x80;
                DebugAns::Si { data, n: reply.clamp(1, 16) }
            }
            // TERM-30: the sentence suspected on the black radio (field 9 about 17 m low, field 11 empty), 1 s old.
            DebugReq::Gga => {
                let s = b"$GNGGA,143210.000,4525.2950,N,07541.8600,W,1,09,0.9,-12.3,M,,M,,*57";
                let mut data = [0u8; ferrite_core::msg::GGA_MAX];
                data[..s.len()].copy_from_slice(s);
                DebugAns::Gga { data, n: s.len() as u8, age_s: 1 }
            }
            #[cfg(feature = "bench")]
            // TERM-36: a module that answers a query with its firmware version, and nothing to a standby.
            DebugReq::Gps { id, .. } => {
                let s: &[u8] = if id == ferrite_core::gpspower::CAS_QUERY { b"$GPTXT,01,01,02,SW=URANUS5,V5.3.0.0*1D" } else { b"" };
                let mut data = [0u8; ferrite_core::msg::GGA_MAX];
                data[..s.len()].copy_from_slice(s);
                DebugAns::GpsSent { data, n: s.len() as u8 }
            }
            #[cfg(feature = "bench")]
            // TERM-27: a module that, like the radio's, says nothing back.
            DebugReq::Bt { .. } => DebugAns::Bt { data: [0; 48], n: 0 },
            #[cfg(feature = "bench")]
            // `bt linkz`: no cycle counter in the simulator: the sizes only
            DebugReq::Linkz => DebugAns::Linkz([0; 11]),
            #[cfg(feature = "bench")]
            // TERM-27, LINK-36: the module off, at the default pace (or the one just set).
            DebugReq::BtStat { pace, .. } => DebugAns::BtStat(ferrite_core::msg::BtStats {
                pace: pace.unwrap_or_default(),
                ..Default::default()
            }),
            #[cfg(feature = "bench")]
            // TERM-29: the fake path machine holds both band receive selects (or refuses, as the radio's).
            DebugReq::RxBoth(None) => DebugAns::RxBoth(self.path.both()),
            #[cfg(feature = "bench")]
            DebugReq::RxBoth(Some(on)) => match self.path.set_both(on, &mut Steps::default()) {
                Ok(()) => DebugAns::RxBoth(on),
                Err(ferrite_core::path::Blocked::Keyed) => DebugAns::Refused("transmitting: try again after the unkey"),
                Err(_) => DebugAns::Refused("an HF slot has the relay energised: tune VHF or UHF first"),
            },
            #[cfg(feature = "bench")]
            // TERM-31: kept and answered, as the Radio task does.
            DebugReq::Settle(s) => {
                self.settle = s.unwrap_or(self.settle);
                DebugAns::Settle(self.settle)
            }
            #[cfg(feature = "bench")]
            // TERM-32: the simulator has no mic: a quiet tap (all zero), as the radio reads at rest.
            // TERM-33: armed and shown as a tx build does; the simulator has no DAC, so nothing plays at a key-up.
            DebugReq::TxTest(ask) => {
                if let ferrite_core::txtest::Ask::Set(a) = ask {
                    self.txtest = a;
                }
                DebugAns::TxTest(self.txtest)
            }
            #[cfg(feature = "bench")]
            // WATCH-17.1: the simulator's turns aren't the chips' (it has none to retune).
            DebugReq::Turns { .. } => DebugAns::Refused("the simulator has no chips to time: bk turns is the radio's"),
            #[cfg(feature = "bench")]
            DebugReq::Spi { .. } => DebugAns::Refused("the simulator has no chip bus: bk spi is the radio's"),
            #[cfg(feature = "bench")]
            // SAFE-8.1: the fault and the watchdog's restart are the radio's.
            DebugReq::Crash(_) => DebugAns::Refused("the simulator doesn't crash: crash is the radio's"),
            #[cfg(feature = "bench")]
            // TERM-40, TERM-41: the power bench's switches are the radio's.
            DebugReq::Save(_) | DebugReq::Lcd(_) => DebugAns::Refused("the power bench's switches are the radio's"),
            #[cfg(feature = "bench")]
            DebugReq::Mic(_) => DebugAns::Mic(ferrite_core::vox::MicCapture::from_samples(&[0; ferrite_core::vox::MIC_SAMPLES], 32_000)),
            // TERM-42 - TERM-46: register overrides, the meter, FSK tests and the audio pins act on real chips and pins
            #[cfg(feature = "bench")]
            DebugReq::RxAf(_) | DebugReq::RxIf(_) | DebugReq::Hold(_) | DebugReq::Swap(_) | DebugReq::Meter(_) | DebugReq::Fsk(_) | DebugReq::SynthFsk(_) | DebugReq::Pin { .. } => {
                DebugAns::Refused("a bench test of the radio's chips and pins: not in the simulator")
            }
        }
    }
}
