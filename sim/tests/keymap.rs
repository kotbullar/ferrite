//! KEY-26: what every key does on every screen, as a reviewed record. For each screen the simulator can open and
//! each key (tapped and held, the side keys and PTT A included, and the knob), a fresh radio on that screen takes
//! the key, and the record says where it ended up, what it asked the radio to do, and any message shown. The record
//! lives in `sim/golden/keys/`; a change in what a key does fails here until it is looked at and accepted with
//! `UPDATE_GOLDEN=1`. Two bugs of Nov 18, 2026 were keys doing the wrong thing in a place no test pressed them:
//! SIDE1 monitoring only on home, and the question of what SIDE2 does on the satellite pages.

mod common;

use common::Radio;
use ferrite_core::keys::Key;
use ferrite_sim::simcore::jump_names;
use std::path::PathBuf;

#[derive(Clone, Copy)]
enum Act {
    Tap(Key),
    Hold(Key),
    Ptt,
    Knob(i32),
}

fn acts() -> Vec<Act> {
    let mut keys = vec![Key::Menu, Key::Exit, Key::Up, Key::Down, Key::Left, Key::Right, Key::Slot, Key::VfoMem, Key::Star, Key::Hash];
    keys.extend((0..10).map(Key::Digit));
    keys.extend([Key::Side(1), Key::Side(2)]);
    let mut v: Vec<Act> = keys.iter().map(|&k| Act::Tap(k)).collect();
    v.extend(keys.iter().map(|&k| Act::Hold(k)));
    v.extend([Act::Ptt, Act::Knob(1), Act::Knob(-1)]);
    v
}

fn name(a: Act) -> String {
    let k = |k: Key| match k {
        Key::Digit(d) => d.to_string(),
        Key::Side(s) => format!("SIDE{s}"),
        Key::Menu => "MENU".into(),
        Key::Exit => "BACK".into(),
        Key::VfoMem => "V/M".into(),
        Key::Slot => "ABC".into(),
        other => format!("{other:?}").to_uppercase(),
    };
    match a {
        Act::Tap(x) => k(x),
        Act::Hold(x) => format!("hold {}", k(x)),
        Act::Ptt => "PTT".into(),
        Act::Knob(d) => format!("knob {d:+}"),
    }
}

/// The commands a key sent, by kind, in order, without the ones every key or every few seconds sends (the key's
/// beep, the keep-awake every key sends, the mode's lease renewal).
fn cmds(r: &mut Radio) -> String {
    let mut out: Vec<String> = Vec::new();
    for c in r.take_cmds() {
        let s = format!("{c:?}");
        if s.starts_with("Beep") || s.starts_with("Lease") || s == "Activity" {
            continue;
        }
        if out.last() != Some(&s) {
            out.push(s);
        }
    }
    out.join("; ")
}

/// The message on screen after the key: the readback's toast or popup line.
fn message(r: &Radio) -> String {
    r.sim.readback().lines().find(|l| l.starts_with("toast ") || l.starts_with("popup ")).unwrap_or("").to_string()
}

/// The record for screens `from..to` of [`jump_names`].
fn record(from: usize, to: usize) -> String {
    let mut out = String::new();
    for (i, screen) in jump_names().enumerate().skip(from).take(to - from) {
        out.push_str(&format!("## {i} {screen}\n"));
        for a in acts() {
            let mut r = Radio::new();
            r.sim.jump(i);
            r.wait(500);
            let at = r.screen();
            r.take_cmds();
            match a {
                Act::Tap(k) => {
                    r.press(k);
                }
                Act::Hold(k) => {
                    r.hold(k);
                }
                Act::Ptt => {
                    r.sim.key(Key::Ptt(0), true);
                    r.wait(300);
                    r.sim.key(Key::Ptt(0), false);
                }
                Act::Knob(d) => {
                    r.knob(d);
                }
            }
            r.wait(300);
            let now = r.screen();
            let to_ = if now == at { "=".to_string() } else { now.to_string() };
            let mut line = format!("{:<10} -> {to_}", name(a));
            let c = cmds(&mut r);
            if !c.is_empty() {
                line.push_str(&format!(" | {c}"));
            }
            let m = message(&r);
            if !m.is_empty() {
                line.push_str(&format!(" | {m}"));
            }
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Compares part `from..to` with its record (`UPDATE_GOLDEN=1` writes it instead).
fn check(from: usize, to: usize) {
    let got = record(from, to);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("golden/keys/screens-{from:02}-{:02}.txt", to - 1));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if got != want {
        let first = got.lines().zip(want.lines()).position(|(a, b)| a != b).unwrap_or(want.lines().count().min(got.lines().count()));
        let ctx = |s: &str| s.lines().skip(first.saturating_sub(1)).take(3).collect::<Vec<_>>().join("\n    ");
        panic!(
            "KEY-26: what a key does changed in {} (line {}):\n  now:\n    {}\n  record:\n    {}\nLook at it; if meant, UPDATE_GOLDEN=1 and review the diff.",
            path.display(),
            first + 1,
            ctx(&got),
            ctx(&want)
        );
    }
}

// Ten screens a test, so the runner spreads them over its threads.
/// KEY-26: screens 0-9 against their record (the others below, ten a test).
#[test]
fn keys_on_screens_00_09() {
    check(0, 10);
}
#[test]
fn keys_on_screens_10_19() {
    check(10, 20);
}
#[test]
fn keys_on_screens_20_29() {
    check(20, 30);
}
#[test]
fn keys_on_screens_30_39() {
    check(30, 40);
}
#[test]
fn keys_on_screens_40_49() {
    check(40, 50);
}
#[test]
fn keys_on_screens_50_59() {
    check(50, 60);
}
#[test]
fn keys_on_screens_60_69() {
    check(60, 70);
}

#[test]
fn keys_on_screens_70_onward() {
    check(70, jump_names().count());
}
