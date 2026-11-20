//! Radio: the only task that touches the BK4829s and their bus, the SI4732, the RF path pins and the PA. Runs on
//! the highest-priority interrupt executor.
//!
//! Three slots on two chips (manual chapters 6, 13, 14): the host-tested engines decide, this task carries out.
//! - [`Watch`] names the slot each chip listens to (WATCH-2), whose audio is heard (WATCH-3) and when watch is
//!   paused or cancelled (WATCH-6, WATCH-11, WATCH-16). The active slot never changes here (WATCH-5).
//! - [`Hop`] moves the one receive path select between the two chips while both have a slot, as stock's dual
//!   watch hops: a chip without its band's front end is taken as hearing nothing (to confirm on the bench).
//! - [`Scan`] walks a slot's band (a range at its step, the fast pass of SCAN-21) while the slot has a chip,
//!   or its channels: a channel scan (zone, zone set) from the set the UI sends, asking the UI for the
//!   frequencies a few stops ahead ([`scan::ChanScans`], shared with the simulators' fake radio).
//! - One BK4829 awake at a time, as stock (WATCH-2, PWR-7, decision 111): the chip the hop selects; the other
//!   sleeps until the hop comes back to it, and a key-up sleeps the chip not transmitting. APRS mode's C keeps its
//!   chip awake beside it ([`RadioCmd::Awake`]), through Battery Save's sleep too (PWR-4).
//! - Battery Save (`ferrite_core::rxsleep`) sleeps both chips together (WATCH-8, as stock's dual watch), but for
//!   a kept one.
//!
//! Every path select, the PA drive and the band relay change through the path machine (`ferrite_core::path`,
//! SAFE-11): it plans, [`Radio::run`] carries the plan out to the end. Watch, scan and the path hop ask with
//! [`Cause::Auto`], which never moves the relay (SAFE-20, WATCH-10); only the active slot and HF mode do.
//!
//! The bring-up build logs every relay move and path select on the cable (plain byte lines: this task runs from
//! the fast flash and never calls the UI's text code).
//!
//! Transmit is only in the `tx` build (milestone 4): without it the bk4829 crate's transmit methods don't exist,
//! the transmit command does nothing and no transmit path pin or PA drive is ever raised (only a key-up plans
//! one, and only the `tx` build asks for a key-up). With it, every key-up goes through the sequencer
//! (`ferrite_core::txseq`, TX-17) with its own checks (region, calibration, battery, duty, PTT seen released
//! since boot), then the path machine's TX-23 order: PA 0, paths clear, the TX path, the chip to transmit with
//! its read-back (TX-7), the PA ramp to the calibrated level (`ferrite_core::txcal`, stock's lookup).
//!
//! The crossband repeater (manual chapter 19, decision 93) is the `tx` build's too: [`Repeater`] decides, the
//! receive path hops between the two sides while ready, and a pass keys the other chip with the receiving one
//! left in receive (`Path::key_up_xb`), the receiving chip's audio routed into the transmitting chip's mic input
//! with stock's timing (decision 95: PA15 low 10-20 ms after the PA is up, high again at the receive drop while
//! still keyed; PB8 cleared at TX setup unless PC15 says an accessory; `analysis/crossband_repeat.md`, to confirm
//! on the bench).

use crate::board::{ACC_ROUTE, AMP_EN, BK_CS, BK_SCK, BK_SDA, RELAY, REPEAT_AUDIO, RF_PINS, SI4732_ADDR, SI_IN, SI_RST, SI_SCL, SI_SDA};
use crate::bus::{BusyDelay, Io, Out, SiBus};
use crate::chan::{ALERTS, EVENTS, LIVE, RADIO};
#[cfg(feature = "bench")]
use crate::chan::{DEBUG_ANS, DEBUG_REQ, TERM_WAKE};
use crate::hal::gpio::Mode;
use crate::hal::i2c::I2c;
use crate::tasks::{check, CHECK_BK0, CHECK_BK1};
use bk4829::wire::ThreeWire;
use bk4829::{int, AfOut, Bk4829};
use embassy_futures::select::{select4, Either4};
use embassy_time::{Instant, Timer};
use ferrite_core::freq::{self, mhz, Freq10};
#[cfg(feature = "bench")]
use ferrite_core::msg::{bk_write_refused, DebugAns, DebugReq};
use ferrite_core::msg::{BroadcastCmd, Event, Kind, LiveAll, ModeLive, Param, RadioCmd, SameDecoded, SiOpts, Source, SLOTS};
use ferrite_core::same_rx::{Assembler, Repeats, Step};
#[cfg(feature = "bench")]
use ferrite_core::path::Blocked;
use ferrite_core::path::{Cause, Path, Rx, Sel, Steps};
use ferrite_radio::chip::{self as radio_chip, RxSetup};
use ferrite_radio::Pin;
use ferrite_core::rxsleep::{Action, Level, RxSleep};
use ferrite_core::scan::{self, Check, Range, Scan};
use ferrite_core::settle::Settle;
use ferrite_core::sweephold::{self, SweepHold};
use ferrite_core::tonegen;
use ferrite_core::tonehint::{Act as HintAct, Hint};
use ferrite_core::watch::{self, Hop, Watch};
use si4732::{Mode as SiMode, Si4732, Sideband};
#[cfg(feature = "tx")]
use ferrite_core::{
    msg::TxLive,
    txcal::{Gauge, TxCal},
    txplan::{Refusal, Region},
    txseq::{self, Kind as TxKind, Req, TxSeq},
    xb::{self, Cmd as XbCmd, Repeater},
};

mod cmd;
#[cfg(feature = "bench")]
mod debug;
#[cfg(feature = "bench")]
mod fskbench;
#[cfg(all(feature = "bench", feature = "tx"))]
mod synthbench;
mod tune;
pub(crate) mod shadow;
mod tick;
mod same;
mod dtmf;
mod bcast;
mod hf;
mod sat;
mod beep;
mod counter;
mod scope;
mod hw;
#[cfg(feature = "tx")]
mod tx;
#[cfg(all(feature = "tx", feature = "bench"))]
pub mod afskbk;
#[cfg(all(feature = "tx", feature = "bench"))]
mod suite;
#[cfg(feature = "tx")]
mod crossband;

type Wire = ThreeWire<Out, Io, BusyDelay>;

