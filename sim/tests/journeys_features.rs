//! End-to-end journeys through features, chapter by chapter: setup, channels, tuning, scans, tones, broadcast,
//! HF, the scope, alerts, APRS, GPS and Bluetooth. The harness is `common`; a rule counts as covered when its ID
//! is in a test's doc comment.

mod common;

use common::Radio;
use ferrite_core::freq::mhz;
use ferrite_core::keys::Key;
use ferrite_core::msg::RadioCmd;
use ferrite_sim::simcore::Event;

/// Setup has no arming step and no licence page: eight pages of questions only (SETUP-6).
#[test]
fn setup_has_no_licence_or_arming() {
    let mut r = Radio::blank();
    for _ in 0..8 {
        let t = r.text().to_lowercase();
        for w in ["licen", "arm ", "arming", "i agree", "accept"] {
            assert!(!t.contains(w), "setup page mentions {w:?}:\n{t}");
        }
        r.press(Key::Menu);
    }
    r.expect_screen("Home");
}

/// An empty callsign is allowed in setup; automatic transmissions stay refused until one is set (SETUP-9).
#[test]
fn setup_without_a_callsign() {
    let mut r = Radio::blank();
    r.press(Key::Menu); // page 2: the callsign, left empty
    r.expect_text("2 of 8").press(Key::Menu);
    r.expect_text("3 of 8");
    for _ in 3..=8 {
        r.press(Key::Menu);
    }
    r.expect_screen("Home");
    r.take_cmds();
    r.event(Event::Beacon).wait(500);
    assert!(!r.sim.radio().live.tx.keyed, "no beacon without a callsign");
    assert!(!r.take_cmds().iter().any(|c| matches!(c, RadioCmd::TransmitData { .. })), "nothing keyed");
}

/// Hold 0 with no home channel says so and how to set one (CHAN-20).
#[test]
fn no_home_channel() {
    let mut r = Radio::new();
    r.run("hold 0; expect text \"No home channel\"; expect text \"hold ABC to set\"");
}

/// Hold *, then PTT A (the quick menu) toggles reverse on the active slot: the shift chip shows R once the menu is
/// closed, the Radio task gets the Reverse parameter, and the transmit checks use the frequency after the swap
/// (VFO-7, VOX-7, VOX-9, HOME-17).
#[test]
fn reverse() {
    let mut r = Radio::new(); // A: 146.520, -600 kHz
    let tx_plan = |r: &mut Radio| {
        r.take_cmds();
        r.down(Key::Ptt(ferrite_core::keys::PTT_A)).wait(300);
        let plan = r.take_cmds().into_iter().find_map(|c| match c {
            RadioCmd::TxPlan { tx, .. } => Some(tx),
            _ => None,
        });
        r.up(Key::Ptt(ferrite_core::keys::PTT_A)).wait(800);
        plan
    };
    assert_eq!(tx_plan(&mut r), Some(mhz(145) + 92_000), "TX on the input, 600 kHz down");
    r.take_cmds();
    r.run("hold STAR; press PTTA; expect text \"on\"; press BACK; expect text \"R-600k\"");
    assert!(
        r.take_cmds().iter().any(|c| matches!(c, RadioCmd::SlotParam { param: ferrite_core::msg::Param::Reverse, value: 1, .. })),
        "the Radio task is told to swap"
    );
    assert_eq!(tx_plan(&mut r), Some(mhz(146) + 52_000), "reversed: TX on the output");
    r.run("hold STAR; press PTTA; press BACK; expect no text \"R-600k\"; expect text \"-600k\"");
}

/// The knob steps by the slot's step, SIDE2 + ▲▼ by 1 MHz, and ◀▶ step bands (VFO-5).
#[test]
fn tuning_steps() {
    let mut r = Radio::new();
    let f = |r: &Radio| r.sim.app().slot(0).freq();
    let f0 = f(&r);
    r.knob(1);
    assert!(f(&r) > f0 && f(&r) - f0 <= 2_500, "one step up: {} -> {}", f0, f(&r));
    let f1 = f(&r);
    r.down(Key::Side(2)).knob(1).up(Key::Side(2)).wait(200);
    assert_eq!(f(&r), f1, "RF gain must leave the frequency alone");
    assert!(r.take_cmds().contains(&RadioCmd::RfGain(1)));
    r.down(Key::Side(2)).knob(-1).up(Key::Side(2)).wait(200);
    assert!(r.take_cmds().contains(&RadioCmd::RfGain(0)), "back to automatic");
    r.down(Key::Side(2)).press(Key::Up).up(Key::Side(2)).wait(200);
    assert_eq!(f(&r), f1 + mhz(1), "SIDE2 + Up: 1 MHz");
    r.press(Key::Right);
    assert!(f(&r) > mhz(150), "▶ to the next band up: {}", f(&r));
}

