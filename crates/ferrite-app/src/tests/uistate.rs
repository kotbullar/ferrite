//! The UI readback (LINK-19, TERM-37): every screen's lines, the compact status, the key names, and the screen as
//! drawn (LINK-19.5).

use super::{store, T};
use crate::chan::ChannelStore;
use crate::data::Data;
use std::borrow::ToOwned;
use crate::sample::Sample;
use crate::App;
use ferrite_core::msg::LiveAll;
use crate::screens::nav::{Dest, ALL, MENU0};
use crate::term::cmd::Line;
use crate::uistate::{self, id, key_code, key_line, key_name, line, line_all, status_line};
use ferrite_core::keys::Key;
use ferrite_core::msg::{Device, GpsState};
use std::string::String;
use std::vec::Vec;

/// The readback's lines, with this device.
fn ui_dev(t: &T, dev: &Device) -> Vec<String> {
    let s = store();
    let mut l = Line::new();
    let mut v = Vec::new();
    let mut i = 0;
    while line(&t.app, &s, dev, &t.live, i, &mut l) {
        v.push(String::from_utf8_lossy(l.as_bytes()).into_owned());
        i += 1;
        assert!(i < 200, "the readback never ends");
    }
    v
}

fn ui(t: &T) -> Vec<String> {
    ui_dev(t, &Device::default())
}

/// The screen as drawn (LINK-19.5): `line_all` with `texts`.
fn ui_texts(t: &mut T, texts: bool) -> Vec<String> {
    let s = store();
    let dev = Device::default();
    let mut l = Line::new();
    let mut v = Vec::new();
    let mut i = 0;
    while line_all(&mut t.app, &s, &dev, &t.live, texts, i, &mut l) {
        v.push(String::from_utf8_lossy(l.as_bytes()).into_owned());
        i += 1;
        assert!(i < 200, "the readback never ends");
    }
    v
}

fn find<'a>(v: &'a [String], start: &str) -> &'a str {
    v.iter().find(|l| l.starts_with(start)).unwrap_or_else(|| panic!("no {start:?} line in {v:#?}"))
}

/// Quotes come in pairs once `\"` and `\\` are taken out.
fn quotes_balanced(l: &str) -> bool {
    let mut n = 0;
    let mut esc = false;
    for c in l.chars() {
        match c {
            _ if esc => esc = false,
            '\\' => esc = true,
            '"' => n += 1,
            _ => {}
        }
    }
    n % 2 == 0
}

/// LINK-19, TERM-37: every destination reads back as the screen it opened: `screen ID` first (the variant's stable
/// id), then `state` and `bar`, the screen's own lines, and `hints` last; strings quoted and escaped.
#[test]
fn every_destination_reads_back() {
    let mut t = T::new();
    for dest in ALL.into_iter().chain(MENU0.map(|(_, d)| d)) {
        t.hold(Key::Exit);
        t.app.open(dest);
        t.wait(200);
        let v = ui(&t);
        let want = id(t.app.screen());
        assert!(v[0].starts_with(&std::format!("screen {want} ")), "{dest:?}: {}", v[0]);
        assert!(v[1].starts_with("state frame="), "{dest:?}: {}", v[1]);
        assert!(v.iter().any(|l| l.starts_with("bar lock=")), "{dest:?}");
        assert!(v.last().unwrap().starts_with("hints"), "{dest:?}: {:?}", v.last());
        assert!(v.len() <= 48, "{dest:?}: {} lines (LINK-19.5: every screen fits Live View)", v.len());
        for l in &v {
            assert!(quotes_balanced(l), "{dest:?}: {l}");
        }
        // the screen as drawn: the same head, then `t` lines (home keeps its cards)
        let d = ui_texts(&mut t, true);
        assert_eq!(d[0], v[0], "{dest:?}");
        if want != "Home" {
            assert!(d.iter().any(|l| l.starts_with("t ")), "{dest:?}: nothing drawn: {d:#?}");
        }
        assert!(d.len() <= 48, "{dest:?}: {} lines as drawn", d.len());
    }
}