/// Start-up frequency until the UI's slots arrive: 2 m calling.
const DEFAULT: Freq10 = freq::mhz(146) + 52_000;
/// Battery Save until settings exist.
const DEFAULT_SAVE: Level = Level::L1;
/// Signal meter and squelch poll while one slot listens alone.
const POLL_MS: u64 = 50;
/// The engines' tick while watch hops or a scan runs: under the fast scan step (15 ms) and the watched dwell
/// (70 ms).
const TICK_MS: u64 = 10;
/// The fast pass (SCAN-21) counts a step as "over the floor" this far above the chip's noise floor (to confirm
/// on the bench).
const FAST_MARGIN_DB: i16 = 10;
/// The HF mode's internal slot index (the chips' `tuned` entry for it).
const HF_SLOT: u8 = 3;
/// The sequencer's settings in the `tx` build: the timeout a little after the UI's own (0 8 1, sent as
/// [`RadioCmd::TxTimeout`]: the UI unkeys first; this is the backstop), no pre-alert (the UI shows it). End tone,
/// CW ID and tail elimination follow what the UI sends ([`RadioCmd::TxTones`], [`Param::EndTone`],
/// [`Param::Tail`]): see `Radio::tx_cfg`.
#[cfg(feature = "tx")]
const TX_CFG: txseq::Cfg = txseq::Cfg { timeout_s: 125, prealert_s: 0, end_tone: false, cw_id: false, cw_each: false, tail: false };
/// TX-30: the Radio task's backstop runs this long after the UI's timeout.
#[cfg(feature = "tx")]
const TX_BACKSTOP_S: u16 = 5;
/// The crossband repeater's receive path turn per side while ready (XB-4: a pass needs 30 ms of open squelch; to
/// confirm on the bench with the unselected front end, which may hear enough to drop the hop).
#[cfg(feature = "tx")]
const XB_TURN_MS: u32 = 50;

/// The speaker as [`speaker`] last set it: a beep reads it to know whether audio is already playing (SND-13).
static SPEAKER_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Speaker amplifier and the shared audio stage (both roles likely, from stock).
#[inline(never)]
fn speaker(on: bool) {
    SPEAKER_ON.store(on, core::sync::atomic::Ordering::Relaxed);
    pb8(on);
    AMP_EN[1].write(on);
}

/// The shared audio stage (PB8). Held on while a speaker mic is connected (SND-12): stock's arbiter and PTT
/// handler never clear it while its PC15 flag is set.
#[inline(never)]
fn pb8(on: bool) {
    AMP_EN[0].write(on || crate::chan::ACCESSORY.load(core::sync::atomic::Ordering::Relaxed) & ferrite_core::acc::ON != 0);
}

/// What the Radio task keeps of a slot's settings for receiving (the transmit side is the `tx` build's).
#[derive(Clone, Copy)]
struct SlotRx {
    freq: Freq10,
    narrow: bool,
    /// The receive tone word (CHAN-3): CTCSS in 0.1 Hz, bit 15 DCS, bit 14 inverted; 0 none.
    rx_tone: u16,
    /// The clarifier, Hz (VFO-22): receive only.
    clar: i16,
    /// Squelch 0-9; 0 is open (SQL: always heard, like monitor).
    sq: u8,
    /// AM (Param::Mode 2): receive only (TX-20).
    am: bool,
    /// Tail elimination on receive (TONE-6, [`Param::Tail`] bit 0): with no receive tone, the 55 Hz tail mutes.
    rx_tail: bool,
    /// Busy lockout (TX-21) and a receive-only channel (CHAN-4): the Radio task refuses key-ups too (TX audit F11).
    busy: bool,
    rx_only: bool,
}

impl SlotRx {
    const DEFAULT: SlotRx = SlotRx { freq: DEFAULT, narrow: false, rx_tone: 0, clar: 0, sq: 3, am: false, rx_tail: false, busy: false, rx_only: false };

    /// The slot's audio opens (WATCH-3): squelch `open` and its tone heard; a tone channel goes quiet when its tone
    /// stops (the tail's phase shift or turn-off code ends it, TONE-6); with no tone and receive tail on, the 55 Hz
    /// tail mutes.
    fn toned(&self, open: bool, st: &bk4829::Status) -> bool {
        open && if self.rx_tone == 0 { !(self.rx_tail && st.tail) } else { radio_chip::tone_heard(self.rx_tone, st) }
    }

    /// Where the chip tunes: the frequency moved by the clarifier (10 Hz units).
    fn rx_freq(&self) -> Freq10 {
        (self.freq as i32 + self.clar as i32 / 10).max(0) as Freq10
    }
}

/// HF mode (manual chapter 20a): the frequency, its mode (as [`Param::Mode`]) and which receiver has it.
/// HF-19: the SI4732's SSB tune (kHz, USB) as last set, so a step within it only moves the BFO; None after a
/// power-up or in AM.
type SsbAt = Option<(u16, bool)>;

#[derive(Clone, Copy, PartialEq)]
struct HfOn {
    freq: Freq10,
    mode: u8,
    /// The SI4732 (below 18 MHz, and AM up to 30 MHz: HF-10); otherwise chip 1 through the energised relay.
    si: bool,
}