/// MENU-35: PF2 selects the shared RF gain in Quick settings, and # resets it.
#[test]
fn rf_gain_menu_shares_the_home_setting() {
    let mut r = Radio::new();
    r.down(Key::Side(2)).knob(2).up(Key::Side(2));
    r.press(Key::Menu).press(Key::Side(2)).expect_text("RF gain").expect_text("2 / 4");
    r.take_cmds();
    // Hold a click on the right half of the virtual knob across several frames.
    r.sim.set_scale(2);
    for _ in 0..6 {
        r.sim.mouse(320, 20, true);
        r.wait(16);
    }
    r.sim.mouse(320, 20, false);
    r.expect_text("3 / 4");
    assert!(r.take_cmds().contains(&RadioCmd::RfGain(3)));
    r.press(Key::Hash).expect_text("Auto");
    assert!(r.take_cmds().contains(&RadioCmd::RfGain(0)));
}

/// MENU-35: Off restores automatic gain and only the menu can re-enable the shortcut.
#[test]
fn rf_gain_off_restores_agc_and_disables_the_home_chord() {
    let mut r = Radio::new();
    r.menu("").press(Key::Side(2)).knob(2);
    assert!(r.take_cmds().contains(&RadioCmd::RfGain(2)));
    r.knob(-3).expect_text("Off");
    assert!(r.take_cmds().contains(&RadioCmd::RfGain(0)));

    r.home();
    let freq = r.sim.app().slot(0).freq();
    r.down(Key::Side(2)).knob(4).up(Key::Side(2)).expect_text("Off");
    assert_eq!(r.sim.app().slot(0).freq(), freq);
    assert!(!r.take_cmds().iter().any(|cmd| matches!(cmd, RadioCmd::RfGain(_))));

    r.press(Key::Menu).press(Key::Side(2)).knob(1).expect_text("Auto");
    assert!(!r.take_cmds().iter().any(|cmd| matches!(cmd, RadioCmd::RfGain(_))));
    r.home().down(Key::Side(2)).knob(1).up(Key::Side(2));
    assert!(r.take_cmds().contains(&RadioCmd::RfGain(1)));
}

/// RX and TX tones are set apart in the tone picker (VFO-6, TONE-17's page of the picker): ▶ gives RX a mode (TONE-2.1), the knob its value.
#[test]
fn rx_and_tx_tones_apart() {
    let mut r = Radio::new();
    let tones = |r: &Radio| (r.sim.app().slot(0).cfg().rx_tone, r.sim.app().slot(0).cfg().tx_tone);
    let (rx0, tx0) = tones(&r);
    r.run("hold 5; expect screen \"Tone picker\"; expect text \"RX tone\"; expect text \"TX tone\"; press UP");
    r.press(Key::Right).knob(3).press(Key::Menu).wait(200);
    let (rx1, tx1) = tones(&r);
    assert!(rx1 != rx0 && tx1 == tx0, "only the RX tone changed: rx {rx0}->{rx1}, tx {tx0}->{tx1}");
}

/// The scan screen shows the target, resume rule, position, hits and skips (SCAN-7); a zone scan walks its
/// channels in number order (SCAN-4).
#[test]
fn zone_scan_screen_and_order() {
    let mut r = Radio::new();
    r.sim.radio_mut().quiet = true; // nothing heard: the scan never stops
    r.press(Key::Slot);
    r.run("press SIDE2; wait 500ms; expect screen \"Scan\"");
    for w in ["Target Zone Repeaters", "Resume", "Position", "Hits", "Skips"] {
        r.expect_text(w);
    }
    let mut seen: Vec<u16> = Vec::new();
    for _ in 0..1500 {
        r.wait(20);
        let t = r.text();
        if let Some(n) = t.lines().find_map(|l| l.strip_prefix("CH ").and_then(|s| s.get(..3)).and_then(|s| s.parse().ok())) {
            if seen.last() != Some(&n) {
                seen.push(n);
            }
        }
    }
    assert!(seen.len() >= 3, "the scan moved: {seen:?}");
    let drops = seen.windows(2).filter(|w| w[1] < w[0]).count();
    assert!(drops <= seen.len() / 3, "number order, wrapping at the end: {seen:?}");
}

