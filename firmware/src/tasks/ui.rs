//! UI: the only task that draws. The boot screen with the self-test as the other tasks report it, then the app
//! (`ferrite-app`, the same state machine the simulator runs): keys and the knob in, the Radio task's snapshot
//! in, its commands out to the Radio task, and the screen redrawn where it changed.

use crate::board::{LED_ACTIVE_HIGH, LED_GREEN, LED_RED, POWER_HOLD, POWER_SW};
use crate::hal::backlight;
use crate::bringup;
use crate::chan::{Input, ALERTS, BT_IN, BT_LINK_ON, BT_ON, BT_OUT, CABLE_BAUD, INPUT, LINK_BAUD, LINK_HELLOS, LINK_ON, LIVE, RADIO, TERM_IN, TERM_OUT, TERM_WAKE};
#[cfg(feature = "bench")]
use crate::chan::{DEBUG_ANS, DEBUG_REQ};
use crate::display::Screen;
use crate::store;
use crate::tasks::{check, CHECK_CAL, CHECK_FLASH, CHECK_MCU, CHECK_STATE, FAILED, PASSED, UNTESTED};
use core::sync::atomic::Ordering::Relaxed;
use embassy_futures::select::{select, select4, Either, Either4};
use embassy_time::{Duration, Instant, Timer};
use ferrite_app::ports::{Port, Ports};
use ferrite_app::term::Sink;
use ferrite_app::App;
use ferrite_core::chipid;
use ferrite_core::keys;
use ferrite_core::msg::{Device, LiveAll, RadioCmd, Source};
use ferrite_core::VERSION;
use ferrite_ui::boot::{self, BootInfo, Check, State};
use ferrite_ui::gfx::BG;

/// How often the app is ticked: often while a key is held (long presses fire at 700 ms, repeats every 100 ms:
/// KEY-3, KEY-4), rarely otherwise (toasts, undo, timeouts, the clock), so the MCU sleeps.
const TICK_HELD_MS: u64 = 20;
const TICK_IDLE_MS: u64 = 250;
/// PWR-26.15: the idle tick while the screen is dark (nothing on it to update), and the housekeeping's turn (battery,
/// APRS, decoders, GPS, autosave, an idle link) at any speed.
const TICK_DARK_MS: u64 = 1000;
const HOUSE_MS: u32 = 1000;
/// PWR-26.1: how long after the panel wakes the build's speed holds for its whole redraw.
const WAKE_FAST_MS: u32 = 1_000;

/// A port's output for the terminal and the link: its transmit ring, emptied by the Link task (the cable's
/// `TERM_OUT`, Bluetooth's `BT_OUT`). `Out(ring, false)`: held (the cable while its log has it, TERM-28): no
/// room, so nothing is written and the cable's terminal and session wait.
struct Out(&'static crate::ring::Ring<'static>, bool);

impl Sink for Out {
    fn room(&self) -> usize {
        if !self.1 {
            return 0;
        }
        self.0.room() as usize
    }
    /// One whole frame (or a terminal's bytes). Into `TERM_OUT` with [`crate::chan::TERM_OUT_LOCK`] held for the copy
    /// only (decision 331): the DSP task's audio frames go in between, never while the session works out an answer
    /// or the Live View draws.
    fn put(&mut self, b: &[u8]) {
        let r = self.0;
        let copy = || {
            for &c in b {
                r.put(c);
            }
        };
        if core::ptr::eq(r, &TERM_OUT) {
            crate::chan::term_out_locked(copy);
        } else {
            copy();
        }
    }
}

/// The milliseconds the app counts in (wrapping).
/// BOOT-18: the boot animation's frame period, ms (30 a second).
const FRAME_MS: u64 = 33;

fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

/// MENU pressed or released: whether it is held, for the settings reset at power on (BOOT-14).
fn menu_state(i: &Input, held: &mut bool) {
    if let Input::Key(keypad::Event::Press(c) | keypad::Event::Release(c)) = *i {
        if keys::key(c) == Some(ferrite_core::keys::Key::Menu) {
            *held = matches!(i, Input::Key(keypad::Event::Press(_)));
        }
    }
}

/// TERM-27: a `bt` query in progress (a bench build's, TERM-38). The link task keeps USART1 and PB9; this only borrows the module's rings
/// (`BT_OUT`, `BT_IN`) for the query, and holds 0 9 2 1 on for it when Bluetooth is off.
#[cfg(feature = "bench")]
struct BtQuery {
    text: [u8; 16],
    n: u8,
    /// Bluetooth was off: powered for the query, off again after.
    forced: bool,
    sent: bool,
    at: Instant,
    buf: [u8; 48],
    got: u8,
}

/// MODE-6: the App's commands to the Radio task, as many as its channel has room for, without waiting; the one
/// refused is kept in `held` and goes first next time (the Radio task ignores what it doesn't run yet).
/// TX-48: Dry TX's switch goes to the PA's choke point first, so no key-up reaches the Radio task ahead of it.
fn to_radio(app: &mut App, held: &mut Option<RadioCmd>) {
    crate::chan::DRY_TX.store(app.dry_tx(), core::sync::atomic::Ordering::SeqCst);
    while let Some(c) = held.take().or_else(|| app.pop_cmd()) {
        if RADIO.try_send(c).is_err() {
            *held = Some(c);
            break;
        }
    }
}

/// MODE-6, TX-33: power off and reboot: what is waiting, then Shutdown, given at most 1 s (the Radio task takes a
/// command at least every half second, MODE-7); the power goes regardless.
/// PWR-17, TX-33: a power-off step in the cable log: `power off: saving at 123456 ms`.
fn power_log(what: &str) {
    bringup::linef(|b| {
        let _ = ufmt::uwrite!(b, "power off: {} at {} ms", what, now_ms());
    });
}

async fn send_shutdown(app: &mut App, held: &mut Option<RadioCmd>) {
    let t = Instant::now();
    loop {
        to_radio(app, held);
        if held.is_none() && RADIO.try_send(RadioCmd::Shutdown).is_ok() || t.elapsed() >= Duration::from_secs(1) {
            return;
        }
        Timer::after_millis(5).await;
    }
}

/// The calibration sector's tables for the Radio task and the battery gauge, from one read (read only, chapter
/// 35): the squelch tables (SQL-1, every build), the PA curves, TX windows and battery gauge (the `tx` build; the
/// cable log says why transmit is locked, if it is), and the gauge for the percentage (PWR-22). Its own frame
/// (never inlined), so the sector's copy isn't held in the UI task's frame for the whole power-on (SBAR-23).
/// CAL-20 (issue #40): the transmit calibration's verdict for About and the refusal, as [`cal_code`] encodes a
/// [`ferrite_core::msg::CalState`]. About said "Valid" whenever the sector wasn't blank, while the gate below had
/// locked transmit (a sector changed since install): the owner saw "Ready" and couldn't transmit.
static CAL_VERDICT: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
/// CAL-20.4: the check that failed ([`ferrite_core::msg::CalFault`] as a byte) and where, bytes 1-3.
static CAL_FAULT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// CAL-20.4: the failed check and where, from [`CAL_FAULT`].
fn cal_fault() -> (ferrite_core::msg::CalFault, [u8; 3]) {
    use ferrite_core::msg::CalFault::*;
    let v = CAL_FAULT.load(Relaxed).to_le_bytes();
    let f = match v[0] {
        1 => PaCurve,
        2 => LevelOrder,
        3 => Windows,
        4 => Trim,
        5 => ToneGain,
        6 => Unchecked,
        _ => None,
    };
    (f, [v[1], v[2], v[3]])
}

#[cfg(feature = "tx")]
fn cal_code(c: ferrite_core::msg::CalState) -> u8 {
    use ferrite_core::msg::CalState::*;
    match c {
        Unknown => 0,
        Ok => 1,
        Blank => 2,
        Invalid => 3,
        CrcMismatch => 4,
    }
}

/// The calibration's state for About: the gate's verdict in a transmit build, the boot check's otherwise.
fn cal_state() -> ferrite_core::msg::CalState {
    use ferrite_core::msg::CalState::*;
    if cfg!(feature = "tx") {
        return match CAL_VERDICT.load(Relaxed) {
            1 => Ok,
            2 => Blank,
            3 => Invalid,
            4 => CrcMismatch,
            _ => Unknown,
        };
    }
    if CHECK_STATE[CHECK_CAL as usize].load(Relaxed) == PASSED { Ok } else { Blank }
}

#[inline(never)]
fn read_calibration(store: &store::Store, flash: bool, cal_ok: bool, mcu_ok: bool, cal_crc: ferrite_core::txcal::CalCrc) -> ferrite_core::txcal::Gauge {
    use ferrite_core::sqcal::{self, SqCal};
    use ferrite_core::txcal::{Gauge, READ_LEN};
    const { assert!(sqcal::READ_LEN <= READ_LEN) };
    let mut b = [0xFFu8; READ_LEN];
    let read = flash && ferrite_store::Nor::read(&mut *store.nor(), ferrite_core::layout::CALIBRATION.start, &mut b).is_ok();
    let sq: &[u8; sqcal::READ_LEN] = b[..sqcal::READ_LEN].try_into().unwrap_or(&[0xFF; sqcal::READ_LEN]);
    crate::chan::SQ_CAL.signal(if read { SqCal::parse(sq) } else { SqCal::DEFAULT });
    #[cfg(feature = "tx")]
    {
        use ferrite_core::txcal::{gate, Fault, TxCal};
        // TX-13 to TX-15 (TX audit F8): the MCU check, the CRC since install, then the sector itself.
        let tx = gate(if read && cal_ok { TxCal::read(&b) } else { Err(Fault::Blank) }, mcu_ok, cal_crc);
        if let Err(f) = tx {
            bringup::line(match f {
                Fault::Blank => b"tx calibration: locked, sector blank or unread",
                Fault::PaCurve => b"tx calibration: locked, a PA curve byte is 0x00 or 0xFF",
                Fault::Windows => b"tx calibration: locked, TX windows blank or invalid",
                Fault::Trim => b"tx calibration: locked, factory crystal trim over 20 ppm",
                Fault::ToneGain => b"tx calibration: locked, a sub-tone gain byte is over 0x7F",
                Fault::LevelOrder => b"tx calibration: locked, PA curves not Low < Mid < High",
                Fault::Mcu => b"tx calibration: locked, the MCU isn't an AT32F403AVGT7",
                Fault::Changed => b"tx calibration: locked, the sector changed since recorded (or unchecked)",
            });
        }
        // CAL-20.4: what About shows, the sector's own checks first (a sector failing one and changed too reads
        // Invalid, with which check and where); the MCU shows on its own row.
        use ferrite_core::msg::CalState;
        use ferrite_core::txcal::CalCrc;
        let sector = if read && cal_ok { TxCal::read(&b) } else { Err(Fault::Blank) };
        let (state, mut fault) = match (&sector, cal_crc) {
            (Err(Fault::Blank), _) => (CalState::Blank, 0),
            (Err(Fault::Windows), _) => (CalState::Invalid, 3),
            (Err(Fault::Trim), _) => (CalState::Invalid, 4),
            (Err(Fault::ToneGain), _) => (CalState::Invalid, 5),
            (Err(_), _) => (CalState::Invalid, 0),
            (Ok(_), CalCrc::Changed) => (CalState::CrcMismatch, 0),
            (Ok(_), CalCrc::Unknown) => (CalState::CrcMismatch, 6),
            (Ok(_), CalCrc::Same | CalCrc::Recorded) => (CalState::Ok, 0),
        };
        // CAL-6.1: a curve fault locks its band only: the sector stands (Valid), the fault and where for About
        let mut at = [0; 3];
        if fault == 0 && sector.is_ok() {
            if let Some((f, w)) = TxCal::curve_fault_at(&b) {
                (fault, at) = (if f == Fault::PaCurve { 1 } else { 2 }, w);
            }
        }
        CAL_FAULT.store(u32::from_le_bytes([fault, at[0], at[1], at[2]]), Relaxed);
        CAL_VERDICT.store(cal_code(state), Relaxed);
        crate::chan::TX_CAL.signal((tx.ok(), Gauge::parse(&b)));
    }
    #[cfg(not(feature = "tx"))]
    let _ = (cal_ok, mcu_ok, cal_crc);
    if read { Gauge::parse(&b) } else { Gauge::defaults() }
}

