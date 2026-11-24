//! Broadcast FM by frequency (manual chapter 20, BCR-21 to BCR-30): a slot in the broadcast band plays on the
//! SI4732, one slot at a time, sharing the FM screen's state.

use super::*;
use crate::bcast::rds_text;
use crate::screens::activity::Mode;
use ferrite_core::freq::mhz;
use ferrite_core::msg::{BroadcastCmd, ModeLive};
use ferrite_core::txplan::Region;

const FM1005: Freq10 = mhz(100) + 50_000;
const FM991: Freq10 = mhz(99) + 10_000;
const TWO_M: Freq10 = mhz(146) + 52_000;

fn ready() -> T {
    let mut t = T::new();
    t.app.radio.tx_built = true;
    t.app.set_region(Some(Region::Us), false);
    t.wait(100);
    t.cmds();
    t
}

fn tune(t: &mut T, s: u8, f: Freq10) -> Vec<RadioCmd> {
    t.app.set_vfo(s, f);
    t.wait(100);
    t.cmds()
}

fn live_fm(t: &mut T, freq: u16, heard: bool) {
    let mut rt = [0u8; 64];
    rt[..9].copy_from_slice(b"Some text");
    t.live.mode = ModeLive::Fm { freq, ps: *b"STATION ", rt, rssi: 40, snr: 20, stereo: true, pty: 0, heard };
    t.live.bcast_heard = heard;
}

/// BCR-21, BCR-22: each slot tuned into the band plays broadcast on the SI4732 by itself, and tuned out is a
/// BK4829 slot again.
#[test]
fn each_slot_enters_and_leaves_the_band() {
    for s in 0..3u8 {
        let mut t = ready();
        let c = tune(&mut t, s, FM1005);
        assert_eq!(t.app.bcast_slot(), Some(s));
        assert_eq!(t.app.radio.mode, Mode::Fm);
        assert!(c.contains(&RadioCmd::BcastSlot(Some(s))), "{c:?}");
        assert!(c.contains(&RadioCmd::Broadcast(BroadcastCmd::Fm(10_050))), "{c:?}");
        let c = tune(&mut t, s, TWO_M);
        assert_eq!(t.app.bcast_slot(), None);
        assert_eq!(t.app.radio.mode, Mode::Off);
        assert!(c.contains(&RadioCmd::Broadcast(BroadcastCmd::Off)) && c.contains(&RadioCmd::BcastSlot(None)), "{c:?}");
    }
}

/// BCR-21: the band's edges, inclusive; the APRS slot is never broadcast (APRS-19).
#[test]
fn band_edges_and_the_aprs_slot() {
    let mut t = ready();
    for (f, inb) in [(mhz(87) + 40_000, false), (mhz(87) + 50_000, true), (mhz(108), true), (mhz(108) + 2_500, false)] {
        tune(&mut t, 0, f);
        assert_eq!(t.app.bcast_slot().is_some(), inb, "{f}");
    }
    tune(&mut t, 0, TWO_M);
    t.app.set.aprs_mode = true;
    tune(&mut t, 2, FM1005);
    assert_eq!(t.app.bcast_slot(), None, "APRS on C");
}

/// BCR-21: region Japan's band is 76-95 MHz.
#[test]
fn japan_band() {
    let mut t = ready();
    t.app.set_region(Some(Region::Japan), false);
    for (f, inb) in [(mhz(80), true), (mhz(90), true), (mhz(95), true), (FM1005, false), (mhz(75), false)] {
        tune(&mut t, 0, f);
        assert_eq!(t.app.bcast_slot().is_some(), inb, "{f}");
    }
    t.app.set_region(Some(Region::Us), false);
    tune(&mut t, 0, mhz(80));
    assert_eq!(t.app.bcast_slot(), None, "80 MHz outside Japan");
}

