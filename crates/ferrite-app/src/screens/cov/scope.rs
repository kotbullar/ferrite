//! The band scope (chapter 21): listening and its hints, the trigger (listen and log), the waterfall, the
//! trigger line, and the labels.

use super::*;
use crate::screens::scope::{self, Scope};
use ferrite_core::msg::{ModeLive, SCOPE_BARS};
use ferrite_ui::text::Buf;

fn open() -> T {
    let mut t = T::ready();
    t.open(Dest::BandScope);
    assert_eq!(t.name(), "Scope");
    t
}

fn sc(t: &T) -> Scope {
    match t.screen() {
        Screen::Scope(s) => s,
        s => panic!("not the scope: {}", s.name()),
    }
}

/// A sweep: noise at 40 with a peak of `peak` on bar `at`, sweep number `sweeps`.
fn sweep(t: &mut T, at: usize, peak: u8, sweeps: u8, open: bool) {
    let n = scope::bars_n(&t.app);
    let mut bars = [0u8; SCOPE_BARS];
    bars[..n].fill(40);
    bars[at] = peak;
    let c = t.app.radio.centre;
    t.live.mode = ModeLive::Scope { centre: c, bars, n: n as u8, sweeps, listen: 0, open, si: false };
    t.wait(100);
}

/// SCOPE-3, SCOPE-12: SIDE1 listens at the marker, SIDE2 at the strongest, each with its own hints; the box
/// says Listen and whether a signal is there; SIDE again, BACK, # or the knob go back to sweeping.
#[test]
fn listening() {
    let mut t = open();
    assert_eq!(t.screen().mockup(), Some("Scope.png"));
    sweep(&mut t, 5, 90, 1, false);
    t.tap(Key::Side(1));
    assert_eq!(sc(&t).listen, 1);
    t.reads("hints \"PTT\" \"Talk\" \"SIDE1\" \"Stop\" \"OK\" \"Go\"");
    t.shows("Listen");
    t.shows("Quiet  \u{b7} PTT talks here");
    sweep(&mut t, 5, 90, 2, true);
    t.shows("Signal \u{b7} PTT talks here");
    t.tap(Key::Hash); // centre on the strongest: listening stops
    assert_eq!(sc(&t).listen, 0);
    sweep(&mut t, 5, 90, 3, false);
    t.tap(Key::Side(2));
    assert_eq!(sc(&t).listen, 2);
    t.reads("hints \"PTT\" \"Talk\" \"SIDE2\" \"Stop\" \"OK\" \"Go\"");
    t.knob(1);
    assert_eq!(sc(&t).listen, 0, "the knob moves the centre and sweeps");
    t.tap(Key::Side(1));
    t.tap(Key::Exit);
    assert_eq!(sc(&t).listen, 0, "BACK stops listening first");
    assert_eq!(t.name(), "Scope");
}

/// SCOPE-3: a centre typed while listening stops the listening and sweeps there.
#[test]
fn recentre_while_listening() {
    let mut t = open();
    sweep(&mut t, 5, 90, 1, false);
    t.tap(Key::Side(1));
    t.cmds();
    let cur = sc(&t);
    let s = scope::recentre(&mut t.app, cur, ferrite_core::freq::mhz(146));
    assert!(matches!(s, Screen::Scope(x) if x.listen == 0));
    assert!(t.cmds().iter().any(|c| matches!(c, RadioCmd::ScopeListen { freq: 0, .. })), "listening off");
}

/// SCOPE-10: the trigger set to listen: three sweeps over the line listen to the peak (BACK stops, in the
/// hints); its squelch closing after it was heard sweeps again.
#[test]
fn trigger_listens() {
    let mut t = open();
    t.app.set.scope_trig = 1;
    t.hold(Key::Digit(2)); // the trigger line low
    for k in 1..=3 {
        sweep(&mut t, 7, 120, k, false);
    }
    assert_eq!(sc(&t).listen, 3, "{:?}", sc(&t));
    t.reads("hints \"PTT\" \"Talk\" \"BACK\" \"Stop\" \"OK\" \"Go\"");
    t.shows("60 bars \u{b7} 25k \u{b7} 10.0 ms \u{b7} Mon"); // the trigger named in the info line, all of it shown
    t.hides("Monitor");
    assert!(t.px(50, 162, ferrite_ui::gfx::RED) > 0, "the trigger line and the bar over it red");
    sweep(&mut t, 7, 120, 4, true);
    assert!(sc(&t).heard);
    sweep(&mut t, 7, 120, 5, false);
    assert_eq!(sc(&t).listen, 0, "it went quiet: sweeping again");
}

/// SCOPE-10: the trigger set to log says so in a toast.
#[test]
fn trigger_logs() {
    let mut t = open();
    t.app.set.scope_trig = 2;
    t.hold(Key::Digit(2));
    for k in 1..=3 {
        sweep(&mut t, 7, 120, k, false);
    }
    assert!(t.toast().starts_with("Over the line / "), "{}", t.toast());
    assert_eq!(sc(&t).listen, 0);
}

/// SCOPE-6: the waterfall paints one row a sweep in the levels' colours.
#[test]
fn waterfall_rows() {
    let mut t = open();
    t.app.set.scope_wf = true;
    sweep(&mut t, 3, 200, 1, false);
    assert!(t.px(184, 220, scope::heat(40)) > 0, "the noise's colour");
    assert!(t.px(184, 220, scope::heat(200)) > 0, "the peak's colour");
    sweep(&mut t, 3, 200, 2, false);
    let colours: std::collections::BTreeSet<u16> = (0..=255u8).step_by(8).map(scope::heat).collect();
    assert!(colours.len() >= 5, "dark blue through teal and green to amber: {colours:?}");
}

/// SCOPE-2: the step label, short: hundreds of kHz in M, fractions kept.
#[test]
fn step_labels() {
    let mut b = Buf::<40>::new();
    for (f, want) in [(140_000u32, "1.4M"), (100_000, "1M"), (1_250, "12.5k"), (80_000, "800k"), (125, "1.25k")] {
        b.clear();
        scope::khz(f, &mut b);
        assert_eq!(b.as_bytes(), want.as_bytes(), "{f}");
    }
}