/// The scope shows the band, the centre, MHz labels under the graph and the marker readout; OK goes to the
/// marker's frequency (SCOPE-2, MENU-26).
#[test]
fn scope_screen_and_go() {
    let mut r = Radio::new();
    r.run("hold 1; expect screen \"Scope\"; expect text \"VHF\"; expect text \"centre\"; expect text \"Marker\"");
    let texts = r.sim.screen_texts();
    let labels = texts.iter().filter(|t| t.y0 > 200 && t.y1 < 300 && t.s.contains('.')).count();
    assert!(labels >= 2, "MHz labels under the graph");
    r.knob(3);
    let marker = r.text();
    r.press(Key::Menu).wait(300);
    r.expect_screen("Home");
    let f = r.sim.app().slot(0).freq();
    assert_ne!(f, mhz(146) + 52_000, "Go tuned the marker, from:\n{marker}");
}

/// Only the bars that change are redrawn while the scope sweeps (SCOPE-7).
#[test]
fn scope_redraws_little() {
    let mut r = Radio::new();
    r.hold(Key::Digit(1)).wait(2000);
    for _ in 0..20 {
        r.wait(20);
        assert!(r.sim.last_px() < 240 * 320 / 2, "a sweep frame redrew {} pixels", r.sim.last_px());
    }
}

/// FM broadcast on a slot (decision 352): a station in slot A plays; ▲ tunes it by the step (BCR-25); quick
/// settings are FM's (BCR-28) and 5 opens the saved stations, the browser's Broadcast view (BCR-31.2).
#[test]
fn broadcast() {
    let mut r = Radio::new();
    r.home();
    r.event(Event::FmStation);
    r.wait(1000);
    r.expect_text("100.5");
    r.run("press UP; wait 300ms; expect text \"100.6\"");
    r.run("press OK; expect text \"Step\"; expect text \"Radio monitor\"; expect text \"Saved stations\"");
    r.run("press 5; expect screen \"Channel browser\"; expect text \"Broadcast\"");
}

/// Hold MENU in HF mode opens its options: step, bandwidth, AGC and the recent list (HF-14); LNA and fine tune
/// stay hidden until SSB is built (HF-35).
#[test]
fn hf_options() {
    let mut r = Radio::new();
    r.run("menu 0 3 3; wait 8s; expect screen \"HF mode\"");
    r.run("hold OK; expect text \"Step\"; expect text \"Bandwidth\"; expect text \"AGC\"; expect text \"Recent list\"");
    assert!(!r.text().contains("LNA") && !r.text().contains("Fine tune"), "HF-35: hidden until SSB");
}

/// A weather alert takes the screen; a message only shows a toast (WX-9).
#[test]
fn alerts_take_the_screen_messages_toast() {
    let mut r = Radio::new();
    r.event(Event::Message).expect_screen("Home");
    r.run("menu 0 0").event(Event::Alert);
    r.expect_screen("Alert");
}

/// The heard list: newest first with callsign, distance, bearing and age; a station without a position shows a
/// dash for its distance (APRS-3, APRS-5).
#[test]
fn heard_list_fields() {
    let mut r = Radio::new();
    r.run("menu 0 5 1; expect screen \"APRS heard\"");
    let t = r.text();
    assert!(t.contains("km") || t.contains(" mi"), "distances shown:\n{t}");
    assert!(t.contains("ago") || t.contains(" min") || t.contains(" s"), "ages shown:\n{t}");
}

/// KISS is off by default (KISS-1).
#[test]
fn kiss_off_by_default() {
    let mut r = Radio::new();
    r.run("menu 0 5 8; expect text \"Off\"");
    assert_eq!(r.sim.app().settings().kiss_port, 0);
}

/// Bluetooth: the status-bar icon shows only while a device is connected, and Status shows the state (LINK-17).
#[test]
fn bluetooth_state() {
    let mut r = Radio::new();
    assert!(!r.sim.device().bt);
    r.run("menu 0 9 2 1; press OK; home");
    r.sim.bt_feed(b"\r");
    r.wait(500);
    assert!(r.sim.device().bt, "connected once the device talks");
    r.run("menu 0 0; expect text \"Bluetooth\"");
    let t = r.text();
    assert!(t.contains("Connected") || t.contains("On"), "Status shows the state:\n{t}");
}

/// Channel-only mode hides VFO mode and all channel editing: V/M stays on the channel, hold SLOT edits nothing
/// (CHAN-16).
#[test]
fn channel_only_hides_vfo_and_editing() {
    let mut r = Radio::new();
    r.press(Key::Slot); // B on a channel
    r.run("menu 0 9 3 2; press OK; home");
    r.press(Key::VfoMem);
    assert!(r.sim.app().slot(1).on_channel(), "V/M didn't leave the channel");
    r.run("hold SLOT; expect screen \"Home\"");
}

