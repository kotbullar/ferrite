//! Group B's rules (manual chapters 6, 7, 12, 14, 15, 19-23): keys in, screens and Radio task commands out.

use super::*;
use crate::screens::activity::Mode;
use crate::screens::nav::Dest;
use ferrite_core::msg::{BroadcastCmd, ModeLive, XbCfg, XbLive};
use ferrite_core::txplan::{Refusal, Region};
use std::string::String;

fn ready() -> T {
    let mut t = T::new();
    t.app.radio.tx_built = true;
    t.app.set_region(Some(Region::Us), false);
    t
}

/// Tune the active slot's VFO.
fn tune(t: &mut T, f: Freq10) {
    let a = t.app.active as usize;
    t.app.slots[a].vfo = f;
    t.app.slots[a].vfo_mode = true;
}

fn ptt(t: &mut T, down: bool) {
    let s = store();
    if down {
        t.app.key_down(Key::PTT, t.t, &s);
    } else {
        t.app.key_up(Key::PTT, t.t, &s);
    }
    t.t += 50;
    t.app.tick(t.t, &t.live, &s);
}

/// MENU 0 then the digits.
fn menu0(t: &mut T, path: &[u8]) {
    t.tap(Key::Menu);
    t.tap(Key::Digit(0));
    for &d in path {
        t.tap(Key::Digit(d));
    }
}

fn refused(t: &T) -> Option<Refusal> {
    match t.app.screen() {
        Screen::TxRefused(r) => Some(r.why),
        _ => None,
    }
}

/// TX-22.2, TX-25: PTT keys the active slot and shows the Transmit screen; PTT up unkeys and goes home.
#[test]
fn ptt_keys_and_unkeys() {
    let mut t = ready();
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: true }));
    t.tap(Key::Menu); // TX-22.3: MENU does nothing while keyed
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
    ptt(&mut t, false);
    assert_eq!(t.app.screen(), Screen::Home);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }));
}

/// Hold BACK while keyed goes home, and PTT up still unkeys (TX-25).
#[test]
fn ptt_up_unkeys_wherever_the_screen_went() {
    let mut t = ready();
    ptt(&mut t, true);
    let s = store();
    t.app.key_down(Key::Exit, t.t, &s);
    t.wait(800);
    t.app.key_up(Key::Exit, t.t, &s);
    assert_eq!(t.app.screen(), Screen::Home);
    t.cmds();
    ptt(&mut t, false);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }));
}

/// TX-20.1: out of the region's band: refused with the reason and the fix; OK goes there, BACK closes.
#[test]
fn out_of_band_is_refused_with_the_fix() {
    let mut t = ready();
    t.app.set_region(Some(Region::Canada), false);
    tune(&mut t, mhz(420) + 50_000);
    ptt(&mut t, true);
    assert_eq!(refused(&t), Some(Refusal::OutOfBand));
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })), "never keyed");
    ptt(&mut t, false);
    assert_eq!(refused(&t), Some(Refusal::OutOfBand), "stays until closed");
    t.tap(Key::Menu);
    assert_eq!(t.app.screen().name(), Dest::Group(8).name(), "OK: MENU 0 8 (Extend TX is 0 8 8)");
    t.hold(Key::Exit);
    // Extend TX on: allowed (the calibration windows are the Radio task's).
    t.app.set_region(Some(Region::Canada), true);
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
    ptt(&mut t, false);
    // US: 420 is in band.
    t.app.set_region(Some(Region::Us), false);
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
}

#[test]
fn rx_only_build_and_battery_refuse() {
    let mut t = T::new();
    ptt(&mut t, true);
    assert_eq!(refused(&t), Some(Refusal::RxOnlyBuild));
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home);
    t.app.radio.tx_built = true;
    t.app.radio.batt_cut = true;
    ptt(&mut t, false);
    ptt(&mut t, true);
    assert_eq!(refused(&t), Some(Refusal::Battery));
}

/// The Radio task's own refusal (calibration, read-back) replaces the Transmit screen.
#[test]
fn radio_refusal_shows() {
    let mut t = ready();
    ptt(&mut t, true);
    t.live.tx = ferrite_core::msg::TxLive { keyed: false, slot: 0, mic: 0, refused: Some(Refusal::Locked), refusals: 1, dtmf_sent: 0, voice: false, ptt_stuck: 0 };
    t.wait(100);
    assert_eq!(refused(&t), Some(Refusal::Locked));
}

/// TX-50: an earlier refusal still in the Radio task's status doesn't end the next key-up: the screen used to read it
/// as this key-up's answer, drop its keyed state and never unkey, while the Radio task keyed (the carrier stayed up
/// to its 605 s backstop; a review of Nov 20, 2026). Only a refusal after the key-up counts, and it unkeys too.
#[test]
fn an_old_refusal_doesnt_end_the_next_key_up() {
    let mut t = ready();
    // an earlier refusal, answered and gone
    ptt(&mut t, true);
    t.live.tx = ferrite_core::msg::TxLive { keyed: false, slot: 0, refused: Some(Refusal::Battery), refusals: 1, ..Default::default() };
    t.wait(100);
    assert_eq!(refused(&t), Some(Refusal::Battery));
    ptt(&mut t, false);
    t.tap(Key::Exit);
    t.cmds();
    // PTT again; the Radio task hasn't answered yet: its status still holds the old refusal
    ptt(&mut t, true);
    t.wait(100);
    assert_eq!(refused(&t), None, "the old refusal isn't this key-up's");
    assert!(t.app.radio.keyed.is_some(), "still keyed");
    // let go: the unkey goes
    t.cmds();
    ptt(&mut t, false);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }), "PTT up unkeys");
    // a refusal after the key-up still ends it, with an unkey in case the Radio task keyed
    ptt(&mut t, true);
    t.cmds();
    t.live.tx.refusals = 2;
    t.wait(100);
    assert_eq!(refused(&t), Some(Refusal::Battery));
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }), "unkeyed all the same");
}

/// TX-50: the refusal count wraps (u8): a refusal after the key-up is still taken when the count went past 255.
#[test]
fn a_refusal_counted_past_255_is_taken() {
    let mut t = ready();
    t.live.tx.refusals = 255;
    t.wait(50);
    ptt(&mut t, true);
    assert_eq!(t.app.radio.refusals_at_key, 255);
    t.live.tx = ferrite_core::msg::TxLive { keyed: false, slot: 0, refused: Some(Refusal::Battery), refusals: 0, ..Default::default() };
    t.wait(100);
    assert_eq!(refused(&t), Some(Refusal::Battery));
}

/// TX-50: a refusal for another slot doesn't end this one's key-up.
#[test]
fn another_slots_refusal_is_not_this_ones() {
    let mut t = ready();
    ptt(&mut t, true);
    t.live.tx = ferrite_core::msg::TxLive { keyed: false, slot: 2, refused: Some(Refusal::Battery), refusals: 1, ..Default::default() };
    t.wait(100);
    assert_eq!(refused(&t), None);
    assert!(t.app.radio.keyed.is_some());
}

/// TX-50: PTT again from the refused screen is a new key-up: the refusal it showed isn't taken again for it.
#[test]
fn ptt_again_from_the_refused_screen_starts_over() {
    let mut t = ready();
    ptt(&mut t, true);
    t.live.tx = ferrite_core::msg::TxLive { keyed: false, slot: 0, refused: Some(Refusal::Hot), refusals: 1, ..Default::default() };
    t.wait(100);
    assert_eq!(refused(&t), Some(Refusal::Hot));
    ptt(&mut t, false);
    ptt(&mut t, true); // PTT again tries again (TxRefused's ptt)
    t.wait(100);
    assert_eq!(refused(&t), None, "the old refusal isn't this key-up's");
    assert!(t.app.radio.keyed.is_some());
}

/// TX-30.3: unkey at the timeout (0 8 1, 120 s by default), home with a toast.
#[test]
fn timeout_unkeys() {
    let mut t = ready();
    ptt(&mut t, true);
    t.cmds();
    t.wait(120_000 + 200);
    assert_eq!(t.app.screen(), Screen::Home);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }));
}

/// TX-30.3, TX-30.4 (TX audit F10): 0 8 1 sets the timeout the UI unkeys at, and goes to the Radio task for its
/// backstop.
#[test]
fn timeout_follows_the_setting() {
    let mut t = ready();
    t.app.set.tx_timeout = 1; // 30 s
    t.app.send_radio_settings();
    assert!(t.cmds().contains(&RadioCmd::TxTimeout(30)), "the Radio task's backstop follows");
    ptt(&mut t, true);
    t.cmds();
    t.wait(29_000);
    assert!(!t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }), "not before the setting");
    t.wait(1_200);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }));
}

/// TX-3.1, TX-3.2: on an emergency frequency the first PTT only prompts; a second within 5 s transmits.
#[test]
fn emergency_frequency_needs_ptt_twice() {
    let mut t = ready();
    t.app.set_region(Some(Region::Us), true);
    tune(&mut t, mhz(406) + 2_500); // EPIRB (243.0: band_125m_and_hf_key_as_stock)
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Emergency(_)));
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })));
    ptt(&mut t, false);
    assert!(matches!(t.app.screen(), Screen::Emergency(_)), "the first release does nothing");
    t.wait(2000);
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::Transmit { slot: 0, on: true }));
    // TX-35.6, TX audit F11: the plan says a person confirmed it, for the Radio task's own check.
    assert!(c.iter().any(|c| matches!(c, RadioCmd::TxPlan { confirmed: true, .. })));
    ptt(&mut t, false);
    // Unanswered: home after 5 s.
    ptt(&mut t, true);
    ptt(&mut t, false);
    t.wait(5200);
    assert_eq!(t.app.screen(), Screen::Home);
}

/// APRS-21.3, TX-35.7, TX-3: while C is the APRS slot, voice PTT on A on the APRS frequency only prompts ("APRS frequency:
/// data only"); a second PTT within 5 s transmits, unanswered it goes home. The power chip keeps its level (a
/// passing gate, not "No TX", HOME-4.2). With APRS off, or A elsewhere, PTT keys at once. VOX never keys there.
#[test]
fn aprs_frequency_voice_needs_ptt_twice() {
    let mut t = ready();
    t.app.set_aprs_mode(true);
    let f = t.app.aprs_freq();
    tune(&mut t, f);
    assert!(t.app.aprs_voice_gate(0) && !t.app.aprs_voice_gate(2), "A gated, C (the APRS slot) not");
    assert_ne!(crate::screens::transmit::power_name(&t.app, 0), "No TX", "a passing gate: the level stays");
    t.cmds();
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Emergency(e) if e.aprs), "{:?}", t.app.screen());
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })));
    ptt(&mut t, false);
    assert!(matches!(t.app.screen(), Screen::Emergency(_)), "the first release does nothing");
    t.wait(2000);
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: true }));
    ptt(&mut t, false);
    ptt(&mut t, true);
    ptt(&mut t, false);
    t.wait(5200);
    assert_eq!(t.app.screen(), Screen::Home, "unanswered: home after 5 s");
    // Another frequency: no prompt.
    tune(&mut t, mhz(146) + 52_000);
    t.cmds();
    ptt(&mut t, true);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: true }), "146.52: keys at once");
    ptt(&mut t, false);
    t.wait(500);
    // APRS off: the APRS frequency is an ordinary one.
    t.app.set_aprs_mode(false);
    tune(&mut t, f);
    assert!(!t.app.aprs_voice_gate(0));
    t.cmds();
    ptt(&mut t, true);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: true }), "APRS off: keys at once");
    ptt(&mut t, false);
    t.wait(500);
    // VOX there: a toast, never a key-up.
    t.app.set_aprs_mode(true);
    crate::screens::groups::change(&mut t.app, &[8, 2, 1], 1);
    t.cmds();
    for _ in 0..3 {
        t.live.tx.voice = true;
        t.wait(200);
        t.live.tx.voice = false;
        t.wait(200);
    }
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })));
    assert!(t.app.toast.is_some_and(|x| x.body.as_bytes() == b"APRS frequency"));
    // The prompt's words fit: the title on the dialog's bar (210 px), the text in its two lines (204 px each).
    use ferrite_ui::gfx::text4_width;
    use ferrite_ui::assets::{UI_12, UI_12B};
    assert!(text4_width(b"APRS frequency: data only", &UI_12B) <= 210);
    assert!(text4_width(b"144.390 MHz carries APRS packets; voice blocks them.", &UI_12) <= 2 * 204 - 40);
}

/// APRS-21.2: A on the APRS frequency while C is the APRS slot reads "APRS on C" until C receives, then RX; on
/// another frequency, or with APRS off, its own state.
#[test]
fn a_slot_on_the_aprs_frequency_says_aprs_on_c() {
    let mut t = ready();
    t.app.set_aprs_mode(true);
    let f = t.app.aprs_freq();
    tune(&mut t, f);
    assert!(t.app.defers_to_c(0) && !t.app.defers_to_c(2));
    assert_eq!(t.app.state(0, &t.live).0, "APRS on C");
    t.live.slots[0].squelch_open = true;
    assert_eq!(t.app.state(0, &t.live).0, "RX", "C receives: A shows it");
    t.live.slots[0].squelch_open = false;
    t.app.set_aprs_mode(false);
    assert!(!t.app.defers_to_c(0));
    assert_ne!(t.app.state(0, &t.live).0, "APRS on C");
    t.app.set_aprs_mode(true);
    tune(&mut t, mhz(146) + 52_000);
    assert_ne!(t.app.state(0, &t.live).0, "APRS on C");
}

/// TX-35.2, TX-35.3, TX-35.5: automatic transmissions are refused on an emergency frequency, with a toast.
#[test]
fn automatic_tx_refused_on_emergency() {
    let mut t = ready();
    t.app.set_region(Some(Region::Us), true);
    t.app.set_callsign(b"VA3ABC");
    assert!(crate::screens::transmit::auto_tx(&mut t.app, 0, b"Beacon"));
    tune(&mut t, mhz(406) + 2_500);
    assert!(!crate::screens::transmit::auto_tx(&mut t.app, 0, b"Beacon"));
    assert!(t.app.toast.is_some_and(|x| x.body.as_bytes() == b"emergency frequency"));
}

/// TX-12.1: the level name only, 1.25 m too (TX-9: always LOW there).
#[test]
fn power_level_names() {
    let mut t = ready();
    assert_eq!(crate::screens::transmit::power_name(&t.app, 0), "LOW");
    tune(&mut t, mhz(223) + 50_000);
    assert_eq!(crate::screens::transmit::power_name(&t.app, 0), "LOW");
}

/// HOME-4.2: where PTT can never transmit (out of the band plan, AM, a receive-only build) the power chip says
/// "No TX" and hold 2 says why instead of changing the power; a passing refusal (busy) keeps the level. The UI
/// readback (LINK-19) reports the chip as drawn.
#[test]
fn rx_only_power_chip() {
    use crate::screens::transmit::{power_name, tx_block};
    let mut t = ready();
    assert_eq!(power_name(&t.app, 0), "LOW");
    tune(&mut t, mhz(121) + 50_000); // airband: AM, outside the plan
    assert!(tx_block(&t.app, 0).is_some());
    assert_eq!(power_name(&t.app, 0), "No TX");
    assert!(line_of(&t, "card A").contains("power=\"No TX\""), "the readback says what the chip shows");
    let p = t.app.slot(0).cfg().power;
    t.hold(Key::Digit(2));
    assert_eq!(t.app.slot(0).cfg().power, p, "hold 2 changes nothing there");
    assert!(t.app.toast.is_some_and(|x| x.head.as_bytes() == b"No TX"), "it says why");
    tune(&mut t, mhz(146) + 52_000);
    assert_eq!(power_name(&t.app, 0), "LOW", "back in the plan");
    t.app.radio.tx_built = false;
    assert_eq!(power_name(&t.app, 0), "No TX", "a receive-only build");
    assert!(ferrite_ui::gfx::text4_width(b"No TX", &ferrite_ui::assets::UI_12) <= ferrite_ui::widgets::SLOT_POWER.1 - 6, "fits the chip: {}", ferrite_ui::gfx::text4_width(b"No TX", &ferrite_ui::assets::UI_12));
}

/// SCAN-1, SCAN-11: SIDE2 starts and shows the scan; * skips a hit; SIDE2 stops.
#[test]
fn scan_screen() {
    let mut t = T::new();
    t.tap(Key::Side(2));
    assert!(matches!(t.app.screen(), Screen::Scan(_)));
    assert!(t.cmds().contains(&RadioCmd::Scan { slot: 0, on: true }));
    t.live.slots[0] = SlotLive { freq: mhz(146) + 94_000, scanning: true, squelch_open: true, carrier: true, rssi: -80, hits: 1, ..Default::default() };
    t.wait(100);
    assert_eq!(t.app.radio.hits[0].what, mhz(146) + 94_000, "the hit is listed");
    t.tap(Key::Star);
    assert!(t.cmds().contains(&RadioCmd::ScanSkip { slot: 0 }));
    t.tap(Key::Side(2));
    assert_eq!(t.app.screen(), Screen::Home);
    assert!(t.cmds().contains(&RadioCmd::Scan { slot: 0, on: false }));
}

