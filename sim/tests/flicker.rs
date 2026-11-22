//! DISP-25: no blank-then-draw on live updates. Every screen of the jump list and home in its three layouts,
//! driven through the updates a person sees (time passing with the fake radio's traffic, the clock, the status
//! bar's events, keys, the knob), with the frame buffer watching each render pass: a pixel that shows a value
//! that is neither its old one nor its new one flickers on the radio's slow bus.
mod common;
use common::Radio;
use ferrite_core::keys::Key;
use ferrite_sim::simcore::{jump_names, Event};
use std::collections::BTreeMap;

#[derive(Default, Debug)]
struct Tally {
    passes: u32,
    flicker: u64,
    bbox: Option<(i32, i32, i32, i32)>,
    writes: u64,
    changed: u64,
}

fn tally(r: &mut Radio, into: &mut BTreeMap<String, Tally>) {
    for h in r.sim.passes.drain(..) {
        if !h.live {
            continue; // a change of screen redraws everything: not a live update
        }
        let t = into.entry(h.screen.to_string()).or_default();
        t.passes += 1;
        t.writes += h.report.writes;
        t.changed += h.report.changed as u64;
        if h.report.flicker > 0 {
            t.flicker += h.report.flicker as u64;
            let b = h.report.bbox.unwrap();
            t.bbox = Some(match t.bbox {
                None => b,
                Some(a) => (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)),
            });
        }
    }
}

/// What a person does on any screen: let it run, step the knob and arrows, and the status bar's events.
fn exercise(r: &mut Radio) {
    r.wait(2500);
    for e in [Event::Battery, Event::Gps, Event::Bluetooth] {
        r.event(e);
        r.wait(300);
    }
    r.knob(1).wait(200).knob(-1).wait(200);
    r.press(Key::Down).wait(200).press(Key::Up).wait(200);
    r.wait(61_000); // a clock minute and a backlight timeout's worth of live traffic
}

fn survey() -> BTreeMap<String, Tally> {
    let mut all = BTreeMap::new();
    let names: Vec<String> = jump_names().collect();
    for i in 0..names.len() {
        let mut r = radio();
        r.sim.jump(i);
        r.wait(1000);
        r.sim.watch_passes(true);
        exercise(&mut r);
        tally(&mut r, &mut all);
    }
    for layout in 0..3 {
        let mut r = radio();
        for _ in 0..layout {
            r.event(Event::Layout);
        }
        r.home();
        r.wait(1000);
        r.sim.watch_passes(true);
        exercise(&mut r);
        for k in [Key::Slot, Key::Slot, Key::VfoMem, Key::VfoMem, Key::Side(2), Key::Side(2)] {
            r.press(k).wait(800);
        }
        r.event(Event::Message);
        r.wait(4000);
        r.event(Event::ToneHint);
        r.wait(2000);
        r.event(Event::ToneHint);
        r.wait(2000);
        tally(&mut r, &mut all);
    }
    all
}

/// The survey's table, for review: `cargo test -p ferrite-sim --test flicker -- --ignored --nocapture`.
#[test]
#[ignore]
fn flicker_survey() {
    println!("{:28} {:>6} {:>9} {:>10} {:>9}  bbox", "screen", "passes", "flicker", "writes", "changed");
    for (s, t) in survey() {
        println!("{:28} {:>6} {:>9} {:>10} {:>9}  {:?}", s, t.passes, t.flicker, t.writes, t.changed, t.bbox);
    }
}

/// Where a screen flickers: 8 x 8 cells with flickering pixels, hottest first (for finding the drawing code).
fn hot_cells(heat: &[u32]) -> Vec<(u32, i32, i32)> {
    let mut cells = BTreeMap::new();
    for (i, &h) in heat.iter().enumerate() {
        if h > 0 {
            let (x, y) = (i as i32 % 240, i as i32 / 240);
            *cells.entry((x / 8 * 8, y / 8 * 8)).or_insert(0u32) += h;
        }
    }
    let mut v: Vec<_> = cells.into_iter().map(|((x, y), h)| (h, x, y)).collect();
    v.sort_by_key(|c| std::cmp::Reverse(c.0));
    v
}

