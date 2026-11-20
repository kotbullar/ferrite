//! What the six tasks say to each other (`docs/firmware-design.md`, "Messages"). Each piece of hardware has one
//! owning task; others send it a command on its channel, and owners announce what happened as [`Event`]s on one
//! bus. Live values the UI draws every frame ([`Live`]) are published as a snapshot, never queued.
//!
//! The channels themselves are statics in the firmware; these are the types, so the logic and the simulator can
//! use them without an executor.

use crate::freq::Freq10;

/// One of the two BK4829s: 0 = VHF and 200-300 MHz, 1 = 300 MHz up and HF.
pub type Chip = u8;

/// To the Radio task: the only one that touches the BK4829s, the SI4732, the RF path and the PA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioCmd {
    /// Receive on a frequency (the Radio task picks the chip and path from the band).
    Tune(Freq10),
    /// Squelch level 0-9.
    Squelch(u8),
    /// Receiver gain on the BK4829s: 0 automatic, 1–4 fixed (lowest to highest).
    RfGain(u8),
    /// Battery Save: the receiver sleep cycle.
    BatterySave(crate::rxsleep::Level),
    /// A key or knob: counts as activity for receiver sleep.
    Activity,
    /// Low Power Mode on or off (PWR-24): Battery Save runs [`crate::rxsleep::LOW_POWER`] in place of its level.
    LowPower(bool),
    /// Broadcast receiver: FM in 10 kHz units, AM in kHz.
    Broadcast(BroadcastCmd),
    /// The slot playing broadcast FM (BCR-21), none = the FM screen's own: while the SI4732 plays FM no chip
    /// listens to it (BCR-23).
    BcastSlot(Option<u8>),
    /// Everything off but the MCU (power switch).
    Shutdown,
    /// Receive on a frequency in one slot (0 = A, 1 = B, 2 = C).
    TuneSlot { slot: u8, freq: Freq10 },
    /// One of a slot's settings (the UI sends it whenever the user changes it).
    SlotParam { slot: u8, param: Param, value: i32 },
    /// The active slot: heard first, owns the band relay (WATCH-3, WATCH-10).
    Active(u8),
    /// The watch set, a bit per slot (0 = watch off).
    Watch(u8),
    /// Slots whose chip stays awake whatever the hop and Battery Save do, a bit per slot: APRS mode's (or KISS's)
    /// C, for its decoder (APRS-16, PWR-4, decision 111); also the slots with receive priority on their chip
    /// (APRS-21.1, decision 181).
    Awake(u8),
    /// The APRS slot, a bit per slot (C while APRS mode, KISS or beacons hold it): its squelch listens with the
    /// open delay at 0 (SQL-17, decision 181).
    AprsSlot(u8),
    /// Start or stop scanning in a slot.
    Scan { slot: u8, on: bool },
    /// Channel scans also listen on each channel's transmit frequency when it has a shift or split (SCAN-22,
    /// 0 1 6 3; sent at start-up and when it changes).
    ScanTx(bool),
    /// The CTCSS/DCS level sent: 0 low, 1 the calibration's, 2 high (TONE-16, 0 8 8 1; sent at start-up and when
    /// it changes).
    ToneLevel(u8),
    /// The transmit timeout in seconds, 30-600 (TX-30, 0 8 1; sent at start-up and when it changes): the UI
    /// unkeys at it, the Radio task's sequencer 5 s later as the backstop.
    TxTimeout(u16),
    /// Mic gain 1–12 (VOX-15, 0 8 3; sent at start-up and when it changes): REG_7D at every voice key-up
    /// (`vox::mic_reg`).
    MicGain(u8),
    /// The beep volume, 1-5 (SND-3, 0 7 5 2; sent at start-up and when it changes): every beep's tone gain.
    BeepVol(u8),
    /// VOX (VOX-1 to VOX-5, VOX-13, VOX-14; 0 8 2): level 1-9, or 0 for off, and the tail in 100 ms. The Radio
    /// task compares the mic (PA0) with the level's threshold and reports speech in [`TxLive::voice`]; the UI
    /// keys the active slot from it, with every PTT check (VOX-4).
    Vox { level: u8, tail: u8 },
    /// VOX-3 (as stock's UI mode 0, 0x08025994): VOX may sample (true) only with home on the screen; false blocks
    /// it, the detector reset, and it samples again 1.5 s after the next true. Sent when it changes.
    VoxScreen(bool),
    /// Pause watching for this many seconds (WATCH-6, WATCH-16); 0 resumes it now.
    PauseWatch(u16),
    /// Key or unkey a slot (3 = the HF mode frequency), after the UI's checks; the Radio task checks again
    /// (TX-17) and answers in [`LiveAll::tx`].
    Transmit { slot: u8, on: bool },
    /// Unkey a slot as a cut (TX-47: no end tone, CW ID or 400 ms carrier hold, as the timeout, duty limit and
    /// battery): a remote transmission whose keepalive stopped or whose link session closed or moved (LINK-20).
    TxCut { slot: u8 },
    /// A data key-up or unkey (APRS beacons, messages, acks, digipeats: APRS-7, APRS-20): as [`RadioCmd::Transmit`]
    /// after the UI's checks, at `power` (0 low, 1 mid, 2 high: 0 5 6 3, default low), with no end tone or CW ID
    /// after it (TONE-11). The DSP task plays the frame once it is keyed. Preceded by [`RadioCmd::TxPlan`].
    TransmitData { slot: u8, power: u8, on: bool },
    /// Sent just before each [`RadioCmd::Transmit`] key-up: the frequency the UI checked (offset, split and
    /// reverse applied), the region (an index into `txplan::Region::ALL`) and Extend TX, for the Radio task's own
    /// checks (TX-17). Apart from `Transmit` so a command stays 8 bytes. `confirmed`: a person's second PTT on the
    /// emergency prompt keyed it (TX-3, TX-35); never for VOX or anything automatic.
    TxPlan { tx: Freq10, region: u8, extend: bool, confirmed: bool },
    /// Scanning: skip the current hit for the rest of this scan (SCAN-11).
    ScanSkip { slot: u8 },
    /// A channel scan's list: `len` positions (the slot's zone's channels, then the zones added with digits:
    /// [`crate::scan::ChanSpan`], SCAN-25). Sent before [`RadioCmd::ScanChans`]; it forgets the slot's priority
    /// channels and frequencies.
    ScanSet { slot: u8, len: u16 },
    /// A channel scan's priority channel `k` at position `pos` (SCAN-12; 0xFFFF none).
    ScanPrio { slot: u8, k: u8, pos: u16 },
    /// Start a channel scan on the slot at position `from` of its list; `prio_s`: the priority look's interval in
    /// seconds (0 1 3; 0 off).
    ScanChans { slot: u8, from: u16, prio_s: u8 },
    /// The resume rule for the slot's next scan (SCAN-5, SCAN-24), as [`crate::scan::Resume::from_code`]:
    /// already the zone's or the preset's own, or the radio's (0 1 2).
    ScanResume { slot: u8, code: u8 },
    /// The answer to [`LiveAll::want`]: the receive frequency of the channel at the slot's position `pos` (0: not a
    /// stop: a Skip-flagged or HF channel, or none there now).
    ScanEntry { slot: u8, pos: u16, rx: Freq10 },
    /// Its transmit frequency when it has a shift or split, after its [`RadioCmd::ScanEntry`] (SCAN-22, only while
    /// 0 1 6 3 is on).
    ScanEntryTx { slot: u8, pos: u16, tx: Freq10 },
    /// While keyed: the tone burst (PTT + SIDE2) at 1750 Hz for as long as SIDE2 is held (TONE-13, TONE-13.1).
    ToneBurst { on: bool },
    /// While keyed: a DTMF digit ('0'-'9', '*', '#', 'A'-'D') sent live for as long as its key is held (DTMF-7,
    /// DTMF-11).
    Dtmf { key: u8, on: bool },
    /// Monitor latched on a slot: squelch held open until turned off (SIDE1 tap, SQL-2; shown MON on its card).
    Monitor { slot: u8, on: bool },
    /// Quick mute (HOME-15), a bit per slot: these slots still receive (squelch, S-meter, the green LED) but their
    /// audio never reaches the speaker, and they never take it from another slot (WATCH-3). RAM only.
    Mute(u8),
    /// Slots turned off (\* cycles a slot Normal, Muted, Off: HOME-15.1), a bit per slot: no chip listens to them,
    /// the active one too, so their watch turns go to the others (WATCH-2.1); never heard, never scanned. RAM only.
    SlotOff(u8),
    /// Scanning: go on up or down from here (SCAN-16); `resume` leaves the hit it is stopped on at once.
    ScanDir { slot: u8, up: bool, resume: bool },
    /// Band scope sweep around a centre at a step (10 Hz units), with the last [`RadioCmd::ScopeCfg`]; off when
    /// `on` is false (SCOPE-4). The active slot's chip sweeps with its audio off.
    Sweep { centre: Freq10, step: u16, on: bool },
    /// The band scope's points per sweep and dwell per point in 0.1 ms (SCOPE-9, 0 3 1 3); sent before a sweep.
    /// Apart from [`RadioCmd::Sweep`] so a command stays 8 bytes. `strip`: the sweep is home's strip, which keeps
    /// looking at the active slot's own frequency and holds there on a carrier (MODE-5, decision 115).
    ScopeCfg { bars: u8, dwell: u8, strip: bool },
    /// A key or knob step while the scope or home's strip sweeps: the sweep waits and its pass starts again, so the
    /// UI handles the input first (SCOPE-14, `ferrite_core::sweephold`).
    SweepHold,
    /// Band scope: listen on a frequency with the scope's mode (0 FM, 1 NFM, 2 AM), the path set first
    /// (SCOPE-11); `freq` 0 goes back to sweeping.
    ScopeListen { freq: Freq10, mode: u8 },
    /// Tone search on a slot (TSRCH-1).
    ToneSearch { slot: u8, on: bool },
    /// The frequency counter (TSRCH-6).
    Counter(bool),
    /// MODE-3: the UI still wants the exclusive mode running (sent at least every [`crate::lease::RENEW_MS`]
    /// while one runs); without it for [`crate::lease::WINDOW_MS`] the Radio task stops the mode itself.
    Lease,
    /// SAME weather alerts on or off (0 4 5 1 and the region, WX-3): the BK4829's SAME receiver runs on the chip of
    /// any slot tuned to a weather channel (DEC-19).
    Same(bool),
    /// DEC-6: the BK4829's DTMF detector on (0 4 3).
    DtmfRx(bool),
    /// HF mode: enter on a frequency and mode (as [`Param::Mode`]), retune, or leave (HF-3, HF-4). Entering
    /// powers the SI4732 up with the SSB patch and pauses watch, scan, decoders, jobs and beacons; leaving undoes
    /// all of it (HF-8). The receiver follows from frequency and mode (HF-10), and the relay from the receiver:
    /// energised for chip 1, released for the SI4732 (HF-11).
    Hf { freq: Freq10, mode: u8, on: bool },
    /// The SI4732's receive settings (BCR-32, HF-35; decision 292): applied when it powers up and whenever they
    /// change while it runs.
    SiOpts(SiOpts),
    /// HF mode: the SI4732's own seek, up or down from the frequency it is on (AM, below 30 MHz; HF-31). The
    /// frequency it stops on comes back in [`ModeLive::Hf`].
    HfSeek { up: bool },
    /// A short tone from the BK4829's tone generator to the speaker (SND-11), e.g. a key refused at a limit
    /// (TEXT-5).
    Beep { hz: u16, ms: u16 },
    /// One of [`crate::tonegen::CHIMES`] to the speaker at a DAC level (1-11; APRS-6.1, 0 7 5 6's), even over
    /// received audio, as the alert tone.
    Chime { sound: u8, level: u8 },
    /// BOOT-18: the boot sound ([`crate::tonegen::BOOT_SOUND`]) under the boot animation that started at `at` (ms
    /// since power-on): timed from then, so a late start skips what is past and stays in step with the picture.
    BootSound { at: u32 },
    /// The crossband repeater: start with these settings, or stop (XB-14).
    Crossband(Option<XbCfg>),
    /// The Transmit group's radio-wide tone settings (sent at start-up and when one changes): 0 8 8 3's tail
    /// phase (0 55 Hz, 1-3 120°/180°/240°), 0 8 6 2's CW ID (0 off, 1 every 10 min, 2 on unkey) and 0 8 5 4's
    /// DTMF sidetone. A slot's end tone and tail come with it ([`Param::EndTone`], [`Param::Tail`]).
    TxTones { phase: u8, cw: u8, sidetone: bool },
    /// The callsign for CW ID, 5 bytes a chunk (chunk 0: bytes 0-4, 1: 5-9; 0 pads the end). Sent with
    /// [`RadioCmd::TxTones`].
    CwCall { chunk: u8, text: [u8; 5] },
    /// DTMF-3: each digit on and off for `ms`; a sequence starts `delay_10ms` x 10 ms after key-up.
    DtmfTiming { ms: u8, delay_10ms: u8 },
    /// A dialled sequence (DTMF-1), 5 symbols a chunk (bytes as [`RadioCmd::Dtmf`]'s `key`), sent before
    /// [`RadioCmd::DtmfSend`].
    DtmfSeq { chunk: u8, keys: [u8; 5] },
    /// Send the first `len` symbols of the sequence on the transmission just keyed (DTMF-3's timing), then say so
    /// in [`TxLive::dtmf_sent`]; the UI unkeys. The key-up is a data one: no end tone or CW ID after it (TONE-11).
    DtmfSend { len: u8 },
    /// From the input task, not the UI: the side connector's speaker mic connected or removed, debounced
    /// ([`crate::acc`], SND-12). The Radio task switches PE1 to it and holds PB8 on while it is connected.
    Accessory(bool),
    /// Satellite mode (SAT-1, SAT-8, SAT-14): receive on the downlink (Doppler-corrected, retuned each second) with
    /// both chips taken, watch, scans and decoders paused; [`SAT_IDLE`] takes them with nothing tuned yet (the pass
    /// list); 0 leaves, and everything resumes. The uplink's chip waits for the key-up ([`RadioCmd::TxPlan`], then [`RadioCmd::Transmit`] with the satellite slot).
    SatRx(Freq10),
    /// Satellite mode: the uplink frequency for the next key-up and, keyed, to retune to (SAT-8).
    SatUp(Freq10),
    /// Satellite mode: the uplink's CTCSS tone in 0.1 Hz (the arming tone while it plays, SAT-10), half duplex
    /// (the downlink muted while keyed, SAT-11), and the uplink's power for the next key-up (TX-9 levels, SAT-22).
    SatTone { tone: u16, hd: bool, power: u8 },
}

