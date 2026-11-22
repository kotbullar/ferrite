//! Decision 72 (Oct 7, 2026): what was taken from nicFW's feature list.

use super::*;
use crate::screens::save::Save;
use crate::slot::SlotCfg;
use ferrite_core::msg::Param;

static HF_ZONES: [&[u8]; 1] = [b"Mixed"];
static HF_CHANNELS: [Channel; 3] = [
    channel(0, 0, 14_652_000, "Calling", DEFAULT_CFG),
    channel(1, 0, 707_500, "40 m net", SlotCfg { mode: 4, narrow: false, ..DEFAULT_CFG }),
    channel(2, 0, 44_600_000, "UHF call", DEFAULT_CFG),
];

fn hf_store() -> Table<'static> {
    Table { zones: &HF_ZONES, channels: &HF_CHANNELS }
}

fn type_in(t: &mut T, s: &str) {
    for c in s.chars() {
        t.tap(if c == '*' { Key::Star } else { Key::Digit(c as u8 - b'0') });
    }
}

fn press(t: &mut T, k: Key, hold: u32) {
    let s = hf_store();
    t.app.key_down(k, t.t, &s);
    let end = t.t + hold;
    while t.t < end {
        t.t += 10;
        t.app.tick(t.t, &t.live, &s);
    }
    t.app.key_up(k, t.t, &s);
    t.t += 50;
    t.app.tick(t.t, &t.live, &s);
}

/// CHAN-26: a home slot steps past an HF channel; recalled by number it opens HF mode on its frequency and mode
/// (LSB comes up as AM without the SSB patch).
#[test]
fn hf_channels_open_hf_mode() {
    let mut t = T::new();
    press(&mut t, Key::VfoMem, 100);
    assert_eq!(t.app.slot(0).ch.unwrap().num, 0);
    press(&mut t, Key::Up, 100);
    assert_eq!(t.app.slot(0).ch.unwrap().num, 2, "40 m net is HF mode's: stepped past");
    press(&mut t, Key::Down, 100);
    assert_eq!(t.app.slot(0).ch.unwrap().num, 0, "and past it going down");
    for k in [Key::Digit(2), Key::Menu] {
        press(&mut t, k, 100);
    }
    assert!(matches!(t.app.screen(), Screen::Hf(_)), "{:?}", t.app.screen());
    assert_eq!((t.app.set.hf.freq, t.app.set.hf.mode), (707_500, 2), "AM without the patch");
    assert!(t.cmds().contains(&RadioCmd::Hf { freq: 707_500, mode: 2, on: true, keep_rx: false }));
    assert!(!t.app.slot(0).ch.is_some_and(|c| c.is_hf()), "the slot stays where it was");
}

/// CHAN-26, HF-26, KEY-23: hold 9 in HF mode saves its frequency and mode as a channel, and comes back to HF
/// mode; hold V/M there does nothing but say why (HF-13).
#[test]
fn hf_mode_saves_a_channel() {
    let mut t = T::new();
    t.app.radio.ssb_patch = true;
    t.open(crate::screens::nav::Dest::HfMode);
    t.app.set.hf.freq = 1_420_000;
    t.app.set.hf.mode = 3;
    t.live.mode = ferrite_core::msg::ModeLive::Hf { freq: 1_420_000, rssi: -90, snr: 10, si4732: true, loading: 255 };
    t.wait(200);
    t.hold(Key::VfoMem);
    assert!(matches!(t.app.screen(), Screen::Hf(_)), "hold V/M: no zones in HF mode");
    assert!(t.app.toast.is_some_and(|x| x.head.as_bytes() == b"Not in HF mode"));
    t.hold(Key::Digit(9));
    assert!(matches!(t.app.screen(), Screen::Save(s) if s.kind == Save::hf().kind), "the save form, for HF");
    t.tap(Key::Up); // Save (the last row)
    t.tap(Key::Menu);
    match t.app.pop_write() {
        Some(crate::chan::ChanWrite::Put { ch, .. }) => {
            assert_eq!((ch.rx, ch.cfg.mode), (1_420_000, 3));
            assert!(ch.is_hf());
        }
        w => panic!("{w:?}"),
    }
    assert!(matches!(t.app.screen(), Screen::Hf(_)), "back to HF mode: {:?}", t.app.screen());
}