/// SBAR-23: a future polled from a function never inlined, so its locals (a start-up step's buffers) live in that
/// frame while it runs, not in the UI task's frame for the whole power-on.
struct OwnFrame<F>(F);

impl<F: core::future::Future> core::future::Future for OwnFrame<F> {
    type Output = F::Output;
    #[inline(never)]
    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> core::task::Poll<F::Output> {
        // SAFETY: a plain projection to the only field, which is never moved out of the pinned wrapper.
        unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
    }
}

fn own_frame<F: core::future::Future>(f: F) -> OwnFrame<F> {
    OwnFrame(f)
}

/// DISP-27: the ST7789's wait after sleep out before drawing, and between sleep in and sleep out.
const PANEL_SLEEP_MS: u32 = 120;

/// PWR-24.5: the shortest gap between redraws for live changes in Low Power Mode.
const LPM_FRAME_MS: u32 = 1000;

/// PWR-24.9: how long Low Power Mode has been on before the core drops to 24 MHz: the Radio and DSP tasks have
/// stopped what the mode stops (decoders, capture, the second VFO), GPS and Bluetooth are off. Leaving goes back to
/// the build's speed at once.
const LPM_CLOCK_MS: u32 = 1000;

/// PWR-24.10: after a key or the knob, Low Power Mode runs at the build's speed this long, so menus and the knob
/// keep up; then 24 MHz again. The screen going dark ends it at once.
const LPM_BOOST_MS: u32 = 3000;

/// LINK-38: the shortest gap between two redraws driven by live changes alone (20 a second).
const FRAME_MIN_MS: u32 = 50;
/// PWR-26.10: the live-change redraw cap at rest (24 MHz).
const IDLE_FRAME_MS: u32 = 250;
/// PWR-26.1: a key this recent raises the clock before its commands (and beep) go to the Radio task.
const KEY_RAISE_MS: u32 = 300;