/// SCAN-23 (nicFW's smart scan list): a frequency scan counts each stop; OK lists the hits; OK there tunes the
/// VFO, hold 9 saves it (KEY-23); hold V/M does nothing there.
#[test]
fn scan_hit_list() {
    let mut t = T::new();
    t.tap(Key::Side(2));
    let (a, b) = (mhz(146) + 94_000, mhz(147) + 30_000);
    for (n, f) in [(1, a), (2, b), (3, a), (4, a)] {
        t.live.slots[0] = SlotLive { freq: f, last_hit: f, scanning: true, squelch_open: true, carrier: true, rssi: -80, hits: n, ..Default::default() };
        t.wait(100);
        t.live.slots[0].squelch_open = false;
        t.wait(100);
    }
    let h = t.app.radio.hits;
    assert_eq!((h[0].what, h[0].n, h[1].what, h[1].n, h[2].what), (a, 3, b, 1, 0), "a stopped it three times");
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Scan(s) if s.list));
    t.tap(Key::Down);
    t.cmds();
    t.tap(Key::Menu);
    assert_eq!((t.app.screen(), t.freq()), (Screen::Home, b), "OK: the VFO on the second hit");
    assert!(t.cmds().contains(&RadioCmd::Scan { slot: 0, on: false }));
    // hold 9 on a hit: the save form; hold V/M: nothing
    t.live.slots[0] = SlotLive { freq: b, ..Default::default() };
    t.wait(100);
    t.tap(Key::Side(2));
    t.live.slots[0] = SlotLive { freq: a, last_hit: a, scanning: true, hits: 1, ..Default::default() };
    t.wait(100);
    t.tap(Key::Menu);
    let list = t.app.screen();
    t.hold(Key::VfoMem);
    assert_eq!(t.app.screen(), list, "hold V/M: still the list");
    t.hold(Key::Digit(9));
    assert_eq!((t.app.screen().name(), t.freq()), ("Save as channel", a));
}

/// SCAN-16: ▲▼ set the direction; stopped on a hit they leave it at once.
#[test]
fn scan_direction() {
    let mut t = T::new();
    t.tap(Key::Side(2));
    t.live.slots[0] = SlotLive { freq: mhz(146) + 50_000, scanning: true, ..Default::default() };
    t.wait(100);
    t.cmds();
    t.tap(Key::Down);
    assert!(t.cmds().contains(&RadioCmd::ScanDir { slot: 0, up: false, resume: false }));
    t.live.slots[0] = SlotLive { freq: mhz(146) + 94_000, scanning: true, held: true, squelch_open: true, carrier: true, rssi: -80, ..Default::default() };
    t.wait(100);
    t.tap(Key::Up);
    assert!(t.cmds().contains(&RadioCmd::ScanDir { slot: 0, up: true, resume: true }));
    assert_eq!(t.freq(), mhz(146) + 52_000, "the arrows don't tune the slot while it scans");
}

/// SCAN-17: # on a channel scan's hit sets that channel's Skip flag for good and goes on; a band scan says why
/// it can't.
#[test]
fn scan_skip_for_good() {
    let mut t = T::new();
    let st = store();
    let z = (0..16).find(|&z| st.zone_len(z) > 1).unwrap();
    t.app.set_channel(0, z, 0, &st);
    let c = st.zone_at(z, 1).unwrap();
    t.tap(Key::Side(2));
    t.live.slots[0] = SlotLive { freq: c.rx, scanning: true, held: true, squelch_open: true, carrier: true, rssi: -80, ..Default::default() };
    t.wait(100);
    t.cmds();
    t.tap(Key::Hash);
    match t.app.pop_write() {
        Some(ChanWrite::Put { ch, .. }) => assert!(ch.num == c.num && ch.flags & crate::chan::FLAG_SKIP != 0, "{ch:?}"),
        w => panic!("{w:?}"),
    }
    assert!(t.cmds().contains(&RadioCmd::ScanSkip { slot: 0 }));
    // A band scan (VFO) keeps no list.
    let mut t = T::new();
    t.tap(Key::Side(2));
    t.live.slots[0] = SlotLive { freq: mhz(146) + 94_000, scanning: true, held: true, squelch_open: true, carrier: true, ..Default::default() };
    t.wait(100);
    t.tap(Key::Hash);
    assert!(t.app.pop_write().is_none());
    assert!(t.app.toast.is_some_and(|x| x.head.as_bytes() == b"No skip list"));
}

/// SCAN-19: PTT while stopped on a hit ends the scan and transmits there; not on a hit, on the slot's own
/// frequency.
#[test]
fn ptt_while_scanning() {
    let mut t = ready();
    let f0 = t.freq();
    t.tap(Key::Side(2));
    t.live.slots[0] = SlotLive { freq: mhz(146) + 94_000, scanning: true, held: true, squelch_open: true, carrier: true, rssi: -80, ..Default::default() };
    t.wait(100);
    t.cmds();
    ptt(&mut t, true);
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::Scan { slot: 0, on: false }), "{c:?}");
    assert!(c.iter().any(|c| matches!(c, RadioCmd::Transmit { slot: 0, on: true })), "{c:?}");
    assert_eq!(t.freq(), mhz(146) + 94_000, "on the hit");
    ptt(&mut t, false);
    let mut t = ready();
    t.tap(Key::Side(2));
    t.live.slots[0] = SlotLive { freq: mhz(147), scanning: true, ..Default::default() };
    t.wait(100);
    ptt(&mut t, true);
    assert_eq!(t.freq(), f0, "not on a hit: where the scan started");
}

/// SCOPE-1, SCOPE-3: hold 1; the knob moves the centre; OK tunes to the marker; leaving stops the sweep.
#[test]
fn band_scope() {
    let mut t = T::new();
    t.hold(Key::Digit(1));
    assert!(matches!(t.app.screen(), Screen::Scope(_)));
    let f = t.freq();
    assert!(t.cmds().contains(&RadioCmd::Sweep { centre: f, step: 2500, on: true }));
    t.knob(2);
    assert!(t.cmds().contains(&RadioCmd::Sweep { centre: f + 5000, step: 2500, on: true }));
    t.tap(Key::Right);
    t.tap(Key::Menu);
    assert_eq!(t.app.screen(), Screen::Home);
    assert_eq!(t.freq(), f + 5000 + 2500, "tuned to the marker");
    assert!(t.cmds().contains(&RadioCmd::Sweep { centre: 0, step: 0, on: false }));
    t.hold(Key::Digit(1));
    t.cmds();
    t.tap(Key::Exit);
    assert!(t.cmds().contains(&RadioCmd::Sweep { centre: 0, step: 0, on: false }));
}

/// TSRCH-1 to TSRCH-5: OK sets RX, a second OK TX too; BACK leaves the tones alone.
#[test]
fn tone_search() {
    let mut t = T::new();
    t.open(Dest::ToneSearch);
    assert!(matches!(t.app.screen(), Screen::ToneSearch(_)));
    assert!(t.cmds().contains(&RadioCmd::ToneSearch { slot: 0, on: true }));
    t.tap(Key::Exit); // nothing found: BACK leaves
    assert_eq!(t.app.screen(), Screen::Home);
    assert!(t.cmds().contains(&RadioCmd::ToneSearch { slot: 0, on: false }));
    t.open(Dest::ToneSearch);
    t.live.mode = ModeLive::Probe { freq: 0, rssi: -80, tone: 1000 };
    t.wait(100);
    t.tap(Key::Menu);
    assert_eq!(t.app.slot(0).cfg().rx_tone, 1000);
    assert_eq!(t.app.slot(0).cfg().tx_tone, 0, "TX only after the second confirm");
    t.tap(Key::Menu);
    assert_eq!(t.app.slot(0).cfg().tx_tone, 1000);
    assert_eq!(t.app.screen(), Screen::Home);
    // BACK at the question: RX only.
    t.live.mode = ModeLive::Probe { freq: 0, rssi: -80, tone: 885 };
    t.open(Dest::ToneSearch);
    t.wait(100);
    t.tap(Key::Menu);
    t.tap(Key::Exit);
    assert_eq!((t.app.slot(0).cfg().rx_tone, t.app.slot(0).cfg().tx_tone), (885, 1000));
}

/// TSRCH-9: on a channel the tone is loaded into the slot, not written to the channel.
#[test]
fn tone_search_on_a_channel_is_not_saved() {
    let mut t = T::new();
    t.tap(Key::VfoMem);
    assert!(t.app.slot(0).on_channel());
    t.live.mode = ModeLive::Probe { freq: 0, rssi: -80, tone: 1000 };
    t.open(Dest::ToneSearch);
    t.wait(100);
    t.tap(Key::Menu);
    t.tap(Key::Exit);
    assert_eq!(t.app.slot(0).cfg().rx_tone, 1000, "loaded");
    assert_eq!(store().zone_at(0, 0).unwrap().cfg.rx_tone, 0, "the channel record unchanged");
    assert!(t.app.toast.is_some_and(|x| x.body.as_bytes() == b"loaded, not saved"));
}

/// TSRCH-6, TSRCH-7: MENU 0 3 2; OK copies to the VFO.
#[test]
fn frequency_counter() {
    let mut t = T::new();
    menu0(&mut t, &[3, 2]);
    assert!(matches!(t.app.screen(), Screen::Counter(_)));
    assert!(t.cmds().contains(&RadioCmd::Counter(true)));
    t.live.mode = ModeLive::Probe { freq: mhz(147) + 15_000, rssi: -40, tone: 0 };
    t.wait(100);
    t.tap(Key::Menu);
    assert_eq!(t.app.screen(), Screen::Home);
    assert_eq!(t.freq(), mhz(147) + 15_000);
    assert!(t.cmds().contains(&RadioCmd::Counter(false)), "TSRCH-8: it only ever received");
}

/// TSRCH-10.1, TSRCH-10.2, TSRCH-11: only 64–1300 MHz outside 630–756 counts; # freezes the reading while the list
/// keeps filling, and OK copies the held frequency.
#[test]
fn counter_range_and_hold() {
    let mut t = T::new();
    menu0(&mut t, &[3, 2]);
    for f in [mhz(28) + 40_000, mhz(700), mhz(1_400)] {
        t.live.mode = ModeLive::Probe { freq: f, rssi: -40, tone: 0 };
        t.wait(100);
    }
    assert_eq!(t.app.radio.hits[0].what, 0, "nothing outside the range counts");
    t.live.mode = ModeLive::Probe { freq: mhz(1_296), rssi: -60, tone: 0 };
    t.wait(100);
    t.tap(Key::Hash);
    assert!(matches!(t.app.screen(), Screen::Counter(c) if c.hold && c.held == mhz(1_296)));
    t.live.mode = ModeLive::Probe { freq: mhz(446) + 6_250, rssi: -40, tone: 0 };
    t.wait(100);
    assert_eq!(t.app.radio.hits[0].what, mhz(446) + 6_250, "held, it keeps listening");
    t.tap(Key::Menu);
    assert_eq!(t.freq(), mhz(1_296), "OK copies the held reading");
}

/// HOME-8: hold 0 puts the active slot on the home channel at the long press; the toast names the channel.
#[test]
fn hold_0_home_channel_toast() {
    let mut t = T::new();
    let s = store();
    let n = s.by_number(0).expect("a channel").num;
    t.app.set.home_ch = Some(n);
    t.app.key_down(Key::Digit(0), t.t, &s);
    t.wait(800);
    assert_eq!(t.app.screen(), Screen::Home);
    assert!(t.app.slot(0).on_channel());
    let toast = t.app.toast.expect("a toast");
    assert_eq!(toast.head.as_bytes(), b"Home channel");
    assert!(!toast.body.is_empty());
    t.app.key_up(Key::Digit(0), t.t, &s);
}

/// SQL-7.1: SIDE1 held past its long press (the hold's monitor) then the knob: the monitor ends as the level is
/// set, so the squelch opens and closes with it (a review of Nov 20, 2026: it stayed open, "you hear it" at any
/// level). A monitor latched with a tap stays on.
#[test]
fn setting_the_level_ends_the_holds_monitor() {
    let mut t = T::new();
    let s = store();
    t.app.key_down(Key::Side(1), t.t, &s);
    t.wait(800); // past the long press: the hold's monitor
    assert!(t.cmds().contains(&RadioCmd::Monitor { slot: 0, on: true }), "monitor while held");
    t.knob(1);
    assert!(matches!(t.app.screen(), Screen::Squelch(_)));
    assert!(t.cmds().contains(&RadioCmd::Monitor { slot: 0, on: false }), "ended by the knob");
    t.app.key_up(Key::Side(1), t.t, &s);
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Monitor { .. })), "nothing more at the release");
    // latched with a tap first: stays on through the knob
    t.app.key_down(Key::Side(1), t.t, &s);
    t.app.key_up(Key::Side(1), t.t, &s);
    t.cmds();
    t.app.key_down(Key::Side(1), t.t, &s);
    t.knob(1);
    assert!(!t.cmds().contains(&RadioCmd::Monitor { slot: 0, on: false }), "a latched monitor stays");
    t.app.key_up(Key::Side(1), t.t, &s);
}

/// SQL-10: SIDE1 + knob shows the squelch screen while SIDE1 is held.
#[test]
fn squelch_screen_while_side1_is_held() {
    let mut t = T::new();
    let s = store();
    t.app.key_down(Key::Side(1), t.t, &s);
    t.knob(1);
    assert!(matches!(t.app.screen(), Screen::Squelch(_)));
    assert_eq!(t.app.slot(0).cfg().squelch, 5);
    t.app.key_up(Key::Side(1), t.t + 300, &s);
    t.wait(100);
    assert_eq!(t.app.screen(), Screen::Home);
}

/// HF-1, HF-5, HF-24, HF-4.
#[test]
fn hf_mode() {
    let mut t = ready();
    t.app.set_region(Some(Region::Us), false);
    menu0(&mut t, &[3, 3]);
    assert!(matches!(t.app.screen(), Screen::Hf(_)));
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::Hf { on: true, .. })));
    let mut fb = FrameBuffer::new();
    t.render(&mut fb);
    t.app.radio.tx_built = true; // the test's Device is a receive-only build
    let sb = t.app.status_bar(&t.live, &Device::default());
    assert_eq!((sb.watch, sb.scan), (0, false), "HF-5: no TW or SCN");
    ptt(&mut t, true);
    assert_eq!(refused(&t), Some(Refusal::ReceiveOnlyHere), "HF-24: SI4732 frequency");
    t.tap(Key::Exit);
    assert!(matches!(t.app.screen(), Screen::Hf(_)), "BACK returns to HF mode");
    ptt(&mut t, false);
    // 10 m FM on chip 1 transmits from HF mode, without going home (HF-23).
    t.app.set.hf.freq = mhz(29) + 60_000;
    t.app.set.hf.mode = crate::screens::hf::FM;
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)), "{:?}", t.app.screen());
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 3, on: true }));
    ptt(&mut t, false);
    assert!(matches!(t.app.screen(), Screen::Hf(_)), "release: back to HF mode");
    t.tap(Key::Exit);
    assert!(matches!(t.app.screen(), Screen::Group(_)), "BACK: one level, to the menu");
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::Hf { on: false, .. })));
}

/// VFO-24: \* after a digit past the point is OK on every frequency entry: split TX (OK sets it) and HF mode's
/// own typing (OK tunes).
#[test]
fn star_enters_split_tx_and_hf() {
    use crate::screens::entry::EntryFor;
    let mut t = ready();
    tune(&mut t, 44_600_000);
    t.app.change(Item::Offset, 3); // Split
    assert!(t.app.slot(0).cfg().split);
    t.app.open(crate::screens::nav::Dest::Value(Item::Offset));
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Entry(e) if e.to == EntryFor::SplitTx), "{:?}", t.app.screen());
    // prefilled "446." with nothing after the point: * types nothing; BACK deletes it, then 146 * 52 *
    while matches!(t.app.screen(), Screen::Entry(e) if e.n > 0) {
        t.tap(Key::Exit);
    }
    for k in [Key::Digit(1), Key::Digit(4), Key::Digit(6), Key::Star, Key::Digit(5), Key::Digit(2), Key::Star] {
        t.tap(k);
    }
    assert!(matches!(t.app.screen(), Screen::Value(_)), "* set it, as OK: {:?}", t.app.screen());
    assert_eq!(super::super::screens::transmit::tx_freq(&t.app, 0), 14_652_000);
    // HF mode: 7 * 074 * tunes 7.074 MHz
    t.app.set_region(Some(Region::Us), false);
    t.app.go_home();
    menu0(&mut t, &[3, 3]);
    t.wait(100);
    assert!(matches!(t.app.screen(), Screen::Hf(h) if h.loaded), "{:?}", t.app.screen());
    for k in [Key::Digit(7), Key::Star, Key::Star, Key::Digit(0), Key::Digit(7), Key::Digit(4), Key::Star] {
        t.tap(k);
    }
    assert!(matches!(t.app.screen(), Screen::Hf(h) if h.n == 0), "{:?}", t.app.screen());
    assert_eq!(t.app.set.hf.freq, 707_400);
}

/// WX-4, WX-6, WX-7: an alert opens over any screen; OK silences, then opens the log; BACK home.
#[test]
fn alert_screen() {
    let mut t = T::new();
    t.tap(Key::Menu);
    t.app.notify(b"Tornado warning", b"Ottawa", true);
    assert!(matches!(t.app.screen(), Screen::Alert(_)));
    assert!(matches!(t.app.screen(), Screen::Alert(a) if !a.silenced));
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Alert(a) if a.silenced));
    assert!(matches!(t.app.screen(), Screen::Alert(_)));
    t.tap(Key::Menu);
    assert_eq!(t.app.screen().name(), "Event log");
}