/// [`RadioCmd::SatRx`]: satellite mode on with nothing tuned (no pass open yet).
pub const SAT_IDLE: Freq10 = 1;

/// The crossband repeater's settings (manual chapter 19): small enough to ride in a [`RadioCmd`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct XbCfg {
    /// The two slots, 0-2 each.
    pub side1: u8,
    pub side2: u8,
    /// 0 low, 1 mid (XB-3: never above mid).
    pub power: u8,
    /// Per-pass timeout in 10 s units (30 s - 10 min).
    pub timeout_10s: u8,
    /// Hang time in 0.1 s (0 - 3 s).
    pub hang_ds: u8,
    /// Bit 0 tone gate, bit 1 CW ID (its own setting, off by default: stock's repeater sends none, and users
    /// may ID themselves; decision 94), bit 2 speaker.
    pub flags: u8,
}

impl XbCfg {
    pub const TONE_GATE: u8 = 1;
    pub const ID: u8 = 2;
    pub const SPEAKER: u8 = 4;
    pub const DEFAULT: XbCfg = XbCfg { side1: 0, side2: 1, power: 0, timeout_10s: 18, hang_ds: 10, flags: XbCfg::TONE_GATE };
}

/// A slot setting carried by [`RadioCmd::SlotParam`]. Values are the UI's indexes, documented per variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    /// 0 FM, 1 NFM, 2 AM, 3 USB, 4 LSB, 5 CW.
    Mode,
    /// 0 wide, 1 narrow.
    Bandwidth,
    /// 0-9.
    Squelch,
    /// The tone word (CHAN-3): CTCSS in 0.1 Hz, bit 15 DCS, bit 14 inverted, 0 = none.
    RxTone,
    TxTone,
    /// Transmit offset in 10 Hz units, signed (0 = simplex).
    Offset,
    /// 0 low, 1 mid, 2 high.
    Power,
    /// 0 off, 1 on: refuse transmit while the squelch is open (TX-21).
    BusyLock,
    /// 0 off, 1 on.
    Reverse,
    /// Split: the absolute TX frequency (10 Hz units); 0 = not split, TX is RX plus Offset.
    Split,
    /// The clarifier: a receive-only offset in Hz, -2500 to 2500 (VFO-22); never moves the transmit frequency.
    Clarifier,
    /// The end tone this slot sends (TONE-10, the channel's over 0 8 4 already resolved): an index into
    /// `tonegen::END_TONES`, 0 off.
    EndTone,
    /// Tail elimination on this slot (TONE-5 to TONE-7, the channel's over 0 8 8 2 already resolved): bit 0 on
    /// receive, bit 1 on transmit.
    Tail,
    /// 1: the slot is on a receive-only channel (CHAN-4): the Radio task refuses its key-ups too (TX audit F11).
    RxOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroadcastCmd {
    Off,
    Fm(u16),
    Am(u16),
    Seek { up: bool },
}