/// SELF-9.4 (bench builds): the UI pass's cycles by section (the cable log's `ui pass` line every 10 s).
#[cfg(feature = "bench")]
static PASS: [core::sync::atomic::AtomicU32; 8] = [const { core::sync::atomic::AtomicU32::new(0) }; 8];
/// SELF-9.4 (bench builds): the UI task's wakes by what woke it (input, the Radio task's snapshot, its timer, an
/// alert), counted for the cable log's 10 s line.
#[cfg(feature = "bench")]
static WAKES: [core::sync::atomic::AtomicU32; 4] = [const { core::sync::atomic::AtomicU32::new(0) }; 4];
/// SELF-9.4: the cycles of the UI task's passes (interrupts in them too).
#[cfg(feature = "bench")]
static UI_CYCLES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[embassy_executor::task]
pub async fn task() {
    ferrite_ui::prof::set_clock(crate::hal::time::cycles); // bench builds: the drawing profile
    ferrite_ui::canvas::set_band_hook(crate::hal::clock::raise_for_audio); // decision 360
    let mut lcd = Screen::new();
    // Start-up as awaited steps (up to 150 ms each), then black, then the panel on.
    for &s in st7789::STARTUP {
        if let Some(ms) = lcd.0.step(s) {
            Timer::after_millis(ms as u64).await;
        }
    }
    let c0 = crate::hal::time::cycles();
    lcd.0.fill(0, 0, 240, 320, BG);
    let fill_us = crate::hal::time::cycles().wrapping_sub(c0) / crate::hal::clock::mhz();
    lcd.0.on(true);
    // The screen and keypad lights by PWM (DISP-22, DISP-23), full until the settings are read.
    backlight::init();
    backlight::set(1000);
    backlight::keypad(1000);
    // Bring-up: the panel bus speed, from the whole-screen clear (153,600 bytes).
    bringup::linef(|b| {
        let _ = ufmt::uwrite!(b, "panel: full-screen fill {} us, {} ns a byte", fill_us, fill_us * 1000 / 153_600);
    });

    // The MCU check is ours to make: transmit (in the tx build) needs the expected part (TX-15); receive runs
    // regardless.
    let mcu_ok = crate::hal::sys::idcode() == chipid::EXPECTED_PID;
    check(Source::Ui, CHECK_MCU, mcu_ok);
    let mut checks = [
        Check { label: "MCU", value: "", state: State::Pending },
        Check { label: "SPI flash", value: "", state: State::Pending },
        Check { label: "BK4829 VHF", value: "", state: State::Pending },
        Check { label: "BK4829 UHF", value: "", state: State::Pending },
        Check { label: "SI4732", value: "not tested", state: State::Pending },
        Check { label: "Calibration", value: "", state: State::Pending },
    ];
    // The store: mounted before the boot screen, as the settings (callsign, owner card: BOOT-9, MENU 0 7 3 3)
    // are in it. A first boot formats the chip here (about a second, once). Its writes go to the Storage task.
    // The bring-up build never mounts it: nothing on the chip is taken as ours or written, and the settings
    // are the defaults, in RAM for this power-on.
    let (flash, store) = store::make();
    if !cfg!(feature = "bringup") {
        own_frame(store.mount_async()).await;
        // APRS-4: the heard list from its stored rows (their times unknown until GPS time: the GPS task gives
        // them again then).
        store.heard_rows(ferrite_app::aprs::HEARD_MAX as usize, &mut |s, seq| store.extra.aprs.restore(&s, seq, now_ms() / 1000));
    }
    // The SPI parts (decision 335: the text pack, the logo, the terminal's templates): each checked once here (its
    // build ID and CRC, about 30 KB read), before the boot screen draws the logo.
    if flash.is_some() {
        ferrite_core::part::set_reader(store::part_read);
        let ok = ferrite_core::part::mount();
        bringup::linef(|l| {
            let _ = ufmt::uwrite!(l, "spi parts: {} of {} for this build", ok.count_ones(), ferrite_core::part::KINDS.len());
        });
    }
    check(Source::Storage, CHECK_FLASH, flash.is_some());
    // Calibration present: the factory sector isn't blank (read only, CAL-1). Its CRC check comes with 1b.
    let mut cal = [0u8; 32];
    let cal_ok = flash.is_some()
        && ferrite_store::Nor::read(&mut *store.nor(), ferrite_core::layout::CALIBRATION.start, &mut cal).is_ok()
        && cal.iter().any(|&b| b != 0xFF)
        && cal.iter().any(|&b| b != 0);
    check(Source::Storage, CHECK_CAL, cal_ok);
    // CAL-7: the sector's CRC against the one the layout header recorded (recorded on the first boot that checks).
    let cal_crc = if cal_ok && !cfg!(feature = "bringup") { own_frame(store.check_cal()).await } else { ferrite_core::txcal::CalCrc::Unknown };
    bringup::line(match cal_crc {
        ferrite_core::txcal::CalCrc::Same => b"calibration crc: same as recorded",
        ferrite_core::txcal::CalCrc::Recorded => b"calibration crc: recorded now",
        ferrite_core::txcal::CalCrc::Changed => b"calibration crc: CHANGED since recorded",
        ferrite_core::txcal::CalCrc::Unknown => b"calibration crc: not checked",
    });
    let gauge = read_calibration(store, flash.is_some(), cal_ok, mcu_ok, cal_crc);
    let set = store::settings();
    store.load_settings(set);
    let level = ferrite_app::lights::BRIGHTNESS[set.brightness() as usize - 1]; // 0 7 4 2 from here on
    backlight::set(level);
    backlight::keypad(level);
    // D13: the owner card instead of the logo when 0 7 3 3 picks it and the card has something on it (BOOT-9).
    // The bring-up build never mounts the store, so its settings are the defaults: always the logo there.
    let card = set.boot_owner().is_some();
    // The logo for at least 1.5 s (the owner card 4 s; any key skips it), until every check has reported (at
    // most 5 s: a check still pending then counts as failed).
    let least = Duration::from_millis(if card { 4000 } else { 1500 });
    let (mut menu_held, mut skip) = (false, false);
    // Bring-up: a key pressed during the boot screen opens the key test (the key map isn't trusted yet).
    let mut key_seen = false;
    ferrite_ui::theme::set(set.theme); // the boot screen in the saved theme (0 7 1)
    let start = Instant::now();
    // BOOT-18 (decision 321): the logo screen animates for its first ANIM_MS, about 30 frames a second, redrawing
    // only the logo and the wordmark, the self-test running underneath; the Radio task plays the boot sound timed
    // from the same start. The owner card stays as it is. The logo shows 1.5 s at least anyway, so the animation
    // never makes a boot longer.
    let mut animating = !card;
    if animating {
        let _ = RADIO.try_send(RadioCmd::BootSound { at: start.as_millis() as u32 });
    }
    // The boot screen is drawn whole once, then only its progress as checks finish (BOOT-9; a whole redraw per
    // check flashed the logo on the radio). Whole again only if a failure turns the owner card into the logo.
    let mut shown: Option<(bool, [u8; 8])> = None;
    loop {
        for (c, s) in checks.iter_mut().zip(&CHECK_STATE) {
            c.state = match s.load(Relaxed) {
                PASSED => State::Ok,
                FAILED => State::Fail,
                UNTESTED => State::Off,
                _ => State::Pending,
            };
        }
        // BOOT-9 (decision 215): the screen names a check only once start-up has taken half a second.
        let mut note = ferrite_ui::text::Buf::<31>::new();
        if start.elapsed() >= Duration::from_millis(500) {
            if let Some(c) = checks.iter().find(|c| c.state == State::Pending) {
                let _ = ufmt::uwrite!(note, "Checking {}\u{2026}", c.label);
            }
        }
        let info = BootInfo {
            version: VERSION,
            callsign: set.boot_callsign(),
            line: set.boot_line(),
            owner: set.boot_owner(),
            checks: &checks,
            note: (!note.is_empty()).then_some(note.as_bytes()),
        };
        let mut states = [0u8; 8];
        for (d, c) in states.iter_mut().zip(checks.iter()) {
            *d = c.state as u8 + 1;
        }
        states[7] = !note.is_empty() as u8; // the note's coming and going redraws the line
        let now = (boot::shows_card(&info), states);
        let ms = start.elapsed().as_millis() as u32;
        match shown {
            Some((card_shown, _)) if card_shown == now.0 && shown != Some(now) => boot::update(&mut lcd, &info),
            Some(_) if shown == Some(now) => {}
            _ if animating => boot::draw_at(&mut lcd, &info, ms),
            _ => boot::draw(&mut lcd, &info),
        }
        // BOOT-18: the next frame (the last one, at ANIM_MS or after, is the still screen's logo and wordmark).
        if animating && shown.is_some() {
            boot::frame(&mut lcd, &info, ms);
            animating = ms < boot::ANIM_MS;
        }
        shown = Some(now);
        let done = checks.iter().all(|c| c.state != State::Pending);
        let t = start.elapsed();
        if (done && (t >= least || skip) && !animating) || t >= Duration::from_secs(5) {
            break;
        }
        // A frame every 33 ms while animating, on the start's grid (a slow frame skips ahead, never queues up).
        let wait = if animating {
            let next = (start.elapsed().as_millis() / FRAME_MS + 1) * FRAME_MS;
            Timer::at(start + Duration::from_millis(next))
        } else {
            Timer::after_millis(100)
        };
        if card || cfg!(feature = "bringup") {
            // Keys while the card shows: any press skips the rest of it; MENU's state still counts (BOOT-14).
            if let Either::First(i) = select(INPUT.receive(), wait).await {
                skip |= card && front_key(&i);
                key_seen |= front_key(&i);
                log_input(&i);
                menu_state(&i, &mut menu_held);
            }
        } else {
            wait.await;
        }
    }
    for c in checks.iter_mut().filter(|c| c.state == State::Pending) {
        c.state = State::Fail;
    }
    if checks.iter().any(|c| c.state == State::Fail) {
        // The failed check stays on screen until a key.
        boot::draw(&mut lcd, &BootInfo { version: VERSION, callsign: set.boot_callsign(), line: set.boot_line(), owner: None, checks: &checks, note: None });
        while !matches!(INPUT.receive().await, Input::Key(keypad::Event::Press(_))) {}
    }

    // Keys pressed during the boot screen: only whether MENU is still held counts (BOOT-14); the rest are
    // dropped, as a PTT held at power on is (KEY-8).
    while let Ok(i) = INPUT.try_receive() {
        key_seen |= front_key(&i);
        log_input(&i);
        menu_state(&i, &mut menu_held);
    }
    // BOOT-14.6: MENU still down here may be a tap that skipped the owner card (OK is MENU), its release not in yet:
    // the boot screen ends as soon as its checks are done (ferrite#62: the tap opened the reset prompt). A hold from
    // power on is the reset's: it is still down after a long press's time, so wait that long for a release.
    if menu_held {
        let until = Instant::now() + Duration::from_millis(ferrite_app::keys::LONG_MS as u64);
        while menu_held {
            match select(INPUT.receive(), Timer::at(until)).await {
                Either::First(i) => {
                    key_seen |= front_key(&i);
                    log_input(&i);
                    menu_state(&i, &mut menu_held);
                }
                Either::Second(()) => break,
            }
        }
    }
    boot_report(flash, &checks, key_seen);
    // HF-18: a patch the flasher bundled goes into SPI flash (normal build; once: the same patch again writes
    // nothing). The bring-up build stores nothing and only says whether one is there.
    match (ssb_bundle(), cfg!(feature = "bringup")) {
        (None, _) => bringup::line(b"ssb: no patch bundled with this image"),
        (Some(_), true) => bringup::line(b"ssb: patch bundled; not installed (bring-up build stores nothing)"),
        (Some((p, patch)), false) => {
            // BOOT-9 (decision 215): an install that takes a while is named on the boot screen, still up; one that
            // finds the patch already there is over in a moment and shows nothing.
            let mut install = core::pin::pin!(own_frame(store.ssb_install(p, patch, ferrite_core::ssb::SOURCE_FLASHER)));
            let r = match select(&mut install, Timer::after_millis(300)).await {
                Either::First(r) => r,
                Either::Second(()) => {
                    let failed = checks.iter().any(|c| c.state == State::Fail);
                    let owner = if failed { None } else { set.boot_owner() };
                    let note = Some(b"Installing SSB patch".as_slice());
                    boot::update(&mut lcd, &BootInfo { version: VERSION, callsign: set.boot_callsign(), line: set.boot_line(), owner, checks: &checks, note });
                    install.await
                }
            };
            bringup::line(match r {
                Ok(true) => b"ssb: patch installed and verified",
                Ok(false) => b"ssb: patch already installed",
                Err(_) => b"ssb: install FAILED",
            })
        }
    }
    let ssb_ok = !cfg!(feature = "bringup") && store.ssb_check().is_ok_and(|p| p.is_some());
    if ssb_ok {
        bringup::line(b"ssb: stored patch verified");
    }
    // The store's state (the first normal boot formats Ferrite's areas: only layout::WRITABLE, never calibration).
    bringup::line(match (cfg!(feature = "bringup"), store.mounted()) {
        (true, _) => b"store: not mounted (bring-up build)",
        (false, ferrite_store::store::Mounted::Ok) => b"store: mounted",
        (false, ferrite_store::store::Mounted::FirstBoot) => b"store: first boot (Ferrite's areas formatted)",
        (false, ferrite_store::store::Mounted::Failed) => b"store: FAILED (nothing is saved this power-on)",
        // UPD-16: another layout's data, left alone for Console (the App shows D25 from its first tick)
        (false, ferrite_store::store::Mounted::Update(_)) => b"store: another layout's data: finish the update in Ferrite Console",
    });
    // TERM-28: after the boot report the normal build logs only with Cable log on (0 9 9 5); the bring-up build
    // always. Either way the first byte from the PC ends it.
    let mut cable_log = cfg!(feature = "bringup") || set.cable_log;
    if !cable_log {
        bringup::ON.store(false, Relaxed);
    }

    // Hand-off: the whole screen cleared, then the app draws everything once (HOME-2).
    lcd.0.fill(0, 0, 240, 320, BG);
    let mut app = App::new(set, store::last_alert());
    app.sat_attach(crate::sat::data(store)); // chapter 28a: the satellites (the store's, SAT-2) and their passes
    // The slots as they were saved (frequencies, VFO settings, the channel each is on, watch, Battery Save).
    store.restore(&mut app);
    // After the self-test: setup on first boot (BOOT-12); MENU held: the settings reset (PIN-6, BOOT-14), which
    // asks before the PIN; else the PIN if one is set (BOOT-13).
    if cfg!(feature = "bringup") {
        // No setup, reset or PIN: nothing is stored. A key at boot: straight to the key test (hold BACK leaves).
        if key_seen {
            app.open(ferrite_app::screens::nav::Dest::KeyTest);
        }
        app.notice(b"Bring-up build", b"nothing is saved");
    } else if store.first_boot() {
        app.first_boot();
    } else if menu_held {
        app.power_on_reset();
    } else {
        app.ask_pin();
    }
    app.redraw_all();
    if cfg!(all(feature = "tx", feature = "bench")) {
        app.dry_at_boot(); // TX-48.1: the bench build starts every power-on with Dry TX on
    }
    // SAFE-8.2 (decision 364): a start after a fault or a hang says so, on the screen and the cable log.
    match crate::restart::RESTARTED.load(Relaxed) {
        0 => {}
        r => {
            app.notice(b"Radio restarted", if r == crate::restart::HANG { b"after a hang" } else { b"after a fault" });
            let n = crate::restart::IN_A_ROW.load(Relaxed);
            crate::bringup::linef(|l| {
                let _ = if r == crate::restart::HANG {
                    ufmt::uwrite!(l, "restart: after a hang, {} in a row", n)
                } else {
                    ufmt::uwrite!(l, "restart: after a fault (pc 0x{:x}), {} in a row", r, n)
                };
            });
        }
    }
    // Radio-wide state for the status bar and Status page; GPS, battery and the link fill it in later.
    let mut dev = Device { tx_built: cfg!(feature = "tx"), bench: cfg!(feature = "bench"), battery_pct: 100, ..Default::default() };
    dev.ram_statics = crate::hal::sys::statics(); // SELF-9.2
    // SELF-9.1: the idle time counted by the thread executor's sleep, and when it was last read.
    let mut idle_mark = (crate::IDLE_US.load(Relaxed), Instant::now());
    // SELF-9.3: the same at 24 MHz: its sleep's mark, the time at 24 MHz this window, and when the speed last changed
    let mut idle_low_mark = crate::IDLE_LOW_US.load(Relaxed);
    let (mut low_us, mut speed_at) = (0u64, Instant::now());
    let mut bat_logged = 0u32;
    let mut battery = ferrite_core::battery::Battery::default();
    // About and Self-test (SELF-5 to SELF-7): what the boot checks found. The BK4829s from the Radio task's
    // snapshot below; the SI4732 isn't tested at boot; the SSB patch store comes with the link's SSB_WRITE.
    {
        let t = &mut dev.selftest;
        t.idcode = crate::hal::sys::idcode();
        t.uid = crate::hal::sys::uid(); // SELF-8
        t.mcu_ok = t.idcode == chipid::EXPECTED_PID;
        if let Some((id, size)) = flash {
            (t.flash_id, t.flash_mb) = (id, (size >> 20) as u8);
        }
        t.si4732 = CHECK_STATE[crate::tasks::CHECK_SI as usize].load(Relaxed) == PASSED;
        t.si_rev = core::array::from_fn(|i| crate::tasks::SI_REV[i].load(Relaxed)); // SELF-5
        t.ssb_patch = ssb_ok;
        // LINK-19.6: the store's mount, for Console's debug info
        t.store = match store.mounted() {
            _ if cfg!(feature = "bringup") => 0,
            ferrite_store::store::Mounted::Ok => 1,
            ferrite_store::store::Mounted::FirstBoot => 2,
            ferrite_store::store::Mounted::Failed => 3,
            ferrite_store::store::Mounted::Update(_) => 4,
        };
        t.cal = cal_state(); // CAL-20: the gate's verdict, not only "not blank"
        (t.cal_fault, t.cal_at) = cal_fault(); // CAL-20.4
    }
    let mut live_rx = LIVE.receiver().unwrap();
    let mut live = LiveAll::default();
    if let Some(l) = live_rx.try_get() {
        live = l;
    }
    // GPS (roadmap 5a): the fix into the store's GPS data, the status bar's GPS and clock, the arrival alarm.
    let mut gps = crate::tasks::gps::GpsUi::new();
    // Key codes down (mapped ones only): the app is ticked often while any is held.
    let mut down = 0u32;
    #[cfg(feature = "bench")]
    let mut bt_q: Option<BtQuery> = None;
    // DISP-27: the panel asleep while the screen is dark, and when it last changed.
    let (mut panel_asleep, mut panel_at) = (false, now_ms());
    // TERM-40: the display's bench state (the power bench); default is normal.
    #[cfg(feature = "bench")]
    let mut lcdb = ferrite_core::msg::LcdBench::default();
    // The terminal over the cable (chapter 37): it reads and edits the same App, so it runs here.
    // ... and over Bluetooth (roadmap 3d): one terminal, held by one port at a time, and a link session on each
    // port (LINK-28). The link protocol on the cable (chapter 29) is trusted as the cable is (LINK-7); TERM-2
    // routes the bytes.
    let mut ports = Ports::new();
    let fastest = ferrite_core::link::RATES.iter().copied().filter(|&r| crate::hal::uart::rate_ok(crate::report::UART_CPS, r)).max();
    ports.set_cable_max_baud(fastest.unwrap_or(115_200));
    // Bring-up: the receiver on the cable log once a second while it changes.
    let mut logged = (LiveAll::default(), Instant::now());
    // The APRS task (chapter 25): it feeds the App and the store, so it runs here.
    let mut aprs = super::aprs::Aprs::new();
    // SAME and the pager decoder (roadmap 6b).
    let mut decode = super::decode::Decode::new();
    // MODE-6: the command taken from the App that the Radio task's channel had no room for; it goes first.
    let mut held: Option<RadioCmd> = None;
    // LINK-38: when the screen was last drawn, and whether input since wants it drawn now.
    let (mut last_draw, mut urgent) = (0u32, true);
    // PWR-26.11: the slots' squelch as last seen (a change is an urgent redraw). WATCH-17.4 (bench): when it changed
    // until the screen next drew, the cable log's "squelch to screen".
    let mut sq_was = 0u8;
    #[cfg(feature = "bench")]
    let mut sq_at = None::<u32>;
    // PWR-24.9: since when Low Power Mode has been on (None: off).
    let mut lpm_since: Option<u32> = None;
    // PWR-26.3: when the governor last had a reason for the build's speed.
    let mut clock_high_at = 0u32;
    let mut house_at = 0u32; // PWR-26.15
    // SAT-6: a prediction under way: when it started (ms) and the slices' time so far (us), for its log line.
    let mut predict_at: Option<(u32, u64)> = None;
    // LINK-13.4: the DSP task codes receive audio for the PC (the cable's session listens).
    let mut listening = 0u8;
    loop {
        #[cfg(feature = "bench")]
        let pass_c0 = crate::hal::time::cycles();
        #[cfg(feature = "bench")]
        let mut sec_c = pass_c0;
        #[cfg(feature = "bench")]
        macro_rules! sec { ($i:expr) => {{ let c = crate::hal::time::cycles(); PASS[$i].fetch_add(c.wrapping_sub(sec_c), Relaxed); sec_c = c; }} }
        #[cfg(not(feature = "bench"))]
        macro_rules! sec { ($i:expr) => {} }
        // CAL-20: the owner accepted the calibration as it is now: checked again, the Radio task given it, About told.
        match store.take_cal_accepted() {
            Some(true) => {
                let crc = own_frame(store.check_cal()).await;
                read_calibration(store, flash.is_some(), cal_ok, mcu_ok, crc);
                dev.selftest.cal = cal_state();
                (dev.selftest.cal_fault, dev.selftest.cal_at) = cal_fault();
                bringup::line(b"calibration accepted by the owner");
            }
            // it fails a CAL-6 check: invalid, not only changed, and transmit stays locked
            Some(false) => dev.selftest.cal = ferrite_core::msg::CalState::Invalid,
            None => {}
        }
        // Bring-up: the first byte from the PC ends the log; the terminal has the cable from then on.
        if bringup::ON.load(Relaxed) && TERM_IN.count() > 0 {
            bringup::line(b"log off: the terminal has the cable");
            bringup::ON.store(false, Relaxed);
        }
        if bringup::ON.load(Relaxed) && logged.1.elapsed() >= Duration::from_secs(1) {
            if let Some(l) = live_rx.try_get() {
                if l != logged.0 {
                    log_live(&l);
                    logged.0 = l;
                }
            }
            logged.1 = Instant::now();
        }
        // PWR-26.15: the housekeeping's turn, once a second
        let house = now_ms().wrapping_sub(house_at) >= HOUSE_MS;
        if house {
            house_at = now_ms();
        }
        let mut fed = false;
        // A whole line waits until the store has caught up (TERM-13): the bytes after it stay in the ring.
        // The cable first, then Bluetooth (its bytes only while 0 9 2 1 has the module on). The cable's wait while
        // its log has it (TERM-28: the first byte ends the log, above); Bluetooth's never do.
        #[cfg(feature = "bench")]
        let (bt_forced, bt_busy) = (bt_q.as_ref().is_some_and(|q| q.forced), bt_q.is_some());
        #[cfg(not(feature = "bench"))]
        let (bt_forced, bt_busy) = (false, false);
        BT_ON.store(app.bt_on() || bt_forced, Relaxed); // PWR-24: off in Low Power Mode
        for (port, ring) in [(Port::Cable, &TERM_IN), (Port::Bt, &BT_IN)] {
            // While a `bt` query waits, the module's bytes are its answer, not a session's.
            let held = port == Port::Cable && bringup::ON.load(Relaxed) || port == Port::Bt && bt_busy;
            while !held && ports.ready_for(port) {
                let Some(b) = ring.get() else { break };
                fed = true;
                match ports.route(&app, port, b, now_ms()) {
                    ferrite_app::ports::To::Kiss => aprs.kiss_in(b), // KISS has this port (chapter 26)
                    to => ports.feed(&mut app, store, &dev, &live, to, b, now_ms()),
                }
                while let Some(w) = app.pop_write() {
                    store.apply(w);
                }
            }
        }
        // Every pass, bring-up log or not (inside the `!bringup::ON` branch they never updated About). PWR-20: the battery from the Radio task's PA1 reading, with the user trim (10 mV steps).
        let raw = crate::chan::BATTERY_RAW.load(Relaxed);
        // PWR-22: the percentage on this radio's gauge, averaged, held while keyed and after, slow to rise
        // (sampled every 2 s: PWR-26.15's once a second is enough; keyed always, for the hold).
        let shown = if house || live.tx.keyed { battery.update(&gauge, raw, now_ms(), live.tx.keyed) } else { None };
        if let Some(s) = shown {
            let mv = ferrite_core::txcal::battery_mv(raw) as i32 + dev.selftest.battery_trim as i32 * 10;
            dev.battery_mv = mv.max(0) as u16;
            (dev.battery_pct, dev.battery_low, dev.battery_cutoff) = (s.pct, s.low, s.cutoff);
            app.battery(s.pct); // PWR-24.3: Low Power Mode's Auto
            // The cable log (TERM-28): the battery every 10 s (no charger-detect input is known on this board):
            // the reading now, the averaged one the % comes from, and the %.
            if now_ms().wrapping_sub(bat_logged) >= 10_000 {
                bat_logged = now_ms();
                // SBAR-23: the stack's high-water mark (a walk of up to its whole size, so only every 10 s).
                dev.stack_free = crate::hal::sys::stack_free();
                // SELF-9.1: CPU = the share of these 10 s the thread executor wasn't asleep (the interrupt executors
                // and handlers run in its waking time, so they count as busy).
                let idle = crate::IDLE_US.load(Relaxed);
                let us = idle_mark.1.elapsed().as_micros().max(1) as u32;
                let asleep = idle.wrapping_sub(idle_mark.0).min(us);
                dev.cpu_pct = Some((100 - asleep as u64 * 100 / us as u64) as u8);
                idle_mark = (idle, Instant::now());
                // SELF-9.3: awake at each speed, and the share of the window at 24 MHz
                if crate::hal::clock::is_low() {
                    low_us += speed_at.elapsed().as_micros();
                    speed_at = Instant::now();
                }
                let low = (low_us as u32).min(us);
                let idle_low = crate::IDLE_LOW_US.load(Relaxed);
                let asleep_low = idle_low.wrapping_sub(idle_low_mark).min(low).min(asleep);
                let pct = |slept: u32, of: u32| (of >= 100_000).then(|| (100 - slept as u64 * 100 / of as u64) as u8);
                dev.cpu_speeds = [pct(asleep_low, low), pct(asleep - asleep_low, us - low), Some((low as u64 * 100 / us as u64) as u8)];
                (idle_low_mark, low_us) = (idle_low, 0);
                bringup::linef(|b| {
                    let _ = ufmt::uwrite!(b, "stack free {} B, cpu {}%", dev.stack_free, dev.cpu_pct.unwrap_or(0));
                });
                // SELF-9.4 (bench builds): who used it, ms of these 10 s (the thread's includes interrupts in it)
                #[cfg(feature = "bench")]
                {
                    use core::sync::atomic::AtomicU32;
                    static LAST: [AtomicU32; 5] = [const { AtomicU32::new(0) }; 5];
                    let now = [&crate::RADIO_CYCLES, &crate::DSP_CYCLES, &crate::INPUT_CYCLES, &crate::THREAD_CYCLES, &UI_CYCLES].map(|a| a.load(Relaxed));
                    let ms = |i: usize| now[i].wrapping_sub(LAST[i].swap(now[i], Relaxed)) / (crate::hal::clock::mhz() * 1000);
                    let wk = WAKES.each_ref().map(|w| w.swap(0, Relaxed));
                    bringup::linef(|b| {
                        let _ = ufmt::uwrite!(b, "cpu ms/10s: radio {} dsp {} input {} thread {} (ui {}), at {} MHz", ms(0), ms(1), ms(2), ms(3), ms(4), crate::hal::clock::mhz());
                    });
                    bringup::linef(|b| {
                        let _ = ufmt::uwrite!(b, "ui woke: input {} live {} timer {} alert {}", wk[0], wk[1], wk[2], wk[3]);
                    });
                    // PWR-26.16: the BK4829 bus's reads, writes and writes skipped by the shadow, in these 10 s
                    static BK_LAST: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
                    let bk = core::array::from_fn::<u32, 3, _>(|i| {
                        let n = crate::tasks::radio::shadow::COUNTS[i].load(Relaxed);
                        n.wrapping_sub(BK_LAST[i].swap(n, Relaxed))
                    });
                    bringup::linef(|b| {
                        let _ = ufmt::uwrite!(b, "bk bus /10s: reads {} writes {} skipped {}", bk[0], bk[1], bk[2]);
                    });
                    let ps = PASS.each_ref().map(|a| a.swap(0, Relaxed) / (crate::hal::clock::mhz() * 1000));
                    bringup::linef(|b| {
                        let _ = ufmt::uwrite!(b, "ui pass ms/10s: in {} link {} aprs {} store {} gps {} draw {} rest {} tick {}", ps[0], ps[1], ps[2], ps[3], ps[4], ps[5], ps[6], ps[7]);
                    });
                }
                bringup::linef(|b| {
                    let avg = ferrite_core::txcal::battery_mv(s.raw);
                    let _ = ufmt::uwrite!(b, "battery {} mV, avg {} mV, {}%, PA1 {} avg {}", ferrite_core::txcal::battery_mv(raw), avg, s.pct, raw, s.raw);
                });
                // SBAR-20: the MCU temperature's raw counts and what they give.
                let t = crate::chan::MCU_TEMP.load(Relaxed);
                let r = crate::chan::MCU_TEMP_RAW.load(Relaxed);
                if t != i16::MIN {
                    bringup::linef(|b| {
                        let (s, a) = (if t < 0 { "-" } else { "" }, t.unsigned_abs());
                        let _ = ufmt::uwrite!(b, "mcu temp {}{}.{} C, TS {}, VREFINT {}", s, a / 10, a % 10, r >> 16, r & 0xFFFF);
                    });
                }
            }
        }
        sec!(0); // ports in, battery
        dev.selftest.chips = live.chips;
        // SBAR-20: the Radio task's MCU temperature reading, in whole degrees.
        let t = crate::chan::MCU_TEMP.load(Relaxed);
        dev.mcu_temp = (t != i16::MIN).then(|| ferrite_core::mcutemp::whole(t));
        // SND-12: the speaker mic, as the input task debounced it.
        dev.spkmic = crate::chan::ACCESSORY.load(Relaxed) & ferrite_core::acc::ON != 0;
        {
            // Debug commands (TERM-21): memory here, registers by the Radio task (it signals TERM_WAKE).
            #[cfg(feature = "bench")]
            if let Ok(a) = DEBUG_ANS.try_receive() {
                ports.debug_answer(a);
            }
            // The terminal redrawn and one waiting link request answered when the port and the store have room
            // (LINK-1), on the port that holds them.
            // The cable's output waits while its log has it (TERM-28); Bluetooth's sessions run regardless.
            let cable_free = !bringup::ON.load(Relaxed);
            // LINK-19.5: Bluetooth at its unlocked rate carries a faster Live View
            ports.set_bt_fast(crate::hal::uart::rate(crate::report::UART_BT) >= ferrite_core::link::BT_FAST);
            // Decisions 290, 331: the DSP task, which preempts this one, puts its audio frames into TERM_OUT only while
            // no frame of the session's is half written: `Out` holds the flag for each frame's copy, not the poll.
            // PWR-26.15: an idle link (no bytes, no session, nothing waiting) once a second
            if fed || house || ports.busy() || ports.live(&app, now_ms()) {
                ports.poll(&mut app, store, &dev, &live, now_ms(), &mut Out(&TERM_OUT, cable_free), &mut Out(&BT_OUT, true));
            }
            if cable_free {
                crate::hal::uart::tx_ring(crate::report::UART_CPS, &TERM_OUT); // decision 290: the interrupt sends it
            }
            // LINK-13.4, LINK-13.5: receive audio only while the cable's session listens; its blocks to the cable.
            let listen = ports.audio_wanted(now_ms());
            if listen != listening && crate::chan::DSP.try_send(ferrite_core::msg::DspCmd::Audio(listen)).is_ok() {
                listening = listen;
            }
            dev.restart = [crate::restart::RESTARTED.load(Relaxed), crate::restart::IN_A_ROW.load(Relaxed)]; // SAFE-8.2
            // Decision 290: the DSP task frames and sends the audio itself; here only its counters, for the readback.
            {
                use crate::tasks::dsp::{AUDIO_CLIPS, AUDIO_DROPS, AUDIO_GAIN, AUDIO_MADE, AUDIO_PEAK, AUDIO_WAITS};
                dev.audio = [AUDIO_MADE.load(Relaxed), AUDIO_DROPS.load(Relaxed), AUDIO_WAITS.load(Relaxed), AUDIO_CLIPS.load(Relaxed), AUDIO_PEAK.load(Relaxed), AUDIO_GAIN.load(Relaxed)];
                use crate::tasks::dsp::{DROPPED, FRAMES, FX25_FAILED, MISSED, NOISE};
                // APRS-56.1's counts in every build; APRS-56.4's stages and timings in bench builds (decision 363)
                #[cfg(not(feature = "bench"))]
                {
                    dev.decode[..5].copy_from_slice(&[FRAMES.load(Relaxed), MISSED.load(Relaxed), NOISE.load(Relaxed), FX25_FAILED.load(Relaxed), DROPPED.load(Relaxed)]);
                }
                #[cfg(feature = "bench")]
                use crate::tasks::dsp::{CAPTURES, CAPTURE_MAX_MS, CAPTURE_MS, FIRST_DATA_MS, HEARD, SQ_LAG_MS, TICK_PERIOD_MS, WAKE_MAX_US, WORK_MAX_US};
                #[cfg(feature = "bench")]
                {
                    dev.decode = [FRAMES.load(Relaxed), MISSED.load(Relaxed), NOISE.load(Relaxed), FX25_FAILED.load(Relaxed), DROPPED.load(Relaxed), CAPTURE_MS.load(Relaxed), CAPTURE_MAX_MS.load(Relaxed), CAPTURES.load(Relaxed), HEARD.load(Relaxed), crate::hal::audio::ADC_LATE.load(Relaxed), WAKE_MAX_US.load(Relaxed), WORK_MAX_US.load(Relaxed), crate::RADIO_RUN_MAX_US.load(Relaxed), crate::INPUT_RUN_MAX_US.load(Relaxed), crate::LONG_RUNS.load(Relaxed), SQ_LAG_MS.load(Relaxed), TICK_PERIOD_MS.load(Relaxed), FIRST_DATA_MS.load(Relaxed)]; // APRS-56, PWR-26.9.1, APRS-56.4
                }
            }
            if let Some(r) = ports.take_baud() {
                LINK_BAUD.signal(r); // the Link task switches once this answer has gone (LINK-11)
            }
            if let Some(r) = ports.take_bt_baud() {
                crate::chan::BT_LINK_BAUD.signal(r); // LINK-15.1: Bluetooth's, likewise
            }
            // BACKUP-7 (decision 261): the way back to stock has the cable from here until it resets.
            if let Some(seq) = ports.take_restore() {
                send_shutdown(&mut app, &mut held).await;
                super::restore::run(store, &mut lcd, seq, app.settings().leds != 0).await; // DISP-30: LEDs per 0 7 6
            }
            if ports.take_reboot() {
                // LINK-24: the radio quiet, everything written (at most 3 s; 15 s after raw writes, whose queue
                // can hold 8 erases and the stamp: LINK-62, decision 339), the answer out, then a system reset: the
                // stock bootloader listens for the updater first, as after hwtest's `reboot`.
                send_shutdown(&mut app, &mut held).await;
                store.flush(&app);
                store::wake(store);
                let t = Instant::now();
                let most = Duration::from_secs(if store.raw() { 15 } else { 3 });
                // PWR-17.1: a save already under way made the flush wait: flushed again once it is done
                while (!store.idle() || store.unsaved(&app)) && t.elapsed() < most {
                    if store.unsaved(&app) && store.flush(&app) {
                        store::wake(store);
                    }
                    Timer::after_millis(10).await;
                }
                Timer::after_millis(50).await; // the answer's bytes leave the UART
                cortex_m::peripheral::SCB::sys_reset();
            }
            LINK_HELLOS.store(ports.hellos(), Relaxed);
            crate::chan::BT_HELLOS.store(ports.bt_hellos(), Relaxed);
            LINK_ON.store(ports.link_on(Port::Cable, now_ms()), Relaxed);
            BT_LINK_ON.store(ports.link_on(Port::Bt, now_ms()), Relaxed);
            dev.bt = ports.bt_connected(&app, now_ms());
            dev.link = ferrite_core::msg::LinkLive {
                cable: ports.link_on(Port::Cable, now_ms()),
                baud_100: (CABLE_BAUD.load(Relaxed) / 100) as u16,
                write_pct: ports.progress(),
            };
            // TERM-27: the `bt` query: send once the module has power (20 ms after PB9, then the UART), collect for
            // 500 ms, answer.
            #[cfg(feature = "bench")]
            if let Some(q) = bt_q.as_mut() {
                if !q.sent && (!q.forced || q.at.elapsed() >= Duration::from_millis(150)) {
                    BT_IN.clear();
                    for &c in q.text[..q.n as usize].iter().chain(b"\r\n") {
                        BT_OUT.put(c);
                    }
                    (q.sent, q.at) = (true, Instant::now());
                } else if q.sent {
                    while let Some(c) = BT_IN.get() {
                        if (q.got as usize) < q.buf.len() {
                            q.buf[q.got as usize] = c;
                            q.got += 1;
                        }
                    }
                    if q.at.elapsed() >= Duration::from_millis(500) {
                        ports.debug_answer(ferrite_core::msg::DebugAns::Bt { data: q.buf, n: q.got });
                        bt_q = None;
                    }
                }
            }
            #[cfg(feature = "bench")]
            if let Some(a) = crate::chan::GPS_ANS.try_take() {
                ports.debug_answer(a);
            }
            if let Some(r) = ports.debug_req() {
                match r {
                    #[cfg(feature = "bench")]
                    ferrite_core::msg::DebugReq::Mem { .. } => ports.debug_answer(crate::debug::mem(r)),
                    ferrite_core::msg::DebugReq::Gga => ports.debug_answer(crate::debug::gga()),
                    // TERM-36: the GPS task sends it and answers through GPS_ANS (checked again there).
                    #[cfg(feature = "bench")]
                    ferrite_core::msg::DebugReq::Gps { id, arg, quiet } => crate::chan::GPS_SEND.signal((id, arg, quiet)),
                    #[cfg(feature = "bench")]
                    ferrite_core::msg::DebugReq::Linkz => ports.debug_answer(ferrite_core::msg::DebugAns::Linkz(linkz_bench())),
                    #[cfg(feature = "bench")]
                    ferrite_core::msg::DebugReq::BtStat { baud, pace } => {
                        if baud != 0 {
                            crate::chan::BT_RATE.store(baud, Relaxed); // the Link task restarts USART1 at it
                        }
                        let fast = crate::hal::uart::rate(crate::report::UART_BT) >= ferrite_core::link::BT_FAST;
                        let slot = if fast { &crate::chan::BT_PACE_FAST } else { &crate::chan::BT_PACE }; // LINK-36.1
                        if let Some(p) = pace {
                            slot.store(p.pack(), Relaxed); // LINK-36: the Link task's next turn
                        }
                        let st = crate::hal::uart::stats(crate::report::UART_BT);
                        let (framing, overrun) = crate::hal::uart::errors(crate::report::UART_BT);
                        let rate = if baud != 0 { baud } else { crate::hal::uart::rate(crate::report::UART_BT) };
                        let on = crate::board::BT_PWR.out_level();
                        ports.debug_answer(ferrite_core::msg::DebugAns::BtStat(ferrite_core::msg::BtStats {
                            rx: st.rx_bytes,
                            tx: st.tx_bytes,
                            framing,
                            overrun,
                            dropped: st.dropped,
                            baud: rate,
                            on,
                            pace: ferrite_core::link::BtPace::unpack(slot.load(Relaxed)),
                        }));
                    }
                    #[cfg(feature = "bench")]
                    ferrite_core::msg::DebugReq::Lcd(set) => {
                        let mut new = lcdb.apply(set); // `sleep` goes through DISP-27's panel sleep below
                        if set == ferrite_core::msg::LcdSet::Bus && !panel_asleep {
                            // The panel bus speed: the boot's timed fill again, at the clock draws run at now.
                            let c0 = crate::hal::time::cycles();
                            lcd.0.fill(0, 0, 240, 320, BG);
                            let mhz = crate::hal::clock::mhz();
                            new.bus = Some((crate::hal::time::cycles().wrapping_sub(c0) / mhz, mhz as u16));
                            app.redraw_all();
                        }
                        if new.idle != lcdb.idle {
                            lcd.0.idle(new.idle);
                        }
                        if new.partial != lcdb.partial {
                            lcd.0.partial(new.partial.then_some(ferrite_core::msg::LCD_PARTIAL));
                        }
                        lcdb = new;
                        ports.debug_answer(ferrite_core::msg::DebugAns::Lcd(lcdb));
                    }
                    #[cfg(feature = "bench")]
                    ferrite_core::msg::DebugReq::Bt { text, n } => {
                        let forced = !app.settings().bt;
                        bt_q = Some(BtQuery { text, n, forced, sent: false, at: Instant::now(), buf: [0; 48], got: 0 });
                    }
                    #[cfg(feature = "bench")]
                    _ if DEBUG_REQ.try_send(r).is_err() => ports.debug_answer(ferrite_core::msg::DebugAns::Refused("the Radio task is busy")),
                    #[cfg(feature = "bench")]
                    _ => {}
                }
                TERM_WAKE.signal(()); // print the answer, or wait for it, on the next turn
            }
        }
        sec!(1); // link poll
        // TERM-28.1: only the bring-up build (no store) holds them; the normal build's cable log never does.
        if !(cfg!(feature = "bringup") && bringup::ON.load(Relaxed)) {
            // PWR-26.15: once a second, or at once when a frame or a page waits
            if house || aprs.due() {
                aprs.poll(&mut app, store, &live, &dev, now_ms());
            }
            if house || !super::dsp::PAGES.is_empty() {
                decode.poll(&mut app, store, &live);
            }
        }
        sec!(2); // aprs, decode
        // PWR-26.1: a key just in raises the clock here, before its commands go: its beep, started by the Radio task
        // at once, holds any switch until it ends (PWR-26.4), so the governor below found audio busy and the key's
        // redraw ran at 24 MHz (HF mode's open about 800 ms on black, Nov 4, 2026). Not in Fixed (always the
        // build's speed) and not while audio already runs.
        if crate::hal::clock::is_low()
            && crate::hal::clock::AUDIO_RAISES.load(Relaxed) // Dynamic, out of Low Power Mode
            && now_ms().wrapping_sub(app.last_input()) < KEY_RAISE_MS
            && !crate::hal::audio::busy()
            && crate::hal::clock::set_tree(crate::hal::clock::NORMAL)
        {
            clock_high_at = now_ms();
        }
        // MODE-6: never waiting on the Radio task: what its channel has no room for waits in the App's queue (and
        // `held`), in order, and goes on the next turn (every 10 ms meanwhile).
        to_radio(&mut app, &mut held);
        // Channel writes (save as channel, the channel editor, the browser) and the settings and slots (2 s after
        // their last change) queue for the Storage task.
        while let Some(w) = app.pop_write() {
            store.apply(w);
        }
        if house {
            store.autosave(now_ms(), &app); // PWR-26.15: its 2 s delay doesn't need more
        }
        store::wake(store);
        // PWR-17: the power switch off (PE0 high, twice 50 ms apart): "Saving...", the radio off, the pending
        // writes, then the power hold released; if the switch is back on by then, a reset, as stock does.
        // Auto power off (0 7 8) runs out like the switch going off.
        let auto_off = app.take_power_off();
        if POWER_SW.read() || auto_off {
            if !auto_off {
                Timer::after_millis(50).await;
            }
            if auto_off || POWER_SW.read() {
                // TX-33, PWR-17: each step in the cable log with the time (ms since boot; the Radio task's
                // "shutdown" lines carry the same clock), so the order and the 3 s can be judged off the radio.
                power_log(if auto_off { "auto off" } else { "switch off" });
                app.power_off();
                app.render(&mut lcd, &live, &dev, store);
                send_shutdown(&mut app, &mut held).await;
                power_log("radio told");
                // TX-33: power is released only after the safe state; the Radio task clears its keyed flag when
                // its unkey is done (at most 1 s: the unkey wait is far shorter).
                let t = Instant::now();
                if crate::chan::RF_STATE.load(Relaxed) & crate::chan::RF_BUSY_KEYED != 0 {
                    while crate::chan::RF_STATE.load(Relaxed) & crate::chan::RF_BUSY_KEYED != 0 && t.elapsed() < Duration::from_secs(1) {
                        Timer::after_millis(5).await;
                    }
                    power_log("radio unkeyed");
                }
                // Everything queued, then the settings, written before the power goes (at most 3 s).
                store.flush(&app);
                store::wake(store);
                power_log("saving");
                let t = Instant::now();
                // PWR-17.1: a save already under way refused the flush (the settings changed since, in the last
                // seconds before the switch went off): flushed again once it is done, within the same 3 s
                while (!store.idle() || store.unsaved(&app)) && t.elapsed() < Duration::from_secs(3) {
                    if store.unsaved(&app) && store.flush(&app) {
                        store::wake(store);
                    }
                    Timer::after_millis(10).await;
                }
                power_log(if store.idle() && !store.unsaved(&app) { "saved" } else { "save cut at 3 s" });
                power_log("off");
                if bringup::ON.load(Relaxed) {
                    Timer::after_millis(20).await; // the last lines out on the cable before the power goes
                }
                POWER_HOLD.write(false);
                Timer::after_millis(500).await;
                if auto_off && !POWER_SW.read() {
                    // Still powered: the switch is on, and only it cuts the power. Standby until a key: the
                    // chips are off (Shutdown above), the screen, keypad light and LEDs too; then start again.
                    backlight::set(0);
                    backlight::keypad(0);
                    LED_RED.write(!LED_ACTIVE_HIGH);
                    LED_GREEN.write(!LED_ACTIVE_HIGH);
                    while !matches!(INPUT.receive().await, Input::Key(keypad::Event::Press(_))) && !POWER_SW.read() {}
                }
                cortex_m::peripheral::SCB::sys_reset();
            }
        }
        sec!(3); // radio cmds, store
        if house {
            gps.step(&mut app, store, &mut dev); // PWR-26.15: fixes come once a second
        }
        sec!(4); // gps
        // LINK-38: a redraw for live changes alone at most every FRAME_MIN_MS. During a scan the Radio task reports
        // every step and each scan page redraw is ~14 KB (12-17 ms): back to back they left the Link task (same
        // executor) a turn only between draws, too little to answer the link or carry the cable log. Keys and the
        // knob still draw at once.
        // DISP-27: the panel sleeps (display off, sleep in) while the screen is dark, and isn't drawn; it wakes
        // (sleep out, its 120 ms, display on, a whole redraw) before the backlight comes back. The bench's
        // `lcd sleep` (TERM-40) asks the same.
        #[cfg(feature = "bench")]
        let want_sleep = app.backlight_duty() == 0 || lcdb.sleep;
        #[cfg(not(feature = "bench"))]
        let want_sleep = app.backlight_duty() == 0;
        if want_sleep != panel_asleep {
            // The panel needs 120 ms between sleep in and sleep out, either way.
            let gap = now_ms().wrapping_sub(panel_at);
            if gap < PANEL_SLEEP_MS {
                Timer::after_millis((PANEL_SLEEP_MS - gap) as u64).await;
            }
            if want_sleep {
                lcd.0.on(false);
                lcd.0.sleep(true);
            } else {
                lcd.0.sleep(false);
                Timer::after_millis(PANEL_SLEEP_MS as u64).await;
                lcd.0.on(true);
                app.redraw_all();
                urgent = true;
            }
            (panel_asleep, panel_at) = (want_sleep, now_ms());
            crate::chan::SCREEN_DARK.store(want_sleep, Relaxed); // PWR-26.17
        }
        // PWR-24.9, PWR-26: the MCU clock. In Low Power Mode 24 MHz after LPM_CLOCK_MS (and the boost after
        // input, PWR-24.10; transmit stays at 24 MHz); otherwise the governor: 24 MHz at rest, the build's speed
        // while anything needs it (the App's reasons, a live link, audio) and a second after (PWR-26.3); Fixed
        // (0 9 9 4) the build's speed always. Never while audio runs (PWR-26.4). Here, in thread mode between
        // awaits, nothing is mid-transfer (hal::clock::set_low). Before the draw, so a redraw after a key is at
        // full speed.
        let now = now_ms();
        lpm_since = if app.low_power() { Some(lpm_since.unwrap_or(now)) } else { None };
        let since_input = now.wrapping_sub(app.last_input());
        let lpm_wait = lpm_since.map(|t| ferrite_core::clocktree::lpm_low_in(now.wrapping_sub(t), since_input, panel_asleep, LPM_CLOCK_MS, LPM_BOOST_MS));
        let fixed = app.settings().mcu_fixed;
        let audio = crate::hal::audio::busy();
        let mut why = app.clock_reasons(&live, now);
        if ports.live(&app, now) {
            why |= ferrite_core::clocktree::why::LINK;
        }
        if audio || crate::hal::clock::AUDIO_WAITING.load(Relaxed) != 0 {
            why |= ferrite_core::clocktree::why::AUDIO;
        }
        // PWR-26.1: the panel just woke (a key, or a signal with 0 7 4 4's wake on receive): its whole redraw at full
        // speed (at 24 MHz it took 880 ms on black, Nov 4, 2026)
        if !panel_asleep && now_ms().wrapping_sub(panel_at) < WAKE_FAST_MS {
            why |= ferrite_core::clocktree::why::INPUT;
        }
        if why != 0 {
            clock_high_at = now;
        }
        crate::hal::clock::AUDIO_RAISES.store(!fixed && lpm_wait.is_none(), Relaxed);
        let target = ferrite_core::clocktree::clock_target(fixed, lpm_wait, why, clock_high_at, now, audio);
        // PWR-26.8: satellite passes predicted at the top clock
        let top = ferrite_core::clocktree::top_wanted(fixed, lpm_wait, why, audio);
        if let Some(low) = target {
            use crate::hal::clock::{LOW, NORMAL, TOP};
            let switched = crate::hal::clock::set_tree(if low { LOW } else if top { TOP } else { NORMAL });
            if switched {
                // SELF-9.3: a stay at 24 MHz ended (or began)
                if !crate::hal::clock::is_low() {
                    low_us += speed_at.elapsed().as_micros();
                }
                speed_at = Instant::now();
            }
            if switched && bringup::ON.load(Relaxed) {
                let (mhz, us) = (crate::hal::clock::mhz(), crate::hal::clock::LAST_SWITCH_US.load(Relaxed));
                bringup::linef(|b| {
                    let _ = ufmt::uwrite!(b, "clock: {} MHz (interrupts held {} us", mhz, us);
                    for (i, n) in ferrite_core::clocktree::why::NAMES.iter().enumerate() {
                        if why & 1 << i != 0 {
                            b.push(b", ");
                            b.push(n.as_bytes());
                        }
                    }
                    b.push(b")");
                });
            }
        }
        // PWR-26.6: for System and the readback.
        dev.mcu = ferrite_core::msg::McuClock {
            mhz: crate::hal::clock::mhz() as u16,
            switches: crate::hal::clock::SWITCHES.load(Relaxed),
            held_us: crate::hal::clock::MAX_SWITCH_US.load(Relaxed),
            why,
        };
        let since = now_ms().wrapping_sub(last_draw);
        // TERM-40 (bench): a lower redraw cap.
        #[cfg(feature = "bench")]
        let frame_min = if lcdb.fps == 0 { FRAME_MIN_MS } else { 1000 / lcdb.fps as u32 };
        #[cfg(not(feature = "bench"))]
        let frame_min = FRAME_MIN_MS;
        // PWR-24.5: Low Power Mode redraws live changes once a second (keys and the knob still at once).
        let keys_wait = frame_min != FRAME_MIN_MS; // only the bench's cap holds keys back too (TERM-40)
        let frame_min = if app.low_power() && !app.snake_playing() { frame_min.max(LPM_FRAME_MS) } else { frame_min };
        // PWR-26.10: at rest (24 MHz) live changes alone redraw at most every IDLE_FRAME_MS: a level jittering by a
        // dB woke a whole-screen check 5-6 times a second, 25 ms each with nothing to send (14 % of the CPU, black,
        // Nov 4, 2026); keys and the knob raise the clock and draw at once
        let frame_min = if crate::hal::clock::is_low() && !app.snake_playing() { frame_min.max(IDLE_FRAME_MS) } else { frame_min };
        let asleep = panel_asleep;
        let draw = !asleep && (urgent && !keys_wait || since >= frame_min);
        let (c0, b0, i0) = (crate::hal::time::cycles(), crate::bus::LCD_BYTES.load(Relaxed), crate::IRQ_CYCLES.load(Relaxed));
        ferrite_ui::prof::take(); // bench builds: this draw's profile alone
        if draw {
            app.render(&mut lcd, &live, &dev, store);
            (last_draw, urgent) = (now_ms(), false);
            #[cfg(feature = "bench")]
            if let Some(at) = sq_at.take() {
                bringup::linef(|b| {
                    let _ = ufmt::uwrite!(b, "squelch to screen {} ms, at {} MHz", now_ms().wrapping_sub(at), crate::hal::clock::mhz());
                });
            }
        }
        sec!(5); // clock, draw
        // SQL-18.3: who decides the squelch (0 9 5), for the Radio task's tick.
        ferrite_core::softsq::set_chip_decides(!app.settings().sq_soft);
        ferrite_core::watch::set_aprs_first(app.settings().aprs_share == 0); // WATCH-17.3
        // Cable log turned on or off in the menu (0 9 9 5; the bring-up build ignores it: always on).
        if !cfg!(feature = "bringup") && app.settings().cable_log != cable_log {
            cable_log = app.settings().cable_log;
            bringup::ON.store(cable_log, Relaxed);
        }
        if bringup::ON.load(Relaxed) {
            // Redraw timing (Oct 12, 2026: large redraws were slow): total, and how many bytes went to the panel.
            let mhz = crate::hal::clock::mhz();
            let us = crate::hal::time::cycles().wrapping_sub(c0) / mhz;
            if us >= 5_000 {
                let n = crate::bus::LCD_BYTES.load(Relaxed).wrapping_sub(b0);
                // DISP-29: and how much of it the interrupt executors had, at which clock
                let irq = crate::IRQ_CYCLES.load(Relaxed).wrapping_sub(i0) / mhz;
                bringup::linef(|b| {
                    let _ = ufmt::uwrite!(b, "draw {} ms, {} KB to the panel, {}, interrupts {} ms, {} MHz", us / 1000, n / 1024, app.screen().name(), irq / 1000, mhz);
                });
                #[cfg(feature = "bench")]
                {
                    // the drawing profile (ferrite_ui::prof), in tenths of a ms; the bus is in text and masks
                    // drawn straight to the panel, and alone in the bands
                    let p = ferrite_ui::prof::take();
                    let t = |i: usize| p[i] / (mhz * 100);
                    bringup::linef(|b| {
                        let _ = ufmt::uwrite!(b, "  prof /10ms: rec {} big {} text {} mask {} cover {}; rows {}/{}",
                            t(ferrite_ui::prof::REC), t(ferrite_ui::prof::BIG), t(ferrite_ui::prof::TEXT), t(ferrite_ui::prof::MASK),
                            t(ferrite_ui::prof::COVER), p[ferrite_ui::prof::ROWS_DECODED], p[ferrite_ui::prof::ROWS_DRAWN]);
                    });
                    bringup::linef(|b| {
                        let _ = ufmt::uwrite!(b, "  prof2 /10ms: fill {} push {} bands {}", t(ferrite_ui::prof::FILL), t(ferrite_ui::prof::PUSH), t(ferrite_ui::prof::BANDS));
                    });
                    bringup::linef(|b| {
                        let c = ferrite_ui::prof::CARD;
                        let _ = ufmt::uwrite!(b, "  prof3 /10ms: sbar {} screen {} (cards {} {} {}, aprs {}) hint {}", t(ferrite_ui::prof::SBAR),
                            t(ferrite_ui::prof::SCREEN), t(c), t(c + 1), t(c + 2), t(ferrite_ui::prof::APRS), t(ferrite_ui::prof::HINT));
                    });
                }
            }
        }
        // The lights (decision 63): screen and keypad, and the LEDs with their next change.
        #[cfg(not(feature = "bench"))]
        let (bl, keys) = (app.backlight_duty(), app.keypad_duty());
        // TERM-40 (bench): a fixed % over the settings', and dark while the panel sleeps.
        #[cfg(feature = "bench")]
        let (bl, keys) = (
            if panel_asleep { 0 } else { lcdb.bl.map_or(app.backlight_duty(), |p| p as u16 * 10) },
            lcdb.keys.map_or(app.keypad_duty(), |p| p as u16 * 10),
        );
        backlight::set(bl); // DISP-16, DISP-23: 0 while dark
        backlight::keypad(keys); // DISP-22: the same level, 0 while off
        let leds = app.leds(&live, &dev);
        LED_RED.write(leds.red == LED_ACTIVE_HIGH);
        LED_GREEN.write(leds.green == LED_ACTIVE_HIGH);
        let mut period = if down != 0 || ports.busy() { TICK_HELD_MS } else if asleep { TICK_DARK_MS } else { TICK_IDLE_MS };
        if !asleep && app.snake_playing() {
            period = period.min(TICK_HELD_MS);
        }
        if !draw && !asleep {
            period = period.min(frame_min.saturating_sub(since).max(1) as u64); // the frame held back, drawn on time
        }
        if leds.next_ms != 0 {
            period = period.min(leds.next_ms as u64);
        }
        if let Some(ms @ 1..) = lpm_wait {
            period = period.min(ms as u64); // PWR-24.9: down to 24 MHz on time
        }
        if held.is_some() {
            period = period.min(10); // MODE-6: commands waiting for room on the Radio task's channel
        }
        if aprs.busy() {
            period = period.min(20); // keying or playing an APRS frame: its unkey on time
        }
        if app.sat_busy() {
            period = period.min(1); // SAT-6: the next slice of the prediction at once
        }
        #[cfg(feature = "bench")]
        if bt_q.is_some() {
            period = period.min(10); // a `bt` query: send and collect on time (a timer, so the link task runs too)
        }
        // LINK-38: one yield a pass. During a scan the Radio task publishes on every step, so `live_rx.changed()` is
        // ready at once and an await on it never gives up the CPU: the Link task (same executor) starved, the cable
        // and Bluetooth stopped answering and the cable log stopped, until the scan ended (Oct 20, 2026).
        sec!(6); // lights, period
        #[cfg(feature = "bench")]
        let _ = sec_c;
        #[cfg(feature = "bench")]
        UI_CYCLES.fetch_add(crate::hal::time::cycles().wrapping_sub(pass_c0), Relaxed);
        embassy_futures::yield_now().await;
        // SBAR-23: the alerts' arm waits for one to be there, and the alert (~3 KB) is taken in its handler: carried
        // in this select's result it sized a temporary in the UI task's frame for every pass.
        // KEY-25: input first. The select returns the first ready arm, and the Radio task republishes the snapshot
        // on every tick its values change (satellite mode's downlink level: every 10 ms), so with the snapshot first
        // a UI pass longer than a tick never reached the keys: they waited for a quiet tick, then a press and its
        // release came together, a hold read as a short press (hold MENU opened the pass, not the options) and
        // arrows lagged and jumped (black, Nov 1, 2026). Keys are few; the snapshot is read on the next pass.
        // PWR-26.15: a snapshot where only the levels moved (nothing open, scanning or keyed) is kept, not a pass:
        // the levels draw on the next timed pass. Before, RSSI jitter woke 3-4 passes a second at rest.
        let deadline = Instant::now() + Duration::from_millis(period);
        let woke = loop {
            let wait = select(Timer::at(deadline), TERM_WAKE.wait());
            match select4(INPUT.receive(), live_rx.changed(), wait, ALERTS.ready_to_receive()).await {
                Either4::Second(l) if l.levels_only(&live) => live = l,
                w => break w,
            }
        };
        #[cfg(feature = "bench")]
        WAKES[match woke { Either4::First(_) => 0, Either4::Second(_) => 1, Either4::Third(_) => 2, Either4::Fourth(_) => 3 }].fetch_add(1, Relaxed);
        match woke {
            Either4::Second(l) => live = l,
            // WX-4: a decoded SAME header opens the alert over any screen.
            Either4::Fourth(()) => {
                urgent = true;
                super::decode::Decode::take_alert(&mut app, store)
            }
            Either4::First(Input::Key(keypad::Event::Press(code))) => {
                urgent = true;
                log_input(&Input::Key(keypad::Event::Press(code)));
                app.raw_key(code, true); // every code, mapped or not: the key test names them (KEY-2)
                if let Some(k) = keys::key(code) {
                    down |= 1 << code;
                    app.key_down(k, now_ms(), store);
                }
            }
            Either4::First(Input::Key(keypad::Event::Release(code))) => {
                urgent = true;
                log_input(&Input::Key(keypad::Event::Release(code)));
                app.raw_key(code, false);
                if let Some(k) = keys::key(code) {
                    down &= !(1 << code);
                    app.key_up(k, now_ms(), store);
                }
            }
            Either4::First(Input::Key(_)) => {} // the sampler's own long press and repeat are off
            Either4::First(Input::Knob(d)) => {
                urgent = true;
                log_input(&Input::Knob(d));
                app.knob(d, now_ms(), store)
            }
            Either4::Third(_) => {} // the tick, or bytes for the terminal
        }
        // PWR-26.11: a squelch opening or closing on any slot draws at once; at rest still at 24 MHz (receiving is
        // common, the speed costs battery): before, it waited for PWR-26.10's 250 ms cap, so RX showed late.
        let m = live.slots.iter().enumerate().fold(0u8, |m, (i, s)| m | (s.squelch_open as u8) << i);
        if m != sq_was {
            sq_was = m;
            urgent = true;
            #[cfg(feature = "bench")]
            sq_at.get_or_insert(now_ms());
        }
        #[cfg(feature = "bench")]
        let tc = crate::hal::time::cycles();
        app.tick(now_ms(), &live, store);
        #[cfg(feature = "bench")]
        PASS[7].fetch_add(crate::hal::time::cycles().wrapping_sub(tc), Relaxed);
        // SAT-6: satellite passes predicted in time slices of SAT_SLICE_US between keys and redraws (CPU-bound, not
        // one small slice a tick); the cable log's speed line when it ends.
        if app.sat_busy() {
            let (c0, mhz) = (crate::hal::time::cycles(), crate::hal::clock::mhz());
            let at = predict_at.get_or_insert((now_ms(), 0));
            let busy = app.sat_work(&mut || crate::hal::time::cycles().wrapping_sub(c0) < SAT_SLICE_US * mhz);
            at.1 += (crate::hal::time::cycles().wrapping_sub(c0) / mhz) as u64;
            if !busy {
                sat_log(&app, now_ms().wrapping_sub(at.0), at.1);
                predict_at = None;
                // The finished list drawn now: a last slice within FRAME_MIN_MS of a draw had no draw of its own, and
                // with nothing else to wake it the screen kept the half-filled list until a key (black, Nov 3, 2026).
                urgent = true;
            }
        } else if let Some(at) = predict_at.take() {
            sat_log(&app, now_ms().wrapping_sub(at.0), at.1);
        }
        if let Some(e) = app.take_xb_log() {
            // XB-20.6: every crossband start and stop, with who and why, in the cable log
            bringup::linef(|b| {
                b.push(b"xb: ");
                e.text(b);
            });
        }
    }
}