/// VFO-21 (nicFW's range): 1000-1300 MHz receive, typed with a second * after three digits; never transmit there.
#[test]
fn receive_to_1300() {
    let mut t = T::new();
    t.app.slots[0].vfo_mode = true;
    type_in(&mut t, "129**6000"); // the second * takes the point away: a fourth MHz digit
    assert_eq!(t.app.slot(0).vfo, mhz(1296), "{:?}", t.app.screen());
    assert!(ferrite_core::freq::receivable(mhz(1296)) && !ferrite_core::freq::receivable(mhz(700)));
    assert!(ferrite_core::freq::receivable(mhz(1300)) && !ferrite_core::freq::receivable(mhz(1300) + 1), "1300 is in");
    assert!(ferrite_core::freq::receivable(mhz(630)) && !ferrite_core::freq::receivable(mhz(630) + 1), "630 is in");
    for r in ferrite_core::txplan::Region::ALL {
        assert!(ferrite_core::txplan::check(mhz(1296), r, true).is_err(), "{r:?}: no transmit above 1000 MHz");
    }
    // airband entry is unchanged: 118 then 950
    type_in(&mut t, "118950");
    assert_eq!(t.app.slot(0).vfo, mhz(118) + 95_000);
}

/// VFO-21: four digits are three MHz digits and a decimal (1 2 1 5 is 121.5, airband), so 1000-1300 MHz goes
/// in only by \* \* on 100-130 (1 2 9 \* \* 6 is 1296), with a hint at each step: "* again: 4-digit MHz" after
/// the first \*, the fourth digit's range after the second (each fits the hint bar, KEY-9). 630 and 1300, the range's own ends, tune.
#[test]
fn entry_four_mhz_digits_and_the_ends() {
    let menu = |t: &mut T| t.tap(Key::Menu);
    let hints = |t: &T| t.app.screen().hints(&t.app).as_slice().to_vec();
    let mut t = T::new();
    t.app.slots[0].vfo_mode = true;
    for (typed, want) in [
        ("1215", mhz(121) + 50_000),
        ("1300", mhz(130)),
        ("1050", mhz(105)),
        ("130**0", mhz(1300)),
        ("105**0", mhz(1050)),
        ("630", mhz(630)),
    ] {
        type_in(&mut t, typed);
        menu(&mut t);
        assert_eq!((t.app.slot(0).vfo, matches!(t.app.screen(), Screen::Home)), (want, true), "{typed} OK: {:?}", t.app.screen());
    }
    // The hints: "129." asks for \* again, "129" after it names 1290-1299; elsewhere the usual ones.
    type_in(&mut t, "129");
    assert_eq!(hints(&t)[0], ("*", "Point"));
    type_in(&mut t, "*");
    assert_eq!(hints(&t)[0], ("*", "again: 4-digit MHz"));
    let fits = |t: &mut T| {
        let h = hints(t);
        let w = ferrite_ui::widgets::hint_width(&mut ferrite_ui::fb::FrameBuffer::new(), &h);
        assert!(w <= 224, "KEY-9: {h:?} is {w} px");
    };
    fits(&mut t);
    type_in(&mut t, "*");
    assert_eq!(hints(&t)[0], ("0-9", "1290-1299 MHz"));
    fits(&mut t);
    type_in(&mut t, "6");
    assert_eq!(hints(&t)[0], ("*", "Point"));
    // \* after the fourth MHz digit is the point: 1 2 9 \* \* 6 \* 2 \* is 1296.2 (VFO-24)
    type_in(&mut t, "*2*");
    assert_eq!((t.app.slot(0).vfo, matches!(t.app.screen(), Screen::Home)), (mhz(1296) + 20_000, true));
    type_in(&mut t, "130**");
    assert_eq!(hints(&t)[0], ("0", "1300 MHz"));
    t.hold(Key::Exit);
    type_in(&mut t, "146*");
    assert_eq!(hints(&t)[0], ("*", "Point"), "no * again off 100-130");
    t.hold(Key::Exit);
    // 630.01 and 1300.01 are refused
    for typed in ["630*01", "130**0*01"] {
        let before = t.app.slot(0).vfo;
        type_in(&mut t, typed);
        menu(&mut t);
        assert!(matches!(t.app.screen(), Screen::Entry(_)), "{typed}: refused, {:?}", t.app.screen());
        assert_eq!(t.app.slot(0).vfo, before);
        t.hold(Key::Exit);
    }
}