fn xb_ready(t: &mut T) {
    t.app.set_region(Some(Region::Us), false);
    t.app.set_callsign(b"VA3ABC");
    t.app.open(Dest::Crossband);
}

/// XB-2: refuses saying why; XB-14: BACK stops, local PTT doesn't transmit; leaving stops it.
#[test]
fn crossband_repeater() {
    let mut t = ready();
    assert_eq!(t.app.set.xb.flags & XbCfg::ID, 0, "ID off by default (decision 94)");
    t.app.set.xb.flags |= XbCfg::ID;
    t.app.open(Dest::Crossband);
    t.tap(Key::Hash);
    assert_ne!(t.app.radio.mode, Mode::Xb, "ID on with no callsign: refused");
    assert!(t.app.toast.is_some());
    xb_ready(&mut t);
    t.knob(1); // side 1: A -> C, both on chip 0 with B? C is 118.950: VHF chip, B is UHF: fine
    t.knob(-1); // back to A
    t.tap(Key::Hash);
    assert_eq!(t.app.radio.mode, Mode::Xb, "{:?}", t.app.toast.map(|x| std::string::String::from_utf8_lossy(x.body.as_bytes()).into_owned()));
    assert!(t.cmds().contains(&RadioCmd::Crossband(Some(t.app.set.xb))));
    ptt(&mut t, true);
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })), "XB-14: local PTT talks");
    assert_eq!(t.app.radio.mode, Mode::Xb, "the repeater keeps running");
    ptt(&mut t, false);
    t.wait(5200);
    assert!(matches!(t.app.screen(), Screen::Crossband(_)), "back to the running repeater: {:?}", t.app.screen());
    assert_eq!(t.app.radio.mode, Mode::Xb);
    t.live.mode = ModeLive::Xb(XbLive { state: 1, pass_s: 12, ..Default::default() });
    let mut fb = FrameBuffer::new();
    t.render(&mut fb);
    t.app.radio.tx_built = true; // the test's Device is a receive-only build
    t.tap(Key::Exit);
    assert_ne!(t.app.radio.mode, Mode::Xb, "BACK stops");
    assert!(t.cmds().contains(&RadioCmd::Crossband(None)));
    // Hold BACK while running stops it too.
    t.tap(Key::Hash);
    assert_eq!(t.app.radio.mode, Mode::Xb);
    t.hold(Key::Exit);
    assert!(t.cmds().contains(&RadioCmd::Crossband(None)));
    // Same chip on both sides: refused.
    xb_ready(&mut t);
    t.tap(Key::Right);
    t.knob(1); // side 2: B -> C (118.950, chip 0 like A)
    t.tap(Key::Hash);
    assert_ne!(t.app.radio.mode, Mode::Xb, "both sides on one chip");
}

/// HOME-3: the other two layouts draw, keep home's keys, and record activity.
#[test]
fn home_layouts() {
    let mut t = T::new();
    let mut fb = FrameBuffer::new();
    t.live.slots[0] = SlotLive { freq: t.freq(), squelch_open: true, carrier: true, rssi: -85, ..Default::default() };
    for l in [1, 2] {
        t.app.set_layout(l);
        t.wait(3000);
        t.render(&mut fb);
        assert!(count_px(&fb, 20, 300, |c| c != BG) > 3000, "layout {l} drew");
        t.tap(Key::Up);
        assert_eq!(t.freq(), mhz(146) + 53_250, "keys unchanged");
        t.tap(Key::Down);
    }
    assert!(t.app.radio.act[0] & 0b11 != 0, "slot A heard in the last buckets");
    assert_eq!(t.app.radio.act[1], 0);
    t.app.set_layout(1);
    t.wait(600); // MODE-4: the strip waits for the frequency to rest 500 ms after the Up/Down above
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::Sweep { on: true, .. })), "the strip sweeps");
}

/// Every group B screen draws, with live data in.
#[test]
fn radio_screens_draw() {
    let mut t = ready();
    let mut fb = FrameBuffer::new();
    t.app.notify(b"Severe thunderstorm warning", b"Ottawa, Gatineau", true);
    for d in [Dest::Transmit, Dest::TxRefused, Dest::Emergency, Dest::Scan, Dest::BandScope, Dest::ToneSearch, Dest::Counter, Dest::HfMode, Dest::Alert, Dest::Crossband, Dest::Squelch, Dest::ShortcutMap] {
        t.hold(Key::Exit);
        t.render(&mut fb);
        t.app.open(d);
        t.render(&mut fb);
        assert!(count_px(&fb, 20, 300, |c| c != BG) > 500, "{d:?} drew little");
    }
}

/// The text fonts hold every character group B's screens draw.
#[test]
fn fonts_cover_group_b() {
    use ferrite_ui::assets::{UI_12, UI_12B};
    let sources = [
        include_str!("../screens/activity.rs"),
        include_str!("../screens/parts.rs"),
        include_str!("../screens/transmit.rs"),
        include_str!("../screens/scan.rs"),
        include_str!("../screens/scope.rs"),
        include_str!("../screens/probe.rs"),
        include_str!("../screens/fm.rs"),
        include_str!("../screens/hf.rs"),
        include_str!("../screens/xb.rs"),
        include_str!("../screens/squelch.rs"),
        include_str!("../screens/shortcut.rs"),
        include_str!("../screens/layouts.rs"),
        include_str!("../../../ferrite-core/src/txplan.rs"),
    ];
    for src in sources {
        for s in string_literals(src) {
            for c in s.chars().filter(|c| !('\x18'..='\x1b').contains(c) && *c != '\n') {
                assert!(UI_12.has(c) && UI_12B.has(c), "{c:?} in {s:?} is not in the text fonts");
            }
        }
    }
}

/// VOX-12, XB-12: a callsign typed on setup's callsign page (MENU 0 8 6 1) reaches the Radio task for CW ID at
/// once (`CwCall`), as the text editor's does; before, it went only at power-on (the crossband CW ID sent nothing
/// until a restart, Oct 15, 2026). Each change goes again; nothing changed, nothing sent.
#[test]
fn callsign_change_reaches_the_radio() {
    let mut t = ready();
    t.app.set.callsign = crate::settings::Text::EMPTY;
    t.wait(200);
    t.cmds();
    crate::screens::setup::callsign(&mut t.app);
    t.tap(Key::Digit(2)); // A (multi-tap, KEY-12)
    t.wait(1_500);
    t.tap(Key::Digit(3)); // D
    t.wait(200);
    assert_eq!(t.app.set.callsign.as_bytes(), b"AD");
    let calls: Vec<RadioCmd> = t.cmds().into_iter().filter(|c| matches!(c, RadioCmd::CwCall { .. })).collect();
    assert_eq!(calls.last(), Some(&RadioCmd::CwCall { chunk: 1, text: [0; 5] }), "{calls:?}");
    assert!(calls.contains(&RadioCmd::CwCall { chunk: 0, text: [b'A', b'D', 0, 0, 0] }), "{calls:?}");
    t.wait(500);
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::CwCall { .. })), "unchanged: nothing sent again");
}

/// A transmit refused for no callsign: OK opens just the callsign page (not the whole setup), and leaving it
/// returns to where the refusal came over.
#[test]
fn no_callsign_fix_is_the_callsign_page() {
    use crate::screens::setup::Setup;
    let mut t = ready();
    crate::screens::transmit::refuse(&mut t.app, 0, Refusal::NoCallsign);
    t.tap(Key::Menu);
    assert_eq!(t.app.screen(), Screen::Setup(Setup::single(1)), "the callsign page alone");
    t.tap(Key::Exit);
    assert_eq!(t.app.screen(), Screen::Home, "back where it was");
}

/// TX-9, TX-10.4, TX-10.5, TX-3 (decision 224: as stock): 1.25 m keys, Low whatever the slot's level (decision 260); 243.0
/// MHz (Extend TX) takes the emergency double PTT; 6 m keys in the US, CB only with Extend TX (TX-4, TX-41).
#[test]
fn band_125m_and_hf_key_as_stock() {
    let mut t = ready();
    t.app.slots[0].cfg.power = 2;
    tune(&mut t, mhz(223) + 50_000);
    assert_eq!(crate::screens::transmit::power_name(&t.app, 0), "LOW", "1.25 m keys Low, High chosen");
    t.cmds();
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)), "{:?}", t.app.screen());
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::Transmit { slot: 0, on: true }));
    ptt(&mut t, false);
    t.hold(Key::Exit);
    // 6 m in the US: allowed without Extend TX; CB refused without it.
    tune(&mut t, mhz(52) + 52_500);
    assert_eq!(crate::screens::transmit::refusal(&t.app, 0), None);
    tune(&mut t, mhz(27) + 18_500);
    assert_eq!(crate::screens::transmit::refusal(&t.app, 0), Some(Refusal::OutOfBand));
    // Extend TX: CB opens; 243.0 MHz prompts for the second PTT.
    t.app.set_region(Some(Region::Us), true);
    assert_eq!(crate::screens::transmit::refusal(&t.app, 0), None, "CB with Extend TX, as stock");
    tune(&mut t, mhz(243));
    t.cmds();
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Emergency(_)), "{:?}", t.app.screen());
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })), "the first PTT only prompts");
    ptt(&mut t, false);
}

/// TX-20.1: a slot in AM refuses PTT ("AM is receive only": no AM modulator); the fix is MENU 3, Mode.
#[test]
fn am_is_receive_only() {
    let mut t = ready();
    let mut cfg = DEFAULT_CFG;
    cfg.mode = 2;
    t.app.set_vfo_cfg(0, cfg);
    t.cmds();
    ptt(&mut t, true);
    assert_eq!(refused(&t), Some(Refusal::AmRxOnly));
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })), "never keyed");
    ptt(&mut t, false);
}

/// CHAN-4 (safety): a channel with "No TX" on (the channel editor's cell) refuses PTT and automatic
/// transmissions with the reason; the fix opens the channel editor. Off again, it transmits.
#[test]
fn rx_only_channel_refuses() {
    use crate::screens::chedit::ChEdit;
    let mut t = ready();
    t.tap(Key::VfoMem); // channel mode: Calling, 146.520
    assert!(t.app.slot(0).on_channel());
    t.app.open(Dest::ChannelEdit);
    t.app.screen = Screen::ChEdit(ChEdit { cell: 3 + 9 }); // RX only
    t.knob(1);
    assert!(t.app.slot(0).ch.unwrap().flags & crate::chan::FLAG_RX_ONLY != 0, "the editor sets it");
    assert!(matches!(t.app.pop_write(), Some(crate::chan::ChanWrite::Put { ch, .. }) if ch.flags & crate::chan::FLAG_RX_ONLY != 0), "and saves it");
    t.hold(Key::Exit);
    t.cmds();
    ptt(&mut t, true);
    assert_eq!(refused(&t), Some(Refusal::RxOnlyChannel));
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })), "never keyed");
    ptt(&mut t, false);
    assert!(!crate::screens::transmit::auto_tx(&mut t.app, 0, b"Beacon"));
    // TX audit F11: the Radio task is told too (its own refusal, CHAN-4).
    assert!(t.app.rx_only(0));
    t.app.send_slot(0);
    assert!(t.cmds().contains(&RadioCmd::SlotParam { slot: 0, param: ferrite_core::msg::Param::RxOnly, value: 1 }));
    t.tap(Key::Menu); // the fix: the channel editor
    assert!(matches!(t.app.screen(), Screen::ChEdit(_)));
    t.app.screen = Screen::ChEdit(ChEdit { cell: 3 + 9 });
    t.knob(1);
    t.hold(Key::Exit);
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)), "RX only off: transmits");
    ptt(&mut t, false);
}

/// TX-21: busy lockout is on or off, as nicFW: on refuses while the squelch is open, whatever tone it carries;
/// off never refuses.
#[test]
fn busy_lockout_on_off() {
    let mut t = ready();
    let mut cfg = DEFAULT_CFG;
    cfg.rx_tone = 885;
    for (busy, open, refuses) in [(0, true, false), (1, true, true), (1, false, false)] {
        cfg.busy = busy;
        t.app.set_vfo_cfg(0, cfg);
        t.live.slots[0].squelch_open = open;
        t.wait(100);
        ptt(&mut t, true);
        assert_eq!(refused(&t) == Some(Refusal::Busy), refuses, "busy {busy}, squelch open {open}");
        ptt(&mut t, false);
        t.hold(Key::Exit);
    }
}

/// KEY-7.2, KEY-7.4: PTT B transmits on slot B even when A is the active slot; the connector's PTT keys the active
/// slot.
#[test]
fn ptt_b_keys_slot_b() {
    let mut t = ready();
    t.app.slots[1].vfo = 14_652_000;
    t.app.slots[1].vfo_mode = true;
    assert_eq!(t.app.active(), 0);
    let s = store();
    t.app.key_down(Key::Ptt(1), t.t, &s);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 1, on: true }), "PTT B keys slot B");
    t.app.key_up(Key::Ptt(1), t.t + 100, &s);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 1, on: false }));
    // The side connector's PTT keys the active slot (A).
    t.app.key_down(Key::PTT, t.t + 200, &s);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: true }));
    t.app.key_up(Key::PTT, t.t + 300, &s);
}

/// KEY-7.1, KEY-7.5 (decision 77): PTT A keys the active slot, as nicFW's PTT does; there is no PTT C.
#[test]
fn ptt_a_keys_the_active_slot() {
    let mut t = ready();
    t.app.slots[2] = t.app.slots[0]; // slot C's sample VFO is airband AM, which never transmits
    t.app.slots[2].vfo = 44_600_000;
    t.app.slots[2].vfo_mode = true;
    t.app.active = 2;
    let s = store();
    t.app.key_down(Key::Ptt(0), t.t, &s);
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::Transmit { slot: 2, on: true }), "PTT A keys the active slot: {} {c:?}", t.app.screen().name());
    t.app.key_up(Key::Ptt(0), t.t + 100, &s);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 2, on: false }));
}

/// KEY-22: SIDE2 held alone opens the frequency counter when it comes up; SIDE2 + knob sets RF gain, never the
/// counter, even when the knob turns after 700 ms.
#[test]
fn side2_held_alone_is_the_counter() {
    let mut t = ready();
    let s = store();
    let t0 = t.t;
    t.app.key_down(Key::Side(2), t0, &s);
    t.app.tick(t0 + 800, &t.live, &s);
    t.app.key_up(Key::Side(2), t0 + 900, &s);
    assert!(matches!(t.app.screen(), Screen::Counter(_)), "{}", t.app.screen().name());
    t.app.key_down(Key::Exit, t0 + 1000, &s);
    t.app.key_up(Key::Exit, t0 + 1050, &s);
    assert!(matches!(t.app.screen(), Screen::Home), "{}", t.app.screen().name());
    t.app.key_down(Key::Side(2), t0 + 2000, &s);
    t.app.tick(t0 + 2800, &t.live, &s);
    t.app.knob(1, t0 + 2900, &s);
    t.app.key_up(Key::Side(2), t0 + 3000, &s);
    assert!(matches!(t.app.screen(), Screen::Home), "{}", t.app.screen().name());
}

#[test]
fn transmit_keeps_slot_order() {
    use crate::screens::transmit::{place, HF_SLOT};
    // PTT B: A stays on top as a row, B grows in the middle, C below it.
    assert_eq!(place(1, 0), (25, 23));
    assert_eq!(place(1, 1), (54, 164));
    assert_eq!(place(1, 2), (224, 23));
    // Slot A keyed: the card on top; slot C keyed: the card at the bottom.
    assert_eq!(place(0, 0), (25, 164));
    assert_eq!(place(2, 2), (83, 164));
    // HF mode: its card on top, A, B, C below.
    assert_eq!(place(HF_SLOT, HF_SLOT), (25, 164));
    assert_eq!(place(HF_SLOT, 0), (195, 23));
    assert!(place(2, 2).0 + 164 <= 300 && place(HF_SLOT, 2).0 + 23 <= 300);
}

/// KEY-11, VFO-26.3, VFO-26.4: hold 7 opens the shift's options and hold 8 the amount entry, on a VFO and a channel alike.
#[test]
fn shift_and_offset_open_their_editors() {
    use crate::screens::entry::EntryFor;
    let mut t = T::new();
    for ch in [false, true] {
        if ch {
            t.tap(Key::VfoMem);
            assert!(t.app.slot(0).on_channel());
        }
        t.hold(Key::Digit(7));
        assert!(matches!(t.app.screen(), Screen::Value(v) if v.item == Item::Offset), "hold 7: {}", t.app.screen().name());
        t.app.go_home();
        t.hold(Key::Digit(8));
        assert!(matches!(t.app.screen(), Screen::Entry(e) if e.to == EntryFor::OffsetAmount), "hold 8: {}", t.app.screen().name());
        t.app.go_home();
    }
}