/// SAT-6: the slice of each UI pass given to satellite pass prediction while it runs (a key waits at most this
/// long, plus a redraw).
const SAT_SLICE_US: u32 = 8_000;

/// SAT-6: the prediction's speed in the cable log: satellites, propagations, the time it took and the time it ran
/// (the on-radio check of SAT-6's estimate).
fn sat_log(app: &ferrite_app::App, ms: u32, busy_us: u64) {
    let (sats, props) = app.sat_stats();
    let each = if props == 0 { 0 } else { (busy_us * 10 / props as u64) as u32 };
    bringup::linef(|b| {
        let _ = ufmt::uwrite!(b, "sat: {} satellites predicted, {} propagations in {} ms ({} ms working, {}.{} us each, {} MHz)",
            sats, props, ms, (busy_us / 1000) as u32, each / 10, each % 10, crate::hal::clock::mhz());
    });
}

/// Bring-up log: a key code down or up (with the key it maps to, if any), or a knob step.
fn log_input(i: &Input) {
    bringup::linef(|b| match *i {
        Input::Key(keypad::Event::Press(c) | keypad::Event::Release(c)) => {
            let down = matches!(i, Input::Key(keypad::Event::Press(_)));
            let _ = ufmt::uwrite!(b, "key {} {}", c, if down { "down" } else { "up" });
            match keys::key(c) {
                Some(k) => {
                    let mut n = ferrite_ui::text::Buf::<16>::new();
                    ferrite_app::screens::keytest::key_name(k, &mut n);
                    b.push(b" = ");
                    b.push(n.as_bytes());
                }
                None => b.push(b" (not mapped)"),
            }
        }
        Input::Knob(d) => {
            let _ = ufmt::uwrite!(b, "knob {}", d);
        }
        Input::Key(_) => b.push(b"key event"),
    });
}