/// To the Storage task: the only one that touches the SPI flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageCmd {
    SaveSettings,
    Log(Event),
}

/// A SAME header the decoder heard (DSP task to the UI task, which raises the alert: WX-4, WX-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SameDecoded {
    pub header: crate::same::SameHeader,
    /// The slot and frequency it was heard on.
    pub slot: u8,
    pub freq: Freq10,
}

/// To the DSP task: which decoder runs in the shared slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DspCmd {
    /// What the APRS task wants in the decoder slot (AFSK or off).
    Decoder(DecoderKind),
    /// The pager decoder wanted (POCSAG on and a slot on its frequency) or not. The slot runs one decoder:
    /// APRS's AFSK first, the pager decoder when APRS doesn't want it (DEC-18).
    Pager(bool),
    /// LINK-13.4: receive audio wanted for the PC (the cable's session listens): the capture runs for it too, and
    /// the DSP task sends its blocks. The `AUDIO` request's mode: 0 off, 1 the 12 kHz µ-law stream, 2 or 3 the bench's
    /// raw stream (LINK-13.9, [`crate::rawaudio`]).
    Audio(u8),
    Beep { hz: u16, ms: u16 },
    /// TERM-44 (decision 315, a bench test): the receive level meter on (zeroed) or off; the capture runs for it too.
    /// Only a bench build sends it.
    Meter(bool),
    /// PWR-26.9: the APRS slot's squelch (SQL-19) opened or closed: the AFSK decoder captures only while it is
    /// open and for [`SQ_TAIL_MS`] after, so the core rests at 24 MHz between packets.
    Squelch(bool),
}

/// PWR-26.9: how long the AFSK capture runs on after the APRS slot's squelch closes (the frame's last flags).
pub const SQ_TAIL_MS: u32 = 300;
/// PWR-26.9.1: after our own AFSK transmission the capture runs this long whatever the squelch says, so a
/// digipeater's copy (a second or two later, while the chip settles from the key-up) is heard from its first flag.
pub const AFTER_TX_MS: u32 = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderKind {
    Off,
    Afsk,
    Pocsag,
    Dsc,
}

/// Who announced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Radio,
    Dsp,
    Link,
    Storage,
    Gps,
    Ui,
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Boot check result: payload[0] = check index, payload[1] = 1 passed / 0 failed.
    Check,
    SquelchOpen,
    SquelchClose,
    /// A decoder found something; the payload is the start of it, the full record goes to the event log.
    Decoded,
    GpsFix,
    LinkConnected,
    Fault,
}

/// One event: small and fixed-size, so the bus is a static array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub source: Source,
    pub payload: [u8; 12],
}

impl Event {
    pub const fn new(kind: Kind, source: Source) -> Self {
        Event { kind, source, payload: [0; 12] }
    }
}

/// The receiver's state now, published by the Radio task, drawn by the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Live {
    pub freq: Freq10,
    /// dBm.
    pub rssi: i16,
    pub squelch_open: bool,
    /// The receiver is in its sleep phase (Battery Save).
    pub rx_asleep: bool,
    /// Chips that answered with their ID at start-up (bit per chip).
    pub chips: u8,
}

/// Slots on the radio: A, B and C (manual chapter 6).
pub const SLOTS: usize = 3;

/// One slot's receiver now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotLive {
    /// What the receiver is on (while scanning, the current step).
    pub freq: Freq10,
    /// dBm.
    pub rssi: i16,
    /// A carrier above the squelch (the BK4829's RSSI and noise squelch), whatever tone it carries.
    pub squelch_open: bool,
    pub scanning: bool,
    /// The scan is stopped on a hit (held there until it resumes), not just passing a busy step.
    pub held: bool,
    /// Scan hits so far, and the last one.
    pub hits: u8,
    pub last_hit: Freq10,
    /// A channel scan's step and its last hit as positions in its list plus 1 (0: none, or a band scan), so the
    /// UI can show their names (HOME-4.1; the UI knows which channel is at a position, SCAN-25).
    pub chan: u16,
    pub hit_chan: u16,
    /// Not watched: it needs the other band relay state (WATCH-11).
    pub relay: bool,
    /// Nuisance skips in this scan (SCAN-11).
    pub skips: u8,
    /// The tone hint (TSRCH-13): the tone word heard on a carrier the slot's tone squelch keeps shut, 0 none.
    pub heard_tone: u16,
    /// DTMF heard on the slot (DEC-6.1): the digits its card shows for a while.
    pub dtmf: crate::dtmfrx::Heard,
}

/// All three slots, published by the Radio task as one snapshot; [`Live`] stays the single-receiver view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveAll {
    pub slots: [SlotLive; SLOTS],
    pub rx_asleep: bool,
    pub chips: u8,
    /// Transmit: keyed, or the Radio task's refusal.
    pub tx: TxLive,
    /// WATCH-16.2: watched slots the watch leaves out now (a key-up, a relay cancel): the APRS card says "paused".
    pub dropped: u8,
    /// SQL-19.4: the APRS slot's squelch counts since power-on (both chips).
    pub aprs_sq: crate::aprssq::Counts,
    /// The one exclusive mode running, if any: scope, tone search, counter, FM broadcast, HF, crossband.
    pub mode: ModeLive,
    /// MODE-3: counts the exclusive modes the Radio task stopped itself because the UI's lease ran out; the UI
    /// takes a change as "nothing runs" and starts again whatever it still wants.
    pub lease_drops: u8,
    /// BCR-24: the broadcast plays on the speaker now (no amateur slot heard). Apart from [`ModeLive::Fm`], which
    /// home's strip replaces with its bars while it sweeps beside a broadcast (MODE-5).
    pub bcast_heard: bool,
    /// A channel scan needs what is at a position of a slot's list ([`crate::scan::want`]): 1 + slot × 4096 +
    /// position, 0 for none (so the default is none); the UI answers with [`RadioCmd::ScanEntry`]. Read it with
    /// [`LiveAll::wanted`].
    pub want: u16,
}

/// Transmit now (TX-22).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxLive {
    pub keyed: bool,
    /// Which slot (3 = HF mode).
    pub slot: u8,
    /// Mic level, scaled 0–100 from the PA0 level tap (TX-12.3).
    pub mic: u8,
    /// The Radio task refused the last key-up (its own checks: calibration, read-back, battery).
    pub refused: Option<crate::txplan::Refusal>,
    /// Counts each dialled sequence sent to the end ([`RadioCmd::DtmfSend`]): the UI unkeys when it moves.
    pub dtmf_sent: u8,
    /// VOX hears speech (the tail included): the UI keys the active slot while it is set (VOX-13).
    pub voice: bool,
    /// SAFE-14: the PTT inputs held since boot for over 1 s and not let go (`ferrite_radio::ptt` bits: 1 PTT A,
    /// 2 PTT B, 4 the side connector's), and in the high nibble those stuck at some time since boot
    /// ([`crate::stuck::latch`]): the UI shows "PTT stuck" with the names while held and 2 s after.
    pub ptt_stuck: u8,
}