/// BCR-22: a second slot tuned in takes the broadcast; the first shows "FM on B" until it is tuned out; when the
/// broadcast slot leaves, a slot still in the band takes it back.
#[test]
fn handover_between_slots() {
    let mut t = ready();
    tune(&mut t, 0, FM1005);
    let c = tune(&mut t, 1, FM991);
    assert_eq!(t.app.bcast_slot(), Some(1));
    assert!(c.contains(&RadioCmd::BcastSlot(Some(1))) && c.contains(&RadioCmd::Broadcast(BroadcastCmd::Fm(9_910))), "{c:?}");
    assert!(!c.contains(&RadioCmd::Broadcast(BroadcastCmd::Off)), "one receiver, retuned: {c:?}");
    assert_eq!(t.app.state(0, &t.live).0, "FM on B");
    tune(&mut t, 0, mhz(101)); // A moves within the band: B keeps it
    assert_eq!(t.app.bcast_slot(), Some(1));
    let c = tune(&mut t, 1, TWO_M);
    assert_eq!(t.app.bcast_slot(), Some(0), "A takes it back");
    assert!(c.contains(&RadioCmd::Broadcast(BroadcastCmd::Fm(10_100))), "{c:?}");
}

/// BCR-24, BCR-26.4, BCR-26.5: the card says FM while the broadcast plays, muted while a slot heard has the speaker
/// (the Radio task decides, 2 s after: ferrite-core's watch tests); BCR-15: a slot needing the relay says so.
#[test]
fn card_states() {
    let mut t = ready();
    tune(&mut t, 0, FM1005);
    live_fm(&mut t, 10_050, true);
    assert_eq!(t.app.state(0, &t.live).0, "FM");
    live_fm(&mut t, 10_050, false);
    assert_eq!(t.app.state(0, &t.live).0, "muted");
    tune(&mut t, 1, mhz(50) + 12_500);
    assert_eq!(t.app.state(1, &t.live).0, "Watch off: relay");
}

/// HOME-15: \* on the broadcast slot mutes the broadcast too: the mask goes to the Radio task (which keeps the
/// SI4732 muted) and the card says Muted.
#[test]
fn star_mutes_the_broadcast() {
    let mut t = ready();
    tune(&mut t, 0, FM1005);
    live_fm(&mut t, 10_050, true);
    t.tap(Key::Star);
    assert!(t.cmds().contains(&RadioCmd::Mute(0b001)));
    live_fm(&mut t, 10_050, false);
    assert_eq!(t.app.state(0, &t.live).0, "Muted");
    t.tap(Key::Star);
    assert!(t.cmds().contains(&RadioCmd::Mute(0)));
}

/// BCR-26.3, BCR-30: the radio text (all 64 characters), else the station name, else nothing.
#[test]
fn rds_text_rule() {
    let mut rt = [0u8; 64];
    assert_eq!(rds_text(b"CBC R1  ", &rt), b"CBC R1");
    rt[..5].copy_from_slice(b"Hello");
    assert_eq!(rds_text(b"CBC R1  ", &rt), b"Hello");
    assert_eq!(rds_text(b"        ", &[0; 64]), b"");
    let full = [b'x'; 64];
    assert_eq!(rds_text(b"CBC R1  ", &full).len(), 64, "all 64 characters");
}

/// BCR-25: on home the knob tunes a broadcast VFO by the FM step (0 2 6 1) and ◀▶ seek; the station sought is
/// taken into the slot as it lands.
#[test]
fn knob_steps_and_seek() {
    let mut t = ready();
    tune(&mut t, 0, mhz(108) + 7_500); // airband: the slot's own step
    for _ in 0..40 {
        if t.app.bcast_slot().is_some() {
            break;
        }
        t.knob(-1);
        t.wait(50);
    }
    assert_eq!(t.app.bcast_slot(), Some(0), "tuned into the band");
    tune(&mut t, 0, mhz(108));
    t.knob(-1);
    t.wait(100);
    assert_eq!(t.freq(), mhz(107) + 90_000, "100 kHz");
    t.cmds();
    t.tap(Key::Right);
    assert!(t.cmds().contains(&RadioCmd::Broadcast(BroadcastCmd::Seek { up: true })));
    live_fm(&mut t, 10_650, true);
    t.wait(200);
    assert_eq!(t.freq(), mhz(106) + 50_000, "the sought station");
    assert!(!t.cmds().contains(&RadioCmd::Broadcast(BroadcastCmd::Fm(10_650))), "not tuned again");
}