/// LINK-19: the state line says the exclusive mode (MODE-1) by name, and the bar the GPS state, the clock, the
/// battery and the audio counts (LINK-13.4) when there are any.
#[test]
fn state_and_bar_words() {
    let mut t = T::new();
    let dev = Device { clock_min: Some(9 * 60 + 5), gps: GpsState::Searching, battery_pct: 61, audio: [3, 1, 2, 0, 900, 4], ..Default::default() };
    let v = ui_dev(&t, &dev);
    assert!(find(&v, "state").contains(" mode=off"));
    let bar = find(&v, "bar");
    assert!(bar.contains(" gps=searching"), "{bar}");
    assert!(bar.contains(" battery=61"), "{bar}");
    assert_eq!(find(&v, "audio"), "audio made=3 dropped=1 waits=2 clips=0 peak=900 gain=4");
    let v = ui_dev(&t, &Device { gps: GpsState::Fix, ..Default::default() });
    assert!(find(&v, "bar").contains(" gps=fix"));
    assert!(!v.iter().any(|l| l.starts_with("audio")), "no audio line before any audio");
    for (dest, word) in [(Dest::BandScope, "scope"), (Dest::HfMode, "hf")] {
        t.hold(Key::Exit);
        t.open(dest);
        t.wait(300);
        let v = ui(&t);
        assert!(find(&v, "state").contains(&std::format!(" mode={word}")), "{dest:?}: {}", find(&v, "state"));
        let mut st = Line::new();
        status_line(&t.app, &t.live, &mut st);
        let st = String::from_utf8_lossy(st.as_bytes()).into_owned();
        assert!(st.contains(&std::format!(" mode={word}")), "{st}");
    }
}

/// SELF-9.3: the bar's CPU words, overall and per speed with the share of time at 24 MHz; a speed it didn't stay at
/// (None) is left out, and nothing before the first 10 s.
#[test]
fn bar_cpu_per_speed() {
    let t = T::new();
    let v = ui_dev(&t, &Device { cpu_pct: Some(31), cpu_speeds: [Some(28), Some(55), Some(90)], ..Default::default() });
    let bar = find(&v, "bar");
    assert!(bar.contains(" cpu=31 cpu24=28 cpu120=55 at24=90"), "{bar}");
    let v = ui_dev(&t, &Device { cpu_pct: Some(12), cpu_speeds: [Some(12), None, Some(100)], ..Default::default() });
    let bar = find(&v, "bar");
    assert!(bar.contains(" cpu=12 cpu24=12 at24=100") && !bar.contains("cpu120"), "{bar}");
    let v = ui_dev(&t, &Device::default());
    assert!(!find(&v, "bar").contains("cpu"));
}

/// LINK-19 (`UI` what=2, TERM-37's `ui status`): the compact status names the screen, the active slot, each slot's
/// state and meter, and the repeater off.
#[test]
fn compact_status_line() {
    let t = T::new();
    let mut l = Line::new();
    status_line(&t.app, &t.live, &mut l);
    let s = String::from_utf8_lossy(l.as_bytes()).into_owned();
    assert!(s.starts_with("st scr=Home act=A keyed=- remote=0 mode=off A="), "{s}");
    assert!(s.contains(" B=") && s.contains(" C="), "{s}");
    assert!(s.ends_with(" xb=0"), "{s}");
    assert!(s.len() <= 140, "{s}");
}