struct Radio {
    /// The MCU temperature (SBAR-20): when it was last read, and its smoothed tenths.
    temp: (Option<u32>, Option<i16>),
    /// MODE-3: the UI's lease on the exclusive mode running here.
    lease: ferrite_core::lease::Lease,
    /// SQL-1: the calibration's squelch tables (stock's defaults until the UI task has read the sector), and what
    /// each chip was last given (REG_78, REG_4F), so a retune writes them only when they change.
    sqcal: ferrite_core::sqcal::SqCal,
    sq_regs: [Option<(u16, u16)>; 2],
    rf_gain: u8,
    /// Chips that still need the latest gain setting when next receiving.
    rf_gain_pending: u8,
    /// SQL-18: each chip's software squelch, and the (slot, frequency) it is for (a retune starts it closed).
    soft_sq: [ferrite_core::softsq::SoftSq; 2],
    /// SQL-19: the APRS slot's squelch over the chip's, per chip.
    aprs_sq: [ferrite_core::aprssq::AprsSq; 2],
    /// PWR-26.9: the APRS slot's squelch as the DSP task last heard it ([`DspCmd::Squelch`]).
    aprs_open_sent: bool,
    ssb_at: SsbAt,
    /// TERM-29.4 (a bench test): slots received on the other band's chip (`bk swap`); 0 outside the bench builds.
    swap: u8,
    sq_key: [Option<(u8, Freq10)>; 2],
    /// SQL-17: each chip's squelch open delay as last written (true: 0, for a scan step or the strip's look; false:
    /// stock's 7); None after a sleep or a scope start, so the next look or tick writes it.
    open_fast: [Option<bool>; 2],
    /// APRS-55 (decision 309): each chip's REG_28 noise gate and REG_73 AFC as last set (true: off, the APRS slot's
    /// data set-up); None after a sleep, so the next tune writes them.
    data_rx: [Option<bool>; 2],
    /// When the battery was last read (PWR-20).
    bat_at: Option<u32>,
    si: Si4732<SiBus>,
    wire: Wire,
    cs: [Out; 2],
    /// PWR-26.16: each chip's register shadow, kept by [`Radio::chip`]'s writes.
    shadow: [shadow::Shadow; 2],
    chips: u8,
    live: LiveAll,
    save: RxSleep,
    path: Path,
    /// The path machine muted the receiver (around a relay move or while keyed): the squelch doesn't open the
    /// speaker meanwhile.
    muted: bool,
    /// The SI4732 has had the antenna since boot: leaving it sets stock's radio state, PA7 = 1, PC5 = 0 (BCR-20).
    si_used: bool,
    /// FM broadcast's receiver state for the live report (BCR-2, BCR-5).
    fm: Option<FmRx>,
    /// The SI4732's receive settings from the UI (BCR-32, HF-35), applied at its power-up and on a change.
    si_opts: SiOpts,
    /// BCR-6: the SI4732 is the source being heard (broadcast on, no slot heard); its hard mute follows this.
    si_heard: bool,
    /// BCR-24: the broadcast's turn on the speaker (back 2 s after a slot heard goes quiet).
    bturn: watch::BcastTurn,
    /// BCR-23: the slot playing broadcast FM (no chip listens to it while FM plays).
    bcast_slot: Option<u8>,
    slots: [SlotRx; SLOTS],
    active: u8,
    /// The watch set from the UI, a bit per slot.
    watch_set: u8,
    watcher: Watch,
    hop: Hop,
    scans: [Scan; SLOTS],
    ranges: [Range<'static>; SLOTS],
    /// Channel scans: each slot's set of channel numbers and the frequencies asked for (SCAN-3, SCAN-12,
    /// SCAN-22; decision 86).
    chans: scan::ChanScans,
    /// What each chip is tuned to now: the slot and its receive frequency; None asleep, or to tune again.
    tuned: [Option<(u8, Freq10)>; 2],
    /// Chips that may be awake, a bit per chip: set when one is tuned or keyed, cleared when it sleeps. A reset of
    /// `tuned` (after an unkey, a mode) leaves it, so a chip no longer wanted is put to sleep, not left on.
    maybe_awake: u8,
    /// PWR-21 (decision 119): the BK4829 settle waits, from the terminal (`bk settle`, TERM-31), RAM only.
    settle: Settle,
    /// WATCH-17.1 (decision 262): the turns each chip took, timed at the retune (`tune_start`; `bk turns`).
    #[cfg(feature = "bench")]
    turns: ferrite_core::watch::TurnClock,
    /// When each chip's REG_37 supplies came on (ms), None while they are off: what is left of the wake settle
    /// counts from here, so a chip woken a tick early by the hop is tuned at once.
    woke: [Option<u32>; 2],
    /// PWR-21.6: chips asleep beside a broadcast with their REG_37 supplies left on (bit per chip).
    ldo_kept: u8,
    /// Slots whose chip stays awake (APRS mode's C: [`RadioCmd::Awake`], decision 111).
    keep: u8,
    /// The APRS slot (C while APRS mode, KISS or beacons hold it: [`RadioCmd::AprsSlot`]): its chip listens with
    /// the squelch's open delay at 0 (SQL-17, decision 181).
    aprs_slots: u8,
    /// Each chip's noise floor (dBm), a slow average of its RSSI with the squelch closed (the fast pass's
    /// reference).
    floor: [i16; 2],
    /// Monitor latched, a bit per slot (SQL-2).
    monitor: u8,
    /// Quick mute from the UI, a bit per slot (HOME-15): still received, never on the speaker.
    user_mute: u8,
    /// Slots turned off from the UI, a bit per slot (HOME-15.1): no chip listens to them, never on the speaker.
    user_off: u8,
    /// Each slot's tone hint (TSRCH-12 to TSRCH-15): when to measure the tone on a carrier its tone squelch keeps
    /// shut, and what was heard.
    hints: [Hint; SLOTS],
    /// Radio-wide settings the UI sends: scan inputs (SCAN-22, for channel lists) and the subtone level (TONE-16,
    /// the `tx` build's).
    scan_tx: bool,
    tone_level: u8,
    /// The beep volume's tone gain (SND-3, 0 7 5 2: `tonegen::beep_gain`).
    beep_gain: u8,
    /// The DAC beep's level (1-11, `hal::tone`), from the beep volume (SND-3: Low, Mid, High -> 5, 7, 9).
    beep_level: u8,
    hf: Option<HfOn>,
    /// Satellite mode (SAT-1), while the satellite screen is open.
    sat: Option<sat::SatRun>,
    /// HF-31, BCR-18: an SI4732 seek running (HF mode's or FM broadcast's), polled by the tick (MODE-7: no command
    /// waits for it).
    seek: Option<SiSeek>,
    /// The frequency counter (TSRCH-10), while on.
    counter: Option<Counter>,
    /// Tone search (TSRCH-1 to TSRCH-3), while on: the slot searched and what was found.
    tsearch: Option<(u8, ferrite_core::tonesearch::Search)>,
    /// The band scope (SCOPE-4), while it runs; its points per sweep and dwell (0.1 ms) from the last ScopeCfg.
    scope: Option<ScopeRun>,
    /// SCOPE-14: the sweep waits after a key or knob step, so the UI handles it first.
    sweep_hold: SweepHold,
    scope_cfg: (u8, u8, bool),
    /// SAME weather alerts (WX-4, DEC-19): on from the UI, and the BK4829 receiver's state.
    same_on: bool,
    /// DEC-6: the DTMF detector wanted, what each chip was armed for, and each slot's digits.
    dtmf_on: bool,
    dtmf_key: [Option<(u8, Freq10)>; 2],
    dtmf: [ferrite_core::dtmfrx::Track; SLOTS],
    same: SameRx,
    /// TERM-42 (decision 305): `bk rxaf`'s REG_2B bits 10:8, written after every FM tune; None off. RAM only.
    #[cfg(feature = "bench")]
    rxaf: Option<u16>,
    /// TERM-42.2 (decision 315): `bk rxif`'s REG_43, written after every FM tune; None off. RAM only.
    #[cfg(feature = "bench")]
    rxif: Option<u16>,
    /// TERM-42.3 (decision 316): `bk hold`'s registers, written after every FM tune; register 0 an empty slot.
    #[cfg(feature = "bench")]
    holds: [(u8, u16); ferrite_core::msg::HOLDS],
    /// TERM-43 (decision 305): the FSK modem test while it runs.
    #[cfg(feature = "bench")]
    fsk: Option<fskbench::FskRun>,
    #[cfg(feature = "tx")]
    tx: Tx,
    /// The crossband repeater while it runs (168 + 8 bytes).
    #[cfg(feature = "tx")]
    xb: Option<XbRun>,
}

/// The crossband repeater running: the engine, each side's slot, and the receive path's turn while ready.
#[cfg(feature = "tx")]
struct XbRun {
    rep: Repeater,
    sides: [u8; 2],
    hop: u8,
    hop_t: u32,
}

/// An SI4732 seek in progress: whose (HF mode's, else the broadcast's), when it started and when its tune status is
/// read next.
#[derive(Clone, Copy)]
struct SiSeek {
    hf: bool,
    t0: u32,
    poll_at: u32,
}

/// The seek's tune status is read this often (HF-31).
const SEEK_POLL_MS: u32 = 20;
/// A seek that has found nothing by now is cancelled where it is (HF-31, BCR-18).
const SEEK_MAX_MS: u32 = 15_000;
/// A plain SI4732 tune, its power-up's ready: given up after this long (stock's tunes complete in under 100 ms).
const SI_TUNE_MAX_MS: u64 = 500;
/// BCR-7 (decision 290): the SI4732's own volume, 56 of 63. Its audio reaches PA2, the receive audio the DSP task
/// captures, ahead of the analog knob; at stock's 63 it clipped there (bench, Oct 25, 2026: 61 still clipped, 59 at
/// the edge, 57 clean), so 56 leaves a step of margin. A deliberate deviation from stock: broadcast's loudest level
/// on the speaker is about 7 dB lower; the knob makes up for it.
const SI_VOLUME: u8 = 56;

/// The band scope sweeping (SCOPE-4, SCOPE-5): one chip, its audio off, one point per wait. As stock (V0.27
/// 0x08023808 / 0x080235E8): the frequency (REG_38/39), REG_30 0x0200 then 0xBFF1 (the VCO recalibration), the
/// dwell, then REG_67. Stock dwells about 10 ms a point (its 10 ms tick); the dwell is 0 3 1 3's.
struct ScopeRun {
    chip: u8,
    centre: Freq10,
    step: Freq10,
    n: u8,
    dwell_us: u32,
    /// The point measured at the next wait (tuned already), and the raw level of each point last sweep.
    i: u8,
    last: [u8; ferrite_core::msg::SCOPE_BARS],
    sweeps: u8,
    started: u32,
    /// Listening on a frequency (SCOPE-11) instead of sweeping; its demod (0 FM, 1 NFM, 2 AM).
    listen: Freq10,
    mode: u8,
    /// Home's strip (MODE-5, decision 115): the slot whose own frequency it looks at between points, and holds on
    /// while a carrier is there (`listen` set to it); the waits left on that frequency in a look, the points
    /// since the last look, and the closed polls in a row while held.
    home: Option<u8>,
    look: u8,
    since: u8,
    closed: u8,
}

/// MODE-5: the strip looks at its slot's own frequency every this many points (8 x 10 ms at the strip's dwell)...
const LOOK_EVERY: u8 = 8;
/// ...for seven dwells (70 ms, stock's watch dwell, `analysis/watch.md` §2), the chip's open delay at 0 (SQL-17).
/// Two (20 ms, stock's settle alone) never heard a carrier on the radio, open delay 0 or not (Oct 15, 2026):
/// the squelch needs some 40 ms after a retune (decision 166).
const LOOK_WAITS: u8 = 7;
/// The hold ends after this many closed polls in a row: stock's 100 ms receive-end debounce at the 10 ms tick.
const HOLD_CLOSED: u8 = 10;

/// The frequency counter. With both chips, both measure at once with both band receive selects on (TSRCH-10.3,
/// SAFE-24): `pair` keeps each chip's readings. Otherwise (one chip, or the selects refused) they take turns:
/// which chip measures now, when its scan started, and its last reading (stock keeps a reading only when the next
/// one agrees, `tonegen::counter_agree`).
#[derive(Clone, Copy)]
struct Counter {
    /// Both chips at once (TSRCH-10.3).
    both: bool,
    /// The counter set the both-selects hold, so it clears it when it closes (not when the bench test had it on).
    held: bool,
    chip: u8,
    since: u32,
    last: Option<Freq10>,
    pair: tonegen::CounterPair,
}

/// The BK4829's SAME receiver (DEC-19): on the chip of a slot tuned to a weather channel, the burst being put
/// together, and the headers already raised. 280 bytes.
struct SameRx {
    chip: Option<u8>,
    /// The slot and frequency it was set up for: a retune sets it up again.
    at: Option<(u8, Freq10)>,
    /// The modem is set up and listening for the sync word.
    armed: bool,
    asm: Assembler,
    repeats: Repeats,
}

use radio_chip::{COUNTER_TUNE, WX};

/// A chip gives up measuring after this long with no carrier, and the other chip takes a turn (stock's scan time
/// is 0.8 s per reading).
const COUNTER_TURN_MS: u32 = 2_000;

/// The `tx` build's transmit state.
#[cfg(feature = "tx")]
struct Tx {
    seq: TxSeq,
    /// From the UI task once it has read the calibration (`TX_CAL`); None until then, or when blank.
    cal: Option<TxCal>,
    gauge: Gauge,
    /// Each slot's transmit tone word and power level (index 3: HF mode's).
    tone: [u16; 4],
    power: [u8; 4],
    /// The UI's [`RadioCmd::TxPlan`] for the next key-up: frequency, region, Extend TX, the emergency prompt's
    /// second PTT given.
    plan: Option<(Freq10, Region, bool, bool)>,
    /// The key-up now is a data one ([`RadioCmd::TransmitData`]: APRS) at this power level: no end tone or CW ID
    /// after it (TONE-11), the APRS power (0 5 6 3) instead of the slot's.
    data: Option<u8>,
    /// TX-42: every PTT input has read released since boot.
    ptt: ferrite_radio::ptt::PttGuard,
    /// The calibrated PA drive for the key-up being run (`Act::PaRamp`).
    pa: u8,
    /// The read-back failed during a key-up (TX-7): the plan stops and the safe state follows.
    fault: bool,
    /// Each chip's REG_3B as read after its start-up (TX-7: Ferrite never writes it, so a key-up expects it unchanged).
    reg3b: [u16; 2],
    adc: bool,
    /// Each slot's end tone (TONE-10: an index into `tonegen::END_TONES`, 0 off) and transmit tail elimination
    /// (TONE-5), the channel's over the radio's as the UI resolved them; the slot keyed now (index 3: HF mode).
    end: [u8; 4],
    tail: [bool; 4],
    slot: u8,
    /// 0 8 8 3's phase (0 55 Hz, 1-3 the CTCSS phase shifts), 0 8 6 2's CW ID (0 off, 1 every 10 min, 2 on unkey)
    /// with the callsign (0 8 6 1; empty: no ID, VOX-10), 0 8 5 4's DTMF sidetone (DTMF-8).
    phase: u8,
    cw: u8,
    cw_call: [u8; 10],
    cw_len: u8,
    sidetone: bool,
    /// The chip keyed now: the tones at unkey play on it after the path machine has planned the unkey.
    chip: Option<u8>,
    /// VOX (VOX-2, VOX-5, VOX-14): level 1-9 or 0 off, tail in 100 ms, the detector, and when PA0 was last read.
    vox: (u8, u8),
    vox_det: ferrite_core::vox::Vox,
    vox_at: u32,
    /// VOX-3: the UI's screen lets VOX sample (home); false until the UI says so.
    vox_screen: bool,
    /// The region and Extend TX from the UI's last [`RadioCmd::TxPlan`]: the crossband repeater's key-ups use them.
    region: Option<(Region, bool)>,
    /// DTMF-3: each digit's on and off time (ms) and the first digit delay (10 ms); a dialled sequence, and whether
    /// this transmission is one (no end tone or CW ID after it: TONE-11).
    dtmf: (u8, u8),
    digits: [u8; 16],
    dialled: bool,
    /// DTMF-1: the sequence being dialled ([`Radio::dial_step`]).
    dial: Option<Dial>,
    /// Mic gain 1–12 (0 8 3, VOX-15): REG_7D at each voice key-up.
    mic: u8,
    /// TX-30: 0 8 1's timeout (s); the sequencer's backstop is [`TX_BACKSTOP_S`] after it.
    timeout_s: u16,
    /// The keyed slot's bandwidth: stock keys through REG_30 0x0200 when wide (TX audit F3).
    narrow: bool,
    /// TERM-33: the test signal armed for the next PTT key-up (a bench build's, TERM-38); while one plays, or an
    /// APRS burst (every build), the chip whose registers the `flat` or `data` set-up changed (and what they were),
    /// and whether the DSP task is playing a test.
    #[cfg(feature = "bench")]
    test: Option<ferrite_core::txtest::Arm>,
    test_regs: Option<(u8, radio_chip::TestRegs)>,
    /// TERM-33.15, TERM-33.16: what `bypass` and an equaliser changed on the keyed chip (and what was there).
    #[cfg(feature = "bench")]
    test_extra: Option<(u8, radio_chip::ExtraRegs)>,
    #[cfg(feature = "bench")]
    test_playing: bool,
    /// TERM-33 `afsk-bk` while it plays (`afskbk`).
    #[cfg(feature = "bench")]
    bk: Option<afskbk::BkRun>,
    /// TERM-34 `txtest suite` while it runs (`suite`).
    #[cfg(feature = "bench")]
    suite: Option<suite::SuiteRun>,
}

/// DTMF-1: the dialled digit (0-15) of `len`, whether its tone is on, and when the next step is due (ms).
#[cfg(feature = "tx")]
#[derive(Clone, Copy)]
struct Dial {
    k: u8,
    len: u8,
    on: bool,
    at: u32,
}

/// What woke the main loop for a transmit test (TERM-33, TERM-34; only a `tx` bench build has either).
#[cfg_attr(not(all(feature = "tx", feature = "bench")), allow(dead_code))]
enum TestEv {
    /// `afsk-bk`'s timer made a tone change.
    Edge,
    /// The suite's next step is due.
    Step,
}

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// Bring-up log: a tag and a number, as plain bytes (no formatter: this task stays in the fast flash).
fn log(tag: &[u8], v: u32) {
    let mut b = [0u8; 48];
    let n = tag.len().min(36);
    b[..n].copy_from_slice(&tag[..n]);
    let mut d = [0u8; 10];
    let mut k = 0;
    let mut x = v;
    loop {
        d[k] = b'0' + (x % 10) as u8;
        k += 1;
        x /= 10;
        if x == 0 || k == 10 {
            break;
        }
    }
    let mut i = n;
    while k > 0 && i < b.len() {
        k -= 1;
        b[i] = d[k];
        i += 1;
    }
    crate::bringup::line(&b[..i]);
}

/// The scope's own index in [`Radio::tuned`] (after the counter's).
const SCOPE_SLOT: u8 = 5;

/// The counter's own index in [`Radio::tuned`] (after the three slots and HF mode's).
const COUNTER_SLOT: u8 = 4;

/// The SI4732's input path for its mode, as stock (BCR-20, `analysis/audio_path.md` §2): FM broadcast PA7 = PC5 = 0,
/// AM (and SW, SSB, CW) PA7 = PC5 = 1, the LNA path.
fn si_input(mode: SiMode) {
    let [a, b] = radio_chip::si_input(mode != SiMode::Fm);
    SI_IN[0].write(a);
    SI_IN[1].write(b);
}

/// The boot check of the SI4732: reset pulse, FM power-up, ready within 500 ms, its revision read, then powered
/// down and held in reset. True when it answered; its revision goes to `SI_REV` for About.
async fn si_probe(si: &mut Si4732<SiBus>) -> bool {
    let d = &mut BusyDelay;
    SI_RST.write(false);
    Timer::after_millis(10).await;
    SI_RST.write(true);
    Timer::after_millis(10).await;
    let mut ok = si.power_up_start(SiMode::Fm).is_ok();
    if ok {
        let t0 = now_ms();
        ok = loop {
            match si.ready() {
                Ok(true) => break true,
                Ok(false) if now_ms() - t0 < 500 => Timer::after_millis(5).await,
                _ => break false,
            }
        };
    }
    if ok {
        ok = match si.revision(d) {
            Ok(r) => {
                // SELF-5: About shows it
                for (a, v) in crate::tasks::SI_REV.iter().zip([r.fw[0], r.fw[1], r.cmp[0], r.cmp[1], r.chip]) {
                    a.store(v, core::sync::atomic::Ordering::Relaxed);
                }
                true
            }
            Err(_) => false,
        };
    }
    let _ = si.power_down(d); // also clears the driver's mode when the chip didn't answer
    SI_RST.write(false);
    ok
}

/// The broadcast receiver: powered up in the mode asked for (reset pulse, power-up, crystal settling), then
/// tuned by polling. FM in 10 kHz units, AM in kHz. PA7/PC5 take stock's state for the mode (BCR-20). SSB (patch from SPI flash) comes with the link's patch import.
/// FM broadcast while on: the tuned frequency (10 kHz), the RDS decoder, the next report time.
struct FmRx {
    freq: u16,
    rds: si4732::rds::Rds,
    poll_at: u32,
    /// RDS is read on every other report (5 times a second).
    rds_turn: bool,
}

/// Returns the frequency the receiver tuned to (FM 10 kHz units, AM kHz; 0 when switched off).
async fn broadcast(si: &mut Si4732<SiBus>, cmd: BroadcastCmd, opts: SiOpts) -> Result<u16, si4732::Error> {
    let d = &mut BusyDelay;
    let (mode, freq) = match cmd {
        BroadcastCmd::Off => {
            if si.mode().is_some() {
                si.power_down(d)?;
                SI_RST.write(false); // PWR-23: held in reset while off, as after the boot probe
                // BCR-20: leaving broadcast puts stock's radio state, PA7 = 1, PC5 = 0 (it stayed 0/0 after FM)
                SI_IN[0].write(true);
                SI_IN[1].write(false);
            }
            return Ok(0);
        }
        BroadcastCmd::Fm(f) => (SiMode::Fm, Some(f)),
        BroadcastCmd::Am(f) => (SiMode::Am, Some(f)),
        // MODE-7: only started here; the tick polls it (`Radio::seek_poll`).
        BroadcastCmd::Seek { up } => {
            si.seek_start(up, true, d)?; // BCR-18: the broadcast band wraps round
            return Ok(0);
        }
    };
    if si.mode() != Some(mode) {
        if si.mode().is_some() {
            si.power_down(d)?;
        }
        si_input(mode);
        SI_RST.write(false);
        Timer::after_millis(10).await;
        SI_RST.write(true);
        Timer::after_millis(10).await;
        si.power_up_start(mode)?;
        let t0 = now_ms();
        while !si.ready()? {
            if now_ms() - t0 > SI_TUNE_MAX_MS {
                return Err(si4732::Error::Timeout);
            }
            Timer::after_millis(5).await;
        }
        Timer::after_millis(500).await; // crystal
        si.volume(SI_VOLUME, d)?; // BCR-7: the volume knob is analog, after the chip
        if mode == SiMode::Fm {
            let _ = si.rds_on(d); // BCR-2: station name and radio text
        }
        si_apply(si, opts)?;
    }
    if let Some(f) = freq {
        si.tune_start(f, Sideband::Usb, d)?;
    }
    // MODE-7: a plain tune completes in tens of ms; never longer than SI_TUNE_MAX_MS here.
    let t0 = now_ms();
    loop {
        if let Some(t) = si.tune_poll(d)? {
            return Ok(t.freq);
        }
        if now_ms() - t0 > SI_TUNE_MAX_MS {
            return Err(si4732::Error::Timeout);
        }
        Timer::after_millis(10).await;
    }
}

/// BCR-32, HF-35 (decision 292): the receive settings for the mode the SI4732 runs in. FM: de-emphasis by region
/// (50 us outside the US and Canada), the channel filter automatic (the Si4732's default forces it wide), the seek
/// band and spacing by region and step, the AGC or a fixed gain (0 2 6 2). AM: HF mode's bandwidth and AGC, and
/// stock's AVC max gain (0x7800) with soft mute off (chapter 20a).
fn si_apply(si: &mut Si4732<SiBus>, o: SiOpts) -> Result<(), si4732::Error> {
    use si4732::prop;
    let d = &mut BusyDelay;
    match si.mode() {
        Some(SiMode::Fm) => {
            si.set_property(prop::FM_DEEMPHASIS, if o.flags & SiOpts::DEEMPH_50 != 0 { 1 } else { 2 }, d)?;
            si.set_property(prop::FM_CHANNEL_FILTER, 0, d)?;
            let (lo, hi) = o.fm_band();
            si.set_property(prop::FM_SEEK_BAND_BOTTOM, lo, d)?;
            si.set_property(prop::FM_SEEK_BAND_TOP, hi, d)?;
            si.set_property(prop::FM_SEEK_FREQ_SPACING, o.fm_space(), d)?;
            si.agc(SiOpts::agc_index(o.fm_agc, 26), d)
        }
        // HF-19: SSB's bandwidth from the same filter setting; then the AM settings stock also gives SSB
        Some(SiMode::Ssb) => {
            let cfg = si4732::SsbConfig { audio_bw: ferrite_core::ssb::audio_bw(o.am_filter), sideband_cutoff: 1, avc_divider: 0, avc: true, soft_mute_snr: false, afc_off: true };
            si.set_property(prop::SSB_MODE, cfg.bits(), d)?;
            si.set_property(prop::AM_AVC_MAX_GAIN, 0x7800, d)?;
            si.set_property(prop::AM_SOFT_MUTE_MAX_ATTENUATION, 0, d)?;
            si.agc(SiOpts::agc_index(o.am_agc, 37), d)
        }
        Some(_) => {
            si.set_property(prop::AM_CHANNEL_FILTER, o.am_filter.min(3) as u16, d)?;
            si.set_property(prop::AM_AVC_MAX_GAIN, 0x7800, d)?;
            si.set_property(prop::AM_SOFT_MUTE_MAX_ATTENUATION, 0, d)?;
            si.agc(SiOpts::agc_index(o.am_agc, 37), d)
        }
        None => Ok(()),
    }
}

/// Each chip: find the read alignment from its ID, then stock's start-up. Once at power-on; not inlined, so
/// tools/slowcheck.py can tell its register tables (unnamed constants, in the slow flash) from the hot ones.
#[inline(never)]
fn start_chips(r: &mut Radio) {
    for n in 0..2u8 {
        let found = r.wire.detect(&mut r.cs[n as usize]).is_some();
        if found {
            r.chips |= 1 << n;
            r.chip(n).init();
            #[cfg(feature = "tx")]
            {
                r.tx.reg3b[n as usize] = r.chip(n).read(0x3B); // TX-7's reference
            }
            r.chip(n).sleep(); // until the engines give it a slot
        }
        check(Source::Radio, if n == 0 { CHECK_BK0 } else { CHECK_BK1 }, found);
    }
    // PWR-26.12: found and set up at stock's speed; then Ferrite's, kept only if every chip reads its ID right at
    // it eight times running.
    use bk4829::wire::{HALF_US, ID, READ_NS, WRITE_NS};
    r.wire.set_speeds(READ_NS, WRITE_NS);
    let chips = r.chips;
    let ok = (0..2u8).filter(|n| chips & 1 << n != 0).all(|n| (0..8).all(|_| r.chip(n).read(0) == ID));
    if !ok {
        r.wire.set_speeds(HALF_US * 1000, HALF_US * 1000);
    }
}

#[embassy_executor::task]
pub async fn task() {
    for p in AMP_EN {
        p.output(false, Mode::OutPp);
    }
    let bus = I2c { scl: SI_SCL, sda: SI_SDA, half_us: 5 }; // about 50 kHz
    bus.init();
    let mut r = Radio {
        si: Si4732::new(SiBus(bus), SI4732_ADDR),
        wire: ThreeWire::new(Out(BK_SCK), Io(BK_SDA), BusyDelay),
        cs: [Out(BK_CS[0]), Out(BK_CS[1])],
        shadow: Default::default(),
        chips: 0,
        live: LiveAll::default(),
        save: RxSleep::new(DEFAULT_SAVE, now_ms()),
        temp: (None, None),
        lease: ferrite_core::lease::Lease::new(),
        bat_at: None,
        sqcal: ferrite_core::sqcal::SqCal::DEFAULT,
        sq_regs: [None; 2],
        rf_gain: 0,
        rf_gain_pending: 0,
        soft_sq: [ferrite_core::softsq::SoftSq::DEFAULT; 2],
        aprs_sq: [ferrite_core::aprssq::AprsSq::NEW; 2],
        aprs_open_sent: false,
        ssb_at: None,
        swap: 0,
        sq_key: [None; 2],
        open_fast: [None; 2],
        data_rx: [None; 2],
        path: Path::new(),
        muted: false,
        si_used: false,
        si_heard: false,
        bturn: watch::BcastTurn::new(),
        bcast_slot: None,
        fm: None,
        si_opts: SiOpts::default(),
        slots: [SlotRx::DEFAULT; SLOTS],
        active: 0,
        watch_set: 0,
        watcher: Watch::new(),
        hop: Hop::new(),
        scans: [Scan::new(); SLOTS],
        ranges: [scan::band_range(DEFAULT); SLOTS],
        chans: scan::ChanScans::new(),
        tuned: [None; 2],
        maybe_awake: 0,
        settle: Settle::DEFAULT,
        #[cfg(feature = "bench")]
        turns: ferrite_core::watch::TurnClock::new(),
        woke: [None; 2],
        ldo_kept: 0,
        keep: 0,
        aprs_slots: 0,
        floor: [-125; 2],
        monitor: 0,
        user_mute: 0,
        user_off: 0,
        hints: [Hint::new(); SLOTS],
        scan_tx: false,
        tone_level: 1,
        beep_gain: tonegen::beep_gain(3), // Mid
        beep_level: 7,
        hf: None,
        sat: None,
        seek: None,
        counter: None,
        tsearch: None,
        scope: None,
        sweep_hold: SweepHold::new(),
        scope_cfg: (60, 100, false),
        same_on: false,
        dtmf_on: false,
        dtmf_key: [None; 2],
        dtmf: [ferrite_core::dtmfrx::Track::NEW; SLOTS],
        same: SameRx { chip: None, at: None, armed: false, asm: Assembler::new(), repeats: Repeats::new() },
        #[cfg(feature = "bench")]
        rxaf: None,
        #[cfg(feature = "bench")]
        rxif: None,
        #[cfg(feature = "bench")]
        holds: [(0, 0); ferrite_core::msg::HOLDS],
        #[cfg(feature = "bench")]
        fsk: None,
        #[cfg(feature = "tx")]
        tx: Tx {
            seq: TxSeq::new(),
            cal: None,
            gauge: Gauge::default(),
            tone: [0; 4],
            power: [0; 4],
            plan: None,
            data: None,
            ptt: ferrite_radio::ptt::PttGuard::default(),
            pa: 0,
            fault: false,
            reg3b: [0; 2],
            adc: false,
            end: [0; 4],
            tail: [false; 4],
            slot: 0,
            phase: 0,
            cw: 0,
            cw_call: [0; 10],
            cw_len: 0,
            sidetone: true,
            dtmf: (100, 30),
            digits: [0; 16],
            dialled: false,
            dial: None,
            chip: None,
            vox: (0, 20),
            vox_det: ferrite_core::vox::Vox::new(),
            vox_at: 0,
            vox_screen: false,
            region: None,
            mic: 5,
            timeout_s: 120,
            narrow: false,
            #[cfg(feature = "bench")]
            test: None,
            test_regs: None,
            #[cfg(feature = "bench")]
            test_extra: None,
            #[cfg(feature = "bench")]
            test_playing: false,
            #[cfg(feature = "bench")]
            bk: None,
            #[cfg(feature = "bench")]
            suite: None,
        },
        #[cfg(feature = "tx")]
        xb: None,
    };
    // Start from the safe state: every path low, the relay released (board start-up set them low already).
    let mut s = Steps::default();
    r.path.safe(now_ms() as u32, &mut s);
    r.run(&s).await;
    start_chips(&mut r);
    r.live.chips = r.chips;
    for (i, sl) in r.live.slots.iter_mut().enumerate() {
        (sl.freq, sl.rssi) = (r.slots[i].freq, -121);
    }
    // SELF-*: the SI4732 at boot: out of reset (PA6), FM power-up, answering within 500 ms, then powered down and
    // held in reset again until FM broadcast or HF mode needs it. Silent (no audio path changes).
    let si_ok = si_probe(&mut r.si).await;
    check(Source::Radio, crate::tasks::CHECK_SI, si_ok);
    r.select_rx(DEFAULT, Cause::ActiveSlot).await;
    r.tick().await;
    let live = LIVE.sender();
    live.send(r.live);
    // MODE-7: when the tick last ran; its next time counts from here, not from the last command, so a stream of
    // commands never holds it off (the crossband repeater, watch and scans run on it).
    let mut ticked = now_ms();
    // When the last tick began: a scan step's due time at or before it was one that tick didn't move (decision 165).
    let mut started = ticked;

    loop {
        // Wake for the next command, the next Battery Save phase change, or (awake) the next tick. HF mode's tick
        // never steps Battery Save, so its deadline isn't waited on there (decision 163: a deadline the tick doesn't
        // move goes past and the wait fires every 1 ms, starving the UI and the link).
        let mut at = r.save.deadline().filter(|_| r.hf.is_none()).unwrap_or(u64::MAX); // satellite mode steps it (busy)
        // Broadcast FM's report (fm_poll) runs on the tick, also while Battery Save has the BK4829s asleep (BCR-19).
        // SCAN-21: a fast scan step's end ticks on time too (on the 10 ms tick alone a step waited for the next), but
        // only a due time still ahead of the last tick's start, and never within MIN_GAP_MS of its end
        // (`scan::tick_wake`, decision 165: a due time the tick didn't move woke it every 1 ms and hung the radio).
        let tick_at = (!r.save.asleep() || r.kept() != 0 || r.hf.is_some() || r.sat.is_some() || r.fm.is_some() || r.seek.is_some()).then(|| {
            let period = if r.busy() { TICK_MS } else { POLL_MS };
            crate::tasks::dsp::TICK_PERIOD_MS.store(period as u32, core::sync::atomic::Ordering::Relaxed); // APRS-56.4
            ferrite_core::scan::tick_wake(started, ticked, period, r.scan_due(now_ms()))
        });
        if let Some(t) = tick_at {
            at = at.min(t);
        }
        // MODE-3: a held exclusive mode is checked by its lease's end at the latest.
        r.lease_check().await;
        if let Some(d) = r.lease.deadline() {
            at = at.min(d as u64 + 1);
        }
        // DTMF-1: the dial's next step on time.
        #[cfg(feature = "tx")]
        if let Some(d) = r.dial_due() {
            let left = d.wrapping_sub(now_ms() as u32);
            at = at.min(now_ms() + if left > u32::MAX / 2 { 0 } else { left as u64 });
        }
        // Never wait on a time already past: that wait fires at once, and this task runs above the UI.
        at = at.max(now_ms() + 1);
        // The band scope waits its dwell between points (0.1 ms steps), listening or not at the tick.
        let wake = match &r.scope {
            // At least 2 ms between points whatever the dwell: each point is about 1.25 ms of BK4829 writes on a task
            // that runs above the UI, and 1 ms points starved it (keys dead on the radio, Oct 12, 2026).
            // SCOPE-14: held after a key or knob step: the next point once the hold is over.
            Some(sc) if sc.listen == 0 => match r.sweep_hold.deadline() {
                Some(d) => {
                    let left = d.wrapping_sub(now_ms() as u32); // past the end: wraps above HOLD_MS
                    Instant::now() + embassy_time::Duration::from_millis(if left <= sweephold::HOLD_MS { left as u64 } else { 0 })
                }
                None => Instant::now() + embassy_time::Duration::from_micros(sc.dwell_us.max(2_000) as u64),
            },
            _ => Instant::from_millis(at),
        };
        // TERM-33 `afsk-bk`: TMR5's tone changes, waited on only while one plays (`afskbk`); TERM-34: the suite's
        // next step at its time (`suite`). Each handled at once, with nothing else after it (no snapshot: nothing
        // the UI sees changes).
        #[cfg(all(feature = "tx", feature = "bench"))]
        let (bk_on, suite_at) = (r.afsk_bk_on(), r.suite_due());
        #[cfg(all(feature = "tx", feature = "bench"))]
        let test = async {
            let edge = async {
                if bk_on {
                    afskbk::EDGE.wait().await
                } else {
                    core::future::pending().await
                }
            };
            match suite_at {
                Some(at) => match embassy_futures::select::select(edge, Timer::at(Instant::from_millis(at))).await {
                    embassy_futures::select::Either::First(()) => TestEv::Edge,
                    embassy_futures::select::Either::Second(()) => TestEv::Step,
                },
                None => {
                    edge.await;
                    TestEv::Edge
                }
            }
        };
        #[cfg(not(all(feature = "tx", feature = "bench")))]
        let test = core::future::pending::<TestEv>();
        // The terminal's requests to this task are all bench commands (TERM-38): a user build never waits on them.
        #[cfg(feature = "bench")]
        let debug = DEBUG_REQ.receive();
        #[cfg(not(feature = "bench"))]
        let debug = core::future::pending::<core::convert::Infallible>();
        match select4(RADIO.receive(), Timer::at(wake), debug, test).await {
            Either4::Fourth(_ev) => {
                #[cfg(all(feature = "tx", feature = "bench"))]
                match _ev {
                    TestEv::Edge => r.afsk_bk_edge(),
                    TestEv::Step => r.suite_next(),
                }
                continue;
            }
            Either4::First(c) => {
                r.cmd(c).await;
                // MODE-7: commands are taken first, but the tick still runs on time between them.
                if tick_at.is_some_and(|t| now_ms() >= t) {
                    started = now_ms();
                    r.tick().await;
                    ticked = now_ms();
                }
            }
            Either4::Second(()) => {
                started = now_ms();
                r.tick().await;
                ticked = now_ms();
            }
            #[cfg(not(feature = "bench"))]
            Either4::Third(never) => match never {},
            #[cfg(feature = "bench")]
            Either4::Third(d) => {
                let a = match d {
                    DebugReq::RxBoth(on) => r.rx_both(on).await,
                    // SAFE-8.1: the bench's restart test; neither returns
                    DebugReq::Crash(true) => cortex_m::asm::udf(),
                    DebugReq::Crash(false) => loop {
                        core::hint::spin_loop();
                    },
                    // TERM-41 (the power bench): Battery Save's cycle overridden or back to the level; activity.
                    DebugReq::Save(set) => {
                        if let Some(c) = set {
                            let a = r.save.set_cycle(c, now_ms());
                            r.apply(a).await;
                        }
                        DebugAns::Save { cycle: r.save.cycle(), level: ferrite_core::rxsleep::timing(r.save.level()), asleep: r.save.asleep() }
                    }
                    d => r.debug(d),
                };
                DEBUG_ANS.send(a).await;
                TERM_WAKE.signal(()); // the UI task runs the terminal
                continue;
            }
        }
        let snap = r.live;
        // The GPS task's view (GPS-4, GPS-22): keyed, and any slot receiving.
        let rx = snap.slots.iter().any(|s| s.squelch_open);
        let rf = if snap.tx.keyed { crate::chan::RF_BUSY_KEYED } else { 0 } | if rx { crate::chan::RF_BUSY_RX } else { 0 };
        crate::chan::RF_STATE.store(rf, core::sync::atomic::Ordering::Relaxed);
        // TX-43: the cable's transmit interrupt (decision 290) stops while keyed, as the Link task's own check did.
        let dry = crate::chan::DRY_TX.load(core::sync::atomic::Ordering::Relaxed);
        crate::hal::uart::tx_hold(crate::report::UART_CPS, crate::hal::uart::HOLD_KEYED, snap.tx.keyed && !dry);
        // LINK-13.4, decision 290: what the receive audio carries, for the DSP task: the first slot heard, else the
        // broadcast while it plays (its audio reaches PA2 too), else HF mode's.
        let open = core::array::from_fn(|i| snap.slots[i].squelch_open);
        crate::chan::heard_now(open, r.bcast_slot.filter(|_| r.si_heard), r.hf.is_some());
        live.send_if_modified(|v| {
            let changed = *v != Some(snap);
            if changed {
                *v = Some(snap);
            }
            changed
        });
    }
}