#[test]
#[ignore]
fn flicker_where() {
    let pick = std::env::var("SCREEN").unwrap_or_default();
    let names: Vec<String> = jump_names().collect();
    let mut runs: Vec<(String, Radio)> = Vec::new();
    for layout in 0..3 {
        let mut r = radio();
        for _ in 0..layout {
            r.event(Event::Layout);
        }
        r.home();
        runs.push((format!("Home layout {layout}"), r));
    }
    for (i, n) in names.iter().enumerate() {
        if !pick.is_empty() && !n.to_lowercase().contains(&pick.to_lowercase()) {
            continue;
        }
        let mut r = radio();
        r.sim.jump(i);
        runs.push((n.clone(), r));
    }
    for (n, mut r) in runs {
        if !pick.is_empty() && !n.to_lowercase().contains(&pick.to_lowercase()) {
            continue;
        }
        r.wait(1000);
        r.sim.watch_passes(true);
        exercise(&mut r);
        if n.starts_with("Home") {
            for k in [Key::Slot, Key::Slot, Key::VfoMem, Key::VfoMem] {
                r.press(k).wait(800);
            }
        }
        let cells = hot_cells(&r.sim.flicker_heat());
        println!("{n}: {:?}", &cells[..cells.len().min(14)]);
        for h in r.sim.passes.iter().filter(|h| h.report.flicker > 0) {
            println!("  {:>6} ms {:24} live {:5} flicker {:>6} {:?}", h.at, h.screen, h.live, h.report.flicker, h.report.bbox);
        }
    }
}

/// Each jump screen, then home in its three layouts: a radio showing it, named as the tables below name it.
/// A radio with Low Power Mode off: the battery event in [`short`] drops to 15 %, which with Auto (the default,
/// PWR-24.1) enters the mode, a whole other screen; this test measures each screen's own redraws.
fn radio() -> Radio {
    let mut r = Radio::new();
    r.sim.app_mut().settings_mut().lpm = 0;
    r.wait(3000); // the changed setting is saved 2 s later (its redraw isn't a screen's live update)
    r
}

fn runs() -> impl Iterator<Item = (String, Radio)> {
    let mut names: Vec<String> = jump_names().collect();
    // a screen reached two ways (two settings groups) is named twice: the second "#2"
    for i in 0..names.len() {
        let k = names[..i].iter().filter(|n| **n == names[i]).count();
        if k > 0 {
            names[i] = format!("{} #{}", names[i], k + 1);
        }
    }
    let screens = names.into_iter().enumerate().map(|(i, n)| {
        let mut r = radio();
        r.sim.jump(i);
        (n, r)
    });
    let homes = (0..3).map(|layout| {
        let mut r = radio();
        for _ in 0..layout {
            r.event(Event::Layout);
        }
        r.home();
        (format!("Home layout {layout}"), r)
    });
    // BCR-26: home with slot A playing broadcast FM (RDS, signal, stereo, the slot muted and back).
    let bcast = core::iter::once_with(|| {
        let mut r = radio();
        r.home();
        r.event(Event::FmStation);
        ("Home broadcast".to_string(), r)
    });
    screens.chain(homes).chain(bcast)
}

/// The regression run, shorter than [`exercise`]: a few seconds of traffic, the status bar's events, the knob
/// and the arrows; on home also SLOT, V/M, a message and the tone hint.
fn short(name: &str, r: &mut Radio) {
    r.wait(2500);
    for e in [Event::Battery, Event::Gps, Event::Bluetooth] {
        r.event(e);
        r.wait(300);
    }
    r.knob(1).wait(200).knob(-1).wait(200);
    r.press(Key::Down).wait(200).press(Key::Up).wait(200);
    r.wait(2000);
    if name.starts_with("Home") {
        for k in [Key::Slot, Key::Slot, Key::VfoMem, Key::VfoMem] {
            r.press(k).wait(800);
        }
        r.event(Event::Message);
        r.wait(3000);
        r.event(Event::ToneHint);
        r.wait(1500);
    }
}