/// The terminal's debug commands (TERM-21 to TERM-24): what the firmware is asked. Memory is read where the
/// terminal runs; BK4829 and SI4732 requests go to the Radio task, which owns the chips. Not queued with
/// [`RadioCmd`]: an SI4732 command doesn't fit its 8 bytes, and these go one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugReq {
    #[cfg(feature = "bench")]
    /// `n` bytes (at most [`DEBUG_MEM`]) of MCU memory from `addr`; `words`: 32-bit reads (peripherals).
    Mem { addr: u32, n: u8, words: bool },
    #[cfg(feature = "bench")]
    /// A BK4829 register (chip 0 VHF, 1 UHF/HF).
    BkRead { chip: u8, reg: u8 },
    #[cfg(feature = "bench")]
    /// Write a BK4829 register, then read it back. Transmit and PA bits are refused (TERM-23).
    BkWrite { chip: u8, reg: u8, val: u16 },
    #[cfg(feature = "bench")]
    /// An SI4732 command: its first `n` bytes, and `reply` bytes back (the status byte first).
    Si { cmd: [u8; 8], n: u8, reply: u8 },
    #[cfg(feature = "bench")]
    /// A read-only query to the Bluetooth module (TERM-27): its first `n` bytes, sent with CR LF on USART1;
    /// [`bt_query_refused`] has passed it.
    Bt { text: [u8; 16], n: u8 },
    #[cfg(feature = "bench")]
    /// TERM-27: USART1's counters and rate (`bt stat`); with `baud` not 0, USART1 restarted at that rate first, for
    /// this power-on (`bt baud N`: the MCU's side only, nothing is sent to the module); with `pace`, LINK-36's
    /// Bluetooth pacing set to it first, in RAM (`bt pace B G`).
    BtStat { baud: u32, pace: Option<crate::link::BtPace> },
    #[cfg(feature = "bench")]
    /// Decision 285, step 3: the link codecs timed on the radio's own clock over [`crate::linkz_data::SAMPLES`].
    Linkz,
    #[cfg(feature = "bench")]
    /// TERM-29, the bench test of decision 114: hold both band receive selects on (`Some(true)`), back to one
    /// (`Some(false)`), or only ask (`None`). The Radio task refuses while keyed, in HF mode and while the
    /// crossband repeater runs.
    RxBoth(Option<bool>),
    #[cfg(feature = "bench")]
    /// SAFE-8.1 (decision 364, a bench test): the Radio task faults (`true`, an undefined instruction) or hangs with
    /// interrupts on (`false`, a spin above thread mode), to watch the restart. Never answered.
    Crash(bool),
    /// TERM-30: the GPS module's last GGA sentence as received, answered where the terminal runs.
    Gga,
    #[cfg(feature = "bench")]
    /// TERM-31, PWR-21 (decision 119, a bench test): the BK4829 wake and sleep settle waits, set (`Some`, each
    /// within [`crate::settle::MAX_MS`]) or only asked (`None`). RAM only.
    Settle(Option<crate::settle::Settle>),
    #[cfg(feature = "bench")]
    /// TERM-32 (a bench test): a burst of PA0 readings ([`crate::vox::MIC_SAMPLES`] at [`crate::vox::MIC_PERIOD_US`]),
    /// summed up by the Radio task; `true`: PA2 instead, the receive audio the DSP task captures (TERM-32.1, `af`).
    Mic(bool),
    #[cfg(feature = "bench")]
    /// TERM-33 (decision 132, a bench test): the transmit test signal for the next PTT key-up, armed, disarmed or
    /// only asked. The Radio task of a receive-only build refuses it.
    TxTest(crate::txtest::Ask),
    #[cfg(feature = "bench")]
    /// TERM-36 (a bench test): a CASIC sentence to the GPS module, `$PCAS<id>,<arg>`, which
    /// [`crate::gpspower::casic_refused`] has passed (CAS03 NMEA off or Ferrite's set (GPS-34), CAS06 queries, CAS12 standby);
    /// the GPS task sends it ([`crate::gpspower::GpsBench`]). `quiet`: a standby with NMEA off first and not on after.
    Gps { id: u8, arg: u16, quiet: bool },
    #[cfg(feature = "bench")]
    /// TERM-41 (a bench test, the power bench): Battery Save's cycle overridden (`Some(Some)`), back to the level
    /// (`Some(None)`), or only asked (`None`). The Radio task answers with [`DebugAns::Save`].
    Save(Option<Option<crate::rxsleep::Cycle>>),
    #[cfg(feature = "bench")]
    /// TERM-40 (a bench test, the power bench): one change to the display's bench state, or only ask. The UI task
    /// owns the panel and answers with [`DebugAns::Lcd`].
    Lcd(LcdSet),
    #[cfg(feature = "bench")]
    /// WATCH-17.1 (decision 262, a bench measure): each slot's turns on a chip so far
    /// ([`crate::watch::TurnClock`]), zeroed after if `reset`. The Radio task answers with [`DebugAns::Turns`].
    Turns { reset: bool },
    #[cfg(feature = "bench")]
    /// TERM-53 (a bench test): the BK4829 bus's read and write half SCK periods set (`set`, tenths of a us, RAM only),
    /// then `reads` rounds on each chip at `half` for both: its ID read, REG_48 written in four bit patterns and read
    /// again at stock's. The speeds and REG_48 are as before after. Answered with [`DebugAns::Spi`].
    Spi { half: Option<u8>, reads: u16, set: Option<[u8; 2]> },
    #[cfg(feature = "bench")]
    /// TERM-42 (decision 305, a bench test): the receive AF filter override, REG_2B bits 10:8 after every FM tune
    /// (`Some(Some(bits))`), off (`Some(None)`), or only asked (`None`). RAM only. Answered with [`DebugAns::RxAf`].
    RxAf(Option<Option<u16>>),
    #[cfg(feature = "bench")]
    /// TERM-42.2 (decision 315, a bench test): the receive IF override, the whole of REG_43 after every FM tune
    /// (`Some(Some(value))`), off (`Some(None)`), or only asked (`None`). RAM only. Answered with [`DebugAns::RxIf`].
    RxIf(Option<Option<u16>>),
    #[cfg(feature = "bench")]
    /// TERM-42.3 (decision 316, a bench test): registers held after every FM tune on both chips: one added or
    /// replaced (`Some(Some((reg, value)))`), all ended (`Some(None)`), or only asked (`None`). RAM only. Answered
    /// with [`DebugAns::Hold`].
    Hold(Option<Option<(u8, u16)>>),
    #[cfg(feature = "bench")]
    /// TERM-29.4 (a bench test, the fox-hunt measurement): the slots received on the other band's chip, set
    /// (`Some(mask)`, 0 none) or only asked (`None`). RAM only. Answered with [`DebugAns::Swap`].
    Swap(Option<u8>),
    #[cfg(feature = "bench")]
    /// TERM-45 (decision 318, a bench test): 4FSK from the keyed chip's tone generator, the mic off; the Radio task
    /// runs it at once (Dry TX on, keyed only). Answered with [`DebugAns::SynthFsk`].
    SynthFsk(crate::synthfsk::Cfg),
    #[cfg(feature = "bench")]
    /// TERM-46 (decisions 319, 320, a bench test): an audio pin now ([`PINS`]), driven low or high or made an input
    /// with a pull-up or pull-down ([`PinMode`]). The radio's own logic sets them again at its next path step or
    /// burst. Answered with [`DebugAns::Pin`].
    Pin { which: u8, mode: PinMode },
    #[cfg(feature = "bench")]
    /// TERM-44 (decision 315, a bench test): the receive level meter on (zeroed), off, or only asked (`None`).
    /// Answered with [`DebugAns::Meter`].
    Meter(Option<bool>),
    #[cfg(feature = "bench")]
    /// TERM-43 (decision 305, a bench test): the BK4829's FSK modem. Answered with [`DebugAns::Fsk`].
    Fsk(crate::fskbench::Ask),
}

/// TERM-40: a change to [`LcdBench`].
#[cfg(feature = "bench")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LcdSet {
    Ask,
    /// Everything back to normal.
    Reset,
    /// The ST7789's idle mode (IDMON: 8 colours).
    Idle(bool),
    /// The ST7789's partial mode (PTLON) on rows [`LCD_PARTIAL`]; the rest blank.
    Partial(bool),
    /// The panel off and asleep (DISPOFF, SLPIN), no redraws, the backlight 0.
    Sleep(bool),
    /// Redraws at most this many a second (1 to 20); 0 normal (20, LINK-38).
    Fps(u8),
    /// The screen backlight at this %, or the settings' (None).
    Bl(Option<u8>),
    /// The keypad light at this %, or the settings' (None).
    Keys(Option<u8>),
    /// Time one whole-screen fill (153,600 bytes) on the panel bus, then redraw; the answer carries it.
    Bus,
}

/// TERM-40: the display's bench state.
#[cfg(feature = "bench")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LcdBench {
    pub idle: bool,
    pub partial: bool,
    pub sleep: bool,
    pub fps: u8,
    pub bl: Option<u8>,
    pub keys: Option<u8>,
    /// The last `lcd bus` (none yet: `None`): the whole-screen fill's microseconds and the MCU clock in MHz.
    pub bus: Option<(u32, u16)>,
}

#[cfg(feature = "bench")]
impl LcdBench {
    /// The state after `s`.
    pub fn apply(mut self, s: LcdSet) -> Self {
        match s {
            LcdSet::Ask => {}
            LcdSet::Reset => self = Self::default(),
            LcdSet::Idle(on) => self.idle = on,
            LcdSet::Partial(on) => self.partial = on,
            LcdSet::Sleep(on) => self.sleep = on,
            LcdSet::Fps(n) => self.fps = n.min(20),
            LcdSet::Bl(p) => self.bl = p.map(|p| p.min(100)),
            LcdSet::Keys(p) => self.keys = p.map(|p| p.min(100)),
            LcdSet::Bus => {} // the UI task fills in `bus`
        }
        self
    }
}