/// Bring-up log: the receivers' snapshot, a line per slot.
fn log_live(l: &LiveAll) {
    for (i, s) in l.slots.iter().enumerate() {
        bringup::linef(|b| {
            let f = s.freq % 100_000;
            let _ = ufmt::uwrite!(b, "{} rx {}.", ["A", "B", "C"][i], s.freq / 100_000);
            for d in [10_000, 1_000, 100, 10, 1] {
                b.push(&[b'0' + (f / d % 10) as u8]); // five decimals, zeros kept: 146.05200
            }
            let _ = ufmt::uwrite!(b, " MHz  rssi {} dBm  squelch {}", s.rssi, if s.squelch_open { "open" } else { "closed" });
            if s.scanning {
                let _ = ufmt::uwrite!(b, "  scanning, {} hits", s.hits);
            }
            if i == 0 {
                b.push(b"  chips ");
                bringup::hex(b, l.chips as u32);
                if l.rx_asleep {
                    b.push(b"  asleep");
                }
            }
        });
    }
}

/// A front-panel key pressed (BOOT-14, the bring-up key test): a PTT held at power on is not one (SAFE-14: it is
/// ignored until released, and never opens the key test or skips the owner card).
fn front_key(i: &Input) -> bool {
    matches!(i, Input::Key(keypad::Event::Press(c)) if !matches!(keys::key(*c), Some(keys::Key::Ptt(_))))
}