/// VFO-26.2, VFO-26.3: hold 8 types the amount; the direction stays; + and - keep it; from simplex the band's again.
#[test]
fn hold_8_types_the_offset_amount() {
    let mut t = ready();
    tune(&mut t, 14_534_000);
    t.app.set_shift(-60_000);
    t.hold(Key::Digit(8));
    assert!(matches!(t.app.screen(), Screen::Entry(e) if e.n == 0), "empty: the Now row shows today's");
    for k in [Key::Digit(1), Key::Star, Key::Digit(2), Key::Menu] {
        t.tap(k);
    }
    assert!(matches!(t.app.screen(), Screen::Home), "OK keeps it: {}", t.app.screen().name());
    assert_eq!(t.app.slot(0).cfg().offset, -120_000, "1.2 MHz down, the direction kept");
    t.app.change(Item::Offset, -1); // - to +
    assert_eq!(t.app.slot(0).cfg().offset, 120_000, "+ keeps the amount");
    t.app.change(Item::Offset, -1); // to simplex
    t.app.change(Item::Offset, 2); // simplex to -: the band's again
    assert_eq!(t.app.slot(0).cfg().offset, -60_000);
    // Out of range: refused, the entry stays.
    t.hold(Key::Digit(8));
    for k in [Key::Digit(2), Key::Digit(0), Key::Digit(0), Key::Menu] {
        t.tap(k);
    }
    assert!(matches!(t.app.screen(), Screen::Entry(_)), "200 MHz refused: {}", t.app.screen().name());
}

/// VFO-26, VFO-26.1: Europe's 70 cm offset is 7.6 MHz down; 2 m stays 600 kHz; the US keeps 5 MHz.
#[test]
fn region_offsets() {
    use crate::slot::{band_shift, usual_shift, BAND_UHF, BAND_VHF};
    assert_eq!(band_shift(BAND_UHF, Some(Region::Europe)), 760_000);
    assert_eq!(usual_shift(43_900_000, Some(Region::Europe)), -760_000);
    assert_eq!(band_shift(BAND_VHF, Some(Region::Europe)), 60_000);
    assert_eq!(usual_shift(14_570_000, Some(Region::Europe)), -60_000);
    assert_eq!(usual_shift(44_400_000, Some(Region::Us)), 500_000);
    assert_eq!(band_shift(BAND_UHF, None), 500_000);
    // A VFO turned onto + on 70 cm in Europe takes 7.6 MHz.
    let mut t = ready();
    t.app.set_region(Some(Region::Europe), false);
    tune(&mut t, 43_900_000);
    t.app.set_shift(0);
    t.app.change(Item::Offset, 2); // simplex to -
    assert_eq!(t.app.slot(0).cfg().offset, -760_000);
}

/// KEY-11: hold 6 (step) on a channel changes nothing and says why (VFO-8).
#[test]
fn step_on_a_channel_is_vfo_only() {
    let mut t = T::new();
    t.tap(Key::VfoMem);
    let step = t.app.slot(0).cfg().step;
    t.hold(Key::Digit(6));
    assert_eq!(t.app.slot(0).cfg().step, step);
    assert!(t.app.toast.is_some_and(|x| x.body.as_bytes() == b"VFO only"));
}

/// CHAN-14 / SQL-6: a channel's squelch is Global (the slot's level) or its own 0-9; live changes on a Global
/// channel move the slot's level and leave the channel Global.
#[test]
fn squelch_global_on_a_channel() {
    use crate::slot::SQ_GLOBAL;
    let mut t = ready();
    let s = store();
    // Slot B is on a channel; make its squelch Global and the slot's own level 6.
    t.app.slots[1].cfg.squelch = 6;
    let cfg = crate::slot::SlotCfg { squelch: SQ_GLOBAL, ..crate::slot::DEFAULT_CFG };
    t.app.slots[1].ch = Some(crate::chan::channel(11, 0, 44_347_500, "Club repeater", cfg));
    t.app.slots[1].vfo_mode = false;
    t.app.active = 1;
    assert!(t.app.slots[1].on_channel());
    assert_eq!(t.app.sq_level(1), 6, "Global uses the slot's level");
    let mut b = ferrite_ui::text::Buf::<16>::new();
    t.app.item_text(Item::Squelch, true, &mut b);
    assert_eq!(b.as_bytes(), b"Global (6)");
    t.app.step_squelch(1);
    assert_eq!(t.app.sq_level(1), 7);
    assert_eq!(t.app.slot(1).cfg().squelch, SQ_GLOBAL, "still Global");
    assert!(t.cmds().contains(&RadioCmd::SlotParam { slot: 1, param: Param::Squelch, value: 7 }));
    // The channel editor's knob: Global -> 0 (its own level), and # back to Global.
    t.app.change(Item::Squelch, 1);
    assert_eq!(t.app.slot(1).cfg().squelch, 0);
    t.app.reset(Item::Squelch);
    assert_eq!(t.app.slot(1).cfg().squelch, SQ_GLOBAL);
    // A VFO never takes Global: the knob steps past it.
    t.app.active = 0;
    t.app.slots[0].cfg.squelch = 9;
    t.app.change(Item::Squelch, 1);
    assert_eq!(t.app.slot(0).cfg().squelch, 0, "9 wraps to 0, not Global");
    let _ = &s;
}

/// MENU 5's 4th choice, Split: choosing it asks for the TX frequency (BACK keeps the old shift); RX retunes
/// leave TX where it is; reverse swaps; the TX checks and the power cap go by the TX band.
#[test]
fn split_tx() {
    use crate::screens::entry::EntryFor;
    let mut t = ready();
    tune(&mut t, 44_600_000); // RX 446.000 (70 cm)
    // Turning onto Split (VFO-16) keeps today's TX and stays where it is; turning on leaves it again.
    t.app.change(Item::Offset, 3);
    assert_eq!(t.app.screen(), Screen::Home, "no entry from the knob");
    assert!(t.app.slot(0).cfg().split);
    assert_eq!(super::super::screens::transmit::tx_freq(&t.app, 0), 44_600_000, "today's TX: simplex, so RX");
    t.app.change(Item::Offset, 1);
    assert!(!t.app.slot(0).cfg().split, "on past Split: simplex");
    // OK on Split in the value editor: TX frequency entry, prefilled; BACK there keeps the split as it was.
    t.app.open(crate::screens::nav::Dest::Value(Item::Offset));
    t.knob(-1);
    assert!(matches!(t.app.screen(), Screen::Value(_)), "{:?}", t.app.screen());
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Entry(e) if e.to == EntryFor::SplitTx), "{:?}", t.app.screen());
    let clear = |t: &mut T| {
        while matches!(t.app.screen(), Screen::Entry(e) if e.n > 0) {
            t.tap(Key::Exit); // BACK deletes the prefilled digits
        }
    };
    clear(&mut t);
    t.tap(Key::Exit); // and with none left, leaves: back to the value editor
    assert!(matches!(t.app.screen(), Screen::Value(_)), "{:?}", t.app.screen());
    assert!(t.app.slot(0).cfg().split, "BACK keeps the split");
    // Again, typing 146.52.
    t.tap(Key::Menu);
    clear(&mut t);
    for d in [1, 4, 6, 5, 2] {
        t.tap(Key::Digit(d));
    }
    t.tap(Key::Menu);
    let c = *t.app.slot(0).cfg();
    assert!(c.split);
    assert_eq!(super::super::screens::transmit::tx_freq(&t.app, 0), 14_652_000);
    assert!(t.cmds().contains(&RadioCmd::SlotParam { slot: 0, param: Param::Split, value: 14_652_000 }));
    // RX retune: TX stays.
    tune(&mut t, 44_610_000);
    assert_eq!(super::super::screens::transmit::tx_freq(&t.app, 0), 14_652_000);
    // TX goes by its own band: 2 m is allowed in the US.
    assert_eq!(super::super::screens::transmit::refusal(&t.app, 0), None);
    // A split TX on 1.25 m (223.5) with RX on 2 m.
    tune(&mut t, 14_652_000);
    t.app.slots[0].cfg.offset = 22_350_000;
    assert_eq!(super::super::screens::transmit::refusal(&t.app, 0), None, "1.25 m transmits (TX-10.4)");
    // Reverse: transmit on RX, listen on the split TX.
    t.app.slots[0].cfg.reverse = true;
    assert_eq!(super::super::screens::transmit::tx_freq(&t.app, 0), 14_652_000);
    assert_eq!(t.app.slot(0).cfg().rx_of(14_652_000), 22_350_000);
}

/// A channel keeps a split through the store's record, and saving it from the slot keeps it.
#[test]
fn split_on_a_channel() {
    let mut t = ready();
    let cfg = crate::slot::SlotCfg { split: true, offset: 14_652_000, ..crate::slot::DEFAULT_CFG };
    t.app.slots[1].ch = Some(crate::chan::channel(20, 0, 44_600_000, "Cross", cfg));
    t.app.slots[1].vfo_mode = false;
    assert_eq!(super::super::screens::transmit::tx_freq(&t.app, 1), 14_652_000);
    let mut b = ferrite_ui::text::Buf::<16>::new();
    crate::slot::shift_text(t.app.slot(1).cfg(), false, &mut b);
    assert_eq!(b.as_bytes(), b"TX 146.520");
    crate::slot::shift_text(t.app.slot(1).cfg(), true, &mut b);
    assert_eq!(b.as_bytes(), b"Split 146.520");
}

/// TX-37: a split or shift whose TX needs the band relay in the other state from RX is refused when it is set,
/// and (a VFO retuned under a split) at PTT. A cross-band pair on the same side of 64 MHz is fine.
#[test]
fn split_across_the_relay() {
    use crate::screens::transmit::refusal;
    let mut t = ready();
    // RX 446.000 / TX 146.520: chip 1 RX, chip 0 TX, no relay: allowed.
    tune(&mut t, mhz(446));
    assert!(t.app.set_split(mhz(146) + 52_000));
    assert_eq!(refusal(&t.app, 0), None);
    // RX 146.520 / TX 50.125: refused when set, the shift is kept as it was, the toast says why.
    t.app.slots[0].cfg = crate::slot::DEFAULT_CFG;
    tune(&mut t, mhz(146) + 52_000);
    assert!(!t.app.set_split(mhz(50) + 12_500));
    assert!(!t.app.slot(0).cfg().split, "not kept");
    assert_eq!(t.app.toast.map(|x| x.head).as_ref().map(|b| b.as_bytes()), Some(&b"No split TX"[..]));
    // RX 28.400 / TX 146.520: refused too.
    tune(&mut t, mhz(28) + 40_000);
    assert!(!t.app.set_split(mhz(146) + 52_000));
    // Through the entry: OK on a refused frequency keeps the entry open to type another.
    t.app.change(Item::Offset, 3 - Item::Offset.get(t.app.slot(0).cfg()).0); // Split, TX = RX 28.400: allowed
    assert!(t.app.slot(0).cfg().split);
    t.app.open(crate::screens::nav::Dest::Value(Item::Offset));
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Entry(_)));
    while matches!(t.app.screen(), Screen::Entry(e) if e.n > 0) {
        t.tap(Key::Exit);
    }
    for d in [1, 4, 6, 5, 2] {
        t.tap(Key::Digit(d));
    }
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Entry(_)), "still open: {:?}", t.app.screen());
    assert_eq!(t.app.slot(0).cfg().offset, (mhz(28) + 40_000) as i32, "the split keeps its old TX");
    t.app.go_home();
    // A VFO retuned under an allowed split into a crossing pair: PTT refuses, with the reason.
    t.app.slots[0].cfg = crate::slot::DEFAULT_CFG;
    tune(&mut t, mhz(446));
    assert!(t.app.set_split(mhz(146) + 52_000));
    tune(&mut t, mhz(50) + 12_500);
    t.cmds();
    ptt(&mut t, true);
    assert_eq!(refused(&t), Some(Refusal::RelayMismatch));
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })), "never keyed");
    ptt(&mut t, false);
    // A + shift across 64 MHz (RX 63.900, 6 m's 500 kHz) isn't kept.
    t.app.go_home();
    t.app.slots[0].cfg = crate::slot::DEFAULT_CFG;
    tune(&mut t, mhz(63) + 90_000);
    t.app.change(Item::Offset, 1);
    assert_eq!(t.app.slot(0).cfg().offset, 0, "still simplex");
}

/// HOME-4, HOME-4.3: channel mode leads with the name; the header carries the short number and the channel's zone
/// ("012 · Local"; the band, "012 · UHF", for a channel in no zone; a VFO keeps "VFO · VHF"), and a VFO, a scan or
/// a channel with no name has no name line (its frequency stays large).
#[test]
fn channel_mode_leads_with_the_name() {
    let mut t = ready();
    let cfg = crate::slot::DEFAULT_CFG;
    t.app.set_channel_rec(1, crate::chan::channel(11, 0, 44_347_500, "Club repeater", cfg));
    let mut h = ferrite_ui::text::Buf::<32>::new();
    t.app.head_text(1, &t.live, &store(), &mut h);
    assert_eq!(h.as_bytes(), "012 \u{b7} Local".as_bytes(), "a channel names its zone (decision 178)");
    assert_eq!(t.app.chan_name(1, &t.live).as_bytes(), b"Club repeater");
    t.app.set_channel_rec(1, crate::chan::channel(11, crate::chan::NO_ZONE, 44_347_500, "Club repeater", cfg));
    t.app.head_text(1, &t.live, &store(), &mut h);
    assert_eq!(h.as_bytes(), "012 \u{b7} UHF".as_bytes(), "in no zone: the band");
    t.app.set_channel_rec(1, crate::chan::channel(11, 0, 44_347_500, "", cfg));
    assert!(t.app.chan_name(1, &t.live).is_empty(), "no name: the frequency stays large");
    t.app.set_vfo(1, 14_652_000);
    assert!(t.app.chan_name(1, &t.live).is_empty());
    t.app.head_text(1, &t.live, &store(), &mut h);
    assert_eq!(h.as_bytes(), "VFO \u{b7} VHF".as_bytes());
    t.app.set_channel_rec(1, crate::chan::channel(11, 0, 44_347_500, "Club repeater", cfg));
    t.live.slots[1].scanning = true;
    assert!(t.app.chan_name(1, &t.live).is_empty(), "a scan shows its frequency");
}

/// The user's path (Oct 5, 2026): hold SLOT on a VFO, the shift cell, OK, the knob onto Split: the value
/// editor stays (no jump into frequency entry), the knob goes on to Simplex and back, and OK on Split opens
/// the TX frequency entry.
#[test]
fn split_in_the_value_editor() {
    use crate::screens::entry::EntryFor;
    let mut t = ready();
    tune(&mut t, 44_600_000);
    t.hold(Key::Slot);
    assert!(matches!(t.app.screen(), Screen::Edit(_)), "{:?}", t.app.screen());
    // The shift cell: Mode, Bandwidth, Tones on the top row, the shift first on the next.
    t.tap(Key::Down);
    assert_eq!(t.app.screen(), Screen::Edit(crate::screens::edit::Edit { cell: 3 }));
    assert_eq!(crate::slot::GRID[3], Item::Offset);
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Value(_)), "{:?}", t.app.screen());
    for _ in 0..3 {
        t.knob(1); // simplex -> + -> - -> Split
    }
    assert!(t.app.slot(0).cfg().split);
    assert!(matches!(t.app.screen(), Screen::Value(_)), "the knob on Split stays in the editor: {:?}", t.app.screen());
    t.knob(1);
    assert!(!t.app.slot(0).cfg().split, "on to Simplex");
    t.knob(-1);
    assert!(t.app.slot(0).cfg().split);
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Entry(e) if e.to == EntryFor::SplitTx), "{:?}", t.app.screen());
}

/// A scope report: `peak` dB over a -120 dBm floor on bar `at`, sweep number `sweeps`.
fn scope_live(t: &mut T, centre: Freq10, at: usize, peak: u8, sweeps: u8, open: bool, listen: Freq10) {
    let mut bars = [0u8; ferrite_core::msg::SCOPE_BARS];
    for b in bars[..60].iter_mut() {
        *b = 40;
    }
    bars[at] = 40 + peak;
    t.live.mode = ferrite_core::msg::ModeLive::Scope { centre, bars, n: 60, sweeps, listen, open };
    t.wait(100);
}

/// SCOPE-3, SCOPE-12.1, SCOPE-12.2: SIDE1 listens to the marker and PTT then transmits there; PTT while sweeping
/// doesn't.
#[test]
fn scope_listen_and_talk() {
    let mut t = ready();
    t.hold(Key::Digit(1));
    let f = t.freq();
    t.cmds();
    ptt(&mut t, true);
    assert!(matches!(t.app.screen(), Screen::Scope(_)), "PTT while sweeping stays");
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })));
    ptt(&mut t, false);
    t.tap(Key::Right);
    t.tap(Key::Side(1));
    assert!(t.cmds().contains(&RadioCmd::ScopeListen { freq: f + 2500, mode: 0 }));
    ptt(&mut t, true);
    assert_eq!(t.freq(), f + 2500, "the slot is on the listened frequency");
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. })));
}

/// SCOPE-10: Monitor listens to the strongest after 3 sweeps over the line, and sweeps again once it closes.
#[test]
fn scope_trigger_monitors() {
    let mut t = T::new();
    t.app.set.scope_trig = 1;
    t.hold(Key::Digit(1));
    let c = t.freq();
    t.hold(Key::Digit(3)); // the line at 3/10 of the graph: 33 px, 17 dB
    t.cmds();
    for k in 1..=2 {
        scope_live(&mut t, c, 40, 30, k, false, 0);
    }
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::ScopeListen { .. })), "not before the third sweep");
    scope_live(&mut t, c, 40, 30, 3, false, 0);
    let f = c + 10 * 2500;
    assert!(t.cmds().contains(&RadioCmd::ScopeListen { freq: f, mode: 0 }));
    scope_live(&mut t, c, 40, 30, 3, true, f);
    scope_live(&mut t, c, 40, 30, 3, false, f);
    assert!(t.cmds().contains(&RadioCmd::ScopeListen { freq: 0, mode: 0 }), "squelch closed: sweep again");
}

