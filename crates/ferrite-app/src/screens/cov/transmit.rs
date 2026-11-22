//! Transmitting (chapter 15): every refusal's screen, the Transmit screen's rows, the emergency prompt, DTMF
//! while keyed and the dialer's send, and the automatic transmissions' checks.

use super::*;
use crate::screens::transmit::{self as tx, Emergency, Transmit, TxRefused, HF_SLOT};
use ferrite_core::freq::mhz;
use ferrite_core::msg::TxLive;
use ferrite_core::txplan::Refusal;

const ALL: [Refusal; 18] = [
    Refusal::OutOfBand,
    Refusal::NotCalibrated,
    Refusal::RxOnlyChannel,
    Refusal::Busy,
    Refusal::Battery,
    Refusal::Locked,
    Refusal::InProgress,
    Refusal::NoCallsign,
    Refusal::RxOnlyBuild,
    Refusal::ReceiveOnlyHere,
    Refusal::Crossband,
    Refusal::RelayMismatch,
    Refusal::AmRxOnly,
    Refusal::Emergency,
    Refusal::ReleasePtt,
    Refusal::PttAtBoot,
    Refusal::Hot,
    Refusal::Broadcast,
];

fn refused(t: &mut T, why: Refusal) {
    t.app.screen = Screen::TxRefused(TxRefused { why, slot: 0 });
    t.draw();
}

/// TX-20: every refusal draws "Can't transmit" with its own reason, and the fixing path where one exists (the
/// hint bar's OK only then); each has its own few words for a toast.
#[test]
fn every_refusal_has_a_reason_and_maybe_a_fix() {
    let mut t = T::ready();
    let mut shorts = std::collections::BTreeSet::new();
    for why in ALL {
        refused(&mut t, why);
        t.shows("Can't transmit");
        assert_eq!(t.screen().mockup(), Some("TxRefused.png"));
        t.reads("title=\"Refused\"");
        let fixes = tx::fix(&t.app, why);
        if let Some((path, name, _)) = fixes {
            t.shows(path);
            t.shows(name);
            t.reads("hints \"OK\" \"Go there\" \"BACK\" \"Close\"");
        } else {
            t.reads("hints \"BACK\" \"Close\"");
        }
        assert!(shorts.insert(tx::short(why)), "{why:?}'s words are its own");
    }
    refused(&mut t, Refusal::Hot);
    t.shows("The transmitter is cooling after a long time keyed.");
    t.shows("It transmits again in a few minutes.");
    refused(&mut t, Refusal::RelayMismatch);
    t.shows("Keep RX and TX on the same side of 64 MHz.");
    t.dev.selftest.mcu_ok = true;
    refused(&mut t, Refusal::Locked);
    t.shows("Which check failed:");
}

/// CAL-20.1 (issue #40): the lock names the check that failed, as About does: the MCU, a blank or invalid
/// calibration, or one changed since Ferrite first read it (with how to use it as it is).
#[test]
fn the_lock_names_its_check() {
    use ferrite_core::msg::CalState;
    let mut t = T::ready();
    t.dev.selftest.mcu_ok = true;
    for (cal, text) in [
        (CalState::Blank, "the calibration is blank."),
        (CalState::Invalid, "a calibration value failed its check (Passed)."),
        (CalState::CrcMismatch, "the calibration changed since Ferrite first read it."),
    ] {
        t.dev.selftest.cal = cal;
        t.app.screen = Screen::Home;
        t.draw();
        refused(&mut t, Refusal::Locked);
        t.shows(text);
    }
    t.shows("OK on About uses it as it is, if it checks out:");
    // CAL-20.4: which value, and the store not ready
    (t.dev.selftest.cal, t.dev.selftest.cal_fault, t.dev.selftest.cal_at) = (CalState::Invalid, ferrite_core::msg::CalFault::Windows, [0; 3]);
    t.app.screen = Screen::Home;
    t.draw();
    refused(&mut t, Refusal::Locked);
    t.shows("(TX windows).");
    // CAL-6.1: a band's curves: the sector valid, that band locked, the rest transmits
    (t.dev.selftest.cal, t.dev.selftest.cal_fault, t.dev.selftest.cal_at) = (CalState::Ok, ferrite_core::msg::CalFault::LevelOrder, [3, 1, 0]);
    t.app.screen = Screen::Home;
    t.draw();
    refused(&mut t, Refusal::Locked);
    t.shows("locked on this band");
    t.shows("The other bands transmit.");
    (t.dev.selftest.cal, t.dev.selftest.cal_fault) = (CalState::CrcMismatch, ferrite_core::msg::CalFault::Unchecked);
    t.app.screen = Screen::Home;
    t.draw();
    refused(&mut t, Refusal::Locked);
    t.shows("couldn't be checked");
    t.dev.selftest.mcu_ok = false;
    t.app.screen = Screen::Home;
    t.draw();
    refused(&mut t, Refusal::Locked);
    t.shows("the MCU isn't the one this radio has.");
}