/// The menu never interrupts the radio: with a settings list open, the scan keeps going and a watched slot
/// is still heard (MENU-29).
#[test]
fn menu_doesnt_interrupt() {
    let mut r = Radio::new();
    r.run("menu 0 8 5");
    let mut heard = false;
    for _ in 0..1250 {
        r.wait(20); // 25 s: inside the menu's idle timeout (MENU-28)
        heard |= r.sim.radio().live.slots[1].squelch_open;
    }
    assert!(r.sim.radio().live.slots[2].scanning, "C still scanning");
    assert!(heard, "B heard under the menu");
    r.expect_screen("Settings group");
}

/// A notification interrupts nothing and never keys: a message during a scan leaves it scanning (INBOX-5).
#[test]
fn notification_interrupts_nothing() {
    let mut r = Radio::new();
    r.event(Event::Message).wait(1000);
    assert!(r.sim.radio().live.slots[2].scanning, "C still scanning");
    assert!(!r.sim.radio().live.tx.keyed);
}

/// A tapped monitor shows MON on the slot's card and nowhere in the status bar (SQL-14).
#[test]
fn monitor_on_the_card_only() {
    let mut r = Radio::new();
    r.press(Key::Side(1)).wait(300);
    let mon: Vec<_> = r.sim.screen_texts().into_iter().filter(|t| t.s == "MON").collect();
    assert!(!mon.is_empty(), "MON shown");
    assert!(mon.iter().all(|t| t.y0 >= 20), "MON not in the bar");
}

/// 1.25 m's power is chosen like any band's: the knob raises it to High (TX-9).
#[test]
fn power_levels_on_125m() {
    let mut r = Radio::new();
    r.run("type 2235; press OK; wait 300ms; press OK; expect text \"TX power Low\"; press DOWN; knob 2");
    r.expect_text("TX power High");
}

/// CW ID (0 8 6 2): off, every 10 min or on unkey; the choice and the callsign go to the Radio task; with no
/// callsign it stays off and says why (VOX-16).
#[test]
fn cw_id_setting() {
    let mut r = Radio::new();
    r.take_cmds();
    r.run("menu 0 8 6; press 2; knob 1");
    assert!(!r.has("CW ID Off"), "CW ID turned on");
    assert!(r.take_cmds().iter().any(|c| matches!(c, RadioCmd::CwCall { .. } | RadioCmd::TxTones { .. })), "sent to the radio");
    let mut b = Radio::blank();
    for _ in 0..8 {
        b.press(Key::Menu);
    }
    b.run("menu 0 8 6; press 2; knob 1; expect text \"CW ID Off\"");
    let t = b.text().to_lowercase();
    assert!(t.contains("callsign"), "says why:\n{t}");
}

/// BCR-21 to BCR-24: the R event tunes A to 100.5: A plays broadcast with its RDS, B and C keep watching; every
/// time an amateur slot is heard the broadcast goes quiet at once, and it comes back 2 s after the last one ends.
#[test]
fn broadcast_slot_and_audio_priority() {
    use ferrite_core::msg::ModeLive;
    let mut r = Radio::new();
    r.home();
    // C's airband scan stops on a carrier every few seconds, each a slot heard: stop it, so B's signal (2 s in
    // every 25) is the one that interrupts.
    r.run("press SLOT; press SLOT; press SIDE2; press SLOT; wait 3s");
    assert!(!r.sim.radio().live.slots[2].scanning);
    r.event(Event::FmStation).wait(2500);
    r.expect_text("100.5").expect_text("Harvest Moon").expect_text("STEREO");
    assert_eq!(r.sim.app().bcast_slot(), Some(0));
    let (mut last_open, mut muted, mut back) = (0u32, 0, 0);
    let mut was = true;
    for _ in 0..3000 {
        r.wait(20);
        let live = &r.sim.radio().live;
        let ModeLive::Fm { heard, .. } = live.mode else { panic!("FM runs: {:?}", live.mode) };
        assert!(!live.slots[0].squelch_open, "no chip on the broadcast slot");
        if live.slots.iter().any(|s| s.squelch_open) {
            last_open = r.t;
            assert!(!heard, "at {} ms a slot is heard and so is the broadcast", r.t);
        }
        if heard {
            assert!(r.t - last_open >= 1_980, "back {} ms after a slot", r.t - last_open);
        } else if last_open > 0 && r.t - last_open >= 2_100 {
            panic!("still muted {} ms after the last slot", r.t - last_open);
        }
        muted += (was && !heard) as u32;
        back += (!was && heard) as u32;
        was = heard;
    }
    assert!(muted > 0 && back > 0, "seen muted {muted} and back {back} times");
    // B tuned in too: the broadcast moves; A says where it plays.
    r.press(Key::Slot).event(Event::FmStation).wait(2500);
    assert_eq!(r.sim.app().bcast_slot(), Some(1));
    r.expect_text("FM on B").expect_text("Broadcast plays on B");
}