/// Pixels written by live updates in [`short`], per run: what a screen may cost the radio's bus (DISP-25). Measured
/// Oct 12, 2026 with a quarter's headroom; a run over its budget repaints more than it did. Re-measure with
/// `cargo test -p ferrite-sim --release --test flicker overdraw_table -- --ignored --nocapture`.
const BUDGET: &[(&str, u64)] = &[
    ("Home", 797000),
    ("A1 Direct entry", 10000),
    ("A4 Channel editor", 107000),
    ("A5 Value editor", 146000),
    ("A6 Tone picker", 144000),
    ("A7 Text entry", 10000),
    ("A8 Position entry", 49000),
    ("A9 Save as channel", 107000),
    ("A10 Add repeater", 117000),
    ("A11 Range preset", 58000),
    ("A12 Zone editor", 58000),
    ("A13 Location codes", 37000),
    ("A14 DTMF dialer", 10000),
    ("A15 DTMF memories", 37000),
    // The address book on the empty recipient redraws when a message arrives (its Recent group; decision 226).
    ("A16 Compose message", 146000),
    ("A16a Quick messages", 90000),
    // The test's knob turns auto reply on and off: the header, the row and the help line twice (APRS-57).
    ("A16b Auto reply", 105000),
    ("A17 PIN entry", 10000),
    ("A18 Confirm", 10000),
    ("A19 Extend TX warning", 10000),
    ("A20 Setup", 0),
    ("B4 Shortcut map", 96000),
    ("B6 Transmit", 14000),
    ("B7 Transmit refused", 10000),
    ("B8 Emergency frequency", 430000),
    ("B9 Scan", 22000),
    ("B10 Band scope", 13209000),
    ("B11 FM broadcast", 117000),
    ("B12 HF mode", 603000),
    ("B14 Tone search", 65000),
    ("B22 Satellites", 60000),
    ("B15 Frequency counter", 142000),
    ("B17 Alert", 2541000),
    ("B18 Crossband repeater", 135000),
    ("B21 Squelch", 430000),
    ("C1 Event log", 90000),
    ("C2 Event detail", 333000),
    ("C3 APRS heard", 90000),
    ("C3a APRS sent", 75000),
    ("C4 APRS station", 293000),
    ("C5 APRS messages", 87000),
    ("C5a Contacts", 9300),
    ("C6 APRS message", 306000),
    ("C8 Channel browser", 68000),
    // CHAN-27 (decision 155): each move redraws two lines and the preview (its header, four rows and footer, about
    // 27,000 px), four moves in the run
    ("C11 Zones", 195000),
    ("C12 Range presets", 87000),
    ("C13 Skip list", 87000),
    ("C14 Waypoints", 141000),
    ("C15 Go to waypoint", 134000),
    ("C16 GPS position", 93000),
    ("C18 HF recent", 65000),
    ("D1 Quick", 75000),
    ("D2 More", 110000),
    ("D3 Settings group", 64000),
    ("D3 Settings list", 83000),
    ("D6 Status", 727000),
    ("D7 About", 16000),
    ("D8 Calibration", 10000),
    ("D9 Key test", 214000),
    ("D10 Advanced squelch", 10000),
    ("D15 SSB patch confirm", 10000),
    ("D16 SSB patch", 10000),
    ("D17 Link status", 22000),
    ("D21 Writing", 10000),
    ("D22 Power off", 123000),
    ("D23 Empty battery", 67000),
    ("D24 Mailbox", 10000),
    ("D25 Finish the update", 10000),
    ("D26 Snake", 9300),
    ("D27 SOTA / POTA", 9300),
    ("D3 Settings group #2", 88000),
    ("D3 Settings list #2", 77000), // Nov 10, 2026: the menus' rows 25 px from 22 (14 px text), each row 14 % more
    ("A5 Value editor #2", 70000),
    ("Home layout 0", 1061000),
    ("Home layout 1", 10413000),
    ("Home layout 2", 1363000),
    ("Home broadcast", 1010000), // Oct 21, 2026: the test radio waits 3 s more (radio()), which moves the RDS text by 0.1 %
];