/// VFO-22 (nicFW's clarifier): SIDE1 + ◀▶ move the active slot's receive offset 100 Hz, within ±2.5 kHz; the
/// card says CLR; a retune clears it; SIDE1's tap (monitor) doesn't fire.
#[test]
fn clarifier() {
    let mut t = T::new();
    let s = store();
    t.app.key_down(Key::Side(1), t.t, &s);
    t.tap(Key::Right);
    t.tap(Key::Right);
    t.tap(Key::Right);
    t.app.key_up(Key::Side(1), t.t, &s);
    assert_eq!(t.app.slot(0).clar, 3);
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::SlotParam { slot: 0, param: Param::Clarifier, value: 300 }), "{c:?}");
    assert!(!c.iter().any(|c| matches!(c, RadioCmd::Monitor { .. })), "no monitor latch");
    let mut b = ferrite_ui::text::Buf::<16>::new();
    crate::slot::clar_text(3, &mut b);
    assert_eq!(b.as_bytes(), b"CLR +0.3");
    crate::slot::clar_text(-12, &mut b);
    assert_eq!(b.as_bytes(), b"CLR -1.2");
    t.app.key_down(Key::Side(1), t.t, &s);
    for _ in 0..40 {
        t.tap(Key::Left);
    }
    t.app.key_up(Key::Side(1), t.t, &s);
    assert_eq!(t.app.slot(0).clar, -25, "stops at -2.5 kHz");
    t.tap(Key::Up); // a step: retuned
    assert_eq!(t.app.slot(0).clar, 0);
}

/// SQL-12.1: the squelch editor's noise floor follows the signal while the squelch is closed.
#[test]
fn squelch_noise_floor() {
    let mut t = T::new();
    let s = store();
    t.app.key_down(Key::Side(1), t.t, &s);
    t.tap(Key::Up);
    (t.live.slots[0].rssi, t.live.slots[0].rssi_raw) = (-120, -120); // SQL-12.1: the floor reads the raw RSSI
    t.wait(2000);
    let Screen::Squelch(q) = t.app.screen() else { panic!("{:?}", t.app.screen()) };
    assert!((-121..=-119).contains(&q.floor), "{}", q.floor);
    t.live.slots[0] = SlotLive { rssi: -80, rssi_raw: -80, squelch_open: true, carrier: true, ..Default::default() };
    t.wait(500);
    let Screen::Squelch(q2) = t.app.screen() else { panic!() };
    assert_eq!(q2.floor, q.floor, "an open squelch doesn't move it");
    t.app.key_up(Key::Side(1), t.t, &s);
}

/// GPS-21: a grid square as a position, at the square's centre (4 or 6 characters, either case).
#[test]
fn grid_squares() {
    use crate::screens::lists::{grid, grid_pos};
    let p = grid_pos(b"FN31pr").unwrap();
    assert_eq!((p.lat / 10_000, p.lon / 10_000), (41_729, -72_708), "{p:?}");
    assert_eq!(&grid(p), b"FN31pr");
    assert_eq!(grid_pos(b"fn31pr"), Some(p));
    let q = grid_pos(b"FN31").unwrap();
    assert_eq!((q.lat, q.lon), (415_000_000, -730_000_000), "the square's centre");
    for bad in [&b"FN3"[..], b"SN31", b"FNA1", b"FN31py", b"FN31p"] {
        assert!(grid_pos(bad).is_none(), "{bad:?}");
    }
}