/// Bring-up log: what the boot found.
fn boot_report(flash: Option<(u32, u32)>, checks: &[Check], key_seen: bool) {
    bringup::linef(|b| {
        if cfg!(feature = "bringup") {
            let _ = ufmt::uwrite!(b, "Ferrite {} bring-up build: nothing is written to the SPI flash", VERSION);
        } else {
            let _ = ufmt::uwrite!(b, "Ferrite {} {}", VERSION, ferrite_core::build());
        }
    });
    if cfg!(all(feature = "tx", feature = "bench")) {
        bringup::line(b"TX BENCH: Dry TX on at power-on"); // TX-48.1; bench.py --flash looks for it in the image
    }
    bringup::linef(|b| {
        b.push(b"MCU idcode ");
        bringup::hex(b, crate::hal::sys::idcode());
        b.push(b" (expected ");
        bringup::hex(b, chipid::EXPECTED_PID);
        b.push(b")");
    });
    bringup::linef(|b| match flash {
        Some((id, size)) => {
            b.push(b"SPI flash JEDEC ");
            bringup::hex(b, id);
            let _ = ufmt::uwrite!(b, ", {} KB", size / 1024);
        }
        None => b.push(b"SPI flash: no answer"),
    });
    for c in checks {
        bringup::linef(|b| {
            b.push(c.label.as_bytes());
            b.push(match c.state {
                State::Ok => b": ok",
                State::Fail => b": FAILED",
                State::Off => b": not tested at boot",
                State::Pending => b": no report",
            });
        });
    }
    bringup::line(match (key_seen, cfg!(feature = "bringup")) {
        (true, true) => b"key at boot: opening the key test (hold BACK leaves)",
        (true, false) => b"key at boot",
        (false, _) => b"no key at boot",
    });
    bringup::line(b"keys, knob and receiver follow; send any byte for the terminal");
}