/// SCOPE-3: # centres on the strongest; a typed centre sweeps there.
#[test]
fn scope_centre_on_strongest_and_typed() {
    let mut t = T::new();
    t.hold(Key::Digit(1));
    let c = t.freq();
    scope_live(&mut t, c, 10, 30, 1, false, 0);
    t.cmds();
    t.tap(Key::Hash);
    let s = c - 20 * 2500;
    assert!(t.cmds().contains(&RadioCmd::Sweep { centre: s, step: 2500, on: true }));
    for k in [Key::Digit(4), Key::Digit(4), Key::Digit(6), Key::Star, Key::Digit(0), Key::Digit(0), Key::Digit(0)] {
        t.tap(k);
    }
    if matches!(t.app.screen(), Screen::Entry(_)) {
        t.tap(Key::Menu);
    }
    assert!(matches!(t.app.screen(), Screen::Scope(_)), "{:?}", t.app.screen());
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::Sweep { centre: 44_600_000, on: true, .. })));
}

/// TONE-13, DTMF-7: while keyed, SIDE2 sends the tone burst as long as it is held, and the digits send DTMF live.
#[test]
fn burst_and_dtmf_while_keyed() {
    let mut t = ready();
    ptt(&mut t, true);
    t.cmds();
    t.tap(Key::Side(2));
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::ToneBurst { on: true }) && c.contains(&RadioCmd::ToneBurst { on: false }), "{c:?}");
    t.tap(Key::Digit(5));
    t.tap(Key::Hash);
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::Dtmf { key: b'5', on: true }) && c.contains(&RadioCmd::Dtmf { key: b'#', on: false }), "{c:?}");
    // DTMF-11: A-D on MENU, ▲, ▼, BACK, and still keyed after.
    for (k, d) in [(Key::Menu, b'A'), (Key::Up, b'B'), (Key::Down, b'C'), (Key::Exit, b'D')] {
        t.tap(k);
        let c = t.cmds();
        assert!(c.contains(&RadioCmd::Dtmf { key: d, on: true }) && c.contains(&RadioCmd::Dtmf { key: d, on: false }), "{c:?}");
    }
    assert!(matches!(t.app.screen(), Screen::Transmit(_)), "{:?}", t.app.screen());
    ptt(&mut t, false);
    t.cmds();
    t.tap(Key::Digit(5));
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Dtmf { .. })), "not keyed: no DTMF");
}

/// WATCH-16: unkeying pauses watch for 30 s of quiet (nicFW's MultiWatch does the same); a key press ends the
/// pause; a longer # pause isn't shortened.
#[test]
fn watch_waits_after_transmit() {
    let mut t = ready();
    ptt(&mut t, true);
    t.cmds();
    ptt(&mut t, false);
    assert!(t.cmds().contains(&RadioCmd::PauseWatch(crate::TX_PAUSE_S)));
    t.tap(Key::Up);
    assert!(t.cmds().contains(&RadioCmd::PauseWatch(0)), "a key resumes watch");
    t.tap(Key::Up);
    assert!(!t.cmds().contains(&RadioCmd::PauseWatch(0)), "only once");
}

/// PIN-5: in channel-only mode, hold digits that change a value are refused; those that open a view work.
#[test]
fn channel_only_refuses_value_shortcuts() {
    let mut t = T::new();
    t.app.set.chan_only = true;
    let before = *t.app.slot(t.app.active()).cfg();
    t.hold(Key::Digit(2)); // power
    assert_eq!(*t.app.slot(t.app.active()).cfg(), before, "power unchanged");
    t.hold(Key::Digit(4)); // watch
    assert_eq!(t.app.watch, 1, "watch unchanged");
    t.hold(Key::Digit(1)); // band scope: a view
    assert!(matches!(t.app.screen(), Screen::Scope(_)));
}

/// XB-19: an alert while the repeater runs is a banner, the repeater keeps running, and the alert screen opens
/// once it stops.
#[test]
fn alert_during_crossband_is_a_banner() {
    let mut t = ready();
    xb_ready(&mut t);
    t.tap(Key::Hash);
    assert_eq!(t.app.radio.mode, Mode::Xb);
    t.app.notify(b"SAME", b"Tornado warning", true);
    assert!(matches!(t.app.screen(), Screen::Crossband(_)), "no full-screen takeover");
    assert_eq!(t.app.radio.mode, Mode::Xb, "the repeater keeps running");
    assert!(t.app.toast.is_some_and(|x| x.head.as_bytes() == b"ALERT" && x.body.as_bytes() == b"Tornado warning"));
    t.wait(5000);
    assert!(matches!(t.app.screen(), Screen::Crossband(_)), "still waiting while it runs");
    t.app.radio.tx_built = true;
    t.tap(Key::Exit);
    assert_ne!(t.app.radio.mode, Mode::Xb);
    t.wait(100);
    assert!(matches!(t.app.screen(), Screen::Alert(_)), "opens once the repeater stops: {:?}", t.app.screen());
}

/// DTMF-1, DTMF-3: the dialer's Send keys like PTT, hands the digits to the Radio task, and unkeys once the Radio
/// task says they are out; the Transmit group's settings reach the Radio task when they change.
#[test]
fn dialer_sends_then_unkeys() {
    let mut t = ready();
    menu0(&mut t, &[8, 5, 1]);
    for d in [1, 2, 3] {
        t.tap(Key::Digit(d));
    }
    t.cmds();
    t.tap(Key::Menu); // Send
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::DtmfSeq { chunk: 0, keys: [b'1', b'2', b'3', 0, 0] }), "{c:?}");
    assert!(c.contains(&RadioCmd::Transmit { slot: 0, on: true }) && c.contains(&RadioCmd::DtmfSend { len: 3 }), "{c:?}");
    assert!(matches!(t.app.screen(), Screen::Transmit(x) if x.dial));
    // Not yet out: still keyed.
    t.wait(500);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
    t.live.tx.dtmf_sent = t.live.tx.dtmf_sent.wrapping_add(1);
    t.wait(50);
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }), "unkeyed when the digits are out");
    assert!(!matches!(t.app.screen(), Screen::Transmit(_)), "back to the dialer");
}

/// TONE-5 to TONE-9, TONE-10.1, VOX-10, VOX-12, DTMF-3, DTMF-8: the Transmit group's settings go to the Radio task
/// as they change; CW ID needs a callsign; a channel's own end tone and tail win over the radio's.
#[test]
fn transmit_tones_reach_the_radio() {
    let mut t = ready();
    t.app.set.callsign = crate::settings::Text::EMPTY;
    menu0(&mut t, &[8, 6, 2]);
    t.cmds();
    t.app.knob(1, t.t, &store());
    assert_eq!(t.app.set.cw_id, 0, "VOX-10: no callsign, no CW ID");
    t.app.set_callsign(b"va7fer");
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::CwCall { chunk: 0, text: *b"VA7FE" }) && c.contains(&RadioCmd::CwCall { chunk: 1, text: [b'R', 0, 0, 0, 0] }), "{c:?}");
    t.app.knob(1, t.t, &store());
    assert_eq!(t.app.set.cw_id, 1);
    assert!(t.cmds().contains(&RadioCmd::TxTones { phase: 0, cw: 1, sidetone: true }));
    // 0 8 4: every slot's end tone follows (Beep = END_TONES[1]).
    t.app.set.end_tone = 1;
    t.app.send_slot_tones();
    let c = t.cmds();
    for s in 0..3u8 {
        assert!(c.contains(&RadioCmd::SlotParam { slot: s, param: Param::EndTone, value: 1 }), "slot {s}: {c:?}");
    }
    // A channel's own: Off (1) wins over the radio's Beep; tail On (2) is both directions.
    let mut ch = crate::chan::channel(5, 0, mhz(146) + 52_000, "Own", crate::slot::DEFAULT_CFG);
    ch.set_end_tone(1);
    ch.set_tail(2);
    t.app.set_channel_rec(0, ch);
    assert_eq!(t.app.slot_tones(0), (0, 3));
    t.app.set.tail_mode = 2; // transmit only
    assert_eq!(t.app.slot_tones(1), (1, 2), "a VFO follows the radio's");
}

/// DEC-4: 0 4 2 3 types the capcode (digits only, 21 bits at most); empty keeps every capcode.
#[test]
fn capcode_typed_at_0_4_2_3() {
    let mut t = T::new();
    menu0(&mut t, &[4, 2, 3]);
    assert!(matches!(t.app.screen(), Screen::Text(_)));
    for d in [1, 2, 3, 4, 5, 6, 7] {
        t.tap(Key::Digit(d));
    }
    t.tap(Key::Menu);
    assert_eq!(t.app.set.pocsag_cap, 1_234_567);
    t.app.screen = Screen::Home;
    menu0(&mut t, &[4, 2, 3]);
    for _ in 0..7 {
        t.tap(Key::Exit); // delete the digits
    }
    t.tap(Key::Menu);
    assert_eq!(t.app.set.pocsag_cap, 0, "empty: every capcode");
    t.app.screen = Screen::Home;
    menu0(&mut t, &[4, 2, 3]);
    for d in [9, 9, 9, 9, 9, 9, 9] {
        t.tap(Key::Digit(d));
    }
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Text(_)), "over 2097151 stays to be fixed");
    assert_eq!(t.app.set.pocsag_cap, 0);
}

/// GPS-23: OK on 0 6 6 sets the clock by hand, typed in local time; the clock's owner takes it as UTC.
#[test]
fn clock_set_by_hand() {
    let mut t = T::new();
    let utc_minus5 = crate::settings::TIME_ZONES.iter().position(|&q| q == -20).expect("UTC-5") as u8;
    t.app.set.tz = utc_minus5;
    menu0(&mut t, &[6, 6]);
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Text(_)));
    for c in b"202610081430" {
        t.tap(Key::Digit(c - b'0'));
    }
    t.tap(Key::Menu);
    let local = ferrite_core::gps::unix_time(81_026, 143_000).unwrap();
    assert_eq!(t.app.take_clock_set(), Some(local + 5 * 3600), "14:30 at UTC-5 is 19:30 UTC");
    assert_eq!(t.app.take_clock_set(), None, "taken once");
    // A date that doesn't exist stays to be fixed.
    t.app.screen = Screen::Home;
    menu0(&mut t, &[6, 6]);
    t.tap(Key::Menu);
    for c in b"202602301200" {
        t.tap(Key::Digit(c - b'0'));
    }
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Text(_)));
    assert_eq!(t.app.take_clock_set(), None);
}

/// DEC-7: 0 4 4 Popups is a real setting, # puts it back on.
#[test]
fn popups_setting() {
    use crate::screens::groups::{change, reset};
    let mut t = T::new();
    assert!(t.app.set.popups, "on by default");
    change(&mut t.app, &[4, 4], 1);
    assert!(!t.app.set.popups);
    reset(&mut t.app, &[4, 4]);
    assert!(t.app.set.popups);
}

/// SCOPE-7, HOME-3: the scope screen and home's scope strip redraw their bars in place (no blank first: it
/// flickered on the radio, Oct 12, 2026) and end up exactly as a fresh full redraw, through changing sweeps.
#[test]
fn scope_bars_redraw_in_place() {
    use ferrite_ui::fb::FrameBuffer;
    let same = |t: &mut T, fb: &mut FrameBuffer, what: &str| {
        t.render(fb);
        let mut fresh = FrameBuffer::new();
        t.app.redraw_all();
        t.render(&mut fresh);
        for y in 0..320 {
            for x in 0..240 {
                assert_eq!(fb.at(x, y), fresh.at(x, y), "{what}: stale pixel at {x},{y}");
            }
        }
    };
    for layout_strip in [false, true] {
        let mut t = ready();
        let mut fb = FrameBuffer::new();
        if layout_strip {
            t.app.set.layout = 1;
        } else {
            t.hold(Key::Digit(1)); // the scope screen
        }
        let c = t.freq();
        for (k, (at, peak)) in [(10usize, 30u8), (40, 5), (25, 60), (59, 0)].into_iter().enumerate() {
            scope_live(&mut t, c, at, peak, k as u8, false, 0);
            same(&mut t, &mut fb, if layout_strip { "home strip" } else { "scope screen" });
        }
    }
}

/// HOME-3: leaving the single + scope layout stops the strip's sweep, so the slots receive again.
#[test]
fn leaving_the_scope_layout_stops_the_sweep() {
    let mut t = ready();
    t.app.set.layout = 1;
    t.wait(100);
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::Sweep { on: true, .. })), "the strip sweeps");
    t.app.set.layout = 0;
    t.wait(100);
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::Sweep { on: false, .. })), "back to Watch: the sweep stops");
}

/// An exclusive mode's stop command (the Radio task's side of [`App::stop_mode`]).
fn stops(m: Mode, c: &RadioCmd) -> bool {
    match m {
        Mode::Scope | Mode::Strip => matches!(c, RadioCmd::Sweep { on: false, .. }),
        Mode::Tone => matches!(c, RadioCmd::ToneSearch { on: false, .. }),
        Mode::Counter => matches!(c, RadioCmd::Counter(false)),
        Mode::Fm => matches!(c, RadioCmd::Broadcast(BroadcastCmd::Off)),
        Mode::Hf => matches!(c, RadioCmd::Hf { on: false, .. }),
        Mode::Xb => matches!(c, RadioCmd::Crossband(None)),
        Mode::Sat => matches!(c, RadioCmd::SatRx(0)),
        Mode::Off => false,
    }
}

/// Start mode `m` from home, as a person would: its screen (crossband: and # Start), or for the strip the
/// single + scope layout.
fn start(t: &mut T, m: Mode) {
    match m {
        Mode::Scope => t.open(Dest::BandScope),
        Mode::Strip => {
            t.app.set.layout = 1;
            t.wait(100);
        }
        Mode::Tone => t.open(Dest::ToneSearch),
        Mode::Counter => t.open(Dest::Counter),
        Mode::Fm => {
            crate::screens::entry::tune_vfo(&mut t.app, 0, ferrite_core::freq::mhz(100) + 50_000); // BCR-21: a slot in the band
            t.wait(100);
        }
        Mode::Hf => t.open(Dest::HfMode),
        Mode::Xb => {
            t.open(Dest::Crossband);
            t.tap(Key::Hash);
        }
        Mode::Sat => t.open(Dest::Satellites),
        Mode::Off => {}
    }
    assert_eq!(t.app.radio.mode, m, "{m:?} runs");
}

const MODES: [Mode; 8] = [Mode::Scope, Mode::Strip, Mode::Tone, Mode::Counter, Mode::Fm, Mode::Hf, Mode::Xb, Mode::Sat];
/// The modes a screen runs (decision 352: FM broadcast is a slot in the band, which plays until it is tuned out).
const SCREEN_MODES: [Mode; 7] = [Mode::Scope, Mode::Strip, Mode::Tone, Mode::Counter, Mode::Hf, Mode::Xb, Mode::Sat];

/// HOME-2, SCOPE-4 and each mode's own rules: an exclusive mode stops with its screen, however the screen is left
/// (BACK, hold BACK, the screen's timeout, PTT; the strip by leaving its layout), and only then: the Radio task
/// gets the mode's stop command once, and no mode is left running behind another screen.
#[test]
fn every_way_out_of_a_mode_stops_it() {
    #[derive(Debug, Clone, Copy)]
    enum Out {
        Back,
        HoldBack,
        Timeout,
        Ptt,
        Layout,
    }
    for m in SCREEN_MODES {
        let outs: &[Out] = if m == Mode::Strip { &[Out::Ptt, Out::Layout] } else { &[Out::Back, Out::HoldBack, Out::Timeout, Out::Ptt] };
        for &out in outs {
            let mut t = ready();
            start(&mut t, m);
            let shown = t.app.screen();
            t.cmds();
            match out {
                Out::Back => t.tap(Key::Exit),
                Out::HoldBack => t.hold(Key::Exit),
                Out::Timeout => t.wait(15 * 60 * 1000),
                Out::Ptt => {
                    ptt(&mut t, true);
                    // keyed on another screen (Transmit): the mode stopped before the key-up, not after; HF mode
                    // transmits (or refuses) in place (HF-23, HF-24), the repeater is held (XB-14)
                    if t.app.screen() != shown && !matches!(m, Mode::Hf | Mode::Xb) {
                        assert_eq!(t.app.radio.mode, Mode::Off, "{m:?}: PTT left its screen and the mode ran on");
                    }
                    ptt(&mut t, false);
                    t.wait(500);
                }
                Out::Layout => {
                    t.app.set.layout = 0;
                    t.wait(100);
                }
            }
            let cmds = t.cmds();
            let sent = cmds.iter().filter(|c| stops(m, c)).count();
            if let (Mode::Hf | Mode::Xb, Out::Ptt) = (m, out) {
                // HF mode transmits in place (HF-23); the repeater is held and carries on after (XB-14)
                assert_eq!((sent, t.app.radio.mode), (0, m), "{m:?}, PTT: the mode stays");
            } else if let (Mode::Strip, Out::Ptt) = (m, out) {
                // home's strip stops for the transmission and sweeps again once home is back
                assert_eq!((sent, t.app.radio.mode), (1, Mode::Strip), "strip, PTT");
                let again = cmds.iter().rposition(|c| matches!(c, RadioCmd::Sweep { on: true, .. }));
                assert!(again > cmds.iter().position(|c| stops(m, c)), "strip, PTT: sweeping again after");
            } else if t.app.radio.mode == Mode::Off {
                assert_eq!(sent, 1, "{m:?}, {out:?}: the stop command, once");
            } else {
                // still running: only on its own screen (no timeout there; the scope refuses PTT unless listening,
                // SCOPE-12.2), and nothing stopped
                assert_eq!((t.app.radio.mode, sent), (m, 0), "{m:?}, {out:?}");
                assert!(t.app.screen() == shown && matches!(out, Out::Timeout | Out::Ptt), "{m:?}, {out:?}: running behind another screen");
            }
        }
    }
}