/// BCR-1, BCR-25 (decision 352): MENU 0 2 6 is the broadcast's settings, not a screen; 0 2 6 1 the step, which
/// the broadcast slot tunes by.
#[test]
fn fm_settings_group_not_a_screen() {
    let mut t = ready();
    tune(&mut t, 0, FM1005);
    menu0(&mut t, &[2, 6]);
    assert!(matches!(t.app.screen(), Screen::Group(_)), "{:?}", t.app.screen());
    assert_eq!(t.app.radio.mode, Mode::Fm, "the slot still plays");
    menu0(&mut t, &[2, 6, 1]);
    t.knob(1);
    assert_eq!(t.app.set.fm_step, 2);
    t.hold(Key::Exit);
    t.cmds();
    t.knob(1);
    t.wait(100);
    assert_eq!(t.app.slot(0).freq(), mhz(100) + 70_000, "the slot moved by the 200 kHz step");
}

fn menu0(t: &mut T, path: &[u8]) {
    t.hold(Key::Exit);
    t.tap(Key::Menu);
    t.tap(Key::Digit(0));
    for &d in path {
        t.tap(Key::Digit(d));
    }
}

/// BCR-28: quick settings on a broadcast slot are FM's rows.
#[test]
fn quick_settings_rows() {
    let mut t = ready();
    tune(&mut t, 0, FM1005);
    t.tap(Key::Menu);
    t.tap(Key::Digit(2));
    t.knob(1);
    assert_eq!(t.app.set.fm_agc, 1, "row 2: AGC");
}

/// A stored FM AGC or clock value out of range (a bad record) must not overflow the menu's u8 arithmetic: the
/// value text and the knob work, and the knob brings it back in range; load and the link clamp it first.
#[test]
fn out_of_range_agc_and_clock_do_not_overflow() {
    let mut t = ready();
    t.app.set.fm_agc = 200;
    t.app.set.clock = 200;
    menu0(&mut t, &[2, 6, 2]);
    t.knob(1);
    assert!(t.app.set.fm_agc <= crate::settings::AGC_MAX, "{}", t.app.set.fm_agc);
    menu0(&mut t, &[6, 6]);
    t.knob(1);
    assert!(t.app.set.clock <= 1, "{}", t.app.set.clock);
    let mut s = crate::settings::Settings::DEFAULT;
    (s.fm_agc, s.clock) = (255, 255);
    s.sanitize();
    assert_eq!((s.fm_agc, s.clock), (crate::settings::Settings::DEFAULT.fm_agc, crate::settings::Settings::DEFAULT.clock));
    (s.fm_agc, s.clock) = (crate::settings::AGC_MAX, 1);
    s.sanitize();
    assert_eq!((s.fm_agc, s.clock), (crate::settings::AGC_MAX, 1), "in range: kept");
}

/// TX-24, BCR-29.3: PTT on another slot stops the broadcast before the key-up and starts it again after.
#[test]
fn transmit_switches_the_receiver_away_and_back() {
    let mut t = ready();
    tune(&mut t, 1, FM1005);
    let s = store();
    t.app.key_down(Key::PTT, t.t, &s);
    t.t += 50;
    t.app.tick(t.t, &t.live, &s);
    let c = t.cmds();
    let off = c.iter().position(|c| *c == RadioCmd::Broadcast(BroadcastCmd::Off)).expect("broadcast off");
    let key = c.iter().position(|c| matches!(c, RadioCmd::Transmit { on: true, .. })).expect("keyed");
    assert!(off < key, "{c:?}");
    t.app.key_up(Key::PTT, t.t, &s);
    t.wait(200);
    assert!(t.cmds().contains(&RadioCmd::Broadcast(BroadcastCmd::Fm(10_050))));
    assert_eq!(t.app.radio.mode, Mode::Fm);
}