/// HF-18: the SSB patch the flasher bundled after the image (`--ssb-from`, tools/ssbpatch.py): its header and the
/// patch, at the first 1 KB boundary after the image's last bytes (the end of `.data`'s load image, or a boundary
/// just below it when the image ends in erased bytes).
fn ssb_bundle() -> Option<(ferrite_core::ssb::Patch, &'static [u8])> {
    extern "C" {
        static __sidata: u32;
        static __sdata: u32;
        static __edata: u32;
    }
    // Linker symbols: only their addresses are used.
    let end = core::ptr::addr_of!(__sidata) as u32 + (core::ptr::addr_of!(__edata) as u32 - core::ptr::addr_of!(__sdata) as u32);
    let top = end.div_ceil(1024) * 1024;
    for at in (0..4).map(|k| top - 1024 * k) {
        let h = crate::hal::sys::mcu_flash(at, ferrite_core::ssb::HEADER as u32)?;
        if let Some(p) = ferrite_core::ssb::bundle(h.try_into().ok()?) {
            return Some((p, crate::hal::sys::mcu_flash(at + ferrite_core::ssb::HEADER as u32, p.len)?));
        }
    }
    None
}

/// LINK-51 (`bt linkz`): the link codec over the sample frames, timed with the cycle counter: the radio-to-PC
/// direction (encode here, decode there) and the PC-to-radio one, each in one session.
#[cfg(feature = "bench")]
#[inline(never)]
fn linkz_bench() -> [u32; 11] {
    use ferrite_core::linkz::{LzDec, LzEnc, DOWN, UP};
    use ferrite_core::linkz_data::SAMPLES;
    let mut v = [0u32; 11];
    v[0] = crate::hal::clock::mhz();
    v[1] = SAMPLES.iter().map(|s| s.len() as u32).sum();
    let (mut b, mut back) = ([0u8; 600], [0u8; 600]);
    {
        let (mut e, mut d) = (LzEnc::<DOWN>::new(), LzDec::<DOWN>::new());
        for s in SAMPLES {
            let c0 = crate::hal::time::cycles();
            let n = e.encode(s, &mut b).unwrap_or(0);
            let c1 = crate::hal::time::cycles();
            let _ = d.decode(&b[..n], &mut back);
            let c2 = crate::hal::time::cycles();
            (v[2], v[3], v[4]) = (v[2] + n as u32, v[3] + c1.wrapping_sub(c0), v[4] + c2.wrapping_sub(c1));
        }
    }
    let (mut e, mut d) = (LzEnc::<UP>::new(), LzDec::<UP>::new());
    for s in SAMPLES {
        let c0 = crate::hal::time::cycles();
        let n = e.encode(s, &mut b).unwrap_or(0);
        let c1 = crate::hal::time::cycles();
        let _ = d.decode(&b[..n], &mut back);
        let c2 = crate::hal::time::cycles();
        (v[5], v[6], v[7]) = (v[5] + n as u32, v[6] + c1.wrapping_sub(c0), v[7] + c2.wrapping_sub(c1));
    }
    v
}