/// TX-20.1: out of band, the reason names the nearest band with its edges (as short as they go: "24.89"), and
/// the fix is the region while none is set, Extend TX once one is.
#[test]
fn out_of_band_names_the_band_and_its_edges() {
    let mut t = T::ready();
    t.tune(mhz(25) + 50_000);
    refused(&mut t, Refusal::OutOfBand);
    t.shows("25.500 MHz is outside the");
    t.shows("24.89-24.99");
    t.shows("To allow any band the radio is calibrated for:");
    t.shows("MENU 0 8 8");
    t.app.set_region(None, false);
    refused(&mut t, Refusal::Battery); // another first, so the screen draws again
    refused(&mut t, Refusal::OutOfBand);
    t.shows("Region isn't set. To set it:");
    t.shows("MENU 0 9 4");
    t.tap(Key::Menu);
    assert_eq!(t.name(), Dest::Group(9).name(), "OK goes to the region's group");
}

/// TX-20: OK on a refusal without a fix does nothing; on no callsign it opens the callsign page alone.
#[test]
fn refused_ok_goes_to_the_fix() {
    let mut t = T::ready();
    t.open(Dest::TxRefused);
    t.app.screen = Screen::TxRefused(TxRefused { why: Refusal::Hot, slot: 0 });
    t.tap(Key::Menu);
    assert!(matches!(t.screen(), Screen::TxRefused(_)), "no fix: OK does nothing");
    t.tap(Key::Digit(4));
    assert!(matches!(t.screen(), Screen::TxRefused(_)));
    t.app.screen = Screen::TxRefused(TxRefused { why: Refusal::NoCallsign, slot: 0 });
    t.tap(Key::Menu);
    assert_eq!(t.screen(), Screen::Setup(crate::screens::setup::Setup::single(1)));
    t.tap(Key::Exit);
    assert_eq!(t.screen(), Screen::Home);
}

/// HF-23: PTT again on a refusal from HF mode tries HF mode's transmit again (refused on the SI4732's side).
#[test]
fn refused_hf_ptt_tries_hf_again() {
    let mut t = T::ready();
    t.app.set.hf.freq = mhz(7) + 74_000;
    t.open(Dest::HfMode);
    tx::refuse(&mut t.app, HF_SLOT, Refusal::ReceiveOnlyHere);
    t.draw();
    t.shows("Receive only here");
    let beeps = t.app.refusal_beeps;
    t.ptt(true);
    assert!(matches!(t.screen(), Screen::TxRefused(TxRefused { why: Refusal::ReceiveOnlyHere, slot: HF_SLOT })));
    assert_eq!(t.app.refusal_beeps, beeps.wrapping_add(1), "TX-20.5: beeps again");
}

/// HF-23: HF mode's transmit card says "HF mode" and its mode, above A, B and C.
#[test]
fn hf_transmit_card() {
    let mut t = T::ready();
    t.app.set.hf.freq = mhz(29) + 60_000;
    t.app.set.hf.mode = crate::screens::hf::FM;
    t.app.set_region(Some(ferrite_core::txplan::Region::Us), true);
    t.open(Dest::HfMode);
    crate::screens::hf::transmit(&mut t.app);
    t.draw();
    assert!(matches!(t.screen(), Screen::Transmit(Transmit { slot: HF_SLOT, .. })), "{:?}", t.screen());
    t.shows("HF mode");
    t.shows("TRANSMIT");
    assert_eq!(tx::tx_freq(&t.app, HF_SLOT), mhz(29) + 60_000);
}