/// MODE-5, BCR-29.2, HOME-14 (decision 115): on single + scope with another slot playing broadcast, home's strip sweeps
/// around the active slot and the broadcast keeps playing beside it: the knob stops and restarts the strip only
/// (MODE-4), never the broadcast; the strip tells the Radio task it is the strip (it looks at the slot's own
/// frequency). The broadcast slot made active stops the strip and keeps the broadcast; so does an active slot
/// needing the band relay (BCR-15).
#[test]
fn strip_sweeps_beside_the_broadcast() {
    let mut t = ready();
    t.app.set_layout(1);
    t.wait(600);
    let mut c = t.cmds();
    c.extend(tune(&mut t, 1, FM1005));
    t.wait(600);
    c.extend(t.cmds());
    assert_eq!((t.app.radio.mode, t.app.radio.fm_on), (Mode::Strip, true), "{c:?}");
    assert!(c.contains(&RadioCmd::Broadcast(BroadcastCmd::Fm(10_050))), "{c:?}");
    assert!(c.contains(&RadioCmd::ScopeCfg { bars: crate::screens::scope::STRIP_BARS as u8, dwell: 100, strip: true, hf: false, si: false }), "{c:?}");
    assert!(c.iter().any(|c| matches!(c, RadioCmd::Sweep { centre: TWO_M, on: true, .. })), "{c:?}");
    assert!(!t.app.strip_blocked());
    // the knob: the strip waits for it to rest; the broadcast plays on, untouched
    t.knob(1);
    t.wait(100);
    let c = t.cmds();
    assert!(c.iter().any(|c| matches!(c, RadioCmd::Sweep { on: false, .. })), "{c:?}");
    assert_eq!((t.app.radio.mode, t.app.radio.fm_on), (Mode::Fm, true));
    t.wait(600);
    let c = t.cmds();
    assert_eq!(t.app.radio.mode, Mode::Strip);
    assert!(!c.iter().any(|c| matches!(c, RadioCmd::Broadcast(_))), "the broadcast never restarts: {c:?}");
    // the slot heard while the strip sweeps (the strip's report has the mode): B says FM, then muted
    t.live.mode = ModeLive::Scope { centre: TWO_M, bars: [0; ferrite_core::msg::SCOPE_BARS], n: 56, sweeps: 1, listen: 0, open: false, si: false };
    t.live.bcast_heard = true;
    assert_eq!(t.app.state(1, &t.live).0, "FM");
    t.live.bcast_heard = false;
    assert_eq!(t.app.state(1, &t.live).0, "muted");
    // the broadcast slot active: HOME-14, the strip off, the broadcast on
    t.tap(Key::Slot);
    t.wait(600);
    let c = t.cmds();
    assert_eq!(t.app.active, 1);
    assert_eq!((t.app.radio.mode, t.app.radio.fm_on), (Mode::Fm, true), "{c:?}");
    assert!(c.iter().any(|c| matches!(c, RadioCmd::Sweep { on: false, .. })) && !c.iter().any(|c| matches!(c, RadioCmd::Broadcast(_))), "{c:?}");
    // an active slot on 6 m needs the relay the broadcast leaves released: no strip
    t.tap(Key::Slot);
    tune(&mut t, 2, mhz(50) + 12_500);
    t.wait(600);
    assert_eq!(t.app.active, 2);
    assert!(t.app.strip_blocked());
    assert_eq!((t.app.radio.mode, t.app.radio.fm_on), (Mode::Fm, true));
}

/// TX-24, MODE-5: PTT with the strip sweeping beside a broadcast stops both before the key-up, and both come back
/// after.
#[test]
fn transmit_stops_the_strip_and_the_broadcast() {
    let mut t = ready();
    t.app.set_layout(1);
    tune(&mut t, 1, FM1005);
    t.wait(600);
    t.cmds();
    let s = store();
    t.app.key_down(Key::PTT, t.t, &s);
    t.t += 50;
    t.app.tick(t.t, &t.live, &s);
    let c = t.cmds();
    let off = c.iter().position(|c| *c == RadioCmd::Broadcast(BroadcastCmd::Off)).expect("broadcast off");
    let sweep = c.iter().position(|c| matches!(c, RadioCmd::Sweep { on: false, .. })).expect("sweep off");
    let key = c.iter().position(|c| matches!(c, RadioCmd::Transmit { on: true, .. })).expect("keyed");
    assert!(off < key && sweep < key, "{c:?}");
    t.app.key_up(Key::PTT, t.t, &s);
    t.wait(800);
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::Broadcast(BroadcastCmd::Fm(10_050))), "{c:?}");
    assert_eq!((t.app.radio.mode, t.app.radio.fm_on), (Mode::Strip, true));
}