/// TERM-37: `ui keys` lists every key by name with its stock code and other names, then the knob; names work in any
/// case and as their aliases.
#[test]
fn key_names_codes_and_list() {
    assert_eq!(key_name(6), "MENU");
    assert_eq!(key_code(b"menu"), Some(6));
    assert_eq!(key_code(b"OK"), Some(6));
    assert_eq!(key_code(b"exit"), key_code(b"BACK"));
    assert_eq!(key_code(b"*"), key_code(b"STAR"));
    assert_eq!(key_code(b"#"), key_code(b"HASH"));
    assert_eq!(key_code(b"pf1"), key_code(b"SIDE1"));
    assert_eq!(key_code(b"PF2"), key_code(b"SIDE2"));
    assert_eq!(key_code(b"V/M"), key_code(b"VM"));
    assert_eq!(key_code(b"nope"), None);
    let mut l = Line::new();
    let mut v = Vec::new();
    let mut i = 0;
    while key_line(i, &mut l) {
        v.push(String::from_utf8_lossy(l.as_bytes()).into_owned());
        i += 1;
    }
    assert_eq!(v.last().unwrap(), "knob +N|-N (1-20 steps; + clockwise)");
    assert!(v.iter().any(|l| l == "MENU 6 OK"), "{v:?}");
    assert!(v.iter().any(|l| l.starts_with("0 ")), "{v:?}");
    for name in ["UP", "DOWN", "LEFT", "RIGHT", "SLOT", "VM", "STAR", "HASH", "SIDE1", "SIDE2", "BACK"] {
        let c = key_code(name.as_bytes()).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(key_name(c), name);
    }
    let _ = uistate::key_name(255);
}