/// TX-22.2, TX-48: the Transmit screen: the channel's number and name, Dry TX's note and the battery's volts
/// only, the low battery in amber; the other slots' rows with their channel or band, watched ones paused.
#[test]
fn transmit_screen_rows() {
    let mut t = T::ready();
    t.app.set_dry_tx(true);
    t.dev.battery_mv = 7400;
    t.dev.battery_low = true;
    // A on the sample's first channel (MENU 0 2 1 browses them; here: straight onto it).
    let ch = crate::chan::ChannelStore::zone_at(&t.s, 0, 0).expect("a sample channel");
    t.app.slots[1].ch = Some(ch);
    t.app.slots[1].vfo_mode = false;
    t.ptt(true);
    assert_eq!(t.screen().mockup(), Some("Transmit.png"));
    t.reads("title=\"TX\"");
    t.shows("Dry TX: no RF");
    t.shows("7.4 V");
    t.hides("Battery under load");
    assert!(t.px(0, 320, ferrite_ui::gfx::AMBER) > 0, "low battery and Dry TX in amber");
    t.shows(core::str::from_utf8(ch.name()).unwrap());
    t.reads("hints \"PTT\" \"Release to stop\"");
    t.ptt(false);
    t.app.set_dry_tx(false);
    t.ptt(true);
    assert!(matches!(t.screen(), Screen::Transmit(_)), "{:?}", t.screen());
    t.shows("Battery under load 7.4 V");
    t.ptt(false);
}

/// TX-22.2: keyed on a channel, the card's head is the channel's short number and name.
#[test]
fn transmit_on_a_channel_names_it() {
    let mut t = T::ready();
    let ch = crate::chan::ChannelStore::zone_at(&t.s, 0, 0).expect("a sample channel");
    let a = t.app.active as usize;
    t.app.slots[a].ch = Some(ch);
    t.app.slots[a].vfo_mode = false;
    t.ptt(true);
    assert!(matches!(t.screen(), Screen::Transmit(_)), "{:?}", t.screen());
    t.shows(core::str::from_utf8(ch.name()).unwrap());
    t.ptt(false);
}

/// LINK-20: keyed over the link, the card says Remote and the hint bar says a press on the radio stops it.
#[test]
fn remote_key_up_shows_remote() {
    let mut t = T::ready();
    let s = Sample::new();
    let r = t.app.remote_key(0, 0, t.t, &s);
    assert!(r.is_ok(), "{r:?}");
    t.wait(100);
    assert!(matches!(t.screen(), Screen::Transmit(_)), "{:?}", t.screen());
    t.shows("Remote");
    t.reads("hints \"PTT\" \"Stop remote\"");
}

fn code(k: Key) -> u8 {
    (0..ferrite_core::keys::CODES as u8).find(|&c| ferrite_core::keys::key(c) == Some(k)).expect("a matrix code")
}

/// DTMF-7, DTMF-11, TONE-13: while keyed the digits, \* and # send their DTMF, MENU, \u{25b2}, \u{25bc} and BACK
/// send A-D, and SIDE2 the tone burst, each for as long as it is held; nothing when not keyed.
#[test]
fn keys_while_keyed_send_dtmf() {
    let mut t = T::ready();
    t.ptt(true);
    t.cmds();
    for (k, c) in [(Key::Digit(5), b'5'), (Key::Star, b'*'), (Key::Hash, b'#'), (Key::Menu, b'A'), (Key::Up, b'B'), (Key::Down, b'C'), (Key::Exit, b'D')] {
        t.app.raw_key(code(k), true);
        t.app.raw_key(code(k), false);
        let cmds = t.cmds();
        assert_eq!(cmds, [RadioCmd::Dtmf { key: c, on: true }, RadioCmd::Dtmf { key: c, on: false }], "{k:?}");
    }
    t.app.raw_key(code(Key::Side(2)), true);
    assert_eq!(t.cmds(), [RadioCmd::ToneBurst { on: true }]);
    t.app.raw_key(code(Key::Side(2)), false);
    assert_eq!(t.cmds(), [RadioCmd::ToneBurst { on: false }]);
    t.app.raw_key(code(Key::Side(1)), true); // other keys: nothing
    assert!(t.cmds().is_empty());
    // Unkeyed (the Transmit screen still up): nothing.
    t.app.radio.keyed = None;
    t.app.raw_key(code(Key::Digit(5)), true);
    assert!(t.cmds().is_empty());
}