/// TERM-40: the panel rows partial mode keeps (the mock-up's band, rows 80-239 of 320).
#[cfg(feature = "bench")]
pub const LCD_PARTIAL: (u16, u16) = (80, 239);

/// The longest GGA sentence [`DebugAns::Gga`] carries: NMEA's 82 characters less CR LF.
pub const GGA_MAX: usize = 80;

/// The most memory one `DebugReq::Mem` reads (a bench build's).
#[cfg(feature = "bench")]
pub const DEBUG_MEM: usize = 64;
/// TERM-46: the pins `pin` drives, in its order: PA15 (cross-band mic-path select), PC12 (DAC1's mic-line switch),
/// PC9 (an audio switch stock only ever clears), PB8 (the shared audio stage), PE1 (the accessory route), PE4 (the
/// speaker amplifier).
pub const PINS: [&str; 6] = ["pa15", "pc12", "pc9", "pb8", "pe1", "pe4"];

/// TERM-46: what `pin` does to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinMode {
    Low,
    High,
    PullUp,
    PullDown,
}

/// TERM-42.3: how many registers `bk hold` keeps.
pub const HOLDS: usize = 4;

/// The answer to a [`DebugReq`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugAns {
    #[cfg(feature = "bench")]
    Mem { addr: u32, data: [u8; DEBUG_MEM], n: u8 },
    #[cfg(feature = "bench")]
    Reg { chip: u8, reg: u8, val: u16 },
    #[cfg(feature = "bench")]
    Si { data: [u8; 16], n: u8 },
    #[cfg(feature = "bench")]
    /// What the Bluetooth module sent back within 500 ms (TERM-27).
    Bt { data: [u8; 48], n: u8 },
    #[cfg(feature = "bench")]
    /// TERM-27: `bt stat`.
    BtStat(BtStats),
    #[cfg(feature = "bench")]
    /// `bt linkz` (LINK-51): the core clock (MHz), the samples' plain bytes, then for the radio-to-PC and PC-to-radio
    /// windows: bytes, encode cycles, decode cycles (two values unused).
    Linkz([u32; 11]),
    #[cfg(feature = "bench")]
    /// TERM-29: whether both band receive selects are held on now.
    RxBoth(bool),
    #[cfg(feature = "bench")]
    /// TERM-31: the settle waits in force now.
    Settle(crate::settle::Settle),
    #[cfg(feature = "bench")]
    /// TERM-32: the PA0 burst.
    Mic(crate::vox::MicCapture),
    #[cfg(feature = "bench")]
    /// TERM-33: what is armed for the next PTT key-up now (None: nothing).
    TxTest(Option<crate::txtest::Arm>),
    #[cfg(feature = "bench")]
    /// TERM-34: `txtest suite` running: the section (1-7; 0 the lead-in before the first, 8 holding the end tone
    /// until the release) and the ms left to the hold.
    TxSuite { section: u8, left_ms: u32 },
    /// TERM-30: the last GGA sentence from `$` to the checksum (`n` 0: none since boot), and its age in seconds.
    Gga { data: [u8; GGA_MAX], n: u8, age_s: u32 },
    #[cfg(feature = "bench")]
    /// TERM-36: the CASIC sentence went to the module; the first TXT sentence it sent back within
    /// [`GPS_REPLY_MS`] (from `$` to the checksum), `n` 0 for none.
    GpsSent { data: [u8; GGA_MAX], n: u8 },
    #[cfg(feature = "bench")]
    /// TERM-40: the display's bench state now.
    Lcd(LcdBench),
    #[cfg(feature = "bench")]
    /// TERM-41: the cycle in force: the override, or the level's on/off times (None: Battery Save off).
    Save { cycle: Option<crate::rxsleep::Cycle>, level: Option<(u32, u32)>, asleep: bool },
    #[cfg(feature = "bench")]
    /// WATCH-17.1: each slot's whole turns and their ms, before any reset.
    Turns(crate::watch::Turns),
    #[cfg(feature = "bench")]
    /// TERM-53: the half period the reads ran at, each chip's failed rounds (u16::MAX: the chip isn't there), the
    /// mean us an operation took, and the read and write half periods now.
    /// `half` and `now` in tenths of a us.
    /// `rd`: each chip's wrong IDs read at `half`; `wr`: its REG_48 writes at `half` that read back wrong at stock's.
    Spi { half: u8, reads: u16, rd: [u16; 2], wr: [u16; 2], us: u32, now: [u8; 2] },
    #[cfg(feature = "bench")]
    /// TERM-42: the receive AF filter override in force (REG_2B bits 10:8), None off.
    RxAf(Option<u16>),
    #[cfg(feature = "bench")]
    /// TERM-42.2: the receive IF override in force (REG_43), None off.
    RxIf(Option<u16>),
    #[cfg(feature = "bench")]
    /// TERM-42.3: the registers held ([`HOLDS`] slots; register 0 an empty one).
    Hold([(u8, u16); HOLDS]),
    #[cfg(feature = "bench")]
    /// TERM-29.4: the slots on the other band's chip now.
    Swap(u8),
    #[cfg(feature = "bench")]
    /// TERM-45: what the tone-generator 4FSK run did.
    SynthFsk(crate::synthfsk::Report),
    #[cfg(feature = "bench")]
    /// TERM-46: the pin and its level read back.
    Pin { which: u8, mode: PinMode, high: bool },
    #[cfg(feature = "bench")]
    /// TERM-44: the receive level meter's state and its bins.
    Meter(crate::rxmeter::Report),
    #[cfg(feature = "bench")]
    /// TERM-43: the FSK modem test's state and counts.
    Fsk(crate::fskbench::Report),
    /// Not done, and why.
    Refused(&'static str),
}

/// TERM-36: how long the GPS task waits for the module's TXT answer (within the terminal's 2 s).
#[cfg(feature = "bench")]
pub const GPS_REPLY_MS: u32 = 1200;

/// TERM-27: a Bluetooth module query is sent only when it can't change the module (the user's rule, decision 79:
/// a wrong `AT+BAUD=` leaves the radio's Bluetooth silent): `AT`, `AT+HELP`, or `AT+WORD?` (letters and digits).
/// Anything with `=`, anything else: refused. A bench build's (decision 151).
#[cfg(feature = "bench")]
/// USART1, the Bluetooth module's UART (TERM-27's `bt stat`): bytes each way and line errors since boot, the rate it
/// runs at (0: never started), and whether the module is powered (PB9).
#[cfg(feature = "bench")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BtStats {
    pub rx: u32,
    pub tx: u32,
    pub framing: u32,
    pub overrun: u32,
    /// Bytes the receive ring had no room for.
    pub dropped: u32,
    pub baud: u32,
    pub on: bool,
    /// LINK-36's pacing in use.
    pub pace: crate::link::BtPace,
}

/// The rates `bt baud N` takes (TERM-27): the module's resting 9600 and the usual others, the MCU's side only.
#[cfg(feature = "bench")]
pub const BT_BENCH_RATES: [u32; 5] = [9_600, 19_200, 38_400, 57_600, 115_200];

pub fn bt_query_refused(t: &[u8]) -> Option<&'static str> {
    if t.contains(&b'=') {
        return Some("'=' sets something on the module: only queries are sent");
    }
    let up = |c: &u8| c.to_ascii_uppercase();
    let is = |s: &[u8]| t.len() == s.len() && t.iter().map(up).eq(s.iter().copied());
    if is(b"AT") || is(b"AT+HELP") {
        return None;
    }
    let ok = t.len() > 4
        && t[..3].iter().map(up).eq(b"AT+".iter().copied())
        && t[t.len() - 1] == b'?'
        && t[3..t.len() - 1].iter().all(|c| c.is_ascii_alphanumeric());
    if ok {
        None
    } else {
        Some("only AT, AT+HELP or AT+WORD? queries are sent to the module")
    }
}

#[cfg(test)]
mod bt_query_tests {
    use super::bt_query_refused as r;

    /// TERM-27: queries pass; anything that could set the module doesn't.
    #[test]
    fn only_queries_reach_the_module() {
        for ok in [&b"AT"[..], b"at", b"AT+HELP", b"AT+BAUD?", b"AT+VERS?", b"at+name?", b"AT+ADDR?"] {
            assert_eq!(r(ok), None, "{}", core::str::from_utf8(ok).unwrap());
        }
        for bad in [&b"AT+BAUD=9600"[..], b"AT+NAME=x", b"AT+RST", b"AT+BAUD", b"AT+?", b"AT+NA ME?", b"PROGRAMBT9000U", b"", b"AT+NAME?="] {
            assert!(r(bad).is_some(), "{}", core::str::from_utf8(bad).unwrap());
        }
    }
}