/// Starting one exclusive mode stops the one running first: its stop command goes before the new mode's start.
#[test]
fn a_mode_stops_the_one_before_it() {
    for a in SCREEN_MODES {
        for b in SCREEN_MODES {
            if a == b || b == Mode::Strip {
                continue; // the strip starts only from home's layout
            }
            let mut t = ready();
            start(&mut t, a);
            t.cmds();
            if a != Mode::Strip {
                t.app.go_home(); // a mode screen's own keys don't open the others: home first, as a person would
            }
            start(&mut t, b);
            let cmds = t.cmds();
            let stop = cmds.iter().position(|c| stops(a, c));
            assert!(stop.is_some(), "{a:?} then {b:?}: {a:?} stopped ({cmds:?})");
            assert_eq!(t.app.radio.mode, b);
        }
    }
}

/// MODE-4 (UI backlog 5): turning the knob on the single + scope layout stops the strip at the first step, so
/// normal receive follows the knob; it sweeps again 500 ms after the last step, centred on the new frequency.
#[test]
fn strip_rests_while_the_knob_turns() {
    let mut t = ready();
    start(&mut t, Mode::Strip);
    t.cmds();
    t.knob(1);
    assert_eq!(t.app.radio.mode, Mode::Off, "the first step stops the strip");
    assert!(t.cmds().iter().any(|c| stops(Mode::Strip, c)), "its stop sent at once");
    for _ in 0..8 {
        t.wait(200);
        t.knob(1);
        assert_eq!(t.app.radio.mode, Mode::Off, "still turning");
    }
    assert!(!t.cmds().iter().any(|c| matches!(c, RadioCmd::Sweep { on: true, .. })), "no sweep while turning");
    t.wait(400);
    assert_eq!(t.app.radio.mode, Mode::Off, "400 ms after the last step: still resting");
    t.wait(200);
    assert_eq!(t.app.radio.mode, Mode::Strip, "back 500 ms after the last step");
    let f = t.freq();
    let c = t.cmds();
    assert!(c.iter().any(|c| matches!(c, RadioCmd::Sweep { on: true, centre, .. } if *centre == f)), "centred on the new frequency: {c:?}");
}

/// SCOPE-14 (UI backlog 7): a key or knob step while the scope or home's strip sweeps holds the sweep (the Radio
/// task starts the pass again after it); none with no sweep running. With input waiting the scope's bars and the
/// strip don't draw (still dirty), and they catch up on the next pass, to the same picture.
#[test]
fn input_first_on_the_scope() {
    struct Pending(FrameBuffer, Vec<(i32, i32, i32, i32)>, bool);
    impl Panel for Pending {
        fn window(&mut self, a: i32, b: i32, c: i32, d: i32) {
            self.1.push((a, b, c, d));
            self.0.window(a, b, c, d)
        }
        fn push(&mut self, c: u16, n: u32) {
            self.0.push(c, n)
        }
        fn push_buf(&mut self, p: &[u16]) {
            self.0.push_buf(p)
        }
        fn input_pending(&self) -> bool {
            self.2
        }
    }
    for m in [Mode::Scope, Mode::Strip] {
        let mut t = ready();
        start(&mut t, m);
        t.cmds();
        t.tap(Key::Left);
        assert!(t.cmds().contains(&RadioCmd::SweepHold), "{m:?}: a key holds the sweep");
        if m == Mode::Scope {
            t.knob(1);
            assert!(t.cmds().contains(&RadioCmd::SweepHold), "the knob too");
        }
    }
    let mut t = ready();
    t.tap(Key::Left);
    t.knob(1);
    assert!(!t.cmds().contains(&RadioCmd::SweepHold), "no sweep: no hold");
    // The drawing: the scope screen, then home's strip, with bars from the Radio task.
    let dev = Device::default();
    for (m, rows) in [(Mode::Scope, 50..162), (Mode::Strip, 200..234)] {
        let mut t = ready();
        start(&mut t, m);
        let mut bars = [0u8; ferrite_core::msg::SCOPE_BARS];
        for (i, b) in bars.iter_mut().enumerate() {
            *b = 40 + (i * 7 % 60) as u8;
        }
        let n = if m == Mode::Strip { crate::screens::scope::STRIP_BARS } else { crate::screens::scope::bars_n(&t.app) } as u8;
        t.live.mode = ModeLive::Scope { centre: t.app.radio.centre.max(t.freq()), bars, n, sweeps: 1, listen: 0, open: false };
        t.wait(100);
        let mut lcd = Pending(FrameBuffer::new(), Vec::new(), true);
        t.app.render(&mut lcd, &t.live, &dev, &store());
        assert!(!lcd.1.iter().any(|r| rows.contains(&r.1) && r.3 >= rows.start), "{m:?}: bars drew with input waiting");
        lcd.2 = false;
        lcd.1.clear();
        t.app.render(&mut lcd, &t.live, &dev, &store());
        assert!(lcd.1.iter().any(|r| rows.contains(&r.1)), "{m:?}: the bars caught up");
        let after = lcd.0.px.clone();
        t.app.redraw_all();
        t.app.render(&mut lcd, &t.live, &dev, &store());
        assert!(after == lcd.0.px, "{m:?}: the same picture as a full redraw");
    }
}

/// MODE-3: while an exclusive mode runs the app confirms it at least every RENEW_MS; with none running it sends
/// no lease. When the Radio task stopped a mode itself (its lease ran out), the app takes it as stopped and starts
/// again what its screen still wants, with no stop command of its own.
#[test]
fn modes_hold_a_lease() {
    use ferrite_core::lease::{RENEW_MS, WINDOW_MS};
    const { assert!(RENEW_MS * 3 <= WINDOW_MS, "two renewals may go missing before the Radio task gives up") };
    for m in MODES {
        let mut t = ready();
        start(&mut t, m);
        t.cmds();
        for _ in 0..6 {
            t.wait(RENEW_MS + 100);
            assert!(t.cmds().contains(&RadioCmd::Lease), "{m:?}: renewed");
        }
        // the Radio task dropped it
        t.live.lease_drops = t.live.lease_drops.wrapping_add(1);
        t.wait(100);
        let c = t.cmds();
        assert!(!c.iter().any(|c| stops(m, c)), "{m:?}: no stop for a mode already stopped: {c:?}");
        assert_eq!(t.app.radio.mode, m, "{m:?}: started again, its screen still up");
        assert_eq!(t.app.radio.lease_drops, t.live.lease_drops);
    }
    let mut t = ready();
    t.wait(5000);
    assert!(!t.cmds().contains(&RadioCmd::Lease), "no mode, no lease");
}

/// HOME-13.1, HOME-13.2, HOME-13.3 (UI backlog 6): while the knob turns, home redraws only the active slot's part; the
/// other slots, the activity rows and the strip wait, and draw once the knob rests, to the same picture a full redraw
/// gives.
#[test]
fn knob_holds_the_heavier_redraws() {
    struct Count(FrameBuffer, Vec<(i32, i32, i32, i32)>);
    impl Panel for Count {
        fn window(&mut self, a: i32, b: i32, c: i32, d: i32) {
            self.1.push((a, b, c, d));
            self.0.window(a, b, c, d)
        }
        fn push(&mut self, c: u16, n: u32) {
            self.0.push(c, n)
        }
        fn push_buf(&mut self, p: &[u16]) {
            self.0.push_buf(p)
        }
    }
    // per layout: the active slot's rows (the frequency and what goes with it) and where the held parts start
    for (layout, live_end, held_from) in [(0u8, 150, 155), (1, 200, 200), (2, 87, 87)] {
        let mut t = ready();
        let dev = Device { clock_min: Some(600), ..Default::default() };
        let mut lcd = Count(FrameBuffer::new(), Vec::new());
        t.app.set_layout(layout);
        t.live.slots[0] = SlotLive { freq: t.freq(), ..Default::default() };
        t.wait(1000);
        t.app.render(&mut lcd, &t.live, &dev, &store());
        // something changes in the other slots (and the strip's centre with the knob)
        t.live.slots[1] = SlotLive { squelch_open: true, carrier: true, rssi: -70, ..Default::default() };
        t.wait(1300); // the last minute's bucket ticks over
        t.app.render(&mut lcd, &t.live, &dev, &store());
        t.live.slots[1] = SlotLive::default();
        t.live.slots[2] = SlotLive { squelch_open: true, carrier: true, rssi: -60, ..Default::default() };
        let f0 = t.freq();
        lcd.1.clear();
        t.knob(1);
        assert_ne!(t.freq(), f0);
        t.app.render(&mut lcd, &t.live, &dev, &store());
        let held: Vec<_> = lcd.1.iter().filter(|r| r.1 >= 21 && r.1 < 300 && !(r.1 < live_end)).collect();
        assert!(lcd.1.iter().any(|r| r.1 >= 21 && r.1 < live_end), "layout {layout}: the frequency redrew at once");
        assert!(held.is_empty(), "layout {layout}: held parts drew while the knob turned: {held:?}");
        t.wait(500);
        lcd.1.clear();
        t.app.render(&mut lcd, &t.live, &dev, &store());
        assert!(lcd.1.iter().any(|r| r.1 >= held_from && r.1 < 300), "layout {layout}: the held parts drew once it rested");
        let after = lcd.0.px.clone();
        t.app.redraw_all();
        t.app.render(&mut lcd, &t.live, &dev, &store());
        assert!(after == lcd.0.px, "layout {layout}: the same picture as a full redraw");
    }
}