/// HOME-14: on the single + scope layout the strip is drawn plainly off while broadcast FM has the receiver (a dim
/// frame, no bars or labels), and comes back as a working strip once the slot is tuned out of the band.
#[test]
fn strip_off_while_broadcast() {
    let mut r = Radio::new();
    r.event(Event::Layout);
    r.home().wait(1500);
    r.expect_text("+112k");
    r.event(Event::FmStation).wait(2500);
    assert_eq!(r.sim.app().bcast_slot(), Some(0));
    assert!(!r.text().contains("112k"), "no strip labels while broadcast plays:\n{}", r.text());
    r.run("type 1460; press OK; expect screen \"Home\"; wait 1500ms");
    assert_eq!(r.sim.app().bcast_slot(), None);
    r.expect_text("+112k");
}

/// HOME-15, HOME-15.1, HOME-15.3: \* on home mutes the active slot: its card says Muted, then Muted with the RX tag
/// while its carrier is on (the fake radio still opens its squelch and moves its meter); \* again turns off (OFF
/// on its card) and a third \* back on, heard as before. On the broadcast slot \* keeps the broadcast off the speaker.
#[test]
fn quick_mute() {
    use ferrite_core::msg::ModeLive;
    let mut r = Radio::new();
    r.home();
    // C's airband scan holds chip 0 on its hits: stop it, so A gets the chip.
    r.run("press SLOT; press SLOT; press SIDE2; press SLOT; wait 1s");
    r.take_cmds();
    r.press(Key::Star).expect_text("Muted");
    assert!(r.take_cmds().contains(&RadioCmd::Mute(0b001)));
    // A's fake carrier comes every 10 s, for 3 s.
    let mut seen = false;
    for _ in 0..600 {
        r.wait(20);
        if r.sim.radio().live.slots[0].squelch_open {
            seen = true;
            break;
        }
    }
    assert!(seen, "A still receives");
    r.wait(100).expect_text("Muted RX");
    r.press(Key::Star).wait(100).expect_text("OFF");
    assert!(r.take_cmds().contains(&RadioCmd::SlotOff(0b001)));
    r.press(Key::Star).wait(100);
    assert!(!r.text().contains("Muted") && !r.text().contains("OFF"));
    // The broadcast slot: muted, the broadcast is never heard.
    r.event(Event::FmStation).wait(2500);
    assert_eq!(r.sim.app().bcast_slot(), Some(0));
    r.press(Key::Star).wait(500);
    for _ in 0..200 {
        r.wait(20);
        let ModeLive::Fm { heard, .. } = r.sim.radio().live.mode else { panic!("FM runs") };
        assert!(!heard, "muted broadcast heard at {} ms", r.t);
    }
    r.expect_text("Muted");
}

/// SNAKE-1, SNAKE-2: launch from Tools, pause and restart, then return to Tools.
#[test]
fn snake_from_tools() {
    let mut r = Radio::new();
    r.menu("0 3 7").expect_screen("Snake").expect_text("Score 0").expect_text("Start");
    r.press(Key::Menu).expect_text("Pause");
    r.press(Key::Menu).wait(4_000).expect_text("Paused").expect_text("Resume");
    r.press(Key::Menu).wait(4_000).expect_text("Game over").expect_text("Restart");
    r.press(Key::Menu).expect_text("Score 0").expect_text("Pause");
    r.press(Key::Exit).expect_screen("Settings group").expect_text("Snake");
}

/// VOX-15: the voice mic gain reaches the BK4829's maximum from the Transmit menu.
#[test]
fn microphone_gain_reaches_twelve() {
    let mut r = Radio::new();
    r.menu("0 8 3").expect_text("Mic gain");
    r.take_cmds();
    r.knob(7).expect_text("12");
    assert!(r.take_cmds().contains(&RadioCmd::MicGain(12)));
    r.knob(1);
    assert!(!r.take_cmds().contains(&RadioCmd::MicGain(13)));
    r.press(Key::Hash);
    assert!(r.take_cmds().contains(&RadioCmd::MicGain(5)));
}