/// BK4829 REG_30 bits that turn on the transmitter: Tx DSP Enable (1) and PA Gain Enable (3), from the
/// register table (`datasheets/rf/bk4829_registers_table`).
pub const BK_REG30_TX_BITS: u16 = 1 << 1 | 1 << 3;
/// REG_30's PA Gain Enable (3) and PLL/VCO Enable (7:4). Without the PLL the chip makes no carrier, so the Tx DSP
/// alone can't radiate (TERM-23, decision 317: the mic research runs the transmit DSP on an idle chip).
pub const BK_REG30_PA: u16 = 1 << 3;
pub const BK_REG30_PLL: u16 = 0x00F0;

/// Why a terminal write to a BK4829 register is refused, in every build (TERM-23): transmit goes only through
/// the Radio task's own checks, and the chip's GPIO outputs (REG_33, REG_34) are wired to who knows what on
/// this board until the bench says (on the UV-K5 they switch the RF path). A bench build's (decision 151).
#[cfg(feature = "bench")]
pub fn bk_write_refused(reg: u8, val: u16) -> Option<&'static str> {
    match reg {
        0x30 if val & BK_REG30_PA != 0 => Some("REG_30's PA Gain bit is never set from the terminal"),
        0x30 if val & BK_REG30_TX_BITS != 0 && val & BK_REG30_PLL != 0 => Some("REG_30's Tx DSP bit only with the PLL/VCO bits (7:4) off: no carrier"),
        0x33 | 0x34 => Some("REG_33 and REG_34 drive the chip's GPIO pins: unknown wiring, not written from the terminal"),
        0x80.. => Some("BK4829 registers are 0x00 to 0x7F"),
        _ => None,
    }
}

/// The most bars in a band scope sweep (SCOPE-9: 30, 60, 120 or 240 across 240 px).
pub const SCOPE_BARS: usize = 240;
/// Crossband activity: 60 columns of 30 s, the last 30 minutes (XB-16).
pub const XB_COLS: usize = 60;

/// What the running exclusive mode reports. One at a time: they all own the receivers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModeLive {
    #[default]
    Off,
    /// Band scope: one level per step for the first `n` bars (0 = blank: not swept, or it needs the other chip
    /// or relay state, SCOPE-5), else dBm + 160, averaged with the last sweep (SCOPE-4). While listening,
    /// `listen` is the frequency and `open` its squelch; the bars hold still.
    Scope { centre: Freq10, bars: [u8; SCOPE_BARS], n: u8, sweeps: u8, listen: Freq10, open: bool },
    /// Tone search: the tone word found (0 = searching); counter: the strongest frequency (0 = none) and level.
    Probe { freq: Freq10, rssi: i16, tone: u16 },
    /// FM broadcast: frequency (10 kHz), RDS station name and the last complete radio text (all 64 characters,
    /// NUL-padded: BCR-30), level (dBuV), SNR (dB), the stereo pilot, and whether it is on the speaker now (no
    /// amateur slot heard, BCR-24).
    Fm { freq: u16, ps: [u8; 8], rt: [u8; 64], rssi: u8, snr: u8, stereo: bool, pty: u8, heard: bool },
    /// HF mode: frequency, signal (dBm), SNR on the SI4732, which receiver, the SSB patch load (0-100, 255 =
    /// loaded or none).
    Hf { freq: Freq10, rssi: i16, snr: u8, si4732: bool, loading: u8 },
    /// Crossband repeater.
    Xb(XbLive),
    /// Satellite mode: the downlink's level (dBm) and squelch.
    Sat { rssi: i16, open: bool },
}

/// The SI4732's receive settings from the UI (BCR-32, HF-35; decision 292), packed to keep [`RadioCmd`] small.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SiOpts {
    /// FM broadcast's AGC (0 2 6 2): 0 automatic, 1-10 a fixed gain, 1 the most.
    pub fm_agc: u8,
    /// HF mode's AGC (the same scale) on the SI4732's AM.
    pub am_agc: u8,
    /// HF mode's AM bandwidth: AM_CHANNEL_FILTER's AMCHFLT (0 = 6, 1 = 4, 2 = 3, 3 = 2 kHz).
    pub am_filter: u8,
    /// [`SiOpts::DEEMPH_50`], [`SiOpts::BAND_JAPAN`], [`SiOpts::MW_9K`], and the FM seek spacing in bits 4-5
    /// (0 = 50, 1 = 100, 2 = 200 kHz).
    pub flags: u8,
}

impl SiOpts {
    /// FM de-emphasis 50 us (outside the US and Canada).
    pub const DEEMPH_50: u8 = 1;
    /// The FM broadcast band 76-95 MHz (region Japan) instead of 87.5-108.
    pub const BAND_JAPAN: u8 = 2;
    /// Medium wave channels 9 kHz apart (outside the Americas).
    pub const MW_9K: u8 = 4;

    /// The fixed-gain scale (1-10) to the chip's gain index: 1 is index 0 (the most gain), 10 the chip's top.
    pub fn agc_index(v: u8, top: u8) -> Option<u8> {
        (v != 0).then(|| ((v.min(10) - 1) as u16 * top as u16 / 9) as u8)
    }

    /// FM seek spacing in 10 kHz.
    pub fn fm_space(self) -> u16 {
        [5, 10, 20][(self.flags >> 4 & 3).min(2) as usize]
    }

    /// The FM seek band in 10 kHz.
    pub fn fm_band(self) -> (u16, u16) {
        if self.flags & Self::BAND_JAPAN != 0 { (7600, 9500) } else { (8750, 10800) }
    }

    /// The AM seek spacing (kHz) at `khz`: MW and LW 9 or 10 kHz, shortwave 5 kHz (HF-34).
    pub fn am_space(self, khz: u16) -> u16 {
        match khz {
            _ if khz >= 1_800 => 5,
            _ if khz < 520 || self.flags & Self::MW_9K != 0 => 9,
            _ => 10,
        }
    }
}

/// The crossband repeater now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XbLive {
    /// 0 ready, 1 side 1 -> side 2, 2 side 2 -> side 1, 3 hang, 4 timed out (waiting for the signal to drop).
    pub state: u8,
    /// Seconds running, and in the current (or last) pass.
    pub run_s: u32,
    pub pass_s: u16,
    /// Seconds since the last pass ended, and its direction (1 or 2).
    pub idle_s: u16,
    pub last_dir: u8,
    pub passes: u16,
    /// Seconds passed in all.
    pub busy_s: u16,
    /// Transmit duty over the last 10 minutes, %.
    pub duty: u8,
    /// Seconds to the next CW ID.
    pub next_id_s: u16,
    /// Receive level on the input side (dBm).
    pub rssi: i16,
    /// The output side's power is cut back, or a key-up was refused (battery, heat): shown instead of the level
    /// (TX-12).
    pub reduced: bool,
    /// The radio refused to start it ([`crate::xb::Stop`] as a number; 0 = running): the UI says why and stops.
    pub stopped: u8,
    /// Seconds passed per column, oldest first: positive side 1 -> 2, negative 2 -> 1.
    pub strip: [i8; XB_COLS],
}

impl Default for XbLive {
    fn default() -> Self {
        XbLive { state: 0, run_s: 0, pass_s: 0, idle_s: 0, last_dir: 0, passes: 0, busy_s: 0, duty: 0, next_id_s: 0, rssi: -127, reduced: false, stopped: 0, strip: [0; XB_COLS] }
    }
}

impl LiveAll {
    /// PWR-26.15: only the signal levels or the receiver's power-save sleep (read by the cable log alone) moved since
    /// `old`, with no squelch open, no scan, nothing keyed and no exclusive mode: nothing the UI acts on at once (the
    /// levels are drawn on its next pass).
    pub fn levels_only(&self, old: &LiveAll) -> bool {
        if self.tx.keyed || !matches!(self.mode, ModeLive::Off) || self.slots.iter().any(|s| s.squelch_open || s.scanning) {
            return false;
        }
        let mut a = *self;
        a.rx_asleep = old.rx_asleep;
        for (s, o) in a.slots.iter_mut().zip(&old.slots) {
            s.rssi = o.rssi;
        }
        a == *old
    }

    /// The (slot, position) a channel scan asks for, if any ([`LiveAll::want`]).
    pub fn wanted(&self) -> Option<(u8, u16)> {
        self.want.checked_sub(1).map(|w| ((w >> 12) as u8, w & crate::scan::POS_MAX))
    }