/// DTMF-1: the dialer's Send keys and sends the sequence in chunks of five, then unkeys when the Radio task says
/// it is out; refused like PTT (the refused screen), and never on an emergency frequency.
#[test]
fn dial_sends_and_unkeys() {
    let mut t = T::ready();
    assert!(tx::dial(&mut t.app, 0, b"123456789012345678"));
    let cmds = t.cmds();
    let chunks: Vec<_> = cmds.iter().filter_map(|c| if let RadioCmd::DtmfSeq { chunk, keys } = c { Some((*chunk, *keys)) } else { None }).collect();
    assert_eq!(chunks, [(0, *b"12345"), (1, *b"67890"), (2, *b"12345"), (3, [b'6', 0, 0, 0, 0])], "16 keys at most");
    assert!(cmds.contains(&RadioCmd::DtmfSend { len: 16 }));
    assert!(matches!(t.screen(), Screen::Transmit(Transmit { dial: true, .. })), "{:?}", t.screen());
    t.cmds();
    t.live.tx = TxLive { keyed: true, slot: 0, dtmf_sent: t.app.radio.dtmf_seen.wrapping_add(1), ..TxLive::default() };
    t.wait(100);
    assert!(!matches!(t.screen(), Screen::Transmit(_)), "out: unkeyed and back");
    assert!(t.cmds().contains(&RadioCmd::Transmit { slot: 0, on: false }));
    t.live.tx = TxLive::default();
    // Refused: the refused screen.
    t.dev.battery_cutoff = true;
    t.draw();
    assert!(!tx::dial(&mut t.app, 0, b"1"));
    assert!(matches!(t.screen(), Screen::TxRefused(TxRefused { why: Refusal::Battery, .. })));
    t.dev.battery_cutoff = false;
    t.tap(Key::Exit);
    // An emergency frequency: a toast, nothing keyed.
    t.app.set_region(Some(ferrite_core::txplan::Region::Us), true);
    t.tune(mhz(243));
    let a = t.app.active;
    assert!(!tx::dial(&mut t.app, a, b"1"));
    assert_eq!(t.toast(), "No DTMF / emergency frequency");
    assert!(t.app.radio.keyed.is_none());
}

/// TX-3: an emergency frequency's first PTT asks, with a countdown; BACK cancels, and 5 s without a second PTT
/// goes home. VOX never answers it (TX-35.4).
#[test]
fn emergency_prompt_counts_down() {
    let mut t = T::ready();
    t.app.set_region(Some(ferrite_core::txplan::Region::Us), true);
    t.tune(mhz(243));
    t.ptt(true);
    t.ptt(false);
    assert!(matches!(t.screen(), Screen::Emergency(Emergency { aprs: false, .. })), "{:?}", t.screen());
    assert_eq!(t.name(), "Emergency frequency");
    t.reads("title=\"Distress\"");
    t.shows("243.000 MHz is kept for distress and rescue.");
    t.shows("PTT again to transmit");
    t.shows("5 s");
    t.reads("hints \"PTT\" \"Transmit\" \"BACK\" \"Cancel\"");
    t.wait(2000);
    t.shows("3 s");
    t.tap(Key::Digit(1)); // other keys: nothing
    assert!(matches!(t.screen(), Screen::Emergency(_)));
    t.tap(Key::Exit);
    assert_eq!(t.screen(), Screen::Home);
    t.ptt(true);
    t.ptt(false);
    t.app.vox_on = true;
    t.ptt(true);
    assert!(matches!(t.screen(), Screen::Emergency(_)), "VOX doesn't confirm");
    t.app.vox_on = false;
    t.ptt(false);
    t.wait(5200);
    assert_eq!(t.screen(), Screen::Home, "timed out");
}