/// TERM-37: a string with a quote or a backslash is escaped, so the line still parses.
#[test]
fn strings_are_escaped() {
    let mut t = T::new();
    t.app.show_toast(b"Say \"hi\"", b"a\\b", false);
    let v = ui(&t);
    let l = find(&v, "toast");
    assert_eq!(l, r#"toast "Say \"hi\"" "a\\b""#);
}

/// The readback over any store.
fn ui_store<S: ChannelStore>(app: &App, st: &S, dev: &Device, live: &LiveAll) -> Vec<String> {
    let mut l = Line::new();
    let mut v = Vec::new();
    let mut i = 0;
    while line(app, st, dev, live, i, &mut l) {
        v.push(String::from_utf8_lossy(l.as_bytes()).into_owned());
        i += 1;
    }
    v
}

#[allow(dead_code)]
fn dump(v: &[String]) {
    for l in v {
        std::println!("{l}");
    }
}

/// SETUP (LINK-19): each setup page reads back its page, its question and its rows; the last its summary.
#[test]
fn setup_pages_read_back() {
    let mut t = T::new();
    t.open(Dest::Setup);
    let mut heads = Vec::new();
    for _ in 0..8 {
        let v = ui(&t);
        heads.push(find(&v, "setup page=").to_owned());
        t.tap(Key::Menu);
    }
    assert_eq!(heads[0], r#"setup page=1 of=8 head="Welcome to Ferrite""#);
    assert_eq!(heads[1], r#"setup page=2 of=8 head="Your callsign""#);
    assert_eq!(heads[7], r#"setup page=8 of=8 head="All set""#);
    assert_eq!(id(t.app.screen()), "Home", "OK on the last page starts the radio");
    let mut t = T::new();
    t.open(Dest::Setup);
    for _ in 0..2 {
        t.tap(Key::Menu);
    }
    let v = ui(&t);
    assert!(v.iter().any(|l| l == r#"row 0 "Canada" "" sel"#), "{v:#?}");
    assert!(v.iter().any(|l| l == r#"row 0 "Extend TX" "Off""#), "{v:#?}");
    for _ in 0..5 {
        t.tap(Key::Menu);
    }
    let v = ui(&t);
    assert!(v.iter().any(|l| l == r#"row 0 "Channels" "4""#), "{v:#?}");
    assert!(find(&v, "help \"summary\"").contains("No callsign"), "{v:#?}");
}

/// HOME-17, HOME-17.1: the quick menu's rows, knob first, then the side keys.
#[test]
fn quick_menu_rows() {
    let mut t = T::new();
    t.hold(Key::Star);
    let v = ui(&t);
    let q: Vec<_> = v.iter().filter(|l| l.starts_with("qmenu ")).cloned().collect();
    assert_eq!(q[0], r#"qmenu "Knob" "Brightness" "7""#);
    assert_eq!(q[1], r#"qmenu "PTT A" "Reverse" "off""#);
    assert_eq!(q.len(), 5, "{q:?}");
}

/// LINK-19: More's recent settings line names the last setting changed, and its selection when ▼ goes past the
/// groups.
#[test]
fn more_recent_settings() {
    let mut t = T::new();
    t.open(Dest::Advanced);
    assert_eq!(find(&ui(&t), "recent"), "recent", "none yet");
    t.hold(Key::Exit);
    t.open(Dest::Quick);
    t.knob(1);
    t.hold(Key::Exit);
    t.open(Dest::Advanced);
    let r = find(&ui(&t), "recent").to_owned();
    assert!(r.starts_with("recent \""), "{r}");
    if let crate::Screen::Advanced(mut a) = t.app.screen() {
        a.row = crate::screens::advanced::RECENT;
        t.app.screen = crate::Screen::Advanced(a);
    }
    let r = find(&ui(&t), "recent").to_owned();
    assert!(r.ends_with(" sel=1"), "{r}");
}

/// LINK-19: an options list reads back its rows (`>` for one that opens a page), Full menu, and its help; asking
/// to confirm, the question.
#[test]
fn options_rows_help_and_ask() {
    use crate::screens::opts::{Of, Options};
    let mut t = T::new();
    t.app.screen = crate::Screen::Options(Options { of: Of::Log, row: 1, confirm: false });
    let v = ui(&t);
    assert_eq!(v[0].split(' ').nth(1), Some("Options"));
    assert!(v.iter().any(|l| l.starts_with("row 2 ") && l.ends_with(" sel")), "{v:#?}");
    assert!(v.iter().any(|l| l == r#"row 0 "Full menu" ">""#), "{v:#?}");
    assert!(v.iter().any(|l| l.starts_with("help ")), "{v:#?}");
    t.app.screen = crate::Screen::Options(Options { of: Of::Log, row: 1, confirm: true });
    let v = ui(&t);
    let a = find(&v, "ask ");
    assert!(a.contains("?\" text="), "{a}");
    assert!(!v.iter().any(|l| l.starts_with("help ")), "{v:#?}");
}

/// LINK-19: the placeholder for a page not built names it; the emergency and APRS frequency questions name the
/// slot; the edit-in-place and channel editor screens have their ids.
#[test]
fn coming_emergency_and_editors() {
    use crate::screens::coming::Coming;
    use crate::screens::transmit::Emergency;
    let mut t = T::new();
    t.app.screen = crate::Screen::Coming(Coming { dest: Dest::Group(7) });
    let v = ui(&t);
    assert!(v[0].starts_with("screen Coming "), "{}", v[0]);
    assert!(find(&v, "coming ").starts_with("coming \""), "{v:#?}");
    t.app.screen = crate::Screen::Emergency(Emergency { slot: 1, aprs: false });
    let v = ui(&t);
    assert_eq!(find(&v, "emergency"), "emergency slot=B");
    t.app.screen = crate::Screen::Emergency(Emergency { slot: 3, aprs: true });
    assert_eq!(find(&ui(&t), "aprs"), "aprs slot=HF");
    t.app.screen = crate::Screen::Edit(crate::screens::edit::Edit { cell: 0 });
    assert!(ui(&t)[0].starts_with("screen Edit "));
    t.app.screen = crate::Screen::ChEdit(crate::screens::chedit::ChEdit { cell: 0 });
    assert!(ui(&t)[0].starts_with("screen ChEdit "));
}

/// BCR-28, BCR-31, HF (LINK-19): a broadcast slot's quick settings read back as FM's rows, Saved stations and
/// More; HF mode's options as rows with Full menu; HF reads back the SSB patch's load while it loads.
#[test]
fn fm_and_hf_menus() {
    use ferrite_core::msg::ModeLive;
    let mut t = T::new();
    crate::screens::entry::tune_vfo(&mut t.app, 0, ferrite_core::freq::mhz(100) + 50_000);
    t.wait(300);
    t.tap(Key::Menu);
    let v = ui(&t);
    assert!(v.iter().any(|l| l.starts_with("row 1 \"Step\"") && l.ends_with(" sel")), "{v:#?}");
    assert!(v.iter().any(|l| l == r#"row 5 "Saved stations" ">""#), "{v:#?}");
    assert!(v.iter().any(|l| l == r#"row 0 "More" ">""#), "{v:#?}");
    t.hold(Key::Exit);
    t.open(Dest::HfMode);
    t.wait(300);
    let crate::Screen::Hf(h) = t.app.screen() else { panic!("not HF") };
    t.app.screen = crate::Screen::Hf(crate::screens::hf::Hf { menu: true, row: 1, ..h });
    let v = ui(&t);
    assert!(v.iter().any(|l| l.starts_with("row 2 ") && l.ends_with(" sel")), "{v:#?}");
    assert!(v.iter().any(|l| l == r#"row 0 "Full menu" ">""#), "{v:#?}");
    t.app.screen = crate::Screen::Hf(crate::screens::hf::Hf { menu: false, loaded: false, ..h });
    t.app.radio.ssb_patch = true;
    t.live.mode = ModeLive::Hf { freq: 958_000, rssi: -90, snr: 3, si4732: true, loading: 40 };
    assert_eq!(find(&ui(&t), "hf"), "hf loading=40");
}

/// SCOPE (LINK-19): the scope line counts the bars swept, the blank ones and the sweeps from the Radio task.
#[test]
fn scope_line_counts_the_sweep() {
    use ferrite_core::msg::ModeLive;
    let mut t = T::new();
    t.open(Dest::BandScope);
    t.wait(300);
    let mut bars = [0u8; ferrite_core::msg::SCOPE_BARS];
    bars[1] = 30;
    bars[2] = 12;
    t.live.mode = ModeLive::Scope { centre: t.app.radio.centre, bars, n: 4, sweeps: 2, listen: 0, open: false, si: false };
    let l = find(&ui(&t), "scope centre=").to_owned();
    assert!(l.contains(" swept=4 blank=2 sweeps=2"), "{l}");
}

/// LINK-19, TX-20: transmit refused names the slot, why, the beeps and the fix's menu path.
#[test]
fn refused_names_the_fix() {
    let mut t = T::new();
    t.app.radio.tx_built = true;
    t.press(Key::PTT, 300);
    let v = ui(&t);
    let r = find(&v, "refused");
    assert!(r.starts_with("refused slot=A why=\""), "{r}");
    assert!(r.contains(" fix=\""), "{r}");
}

/// LINK-19: the state line's `keyed` names HF when the HF transmitter is keyed, and the strip mode by name.
#[test]
fn keyed_hf_and_strip() {
    use crate::screens::activity::Mode;
    let mut t = T::new();
    t.app.radio.keyed = Some(3);
    t.app.radio.mode = Mode::Strip;
    let s = find(&ui(&t), "state").to_owned();
    assert!(s.contains(" keyed=HF") && s.contains(" mode=strip"), "{s}");
}

/// GPS (LINK-19): the position page with a fix reads back the position, its tiles and the nearest waypoint.
#[test]
fn gps_position_with_a_fix() {
    let s = Sample::new();
    let mut app = App::new(crate::tests::set(), crate::tests::alert());
    app.open(Dest::GpsPosition);
    let dev = Device { gps: GpsState::Fix, sats: 9, ..Default::default() };
    let live = LiveAll::default();
    let v = ui_store(&app, &s, &dev, &live);
    assert!(find(&v, "gps ").starts_with("gps \""), "{v:#?}");
    assert!(find(&v, "pos ").starts_with("pos \""), "{v:#?}");
    assert!(v.iter().filter(|l| l.starts_with("row 0 ")).count() >= 6, "the tiles: {v:#?}");
    let near = find(&v, "near ");
    assert!(near.contains(" deg="), "{near}");
    // no waypoints: None, and how to mark one
    while s.request(crate::data::Op::DeleteWaypoint(0)) {}
    let v = ui_store(&app, &s, &dev, &live);
    assert_eq!(find(&v, "near "), r#"near "" "None: # marks one here""#);
}

/// APRS-40 (LINK-19): compose with no recipient shows the contact book under the To field; a service address
/// names its format.
#[test]
fn compose_book_and_service() {
    use crate::data::{Draft, Op};
    let s = Sample::new();
    s.call.set(Some(ferrite_ui::text::Buf::from(&b"VA7FER"[..])));
    let mut app = App::new(crate::tests::set(), crate::tests::alert());
    app.open(Dest::Compose);
    let dev = Device::default();
    let live = LiveAll::default();
    app.tick(1000, &live, &s);
    app.render(&mut ferrite_ui::fb::FrameBuffer::new(), &live, &dev, &s); // the first draw settles the field
    let v = ui_store(&app, &s, &dev, &live);
    assert!(find(&v, "compose").starts_with("compose to=\""), "{v:#?}");
    let b = find(&v, "book");
    assert!(b.contains(" group=\"") && b.contains(" rows="), "{b}");
    let svc = crate::screens::contacts::SERVICES[0].0;
    assert!(s.request(Op::Draft(Draft { to: ferrite_ui::text::Buf::from(svc.as_bytes()), text: ferrite_ui::text::Buf::new() })));
    let v = ui_store(&app, &s, &dev, &live);
    assert!(!v.iter().any(|l| l.starts_with("book")), "a recipient: no book");
    assert!(find(&v, "service").starts_with("service format=\""), "{v:#?}");
}

/// LINK-19.4, SQL-12.3: a card reads back a scan's hits, and its signal in dBm when 0 7 3 1 says so.
#[test]
fn card_hits_and_dbm() {
    let mut t = T::new();
    t.live.slots[0].hits = 4;
    t.live.slots[0].rssi = -80;
    t.app.set.smeter_dbm = true;
    let v = ui(&t);
    let a = find(&v, "card A");
    assert!(a.contains(" hits=4"), "{a}");
    assert!(a.contains(" s=\"-80 dBm\""), "{a}");
    assert!(!find(&v, "card B").contains(" hits="), "no hits: no word");
}

/// BCR-26, BCR-28: a broadcast slot's card is `kind=broadcast` with its `bcast` report after it; its quick
/// settings are FM's own rows (not described).
#[test]
fn broadcast_card_and_report() {
    use ferrite_core::msg::ModeLive;
    let mut t = T::new();
    t.app.radio.tx_built = true;
    t.app.set_region(Some(ferrite_core::txplan::Region::Us), false);
    t.app.set_vfo(1, ferrite_core::freq::mhz(100) + 50_000);
    t.wait(100);
    let mut rt = [0u8; 64];
    rt[..9].copy_from_slice(b"Some text");
    t.live.mode = ModeLive::Fm { freq: 10_050, ps: *b"STATION ", rt, rssi: 40, snr: 20, stereo: true, pty: 0, heard: false };
    let v = ui(&t);
    assert!(find(&v, "card B").ends_with(" kind=broadcast"), "{v:#?}");
    let b = find(&v, "bcast B");
    assert!(b.ends_with(" stereo=1 dbuv=40 snr=20"), "{b}");
    assert!(!v.iter().any(|l| l.starts_with("bcast A") || l.starts_with("bcast C")));
    while t.app.active() != 1 {
        t.tap(Key::Slot);
    }
    t.open(Dest::Quick);
    let v = ui(&t);
    assert!(v[0].starts_with("screen Quick "), "{v:#?}");
    assert!(v.iter().any(|l| l.starts_with("row 1 \"Step\"")), "FM's rows (BCR-28): {v:#?}");
}

/// MENU-17 (LINK-19): a greyed setting's editor reads back why (Step on a channel: VFO only).
#[test]
fn value_editor_says_why_greyed() {
    let mut t = T::new();
    t.tap(Key::VfoMem);
    t.open(Dest::Value(crate::slot::Item::Step));
    let v = find(&ui(&t), "value ").to_owned();
    assert!(v.ends_with(" grey=\"VFO only\""), "{v}");
}