/// DISP-25: no live update on any screen shows a pixel that is neither its old value nor its new one, and none
/// writes more than its budget.
#[test]
fn no_flicker_and_overdraw_in_budget() {
    let mut bad = Vec::new();
    for (name, mut r) in runs() {
        r.wait(1000);
        r.sim.watch_passes(true);
        short(&name, &mut r);
        let mut writes = 0;
        for h in r.sim.passes.iter().filter(|h| h.live) {
            writes += h.report.writes;
            if h.report.flicker > 0 {
                bad.push(format!("{name}: {} at {} ms: {} px flicker in {:?}", h.screen, h.at, h.report.flicker, h.report.bbox));
            }
        }
        match BUDGET.iter().find(|(n, _)| *n == name) {
            Some(&(_, b)) if writes > b => bad.push(format!("{name}: {writes} px written by live updates, budget {b}")),
            Some(_) => {}
            None => bad.push(format!("{name}: no overdraw budget (add it to BUDGET: {writes} px now)")),
        }
    }
    assert!(bad.is_empty(), "DISP-25 (find a region with flicker_where):\n{}", bad.join("\n"));
}

/// DISP-25, HOME-17: the quick menu over home in each layout while the fake radio's traffic runs: the box comes up,
/// its rows change (every key and the knob), it closes (BACK, and the 5 s idle close), with no blank-then-draw
/// anywhere on the screen. Night red's pass redraws everything, as a theme change does, so it isn't live.
#[test]
fn quick_menu_has_no_flicker() {
    use ferrite_core::keys::{PTT_A, PTT_B};
    let mut bad = Vec::new();
    for layout in 0..3 {
        let mut r = radio();
        for _ in 0..layout {
            r.event(Event::Layout);
        }
        r.home();
        r.wait(1000);
        r.sim.watch_passes(true);
        for close in [Key::Exit, Key::Digit(5)] {
            r.hold(Key::Star).wait(300);
            r.press(Key::Ptt(PTT_A)).wait(300).press(Key::Side(1)).wait(300).press(Key::Side(2)).wait(300);
            r.knob(2).wait(200).knob(-1).wait(200);
            r.press(Key::Ptt(PTT_B)).wait(300).press(Key::Ptt(PTT_B)).wait(300);
            r.wait(2000); // traffic under the box
            r.press(close).wait(1500);
        }
        r.hold(Key::Star).wait(6000); // the idle close
        r.wait(25_000); // a cycle of the fake radio's traffic after it
        let live = r.sim.passes.iter().filter(|h| h.live).count();
        assert!(live > 0, "layout {layout}: live updates watched");
        for h in r.sim.passes.iter().filter(|h| h.live && h.report.flicker > 0) {
            bad.push(format!("layout {layout}: {} at {} ms: {} px flicker in {:?}", h.screen, h.at, h.report.flicker, h.report.bbox));
        }
    }
    assert!(bad.is_empty(), "DISP-25:\n{}", bad.join("\n"));
}

/// BUDGET's rows, measured now with a quarter's headroom.
#[test]
#[ignore]
fn overdraw_table() {
    for (name, mut r) in runs() {
        r.wait(1000);
        r.sim.watch_passes(true);
        short(&name, &mut r);
        let writes: u64 = r.sim.passes.iter().filter(|h| h.live).map(|h| h.report.writes).sum();
        let b = (writes * 5 / 4).div_ceil(1000) * 1000;
        println!("    ({name:?}, {b}),");
    }
}