/// APRS-21.3, TX-35.7: the APRS frequency's prompt says data only.
#[test]
fn aprs_frequency_prompt() {
    let mut t = T::ready();
    t.app.screen = Screen::Emergency(Emergency { slot: 0, aprs: true });
    t.app.radio.t0 = t.app.now.wrapping_add(tx::EMERGENCY_MS);
    t.draw();
    assert_eq!(t.name(), "APRS frequency");
    t.reads("title=\"APRS\"");
    t.shows("APRS frequency: data only");
    t.shows("carries APRS packets; voice blocks them.");
}

/// TX-35: automatic transmissions are refused with a toast: an emergency frequency, no callsign, a callsign over
/// 6 (KISS excepted), the repeater running; a frame that couldn't be built says why too.
#[test]
fn automatic_transmissions_checks() {
    let mut t = T::ready();
    let a = t.app.active;
    assert!(!tx::auto_tx(&mut t.app, a, b"Beacon"));
    assert_eq!(t.toast(), "Beacon not sent: / no callsign set");
    t.app.set.set_callsign(b"VA7FERX-9");
    assert!(!tx::auto_tx(&mut t.app, a, b"Beacon"));
    assert_eq!(t.toast(), "Beacon not sent: / callsign over 6");
    assert!(tx::auto_tx(&mut t.app, a, b"KISS"), "KISS frames carry their own addresses");
    t.app.set.set_callsign(b"VA7FER-9");
    assert!(tx::auto_tx(&mut t.app, a, b"Beacon"), "the SSID isn't counted");
    t.app.radio.mode = crate::screens::activity::Mode::Xb;
    assert_eq!(tx::refusal(&t.app, a), Some(Refusal::Crossband));
    assert_eq!(tx::ptt_refusal(&t.app, a), None, "XB-14: a person's PTT isn't refused");
    assert!(!tx::auto_tx(&mut t.app, a, b"Beacon"));
    assert_eq!(t.toast(), "Beacon not sent: / repeater running");
    t.app.radio.mode = crate::screens::activity::Mode::Off;
    t.app.radio.keyed = Some(1);
    assert_eq!(tx::ptt_refusal(&t.app, a), Some(Refusal::InProgress));
    t.app.radio.keyed = None;
    tx::not_sent(&mut t.app, b"Beacon", b"no position");
    assert_eq!(t.toast(), "Beacon not sent: / no position");
    t.cmds();
    tx::data_unkey(&mut t.app, 2);
    assert_eq!(t.cmds(), [RadioCmd::TransmitData { slot: 2, power: 0, on: false }]);
    // APRS-10.1: a data frame on C while nothing holds C is dropped quietly.
    assert!(!tx::data_key(&mut t.app, crate::aprs::APRS_SLOT, 0, b"Beacon"));
    assert!(t.cmds().is_empty());
}


/// TX-12.3: the live input and gain setting each redraw their own row.
#[test]
fn mic_meter_shows_voice_and_gain_separately() {
    use ferrite_ui::{gfx::{GREEN, LINE, VIOLET}, theme::map};

    let mut t = T::ready();
    t.app.screen = Screen::Transmit(Transmit { slot: 0, dial: false });
    t.live.tx = TxLive { keyed: true, slot: 0, mic: 50, ..TxLive::default() };
    t.app.set.mic_gain = 5;
    t.draw();
    let (y, _) = tx::place(0, 0);
    let first = 59;
    let sixth = first + 5 * 9;
    assert_eq!(t.fb.at(first, y + 115), map(GREEN));
    assert_eq!(t.fb.at(first, y + 123), map(VIOLET));
    assert_eq!(t.fb.at(sixth, y + 123), map(LINE));

    t.app.set.mic_gain = 12;
    t.draw();
    assert_eq!(t.fb.at(first, y + 115), map(GREEN));
    assert_eq!(t.fb.at(sixth, y + 123), map(VIOLET));

    t.live.tx.mic = 0;
    t.draw();
    assert_eq!(t.fb.at(first, y + 115), map(LINE));
    assert_eq!(t.fb.at(sixth, y + 123), map(VIOLET));
}