/// MODE-3, MODE-5: the Radio task dropped the modes (the lease ran out): the app takes both as stopped and starts
/// the broadcast and the strip again.
#[test]
fn lease_drop_restarts_both() {
    let mut t = ready();
    t.app.set_layout(1);
    tune(&mut t, 1, FM1005);
    t.wait(600);
    t.cmds();
    t.live.lease_drops = t.live.lease_drops.wrapping_add(2);
    t.wait(100);
    let c = t.cmds();
    assert!(c.contains(&RadioCmd::Broadcast(BroadcastCmd::Fm(10_050))) && c.iter().any(|c| matches!(c, RadioCmd::Sweep { on: true, .. })), "{c:?}");
    assert!(!c.contains(&RadioCmd::Broadcast(BroadcastCmd::Off)), "{c:?}");
}

/// BCR-5 (decision 352): the sleep timer quick-mutes the broadcast slot once its time is up; the slot stays, and
/// \* on it plays it again.
#[test]
fn sleep_timer_mutes_the_broadcast_slot() {
    let mut t = ready();
    crate::screens::groups::change(&mut t.app, &[2, 6, 4], 1); // 15 min
    tune(&mut t, 0, FM1005);
    t.wait(14 * 60_000);
    assert_eq!(t.app.mute & 1, 0, "still playing at 14 min");
    t.cmds();
    t.wait(61_000);
    assert_eq!(t.app.mute & 1, 1, "muted at 15 min");
    assert!(t.cmds().contains(&RadioCmd::Mute(1)));
    assert_eq!(t.app.bcast_slot(), Some(0), "the slot is still the broadcast slot")
}

/// BCR-4: radio monitor off (0 2 6 3) mutes every slot but the broadcast one while it plays, so none takes the
/// speaker from it; on again, or the broadcast over, they are heard again. The quick mutes (HOME-15) stay as set.
#[test]
fn radio_monitor_off_keeps_the_slots_quiet() {
    let mut t = ready();
    t.app.set.fm_monitor = false;
    let mut c = tune(&mut t, 0, FM1005);
    t.wait(200);
    c.extend(t.cmds());
    assert!(c.contains(&RadioCmd::Mute(0b110)), "B and C quiet while A plays: {c:?}");
    assert_eq!(t.app.mute, 0, "not quick mutes: the cards don't say Muted");
    t.app.set.fm_monitor = true;
    t.wait(100);
    assert!(t.cmds().contains(&RadioCmd::Mute(0)), "monitor on: heard again");
    t.app.set.fm_monitor = false;
    t.wait(100);
    t.cmds();
    let mut c = tune(&mut t, 0, TWO_M);
    t.wait(200);
    c.extend(t.cmds());
    assert!(c.contains(&RadioCmd::Mute(0)), "the broadcast over: heard again: {c:?}");
}

/// BCR-33 (decision 353, as nicFW's tuner channels): a slot in WFM is a broadcast slot anywhere 64-108 MHz, and
/// tunes there (the low band too); in FM the same frequency is a two-way slot; WFM never transmits.
#[test]
fn wfm_is_a_broadcast_slot_on_64_to_108() {
    let mut t = ready();
    tune(&mut t, 0, mhz(70) + 50_000);
    t.wait(100);
    assert_eq!(t.app.bcast_slot(), None, "FM on 70.05: a two-way slot");
    while t.app.slot(0).cfg().mode != crate::slot::WFM {
        t.app.change(crate::slot::Item::Mode, 1);
    }
    t.wait(100);
    assert_eq!(t.app.bcast_slot(), Some(0), "WFM on 70.05: broadcast");
    assert_eq!(t.app.fm_range(), (6_400, 10_800), "its range is the SI4732's");
    assert_eq!(crate::screens::transmit::refusal(&t.app, 0), Some(ferrite_core::txplan::Refusal::WfmRxOnly), "TX-20.6: WFM says it is broadcast");
    assert_eq!(crate::screens::transmit::short(ferrite_core::txplan::Refusal::WfmRxOnly), "WFM is broadcast: RX only");
}