    pub fn set_want(&mut self, w: Option<(u8, u16)>) {
        self.want = w.map_or(0, |(slot, pos)| 1 + ((slot as u16 % 3) << 12 | pos & crate::scan::POS_MAX));
    }

    /// One slot as the single-receiver [`Live`].
    pub fn live(&self, slot: usize) -> Live {
        let s = self.slots[slot % SLOTS];
        Live { freq: s.freq, rssi: s.rssi, squelch_open: s.squelch_open, rx_asleep: self.rx_asleep, chips: self.chips }
    }
}

/// GPS receiver state for the status bar (SBAR-7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpsState {
    #[default]
    Off,
    Searching,
    Fix,
}

/// Radio-wide state the status bar and Status page show, gathered from the GPS, Link and power code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Device {
    /// Minutes since midnight, or None with no time source (`--:--`).
    pub clock_min: Option<u16>,
    pub utc: bool,
    pub gps: GpsState,
    pub sats: u8,
    /// What the GPS power manager has the module doing (GPS-26): the Status page and the terminal show it.
    pub gps_phase: crate::gpspower::GpsPhase,
    /// A Bluetooth device is connected.
    pub bt: bool,
    /// Battery in millivolts and percent.
    pub battery_mv: u16,
    pub battery_pct: u8,
    /// Below the warning level / at the transmit cut-off.
    pub battery_low: bool,
    pub battery_cutoff: bool,
    /// The MCU's die temperature in whole °C (chapter 8, [`crate::mcutemp`]); None until read.
    pub mcu_temp: Option<i8>,
    /// SBAR-23: the main stack's bytes never used since boot (its high-water mark), 0 not measured yet.
    pub stack_free: u32,
    /// SELF-9.2: the statics' bytes (fixed at build time), 0 unknown (the simulators).
    pub ram_statics: u32,
    /// SELF-9.1: the share of the last 10 s the MCU was awake, %; None until the first 10 s.
    pub cpu_pct: Option<u8>,
    /// SELF-9.3: over the same 10 s, the share awake at 24 MHz and at the build's speed (None when it spent under
    /// 0.1 s there), and the share of the time at 24 MHz, %.
    pub cpu_speeds: [Option<u8>; 3],
    /// LINK-13.4: receive audio's blocks since power-on: made by the DSP task, dropped there (the queue full), and
    /// the times one waited for room at the cable; then PA2's counts at a rail since power-on, the last second's
    /// peak (0-32767) and the stream's gain (Q8) (TERM-32.1, LINK-13.8): the readback's `audio` line, for the bench.
    pub audio: [u32; 6],
    /// APRS-56: the packet decoder's counts since power-on: frames decoded, packets missed (an AX.25 address field
    /// but a bad FCS on every slicer), noise (a bad FCS with no address field), FX.25 blocks not corrected, and
    /// frames dropped (the queue to the APRS task full); then the last and the longest time from the APRS slot's
    /// squelch opening to the capture running, ms (PWR-26.9.1); then the AFSK captures started and the times the
    /// demodulator heard 1200-baud data, the capture halves lost, the longest wait for and work on a half, and the
    /// longest run of the Radio and input executors above it while capturing, us, and their runs over 1.5 ms; the last
    /// capture's squelch lag, tick period and first data, ms (APRS-56.4): the readback's `decode` line.
    pub decode: [u32; 18],
    /// SAFE-8.2 (decision 364): what this start follows: 0 an ordinary start, 1 a hang (the watchdog), else the
    /// fault's address (the high bit set when unknown); and the restarts in a row. The readback's `restart` line.
    pub restart: [u32; 2],
    /// A speaker mic is in the side connector (SND-12, [`crate::acc`]): the Status page shows it.
    pub spkmic: bool,
    /// This build can transmit (`--features tx`).
    pub tx_built: bool,
    /// The bench commands are in (the `bench` feature, TERM-38; LINK-35's bit 7).
    pub bench: bool,
    pub selftest: SelfTest,
    pub link: LinkLive,
    /// PWR-26.6: the MCU clock, for System and the readback.
    pub mcu: McuClock,
}

/// PWR-26.6: the MCU clock now and the governor's counters since power on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McuClock {
    /// The core's speed now, MHz; 0 unknown (the simulators).
    pub mhz: u16,
    /// Speed switches since power on.
    pub switches: u32,
    /// The longest a switch has held interrupts off, microseconds.
    pub held_us: u32,
    /// Why it runs at the build's speed now (`clocktree::why` bits; 0 at rest).
    pub why: u16,
}

/// Calibration status from the CAL checks (chapter 35).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CalState {
    /// Not checked yet.
    #[default]
    Unknown,
    Ok,
    /// Erased (0xF0E0 = 0xFF): transmit locked (CAL-5).
    Blank,
    /// A value fails CAL-6.
    Invalid,
    /// The sector's CRC differs from the one stored at install (CAL-7).
    CrcMismatch,
}

/// CAL-20.4: which check locked transmit, for the Calibration page and the refusal ([`SelfTest::cal_at`] says where
/// in the curves for the two curve checks).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CalFault {
    #[default]
    None,
    /// A PA curve entry the bands reach is 0x00 or 0xFF.
    PaCurve,
    /// Low isn't below Mid, or Mid below High, at an entry the bands reach.
    LevelOrder,
    /// The TX windows blank or invalid.
    Windows,
    /// The factory crystal trim over ±20 ppm.
    Trim,
    /// A sub-tone gain over 0x7F.
    ToneGain,
    /// The CRC couldn't be checked: the store wasn't mounted (CAL-7 locks then too).
    Unchecked,
}

/// What the self-test found, for About (SELF-5) and the Status page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelfTest {
    /// DEBUG_IDCODE, 0 until read.
    pub idcode: u32,
    /// The MCU is the expected part (SELF-1).
    pub mcu_ok: bool,
    /// SPI flash JEDEC ID (0 = no answer) and size from it, in MB.
    pub flash_id: u32,
    pub flash_mb: u8,
    pub cal: CalState,
    /// CAL-20.4: the check that failed, and for a curve check where: the row (0 UHF, 1 VHF, 2 200 MHz, 3 HF),
    /// the entry (0-15) and the level (0 Low, 1 Mid, 2 High).
    pub cal_fault: CalFault,
    pub cal_at: [u8; 3],
    /// BK4829s that answered, a bit per chip; the SI4732 answered.
    pub chips: u8,
    pub si4732: bool,
    /// SELF-5: the SI4732's GET_REV at the boot probe, ASCII: firmware major and minor, component major and minor,
    /// chip revision ("6.0", "3.0", "D"); zeros when it didn't answer.
    pub si_rev: [u8; 5],
    /// A verified SSB patch is in SPI flash (HF-20).
    pub ssb_patch: bool,
    /// Battery reading trim in 10 mV (not in the menu until built: decision 276).
    pub battery_trim: i8,
    /// SELF-8: the MCU's 96-bit factory unique ID, lowest word first; zeros until read.
    pub uid: [u32; 3],
    /// LINK-19.6: what the store's mount found: 0 not known (the simulators), 1 mounted, 2 first boot (formatted),
    /// 3 failed (nothing is saved this power-on), 4 another layout's data (UPD-16).
    pub store: u8,
}

/// The link (chapter 29): cable, and a write session's progress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkLive {
    /// The web app is connected over the cable.
    pub cable: bool,
    /// The cable's rate in hundreds of baud (1152 = 115200).
    pub baud_100: u16,
    /// A write session: percent done (LINK-9).
    pub write_pct: Option<u8>,
}

/// The latest GPS fix, published by the GPS task (the firmware's `POSITION` watch) for the UI and the APRS task
/// (manual chapter 28). Compact on purpose: each reader keeps a copy. Position users take the fixed position
/// when there is no fix (GPS-3), and every reader judges the fix by its age (GPS-2): `at_ms` against its own
/// clock, never a fix older than it can use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpsFix {
    /// The module is powered (GPS-1, GPS-4); off: nothing below is current.
    pub on: bool,
    /// 0 no fix, 2 a 2D fix, 3 a 3D fix.
    pub fix: u8,
    /// Satellites used in the fix and in view.
    pub used: u8,
    pub view: u8,
    /// Degrees x 1e7, south and west negative.
    pub lat: i32,
    pub lon: i32,
    /// Altitude above mean sea level, decimetres (3D fixes).
    pub alt_dm: i32,
    /// Speed over ground in tenths of a knot, course over ground in tenths of a degree from true north.
    pub speed_kn10: u16,
    pub course10: u16,
    /// Horizontal dilution of precision x 10 (accuracy is HDOP x 5 m, GPS-2).
    pub hdop10: u16,
    /// UTC as seconds since 1970 from the module's time and date (RMC), 0 until it has both: the clock
    /// (GPS-7) works from this even before a position fix.
    pub unix: u32,
    /// When this was parsed, in the reader's milliseconds since boot (embassy-time's clock on the radio).
    pub at_ms: u32,
}