/// MODE-1, MODE-2, MODE-5, BCR-29.1, BCR-29.3, TX-24: random use (keys, the knob, layouts, screens opened, timeouts,
/// PTT, slots tuned in and out of the broadcast band) never leaves the radio in a mode the app doesn't want: after
/// every step the mode is the wanted one; home on the plain layout with no mode screen under it is normal receive; and
/// the commands sent, played on a model of the Radio task, never start a mode over another and end in the mode the app
/// thinks runs. The broadcast runs beside nothing but home's strip, and only with the active slot a normal one
/// (MODE-5); never while keyed.
#[test]
fn random_use_keeps_modes_in_step() {
    const KEYS: [Key; 16] = [
        Key::Menu, Key::Exit, Key::Up, Key::Down, Key::Left, Key::Right, Key::Slot, Key::VfoMem, Key::Digit(0),
        Key::Digit(1), Key::Digit(3), Key::Digit(7), Key::Star, Key::Hash, Key::Side(1), Key::Side(2),
    ];
    const DESTS: [Dest; 8] = [Dest::BandScope, Dest::ToneSearch, Dest::Counter, Dest::HfMode, Dest::Crossband, Dest::Scan, Dest::Squelch, Dest::Satellites];
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Kind {
        Sweep,
        Tone,
        Counter,
        Fm,
        Hf,
        Xb,
        Sat,
    }
    // the exclusive part (the broadcast is tracked apart: it may play beside the strip)
    fn kind(m: Mode) -> Option<Kind> {
        match m {
            Mode::Off | Mode::Fm => None,
            Mode::Scope | Mode::Strip => Some(Kind::Sweep),
            Mode::Tone => Some(Kind::Tone),
            Mode::Counter => Some(Kind::Counter),
            Mode::Hf => Some(Kind::Hf),
            Mode::Xb => Some(Kind::Xb),
            Mode::Sat => Some(Kind::Sat),
        }
    }
    // the Radio task's view: what a command starts (Some) or stops (None)
    fn effect(c: &RadioCmd) -> Option<(Kind, bool)> {
        Some(match c {
            RadioCmd::Sweep { on, .. } => (Kind::Sweep, *on),
            RadioCmd::ToneSearch { on, .. } => (Kind::Tone, *on),
            RadioCmd::Counter(on) => (Kind::Counter, *on),
            RadioCmd::Broadcast(b) => (Kind::Fm, *b != BroadcastCmd::Off),
            RadioCmd::Hf { on, .. } => (Kind::Hf, *on),
            RadioCmd::Crossband(x) => (Kind::Xb, x.is_some()),
            RadioCmd::SatRx(f) => (Kind::Sat, *f != 0),
            _ => return None,
        })
    }
    // play commands on the model (the exclusive mode, the broadcast); a start over another mode fails, and only a
    // sweep and the broadcast ever run together
    fn play(cmds: Vec<RadioCmd>, radio: &mut (Option<Kind>, bool), at: &str) {
        for c in cmds {
            let Some((k, on)) = effect(&c) else { continue };
            match (k, on) {
                (Kind::Fm, on) => {
                    assert!(!on || matches!(radio.0, None | Some(Kind::Sweep)), "{at}: the broadcast started over {:?}", radio.0);
                    radio.1 = on;
                }
                (k, true) => {
                    assert!(radio.0.is_none() || radio.0 == Some(k), "{at}: {k:?} started over {:?}", radio.0);
                    assert!(!radio.1 || k == Kind::Sweep, "{at}: {k:?} started over the broadcast");
                    radio.0 = Some(k);
                }
                (k, false) if radio.0 == Some(k) => radio.0 = None,
                _ => {}
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut beside = 0; // steps with the strip sweeping beside a broadcast (MODE-5)
    for seed in 1..=32u64 {
        let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        let mut t = ready();
        let mut keyed = false;
        let mut radio: (Option<Kind>, bool) = (None, false);
        for step in 0..2000 {
            let s = store();
            let what = match next(21) {
                0..=7 => {
                    let k = KEYS[next(KEYS.len() as u64) as usize];
                    if next(6) == 0 {
                        t.hold(k);
                    } else {
                        t.tap(k);
                    }
                    "key"
                }
                8..=10 => {
                    let d = if next(2) == 0 { 1 } else { -1 };
                    for _ in 0..=next(4) {
                        t.knob(d);
                        while t.app.pop_write().is_some() {} // the UI task takes each input's write
                        t.t += 30;
                        t.app.tick(t.t, &t.live, &s);
                    }
                    "knob"
                }
                11 => {
                    t.app.set_layout(next(3) as u8);
                    "layout"
                }
                12 => {
                    t.app.set.layout = next(3) as u8; // a setting changed underneath (a link write, a reset)
                    t.wait(100);
                    "setting"
                }
                13..=14 => {
                    let d = DESTS[next(DESTS.len() as u64) as usize];
                    t.open(d);
                    if d == Dest::Crossband && next(2) == 0 {
                        t.tap(Key::Hash); // # Start, often enough that the repeater runs in the walk
                    }
                    "open"
                }
                15..=16 => {
                    t.wait(100 + next(3000) as u32);
                    "wait"
                }
                17 => {
                    // in steps, the commands taken as the UI task does each turn
                    for _ in 0..if next(4) == 0 { 900 } else { 30 } {
                        t.wait(1000);
                        play(t.cmds(), &mut radio, &std::format!("seed {seed} step {step} (timeout)"));
                    }
                    "timeout"
                }
                20 => {
                    // BCR-22: a slot tuned into (or out of) the broadcast band
                    let f = [mhz(100) + 50_000, mhz(99) + 10_000, mhz(146) + 52_000][next(3) as usize];
                    t.app.set_vfo(next(3) as u8, f);
                    t.wait(100);
                    "broadcast"
                }
                _ => {
                    keyed = !keyed;
                    ptt(&mut t, keyed);
                    "ptt"
                }
            };
            while t.app.pop_write().is_some() {}
            play(t.cmds(), &mut radio, &std::format!("seed {seed} step {step} ({what})"));
            let m = t.app.radio.mode;
            seen.insert(std::format!("{m:?}"));
            assert_eq!(m, t.app.wanted_mode(), "seed {seed} step {step} ({what}): reconciled");
            assert_eq!(radio, (kind(m), t.app.radio.fm_on), "seed {seed} step {step} ({what}): the Radio task runs what the app thinks");
            let fm = m == Mode::Fm || m == Mode::Strip && t.app.bcast_slot().is_some();
            assert_eq!(t.app.radio.fm_on, fm, "seed {seed} step {step} ({what}): the broadcast plays in FM, or beside the strip");
            if radio == (Some(Kind::Sweep), true) {
                beside += 1;
                assert_eq!(m, Mode::Strip, "seed {seed} step {step} ({what}): only home's strip sweeps beside a broadcast");
                assert!(!t.app.in_bcast_band(t.app.active), "seed {seed} step {step} ({what}): not with the broadcast slot active (HOME-14)");
            }
            let mode_screen = |s: &Screen| matches!(s, Screen::Scope(_) | Screen::ToneSearch(_) | Screen::Counter(_) | Screen::Hf(_) | Screen::Crossband(_) | Screen::Sat(_));
            if t.app.screen() == Screen::Home && t.app.set.layout == 0 && !t.app.back.iter().flatten().any(mode_screen) {
                let want = if t.app.bcast_slot().is_some() && t.app.radio.keyed.is_none() { Mode::Fm } else { Mode::Off };
                assert_eq!(m, want, "seed {seed} step {step} ({what}): home is normal receive, or the broadcast slot's");
            }
            if let Some(b) = t.app.bcast_slot() {
                assert!(t.app.in_bcast_band(b), "seed {seed} step {step} ({what}): the broadcast slot is in the band");
            }
            if t.app.radio.keyed.is_some() {
                assert!(matches!(m, Mode::Off | Mode::Hf | Mode::Xb | Mode::Sat), "seed {seed} step {step} ({what}): {m:?} ran while keyed");
                assert!(!radio.1, "seed {seed} step {step} ({what}): the broadcast played while keyed (TX-24)");
            }
        }
        if keyed {
            ptt(&mut t, false);
        }
    }
    assert_eq!(seen.len(), MODES.len() + 1, "every mode reached: {seen:?}");
    assert!(beside > 0, "the strip swept beside a broadcast");
}

/// SAFE-14.2: a PTT held from boot for over 1 s (the Radio task's `TxLive::ptt_stuck`) shows "PTT stuck" with the
/// inputs' names as a popup in the middle, kept up while held, over another toast, and 2 s after the release.
#[test]
fn ptt_stuck_notice() {
    use ferrite_core::stuck::{latch, PTT_A, PTT_B, PTT_EXT};
    let mut t = T::new();
    let shown = |t: &T| t.app.toast.filter(|x| x.center).map(|x| (std::string::String::from_utf8_lossy(x.head.as_bytes()).into_owned(), std::string::String::from_utf8_lossy(x.body.as_bytes()).into_owned()));
    t.live.tx.ptt_stuck = latch(0, PTT_A | PTT_EXT);
    t.wait(100);
    assert_eq!(shown(&t), Some(("PTT stuck".into(), "PTT A + side PTT".into())));
    t.wait(10_000);
    assert_eq!(shown(&t).map(|s| s.0), Some("PTT stuck".into()), "kept up while held");
    t.app.show_toast(b"Other", b"toast", false);
    t.wait(100);
    assert_eq!(shown(&t).map(|s| s.0), Some("PTT stuck".into()), "over another toast");
    t.live.tx.ptt_stuck = latch(t.live.tx.ptt_stuck, PTT_A);
    t.wait(100);
    assert_eq!(shown(&t).map(|s| s.1), Some("PTT A".into()));
    t.live.tx.ptt_stuck = latch(t.live.tx.ptt_stuck, 0);
    t.wait(1500);
    assert!(shown(&t).is_some(), "let go: still up 1.5 s after");
    t.wait(600);
    assert!(shown(&t).is_none(), "and gone 2 s after");
    t.wait(10_000);
    assert!(shown(&t).is_none(), "once let go, not again");
    assert!(ferrite_ui::widgets::popup_fits(b"PTT stuck", b"PTT A + PTT B + side PTT"));
    // Released before the UI first looked: named once, for 2 s.
    let mut t = T::new();
    t.live.tx.ptt_stuck = latch(latch(0, PTT_B), 0);
    t.wait(100);
    assert_eq!(shown(&t), Some(("PTT stuck".into(), "PTT B".into())));
    t.wait(2100);
    assert!(shown(&t).is_none());
}

/// SAFE-14.2: booted with PTT A held: home's first draw already has the "PTT stuck" popup on the panel, in the
/// middle of the screen (the hint bar's toast wasn't seen on the radio, Oct 13, 2026).
#[test]
fn ptt_stuck_on_screen_at_boot() {
    use ferrite_ui::gfx::TOAST;
    use ferrite_ui::widgets::POPUP_BOX;
    let mut t = T::new();
    t.live.tx.ptt_stuck = ferrite_core::stuck::latch(0, ferrite_core::stuck::PTT_A);
    t.app.tick(t.t, &t.live, &store());
    assert_eq!(t.app.screen(), Screen::Home);
    let mut fb = ferrite_ui::fb::FrameBuffer::new();
    t.render(&mut fb);
    let (x, y, w, h) = POPUP_BOX;
    let toast = (x + 8..x + w - 8).step_by(4).flat_map(|px| (y + 4..y + h - 4).step_by(4).map(move |py| (px, py))).filter(|&(px, py)| fb.at(px, py) == TOAST).count();
    assert!(toast > 200, "the popup's box on the panel: {toast} samples");
    // Without it, the same spot is home.
    let mut t = T::new();
    let mut fb = ferrite_ui::fb::FrameBuffer::new();
    t.app.tick(t.t, &t.live, &store());
    t.render(&mut fb);
    let bare = (x + 8..x + w - 8).step_by(4).flat_map(|px| (y + 4..y + h - 4).step_by(4).map(move |py| (px, py))).filter(|&(px, py)| fb.at(px, py) == TOAST).count();
    assert!(bare < toast / 2, "{bare}");
}

/// The UI readback's lines (LINK-19), as the link's `UI` and the terminal's `ui` give them.
fn readback(t: &T) -> Vec<String> {
    let (s, dev) = (store(), Device::default());
    let mut l = crate::term::cmd::Line::new();
    (0..)
        .map_while(|i| crate::uistate::line(&t.app, &s, &dev, &t.live, i, &mut l).then(|| String::from_utf8_lossy(l.as_bytes()).into_owned()))
        .collect()
}

fn line_of(t: &T, head: &str) -> String {
    readback(t).into_iter().find(|l| l.starts_with(head)).unwrap_or_else(|| panic!("no {head} line: {:?}", readback(t)))
}

/// TSRCH-2, LINK-19: tone search's readback: "" while searching, then the tone found, the slot's signal and the
/// hits (the receiving session, Oct 15, 2026: the result could only be read off the screen).
#[test]
fn tone_search_readback() {
    let mut t = T::new();
    t.open(Dest::ToneSearch);
    t.wait(100);
    let l = line_of(&t, "tsearch ");
    assert!(l.starts_with("tsearch slot=A found=\"\" open=0"), "{l}");
    t.live.slots[0].squelch_open = true;
    t.live.slots[0].rssi = -60;
    t.live.mode = ModeLive::Probe { freq: 0, rssi: -60, tone: 885 };
    t.wait(100);
    let l = line_of(&t, "tsearch ");
    assert!(l.contains("found=\"T 88.5\" open=1") && l.contains(" ask=0") && l.contains("hits=\"T 88.5\""), "{l}");
    t.live.mode = ModeLive::Probe { freq: 0, rssi: -60, tone: 0x8000 | 0o23 };
    t.wait(100);
    assert!(line_of(&t, "tsearch ").contains("hits=\"D023N,T 88.5\""), "{}", line_of(&t, "tsearch "));
    t.tap(Key::Menu);
    assert!(line_of(&t, "tsearch ").contains("found=\"D023N\"") && line_of(&t, "tsearch ").contains(" ask=1"));
}

/// TSRCH-6, TSRCH-11, LINK-19: the counter's readback: the frequency and level shown, live, then held.
#[test]
fn counter_readback() {
    let mut t = T::new();
    menu0(&mut t, &[3, 2]);
    t.wait(100);
    assert!(line_of(&t, "counter ").starts_with("counter freq=0.00000 s=\"\" meter=0"), "{}", line_of(&t, "counter "));
    t.live.mode = ModeLive::Probe { freq: mhz(147) + 15_000, rssi: -60, tone: 0 };
    t.wait(100);
    let l = line_of(&t, "counter ");
    assert!(l.starts_with("counter freq=147.15000 s=") && l.contains(" live=1 hold=0"), "{l}");
    t.tap(Key::Hash);
    t.live.mode = ModeLive::Probe { freq: mhz(446) + 6_250, rssi: -40, tone: 0 };
    t.wait(100);
    assert!(line_of(&t, "counter ").starts_with("counter freq=147.15000") && line_of(&t, "counter ").ends_with("hold=1"));
}

/// TSRCH-13, LINK-19: a card showing a tone hint says what it reads ("Heard 88.5", TONE-20.5's two forms).
#[test]
fn card_hint_readback() {
    let mut t = T::new();
    t.live.slots[0].squelch_open = true;
    t.wait(100);
    assert!(!line_of(&t, "card A").contains("hint="));
    t.live.slots[0].heard_tone = 885;
    t.wait(100);
    assert!(line_of(&t, "card A").contains("state=\"heard\" hint=\"Heard 88.5\""), "{}", line_of(&t, "card A"));
    t.live.slots[0].heard_tone = 0x8000 | 0o47;
    t.wait(100);
    assert!(line_of(&t, "card A").contains("hint=\"Heard D047N/D023I\""), "{}", line_of(&t, "card A"));
}

/// SQL-12.2, LINK-19: the squelch screen's readback follows the live signal while SIDE1 is held (the receiving
/// session's probe read it closed throughout: its two reads fell before and after the other radio's carrier, keyed
/// over Bluetooth seconds late).
#[test]
fn squelch_readback_follows_the_signal() {
    let mut t = T::new();
    let s = store();
    t.app.key_down(Key::Side(1), t.t, &s);
    t.knob(1);
    t.knob(-1);
    (t.live.slots[0].rssi, t.live.slots[0].rssi_raw) = (-121, -121);
    t.wait(200);
    assert!(line_of(&t, "squelch ").contains("open=0 s=\"S1\""), "{}", line_of(&t, "squelch "));
    t.live.slots[0].squelch_open = true;
    (t.live.slots[0].rssi, t.live.slots[0].rssi_raw, t.live.slots[0].carrier) = (-70, -70, true);
    t.wait(100);
    let l = line_of(&t, "squelch ");
    assert!(l.contains("open=1") && !l.contains("s=\"S1\"") && l.contains("signal +"), "{l}");
    t.app.key_up(Key::Side(1), t.t, &s);
}

/// SBAR-2.1: MENU 0 6 6's 12 h reaches the status bar (it reached only the GPS screen, a user's report Nov 19,
/// 2026); UTC keeps 24 h.
#[test]
fn bar_follows_the_12_hour_clock() {
    let t = ready();
    let dev = Device { clock_min: Some(14 * 60 + 32), ..Default::default() };
    assert!(!t.app.status_bar(&t.live, &dev).h12, "24 h to start");
    t.app.set.clock = 1;
    assert!(t.app.status_bar(&t.live, &dev).h12, "12 h");
}

/// SBAR-2: with no time source the callsign stands in the clock's place (nothing with none set); a clock wins
/// over it; the readback's `bar` carries it as `call`.
#[test]
fn bar_callsign_without_a_clock() {
    let t = ready();
    let none = Device::default();
    assert_eq!(none.clock_min, None);
    assert!(t.app.status_bar(&t.live, &none).call.is_empty(), "no callsign set: nothing");
    t.app.set.set_callsign(b"ve7abc");
    assert_eq!(t.app.status_bar(&t.live, &none).call.as_bytes(), b"VE7ABC");
    let timed = Device { clock_min: Some(600), ..Default::default() };
    let sb = t.app.status_bar(&t.live, &timed);
    assert!(sb.call.is_empty(), "a clock wins");
    assert!(line_of(&t, "bar ").contains(" clock=- utc=0 call=\"VE7ABC\""), "{}", line_of(&t, "bar "));
    // drawn centred where the clock goes
    let mut fb = FrameBuffer::new();
    t.app.status_bar(&t.live, &none).draw(&mut fb, None);
    let lit = (98..142).filter(|&x| (2..18).any(|y| fb.at(x, y) == ferrite_ui::theme::map(ferrite_ui::gfx::TEXT))).count();
    assert!(lit > 20, "the callsign drawn in the clock's slot ({lit} columns)");
}

/// MENU-31: OK (or its digit) on a frequency setting opens the frequency entry box; the digits as on home, OK or a
/// whole frequency keeps it and BACK returns to the list; a frequency outside the setting's range is refused with the
/// digits left to fix. 0 5 4 (APRS) and 0 4 2 2 (pager); a range preset's edges the same way.
#[test]
fn frequency_settings_open_the_entry_box() {
    use crate::screens::entry::{EntryFor, FreqSet};
    let mut t = ready();
    menu0(&mut t, &[5, 4]);
    assert!(matches!(t.app.screen(), Screen::Entry(e) if e.to == EntryFor::Setting(FreqSet::Aprs)), "{:?}", t.app.screen());
    for d in [1, 4, 5, 4, 9, 5] {
        t.tap(Key::Digit(d));
    }
    if matches!(t.app.screen(), Screen::Entry(_)) {
        t.tap(Key::Menu);
    }
    assert_eq!(t.app.aprs_freq(), mhz(145) + 49_500);
    assert!(matches!(t.app.screen(), Screen::Group(_)), "back on the list: {:?}", t.app.screen());
    // out of range: refused, the entry stays with its digits
    t.hold(Key::Exit);
    menu0(&mut t, &[5, 4]);
    for d in [4, 4, 6] {
        t.tap(Key::Digit(d));
    }
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Entry(e) if e.n == 3), "refused: still typing ({:?})", t.app.screen());
    assert_eq!(t.app.aprs_freq(), mhz(145) + 49_500, "unchanged");
    // off the 5 kHz grid
    for _ in 0..3 {
        t.tap(Key::Exit);
    }
    for d in [1, 4, 4, 3, 9, 1] {
        t.tap(Key::Digit(d));
    }
    t.tap(Key::Menu);
    assert_eq!(t.app.aprs_freq(), mhz(145) + 49_500, "144.391 is off the grid");
    // the pager
    t.hold(Key::Exit);
    menu0(&mut t, &[4, 2, 2]);
    assert!(matches!(t.app.screen(), Screen::Entry(e) if e.to == EntryFor::Setting(FreqSet::Pager)));
    for d in [4, 3, 9, 9, 8, 7, 5] {
        t.tap(Key::Digit(d));
    }
    if matches!(t.app.screen(), Screen::Entry(_)) {
        t.tap(Key::Menu);
    }
    assert_eq!(t.app.set.pocsag_freq, mhz(439) + 98_750);
    // BACK on an empty box leaves without a change
    t.hold(Key::Exit);
    menu0(&mut t, &[4, 2, 2]);
    t.tap(Key::Exit);
    assert!(matches!(t.app.screen(), Screen::Group(_)));
    assert_eq!(t.app.set.pocsag_freq, mhz(439) + 98_750);
}

/// MENU-31: a range preset's edges take a typed frequency with the knob's checks: start under end, 18-1300 MHz,
/// never across 18 or 64 MHz (SCAN-14).
#[test]
fn range_edges_typed() {
    use crate::screens::entry::range_edge;
    let p = crate::data::RangePreset { name: Buf::new(), start: mhz(144), end: mhz(148), step: 0, mode: 0, narrow: false, skips: 0, resume: 0 };
    assert_eq!(range_edge(p, true, mhz(145)).map(|q| q.start), Ok(mhz(145)));
    assert_eq!(range_edge(p, false, mhz(150)).map(|q| q.end), Ok(mhz(150)));
    assert!(range_edge(p, true, mhz(149)).is_err(), "start over end");
    assert!(range_edge(p, false, mhz(1400)).is_err(), "over 1300");
    assert!(range_edge(p, true, mhz(50)).is_err(), "across 64 MHz");
}

/// A zone of the test store with two named channels, and the slot put on its first.
fn on_named_zone(t: &mut T) -> (crate::chan::Channel, crate::chan::Channel) {
    let st = store();
    let z = (0..16)
        .find(|&z| st.zone_len(z) > 1 && (0..2).all(|i| st.zone_at(z, i).is_some_and(|c| !c.name().is_empty())))
        .expect("a zone with two named channels");
    t.app.set_channel(0, z, 0, &st);
    (st.zone_at(z, 0).unwrap(), st.zone_at(z, 1).unwrap())
}

/// HOME-4.1: while a channel list scans, the card names the channel it is on and its last hit by name; a band
/// scan keeps the frequency.
#[test]
fn channel_scan_card_shows_names() {
    let mut t = T::new();
    let (a, b) = on_named_zone(&mut t);
    t.live.slots[0] = SlotLive { freq: b.rx, scanning: true, chan: b.num + 1, hits: 1, last_hit: a.rx, hit_chan: a.num + 1, ..Default::default() };
    assert_eq!(t.app.card_name(0, &t.live, &store()).as_bytes(), b.name(), "the step by name");
    assert_eq!(t.app.hit_name(0, &t.live, &store()).as_bytes(), a.name(), "the last hit by name");
    t.live.slots[0] = SlotLive { freq: mhz(146) + 94_000, scanning: true, ..Default::default() };
    assert!(t.app.card_name(0, &t.live, &store()).is_empty(), "a band scan: the frequency");
}

/// SCAN-23.1: a hit keeps how long its last stop lasted; a new stop starts it again.
#[test]
fn scan_hit_length() {
    let mut t = T::new();
    t.tap(Key::Side(2));
    let f = mhz(146) + 94_000;
    t.live.slots[0] = SlotLive { freq: f, last_hit: f, scanning: true, held: true, squelch_open: true, carrier: true, rssi: -80, hits: 1, ..Default::default() };
    t.wait(5000);
    t.live.slots[0].squelch_open = false;
    t.wait(1000);
    let l = t.app.radio.hits[0].len_s();
    assert!((4..=5).contains(&l), "a 5 s over: {l}");
    t.live.slots[0] = SlotLive { freq: f, last_hit: f, scanning: true, held: true, squelch_open: true, carrier: true, rssi: -80, hits: 2, ..Default::default() };
    t.wait(1000);
    assert!(t.app.radio.hits[0].len_s() <= 1, "a new stop starts again");
    assert_eq!(t.app.radio.hits[0].n, 2);
    let mut b = Buf::<16>::new();
    crate::screens::scan::span(14, &mut b);
    assert_eq!(b.as_bytes(), b"14 s");
    crate::screens::scan::span(0, &mut b);
    assert_eq!(b.as_bytes(), b"<1 s");
}

/// SCAN-23.2: on a channel scan OK opens the hit list too, rows by channel name, and OK there puts the slot on
/// that channel.
#[test]
fn channel_scan_hit_list() {
    let mut t = T::new();
    let (_, b) = on_named_zone(&mut t);
    t.tap(Key::Side(2));
    t.live.slots[0] = SlotLive { freq: b.rx, last_hit: b.rx, scanning: true, held: true, squelch_open: true, carrier: true, rssi: -80, hits: 1, ..Default::default() };
    t.wait(200);
    t.live.slots[0].squelch_open = false;
    t.wait(100);
    t.tap(Key::Menu);
    assert!(matches!(t.app.screen(), Screen::Scan(s) if s.list), "OK: the hit list");
    t.cmds();
    t.tap(Key::Menu);
    assert_eq!(t.app.screen(), Screen::Home);
    let sl = *t.app.slot(0);
    assert!(sl.on_channel() && sl.ch.is_some_and(|c| c.num == b.num), "on the hit's channel, not a VFO: {sl:?}");
    assert!(t.cmds().contains(&RadioCmd::Scan { slot: 0, on: false }));
}

/// SCAN-19.1 (the Oct 19 flicker): a scan only counts as stopped on a hit while the Radio task holds it
/// there; passing its last hit's frequency again, or a busy step it doesn't stop on, isn't one, so the hint bar
/// and PTT keep to the scanning case.
#[test]
fn passing_a_hit_is_not_a_stop() {
    let mut t = T::new();
    t.tap(Key::Side(2));
    let f = mhz(146) + 94_000;
    t.live.slots[0] = SlotLive { freq: f, last_hit: f, scanning: true, hits: 1, ..Default::default() };
    t.wait(100);
    assert_eq!(t.app.scan_hit[0], 0, "back on the last hit's frequency, not held");
    t.live.slots[0].squelch_open = true;
    t.wait(100);
    assert_eq!(t.app.scan_hit[0], 0, "a busy step the scan passes");
    t.live.slots[0].held = true;
    t.wait(100);
    assert_eq!(t.app.scan_hit[0], f, "held: stopped on it");
}

/// APRS-15.1: in 0 5 4's entry box, # with nothing typed puts the APRS frequency back on the region's.
#[test]
fn aprs_frequency_back_to_the_region() {
    use crate::screens::entry::{EntryFor, FreqSet};
    let mut t = ready();
    let region = t.app.aprs_freq();
    assert!(t.app.set_aprs_freq(mhz(145) + 49_500).is_ok());
    menu0(&mut t, &[5, 4]);
    assert!(matches!(t.app.screen(), Screen::Entry(e) if e.to == EntryFor::Setting(FreqSet::Aprs)));
    t.tap(Key::Hash);
    assert_eq!((t.app.aprs_freq(), t.app.aprs_cfg().f5k), (region, 0), "the region's again");
    assert!(matches!(t.app.screen(), Screen::Group(_)), "back on the list: {:?}", t.app.screen());
    assert!(t.app.toast.is_some_and(|x| x.head.as_bytes() == b"Region default"));
}

/// APRS-30: our APRS address is the callsign (without any SSID typed into it) and 0 5 5 1's SSID, 7 by
/// default (decision 191); 0 sends the bare callsign. The knob steps it, # puts back 7, the row names it.
#[test]
fn aprs_address_takes_the_ssid_setting() {
    let mut t = ready();
    t.app.set.set_callsign(b"VE3KPW");
    assert_eq!(t.app.aprs_call().as_bytes(), b"VE3KPW-7");
    t.app.set.set_callsign(b"VE3KPW-9");
    assert_eq!(t.app.aprs_call().as_bytes(), b"VE3KPW-7", "the setting wins over one typed in");
    t.app.set_aprs(crate::aprs::AprsCfg { ssid: 0, ..t.app.aprs_cfg() });
    assert_eq!(t.app.aprs_call().as_bytes(), b"VE3KPW");
    let dev = Default::default();
    let mut b = Buf::<24>::new();
    crate::screens::groups::change(&mut t.app, &[5, 5, 1], -1);
    assert_eq!(t.app.aprs_cfg().ssid, 15, "the knob wraps");
    assert_eq!(crate::screens::groups::wired(&t.app, &dev, &[5, 5, 1], &mut b), Some(true));
    assert_eq!(b.as_bytes(), b"15 Generic");
    // the 0 5 5 row in the APRS group shows the address beacons send (it said "-9" whatever the SSID)
    t.app.set_aprs(crate::aprs::AprsCfg { ssid: 7, ..t.app.aprs_cfg() });
    crate::screens::groups::wired(&t.app, &dev, &[5, 5], &mut b);
    assert_eq!(b.as_bytes(), b"VE3KPW-7");
    t.app.set.set_callsign(b"");
    assert!(t.app.aprs_call().is_empty(), "no callsign, no address");
}

/// SBAR-8: Bluetooth on with nothing connected shows a grey icon, connected a blue one, off none; in night red
/// (only red) connected is the rune cut out of a filled badge.
#[test]
fn bluetooth_icon_on_and_connected() {
    use ferrite_ui::gfx::{BG, BLUE, SUB};
    let t = ready();
    // pixels of colour `c` in the icon's place (None: any but the ground)
    let icon = |t: &T, dev: &Device, c: Option<u16>| {
        let mut fb = FrameBuffer::new();
        t.app.status_bar(&t.live, dev).draw(&mut fb, None);
        let bg = ferrite_ui::theme::map(BG);
        (194..210)
            .flat_map(|x| (2..18).map(move |y| (x, y)))
            .filter(|&(x, y)| c.map_or(fb.at(x, y) != bg, |c| fb.at(x, y) == ferrite_ui::theme::map(c)))
            .count()
    };
    let off = Device::default();
    t.app.set.bt = false;
    assert_eq!(icon(&t, &off, None), 0, "off: no icon");
    t.app.set.bt = true;
    assert!(icon(&t, &off, Some(SUB)) > 0 && icon(&t, &off, Some(BLUE)) == 0, "on, nothing connected: grey");
    let on = Device { bt: true, ..Default::default() };
    assert!(icon(&t, &on, Some(BLUE)) > 0 && icon(&t, &on, Some(SUB)) == 0, "connected: blue");
    assert!(line_of(&t, "bar ").contains(" bt_on=1 bt=0 "), "{}", line_of(&t, "bar "));
    ferrite_ui::theme::set(ferrite_ui::theme::NIGHT_RED);
    let (badge, rune) = (icon(&t, &on, None), icon(&t, &off, None));
    ferrite_ui::theme::set(0);
    assert!(badge > rune + 40, "night red: connected is a filled badge ({badge} lit vs {rune})");
}

/// SBAR-23, SELF-9.1, SELF-9.2, SELF-8: System's live rows ("Measuring" until the firmware's first 10 s), and About's
/// unique ID on two rows.
#[test]
fn system_and_about_rows() {
    use crate::screens::info::{def, Page};
    let t = ready();
    let show = |p: Page, label: &str, row: usize, dev: &Device| {
        let rows = def(p).rows;
        let i = rows.iter().position(|r| r.0 == label).expect("the row") + row;
        let mut b = Buf::<40>::new();
        (rows[i].1)(&crate::screens::table::Ctx { app: &t.app, live: &t.live, dev, x: &() }, &mut b);
        std::string::String::from_utf8(b.as_bytes().to_vec()).unwrap()
    };
    let none = Device::default();
    for l in ["CPU", "RAM", "Stack free"] {
        assert_eq!(show(Page::System, l, 0, &none), "Measuring", "{l}");
    }
    let dev = Device { stack_free: 52_428, ram_statics: 30_272, cpu_pct: Some(7), ..Default::default() };
    assert_eq!(show(Page::System, "Stack free", 0, &dev), "51.1 KB");
    assert_eq!(show(Page::System, "CPU", 0, &dev), "7 %");
    assert_eq!(show(Page::System, "RAM", 0, &dev), "45 of 96 KB", "96 KB less the stack never used");
    assert_eq!(show(Page::About, "ID", 0, &none), "Not read");
    let mut id = Device::default();
    id.selftest.uid = [0x3F2A_91C0, 0x7D44_0E18, 0xA2B6_5C93];
    // SELF-8: the 96 bits as 20 base32 digits, one row of four groups of five
    assert_eq!(show(Page::About, "ID", 0, &id), "R28JM FRR1S 27V4T WPTH0", "one row");
}

/// PWR-26.1, PWR-26.9: the App's reasons for the build's speed: a key (3 s), the pager and SAME decoders (not APRS: PWR-26.9)
/// (SAME only on a weather channel), a scan, an exclusive mode, transmit and the Transmit screen; none at rest.
#[test]
fn clock_reasons() {
    use ferrite_core::clocktree::why;
    let mut t = ready();
    t.wait(5_000);
    let r = |t: &T| t.app.clock_reasons(&t.live, t.t);
    assert_eq!(r(&t), 0, "rest");
    t.tap(Key::Down);
    assert_eq!(r(&t) & why::INPUT, why::INPUT, "a key");
    t.wait(3_100);
    assert_eq!(r(&t) & why::INPUT, 0, "3 s after it");
    t.app.set.aprs_mode = true;
    assert_eq!(r(&t), 0, "PWR-26.9: APRS mode alone (its capture, while the squelch is open, is the reason)");
    t.app.set.aprs_mode = false;
    t.app.set.pocsag_on = true;
    t.live.slots[1].freq = t.app.set.pocsag_freq;
    assert_eq!(r(&t), why::DECODE, "the pager decoder");
    t.app.set.pocsag_on = false;
    t.app.set.same = true;
    assert!(t.app.same_wanted(), "alerts on in the US");
    t.live.slots[1].freq = 16_240_000;
    assert_eq!(r(&t), why::DECODE, "SAME on a weather channel");
    t.live.slots[1].freq = 14_652_000;
    assert_eq!(r(&t), 0, "SAME off the weather channels");
    t.live.slots[0].scanning = true;
    assert_eq!(r(&t), why::SCAN);
    t.live.slots[0].scanning = false;
    t.live.mode = ModeLive::Probe { freq: 0, rssi: 0, tone: 0 };
    assert_eq!(r(&t), why::MODE);
    t.live.mode = ModeLive::Off;
    t.live.tx.keyed = true;
    assert_eq!(r(&t) & why::TX, why::TX, "keyed");
}

/// PWR-26.6: System's Clock row: the speed and the switches since power on, with thousands commas.
#[test]
fn system_clock_row() {
    use crate::screens::info::{def, Page};
    let t = ready();
    let show = |dev: &Device| {
        let rows = def(Page::System).rows;
        let i = rows.iter().position(|r| r.0 == "Clock").expect("the row");
        let mut b = Buf::<40>::new();
        (rows[i].1)(&crate::screens::table::Ctx { app: &t.app, live: &t.live, dev, x: &() }, &mut b);
        String::from_utf8(b.as_bytes().to_vec()).unwrap()
    };
    assert_eq!(show(&Device::default()), "Not measured", "the simulators");
    let mut d = Device { mcu: ferrite_core::msg::McuClock { mhz: 24, switches: 1_234, held_us: 180, why: 0 }, ..Default::default() };
    assert_eq!(show(&d), "24 MHz \u{b7} 1,234 switches");
    d.mcu = ferrite_core::msg::McuClock { mhz: 120, switches: 1, held_us: 180, why: 1 };
    assert_eq!(show(&d), "120 MHz \u{b7} 1 switch");
    d.mcu.switches = 1_002_003;
    assert_eq!(show(&d), "120 MHz \u{b7} 1,002,003 switches");
}

/// TX-30.2: at the timeout the alert tone sounds once, a moment after the unkey (never while keyed).
#[test]
fn timeout_beeps_after_the_unkey() {
    let mut t = T::new();
    t.app.radio.tx_built = true;
    t.app.set_region(Some(ferrite_core::txplan::Region::Us), false);
    t.app.set.tx_timeout = 1; // 30 s
    let s = store();
    t.app.key_down(Key::PTT, t.t, &s);
    assert!(matches!(t.app.screen(), Screen::Transmit(_)));
    t.cmds();
    t.wait(30_000);
    let c = t.cmds();
    assert!(!c.iter().any(|c| matches!(c, RadioCmd::Beep { hz: crate::ALERT_HZ, .. })) || t.app.radio.keyed.is_none());
    t.wait(600);
    let c = [c, t.cmds()].concat();
    assert!(c.iter().any(|c| matches!(c, RadioCmd::Beep { hz: crate::ALERT_HZ, .. })), "the beep: {c:?}");
    assert!(t.app.radio.keyed.is_none(), "unkeyed");
    t.app.key_up(Key::PTT, t.t, &s);
}

/// APRS-2, APRS-15.2: 0 5 9 6 Mute speaker decides whether APRS mode mutes C; \* on C in APRS mode flips the
/// setting, and the setting changed in the menu mutes or unmutes C at once. Without APRS mode \* on C is quick
/// mute alone.
#[test]
fn aprs_mute_follows_the_setting_and_star() {
    let mut t = ready();
    let c = 1 << crate::aprs::APRS_SLOT;
    assert!(t.app.set.aprs_mute, "on by default");
    t.app.set_aprs_mode(true);
    assert_ne!(t.app.mute & c, 0, "APRS mode mutes C");
    t.app.active = crate::aprs::APRS_SLOT;
    t.tap(Key::Star);
    assert_eq!(t.app.mute & c, 0);
    assert!(!t.app.set.aprs_mute, "\\* on C is the setting");
    t.app.set_aprs_mode(false);
    t.app.set_aprs_mode(true);
    assert_eq!(t.app.mute & c, 0, "kept off: APRS mode comes back heard");
    crate::screens::groups::change(&mut t.app, &[5, 9, 6], 1);
    assert!(t.app.set.aprs_mute);
    assert_ne!(t.app.mute & c, 0, "the menu mutes C at once");
    t.app.set_aprs_mode(false);
    assert_eq!(t.app.mute & c, 0, "APRS mode off: heard");
    t.tap(Key::Star);
    assert_ne!(t.app.mute & c, 0, "quick mute as any slot");
    assert!(t.app.set.aprs_mute, "the setting untouched without APRS mode");
}

/// DEC-6.1, LINK-19: DTMF heard on a slot takes its card's state word, in green ("DTMF 101#"), ahead of a tone
/// hint; gone again when the Radio task clears it. 0 4 3 is on by default, off in Low Power Mode (DEC-6).
#[test]
fn card_shows_dtmf_heard() {
    let mut t = T::new();
    assert!(t.app.settings().dtmf_rx && t.app.dtmf_wanted());
    t.live.slots[0].squelch_open = true;
    t.live.slots[0].heard_tone = 885;
    let mut d = ferrite_core::dtmfrx::Heard::NONE;
    for c in [1, 0, 1, 15] {
        d.push(c);
    }
    t.live.slots[0].dtmf = d;
    t.wait(100);
    let l = line_of(&t, "card A");
    assert!(l.contains("state=\"DTMF\" hint=\"DTMF 101#\""), "{l}");
    assert_eq!(t.app.state(0, &t.live).1, ferrite_ui::gfx::GREEN);
    t.live.slots[0].dtmf = ferrite_core::dtmfrx::Heard::NONE;
    t.wait(100);
    assert!(line_of(&t, "card A").contains("hint=\"Heard 88.5\""));
}

/// WATCH-11.2, WATCH-11.3, SBAR-17: when the relay cancels watch (the Radio task marks the watched slots that need
/// the other relay state), a toast says so once, and the Status page's Watch row says why until watch resumes.
#[test]
fn relay_cancel_toast_and_status_row() {
    let mut t = T::new();
    t.app.set_watch(2);
    t.wait(100);
    t.app.toast = None;
    t.live.slots[1].relay = true;
    t.live.slots[2].relay = true;
    t.wait(100);
    let toast = t.app.toast.expect("a toast when the relay cancels watch");
    assert_eq!((toast.head.as_bytes(), toast.body.as_bytes()), (&b"Watch off"[..], &b"HF needs the relay"[..]));
    t.app.toast = None;
    t.wait(100);
    assert!(t.app.toast.is_none(), "said once, not every tick");
    t.open(Dest::Status);
    assert!(readback(&t).iter().any(|l| l.contains("Off: HF needs the relay")), "{:?}", readback(&t));
    t.live.slots[1].relay = false;
    t.live.slots[2].relay = false;
    t.wait(100);
    assert!(!readback(&t).iter().any(|l| l.contains("HF needs the relay")), "{:?}", readback(&t));
}