impl GpsFix {
    /// A position to use: on, with a fix.
    pub fn has_fix(&self) -> bool {
        self.on && self.fix >= 2
    }
}

/// One satellite in view, for the sky plot (GPS-18).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkySat {
    pub prn: u8,
    /// Signal, dB-Hz (0: tracked, no signal).
    pub snr: u8,
    /// Elevation in degrees, azimuth in degrees from true north halved (0-179), to keep the list small.
    pub elev: u8,
    pub azim_2: u8,
    /// Bit 0: used in the fix; bit 1: BeiDou (else GPS and the rest).
    pub flags: u8,
}

/// The satellites in view, for the UI only (GPS-18). The ATGM336H reports up to about 24; 16 cover the plot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpsSky {
    pub n: u8,
    pub sat: [SkySat; 16],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small() {
        // The bus holds a few dozen of these; keep them small.
        assert!(core::mem::size_of::<Event>() <= 16);
        assert!(core::mem::size_of::<RadioCmd>() <= 8);
        // The snapshot isn't queued (one copy in the watch, the Radio task's and the UI's); the exclusive
        // modes share one slot in it, the largest being the band scope's 240 bars (SCOPE-9: 308 bytes, about
        // 0.45 KB more over the three copies than with 56 bars; accepted Oct 6, 2026).
        // The tone hint's heard tone (TSRCH-13: 2 bytes a slot, 4 with alignment) took it from 312 to 324; the stuck
        // PTT bits (SAFE-14.1, one byte, 4 with alignment) to 328; a scan's held flag and its step's and last hit's
        // channel numbers (SCAN-19.1, HOME-4.1: 5 bytes a slot, 4 more with the padding, 22 in 24) to 340; the
        // DTMF heard (DEC-6.1: 7 bytes a slot, 12 digits a nibble each) to 364; the slots the watch leaves out
        // (WATCH-16.2) and the APRS squelch's counts (SQL-19.4) to 372.
        assert!(core::mem::size_of::<LiveAll>() <= 372, "{}", core::mem::size_of::<LiveAll>());
        // Each reader of the GPS watch keeps a copy of the fix; the sky list only the UI.
        assert!(core::mem::size_of::<GpsFix>() <= 32);
        assert!(core::mem::size_of::<GpsSky>() <= 81);
    }

    #[test]
    fn event_starts_with_an_empty_payload() {
        let e = Event::new(Kind::GpsFix, Source::Gps);
        assert_eq!((e.kind, e.source, e.payload), (Kind::GpsFix, Source::Gps, [0; 12]));
    }

    /// BCR-32, HF-35: the fixed AGC scale maps 1 to index 0 (the most gain) and 10 to the chip's top; 0 is
    /// automatic (no override).
    #[test]
    fn agc_scale_to_chip_index() {
        assert_eq!(SiOpts::agc_index(0, 37), None);
        assert_eq!(SiOpts::agc_index(1, 37), Some(0));
        assert_eq!(SiOpts::agc_index(10, 37), Some(37));
        assert_eq!(SiOpts::agc_index(10, 26), Some(26));
        assert_eq!(SiOpts::agc_index(5, 26), Some(11));
        assert_eq!(SiOpts::agc_index(200, 26), Some(26), "past 10 stays at the top");
    }

    /// BCR-32: the FM seek band (Japan 76-95 MHz) and spacing (50, 100, 200 kHz) from the packed flags.
    #[test]
    fn fm_seek_band_and_space() {
        let o = |flags| SiOpts { flags, ..Default::default() };
        assert_eq!(o(0).fm_band(), (8750, 10800));
        assert_eq!(o(SiOpts::BAND_JAPAN).fm_band(), (7600, 9500));
        assert_eq!(o(0).fm_space(), 5);
        assert_eq!(o(1 << 4).fm_space(), 10);
        assert_eq!(o(2 << 4).fm_space(), 20);
        assert_eq!(o(3 << 4).fm_space(), 20, "the spare value clamps to 200 kHz");
    }

    /// HF-35: AM seek steps 9 kHz on LW, MW 10 kHz (9 kHz with MW_9K), 5 kHz on shortwave.
    #[test]
    fn am_seek_space() {
        let am = SiOpts::default();
        let eu = SiOpts { flags: SiOpts::MW_9K, ..Default::default() };
        assert_eq!(am.am_space(198), 9);
        assert_eq!(am.am_space(1_000), 10);
        assert_eq!(eu.am_space(1_000), 9);
        assert_eq!(am.am_space(1_800), 5);
        assert_eq!(eu.am_space(9_400), 5);
    }

    #[test]
    fn xb_live_default_level_is_the_floor() {
        let x = XbLive::default();
        assert_eq!((x.state, x.rssi, x.stopped, x.strip), (0, -127, 0, [0; XB_COLS]));
    }

    /// SCAN-25: the (slot, position) a scan wants travels as 1 + slot × 4096 + position, so the default (0) is none.
    #[test]
    fn wanted_position_round_trips() {
        let mut l = LiveAll::default();
        assert_eq!(l.wanted(), None);
        l.set_want(Some((0, 0)));
        assert_eq!((l.want, l.wanted()), (1, Some((0, 0))));
        l.set_want(Some((2, 4095)));
        assert_eq!(l.wanted(), Some((2, 4095)));
        l.set_want(Some((1, 41)));
        assert_eq!((l.want, l.wanted()), (1 + 4096 + 41, Some((1, 41))));
        l.set_want(None);
        assert_eq!((l.want, l.wanted()), (0, None));
    }

    #[test]
    fn one_slot_as_live() {
        let mut l = LiveAll { rx_asleep: true, chips: 5, ..Default::default() };
        l.slots[1] = SlotLive { freq: 14_550_000, rssi: -90, squelch_open: true, ..Default::default() };
        assert_eq!(l.live(1), Live { freq: 14_550_000, rssi: -90, squelch_open: true, rx_asleep: true, chips: 5 });
        assert_eq!(l.live(1 + SLOTS), l.live(1), "the slot wraps");
        assert_eq!(l.live(0).freq, 0);
    }
}

#[cfg(all(test, feature = "bench"))]
mod debug_tests {
    use super::*;

    /// TERM-23: the transmit and PA bits of REG_30, and the GPIO registers, are never written from the terminal.
    #[test]
    fn bk_writes_refused() {
        assert!(bk_write_refused(0x30, 0xBFF1).is_none(), "stock's receive value");
        assert!(bk_write_refused(0x30, 0xC1FE).is_some(), "stock's transmit value");
        assert!(bk_write_refused(0x30, 1 << 3).is_some());
        // decision 317: the Tx DSP alone, with the PLL off, is allowed (no carrier); never with the PLL or the PA
        assert!(bk_write_refused(0x30, 1 << 1).is_none());
        assert!(bk_write_refused(0x30, 0xC306).is_none(), "mic ADC, Tx DSP, AF DAC: no PLL, no PA");
        assert!(bk_write_refused(0x30, 0xC3F6).is_some(), "Tx DSP with the PLL");
        assert!(bk_write_refused(0x30, 0x0208).is_some(), "PA gain, even without the Tx DSP");
        assert!(bk_write_refused(0x33, 0xFF00).is_some());
        assert!(bk_write_refused(0x34, 0).is_some());
        assert!(bk_write_refused(0x80, 0).is_some());
        assert!(bk_write_refused(0x47, 0x6142).is_none());
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// PWR-26.15: RSSI jitter and the receiver's sleep don't wake a pass; a squelch, a scan, a key-up, a mode or
    /// any other change does.
    #[test]
    fn levels_only_wakes() {
        let old = LiveAll::default();
        let mut l = old;
        l.slots[1].rssi = -100;
        l.rx_asleep = true;
        assert!(l.levels_only(&old));
        assert!(old.levels_only(&old));
        let mut o = l;
        o.slots[0].squelch_open = true;
        assert!(!o.levels_only(&old));
        let mut o = l;
        o.slots[2].scanning = true;
        assert!(!o.levels_only(&old));
        let mut o = l;
        o.tx.keyed = true;
        assert!(!o.levels_only(&old));
        let mut o = l;
        o.slots[0].freq = 14_550_000;
        assert!(!o.levels_only(&old));
        let mut o = l;
        o.want = 1;
        assert!(!o.levels_only(&old));
    }
}
