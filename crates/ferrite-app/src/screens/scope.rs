//! Band scope (manual chapter 21, SCOPE-1 to SCOPE-13; Scope mock-up), after nicFW's: a muted sweep on the
//! active slot's chip, bars above the noise floor, centre on the strongest, listen to the marker or the
//! strongest, and a trigger line that listens or logs. ◀▶ move the marker, the knob moves the centre, digits
//! type it, OK tunes the active slot to the marker, MENU opens the scope options (MENU 0 3 1).
//!
//! Bars are redrawn 40 px at a time when their levels change (SCOPE-7). The waterfall keeps no history in
//! RAM (SCOPE-6): each sweep paints one row into a 12-row band that wraps, a dark line marking the newest.
//! (The ST7789 hardware scroll is the radio's later refinement.)

use super::activity::Mode;
use super::entry::{Entry, EntryFor};
use super::hf;
use super::parts;
use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::ChannelStore;
use crate::keys::{KeyEvent, Press};
use crate::settings::{SCOPE_BAR_COUNTS, SCOPE_TRIGGERS};
use crate::slot::{band_name, MODES, STEPS};
use crate::{App, Screen};
use ferrite_core::freq::Freq10;
use ferrite_core::keys::Key;
use ferrite_core::msg::{LiveAll, ModeLive, RadioCmd, SCOPE_BARS};
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::theme;
use ferrite_ui::widgets as w;
use ufmt::uwrite;

/// The graph's rows: bars stand on BASE.
const TOP: i32 = 50;
const BASE: i32 = 162;
const H: i32 = BASE - TOP;
/// Bars are drawn this many pixels per dB above the floor (SCOPE-8).
const PX_PER_DB: i32 = 2;
/// Bars redraw in six 40 px groups (SCOPE-7).
const GROUPS: usize = 6;
/// Waterfall rows, the marker box, the info line.
const WF_Y: i32 = 184;
const WF_ROWS: i32 = 12;
const BOX_Y: i32 = 224;
const INFO_Y: i32 = 272;
/// A trigger listen that hears nothing within this gives up and sweeps again (SCOPE-10).
const TRIGGER_WAIT_MS: u32 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    pub(crate) hf: bool,
    /// Bar index; the centre is n / 2.
    pub marker: u8,
    /// 0 sweeping; 1 the marker, 2 the strongest (SIDE1, SIDE2), 3 the trigger (SCOPE-10).
    pub listen: u8,
    /// The bar being listened to (the centre doesn't move while listening).
    pub at: u8,
    /// The last sweep's strongest bar (255 none), sweeps in a row with it over the trigger line, the last sweep
    /// counted, and whether a trigger listen has heard its squelch open (it ends when it closes) and since when
    /// (app time in 16 ms).
    pub peak: u8,
    pub run: u8,
    pub seen: u8,
    pub heard: bool,
    pub since: u16,
}

/// Bars in a sweep (0 3 1 1, SCOPE-9).
pub fn bars_n(app: &App) -> usize {
    SCOPE_BAR_COUNTS[app.set.scope_bars as usize % SCOPE_BAR_COUNTS.len()] as usize
}

/// The step between bars (0 3 1 2), 10 Hz units.
pub fn step_of(app: &App) -> u16 {
    STEPS[app.set.scope_step as usize % STEPS.len()].0 as u16
}

/// Hold 1 on home, MENU 0 3 1 6 (SCOPE-1): the scope around the active slot's frequency.
pub fn open(app: &mut App) -> Screen {
    open_at(app, app.slot(app.active).freq(), false)
}

pub(crate) fn open_at(app: &mut App, f: Freq10, hf: bool) -> Screen {
    app.radio.centre = f; // MODE-2: the reconciler starts the sweep here
    Screen::Scope(Scope { hf, marker: (bars_n(app) / 2) as u8, listen: 0, at: 0, peak: 255, run: 0, seen: 0, heard: false, since: 0 })
}

/// The nearest scope can be behind frequency entry or scope settings.
pub(crate) fn from_hf(app: &App) -> bool {
    core::iter::once(app.screen).chain(app.back.iter().rev().flatten().copied()).find_map(|screen| {
        if let Screen::Scope(scope) = screen { Some(scope.hf) } else { None }
    }).unwrap_or(false)
}

fn centre_for(app: &App, centre: Freq10) -> Freq10 {
    if from_hf(app) { centre.clamp(hf::CHIP1_FROM, hf::TOP - 1) } else { centre }
}

fn valid_for_hf(app: &mut App, freq: Freq10) -> bool {
    if from_hf(app) && !(hf::CHIP1_FROM..hf::TOP).contains(&freq) {
        app.show_toast(b"HF waterfall", b"18 to 64 MHz", false);
        return false;
    }
    true
}

/// A new centre or setting while the scope runs; [`App::reconcile_mode`] starts it otherwise (MODE-2).
fn sweep(app: &mut App, centre: Freq10) {
    app.radio.centre = centre;
    if app.radio.mode == Mode::Scope {
        send_sweep(app, centre);
    }
}

/// The scope's start or retune: its settings, then the sweep around `centre`.
pub(crate) fn send_sweep(app: &mut App, centre: Freq10) {
    let (step, bars, dwell) = (step_of(app), bars_n(app) as u8, app.set.scope_dwell.clamp(1, 100));
    let hf = from_hf(app);
    app.radio.scope_hf = hf;
    app.out.push(RadioCmd::ScopeCfg { bars, dwell, strip: false, hf });
    app.out.push(RadioCmd::Sweep { centre, step, on: true });
}

/// The frequency of bar i of n.
pub fn bar_freq(centre: Freq10, step: u16, n: usize, i: i32) -> Freq10 {
    (centre as i64 + (i - n as i32 / 2) as i64 * step as i64).max(0) as Freq10
}

/// The noise floor (the lowest level swept) and the strongest bar (SCOPE-4, SCOPE-8).
pub fn floor_peak(bars: &[u8]) -> (u8, Option<u8>) {
    let mut floor = 255;
    let mut pk: Option<u8> = None;
    for (i, &v) in bars.iter().enumerate() {
        if v == 0 {
            continue;
        }
        floor = floor.min(v);
        if pk.is_none_or(|p| bars[p as usize] < v) {
            pk = Some(i as u8);
        }
    }
    (if pk.is_some() { floor } else { 0 }, pk)
}

/// A bar's height above the floor (SCOPE-8); 0 for a blank one.
pub fn bar_h(v: u8, floor: u8) -> i32 {
    if v == 0 {
        0
    } else {
        (v.saturating_sub(floor) as i32 * PX_PER_DB).clamp(1, H)
    }
}

/// The trigger line's height above the base: 1-9 in tenths of the graph, 0 none.
pub fn line_h(line: u8) -> i32 {
    line.min(9) as i32 * H / 10
}

fn listen(app: &mut App, f: Freq10) {
    let mode = if from_hf(app) { app.set.hf.mode } else { app.set.scope_mode % 3 };
    app.out.push(RadioCmd::ScopeListen { freq: f, mode });
}

/// The active slot's VFO on f (OK, PTT while listening).
fn tune(app: &mut App, f: Freq10) {
    let a = app.active as usize;
    let from = core::mem::replace(&mut app.slots[a].vfo, f);
    app.slots[a].vfo_mode = true;
    app.hf_starts_low(a as u8, from); // TX-36
}

/// A typed centre (SCOPE-3): sweep around it, the marker in the middle.
pub fn recentre(app: &mut App, s: Scope, f: Freq10) -> Screen {
    if s.listen != 0 {
        listen(app, 0);
    }
    sweep(app, centre_for(app, f));
    Screen::Scope(Scope { marker: (bars_n(app) / 2) as u8, peak: 255, ..s.sweeping() })
}

impl Scope {
    /// The frequency being listened to.
    fn freq(self, app: &App) -> Freq10 {
        bar_freq(app.radio.centre, step_of(app), bars_n(app), self.at as i32)
    }

    fn sweeping(self) -> Self {
        Scope { listen: 0, at: 0, run: 0, heard: false, ..self }
    }
}

impl View for Scope {
    /// SIDE1 or SIDE2 listens here (SCOPE-*), not the monitor key.
    fn own_side1(self) -> bool {
        true
    }

    /// KEY-19: ◀▶ move the marker; the knob moves the centre.
    fn own_arrows(self) -> bool {
        true
    }

    fn options(self, app: &mut App) -> bool {
        app.open_screen(super::opts::Of::Scope.screen());
        true
    }

    fn name(self) -> &'static str {
        "Scope"
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        b.set(if self.hf { b"HF scope" } else { b"Scope" });
    }

    fn mockup(self) -> Option<&'static str> {
        Some("Scope.png")
    }

    fn key(self, app: &mut App, e: KeyEvent, _: &impl ChannelStore) {
        use Press::*;
        let (n, step, centre) = (bars_n(app), step_of(app), app.radio.centre);
        let m = (self.marker as usize).min(n - 1);
        match (e.key, e.press) {
            (Key::Left | Key::Right, Short | Repeat) => {
                let d = if e.key == Key::Right { 1 } else { -1 };
                let m = (m as i32 + d).clamp(0, n as i32 - 1) as u8;
                app.screen = Screen::Scope(Scope { marker: m, ..self });
            }
            (Key::Menu, Short) => {
                // SCOPE-3: OK (Go) tunes the active slot's VFO to the marker.
                let f = bar_freq(centre, step, n, m as i32);
                if from_hf(app) {
                    if !valid_for_hf(app, f) {
                        return;
                    }
                    hf::tune_to(app, f, None);
                    app.back();
                } else {
                    tune(app, f);
                    app.go_home();
                    app.send_slot(app.active);
                }
                let fq = w::freq3(f);
                app.show_toast(b"Tuned", fq.as_bytes(), false);
            }
            (Key::Hash, Short) => {
                // Centre on the strongest.
                if self.peak != 255 {
                    let c = centre_for(app, bar_freq(centre, step, n, self.peak as i32));
                    app.radio.centre = c;
                    if self.listen != 0 {
                        listen(app, 0);
                    }
                    sweep(app, c);
                    app.screen = Screen::Scope(Scope { marker: (n / 2) as u8, peak: 255, ..self.sweeping() });
                }
            }
            (Key::Side(k @ (1 | 2)), Short) => {
                let which = k;
                if self.listen == which {
                    listen(app, 0);
                    app.screen = Screen::Scope(self.sweeping());
                } else {
                    let i = if which == 1 { Some(m as u8) } else { (self.peak != 255).then_some(self.peak) };
                    if let Some(i) = i {
                        let freq = bar_freq(centre, step, n, i as i32);
                        if !valid_for_hf(app, freq) {
                            return;
                        }
                        listen(app, freq);
                        app.screen = Screen::Scope(Scope { listen: which, at: i, run: 0, ..self });
                    }
                }
            }
            (Key::Star, Short) if !from_hf(app) => {
                app.set.scope_mode = (app.set.scope_mode + 1) % 3;
                if self.listen != 0 {
                    listen(app, self.freq(app));
                }
                app.show_toast(b"Mode", MODES[app.set.scope_mode as usize].as_bytes(), false);
            }
            (Key::Digit(d), Long) => {
                // The trigger line: 1 low to 9 high, 0 clears it (SCOPE-3).
                app.set.scope_line = d;
                app.screen = Screen::Scope(Scope { run: 0, ..self });
            }
            (Key::Digit(d), Short) => app.open_screen(Screen::Entry(Entry::new(EntryFor::ScopeCentre, Some(d)))),
            (Key::Exit, Short) if self.listen != 0 => {
                listen(app, 0);
                app.screen = Screen::Scope(self.sweeping());
            }
            (Key::Exit, Short) => app.back(),
            _ => {}
        }
    }

    fn knob(self, app: &mut App, d: i32, _: &impl ChannelStore) {
        let (n, step) = (bars_n(app), step_of(app));
        let c = centre_for(app, bar_freq(app.radio.centre, step, n, n as i32 / 2 + d));
        app.radio.centre = c;
        if self.listen != 0 {
            listen(app, 0);
            app.screen = Screen::Scope(self.sweeping());
        }
        sweep(app, c);
    }

    /// SCOPE-12: PTT transmits only while listening, on the frequency being listened to.
    fn ptt(self, app: &mut App, down: bool) {
        if !down {
            return;
        }
        if from_hf(app) {
            app.show_toast(b"HF waterfall", b"OK tunes to marker", false);
        } else if self.listen != 0 {
            tune(app, self.freq(app));
            app.send_slot(app.active); // the transmit's way home stops the scope (MODE-1)
            super::nav::transmit(app);
        } else {
            app.show_toast(b"Listen first", b"SIDE1 at the marker", false);
        }
    }

    /// The trigger (SCOPE-10): after 3 sweeps in a row with the strongest bar over the line, listen to it (until
    /// its squelch closes) or log it.
    fn tick(self, app: &mut App, live: &LiveAll) {
        let ModeLive::Scope { centre, bars, n, sweeps, open, .. } = live.mode else { return };
        if from_hf(app) && centre != app.radio.centre {
            return;
        }
        let n = (n as usize).clamp(1, SCOPE_BARS);
        let (floor, pk) = floor_peak(&bars[..n]);
        let mut s = Scope { peak: pk.unwrap_or(255), ..self };
        if s.listen == 3 {
            if open {
                s.heard = true;
            } else if s.heard || ((app.now >> 4) as u16).wrapping_sub(s.since) as u32 * 16 > TRIGGER_WAIT_MS {
                listen(app, 0);
                s = s.sweeping();
            }
        } else if s.listen == 0 && sweeps != s.seen {
            s.seen = sweeps;
            let (trig, line) = (app.set.scope_trig, app.set.scope_line);
            let over = pk.filter(|&p| line != 0 && bar_h(bars[p as usize], floor) >= line_h(line));
            s.run = if over.is_some() { s.run.saturating_add(1) } else { 0 };
            if let (Some(p), 3) = (over.filter(|_| trig != 0), s.run) {
                let f = bar_freq(centre, step_of(app), n, p as i32);
                if trig == 1 {
                    listen(app, f);
                    s = Scope { listen: 3, at: p, heard: false, since: (app.now >> 4) as u16, ..s };
                } else {
                    // Log: the event log's store isn't built yet; until then the toast is the record.
                    app.show_toast(b"Over the line", w::freq3(f).as_bytes(), false);
                }
            }
        }
        if s != self && app.screen == Screen::Scope(self) {
            app.screen = Screen::Scope(s);
        }
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        let step = step_of(app);
        let hf = from_hf(app);
        let (centre, bars, n, sweeps, open) = match cx.live.mode {
            ModeLive::Scope { centre, bars, n, sweeps, open, .. } if !hf || centre == app.radio.centre => (centre, bars, (n as usize).clamp(1, SCOPE_BARS), sweeps, open),
            _ => (app.radio.centre, [0; SCOPE_BARS], bars_n(app), 0, false),
        };
        let bw = (240 / n as i32).max(1);
        let (floor, pk) = floor_peak(&bars[..n]);
        let line = app.set.scope_line;
        // Header: band, centre; the MHz labels under the graph (SCOPE-2).
        if app.dirty(1, Mix::new().h(&centre).h(&n).h(&step).get()) {
            w::field(lcd, 8, 80, 21, 39, 34, band_name(centre).as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
            let fq = w::freq3(centre);
            let fw = text4_width(fq.as_bytes(), &UI_12B);
            w::field(lcd, 88, 144 - fw - 4, 21, 39, 34, b"centre", const { &w::st(&UI_12, SUB, BG, Align::Right) });
            w::field(lcd, 232 - fw, fw, 21, 39, 34, fq.as_bytes(), const { &w::st(&UI_12B, TEXT, BG, Align::Left) });
            lcd.fill(0, 40, 240, 1, LINE);
            let lo = w::freq3(bar_freq(centre, step, n, 0));
            let hi = w::freq3(bar_freq(centre, step, n, n as i32 - 1));
            lcd.fill(0, BASE, 240, 1, LINE);
            // Inside the 8 px side margins (DISP-2).
            w::field(lcd, 8, 74, BASE + 2, BASE + 18, BASE + 14, lo.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
            w::field(lcd, 82, 76, BASE + 2, BASE + 18, BASE + 14, fq.as_bytes(), const { &w::st(&UI_12, BLUE, BG, Align::Center) });
            w::field(lcd, 158, 74, BASE + 2, BASE + 18, BASE + 14, hi.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Right) });
        }
        // Bars, 40 px at a time (SCOPE-7): blank bars grey, the centre line blue, the marker amber, the bar being
        // listened to green, bars over the trigger red (SCOPE-8), the trigger line dashed red.
        let m = (self.marker as usize).min(n - 1);
        let heard = match self.listen {
            0 => None,
            _ => Some(self.at as i32),
        };
        let per = (40 / bw) as usize;
        let lh = line_h(line);
        for g in 0..GROUPS {
            let (a, b) = (g * per, ((g + 1) * per).min(n));
            if a >= n {
                break;
            }
            // SCOPE-14: input waiting: the rest of the redraw waits for the next pass (the groups not drawn stay
            // dirty), so the key or knob step is handled first.
            if lcd.input_pending() {
                return;
            }
            let r = &bars[a..b];
            let has_m = (a..b).contains(&m).then_some(m);
            let has_l = heard.filter(|&i| (a as i32..b as i32).contains(&i));
            if !app.dirty(2 + g, Mix::new().h(&r).h(&floor).h(&line).h(&has_m).h(&has_l).h(&n).get()) {
                continue;
            }
            let x0 = g as i32 * 40;
            // The group drawn off-screen and sent once (DISP-25): blanking it before the bars flickered on the radio.
            ferrite_ui::canvas::banded(lcd, x0, TOP - 8, 40, H + 8, BG, |lcd| {
                for (k, &v) in r.iter().enumerate() {
                    let i = a + k;
                    let bx = x0 + k as i32 * bw;
                    if v == 0 {
                        lcd.fill(bx, TOP, bw, H, SURFACE); // SCOPE-5: the other chip or relay state
                        continue;
                    }
                    if i == n / 2 {
                        lcd.fill(bx + bw / 2, TOP, 1, H, BLUE);
                    }
                    let h = bar_h(v, floor);
                    let c = if i == m {
                        AMBER
                    } else if has_l == Some(i as i32) {
                        GREEN
                    } else if lh > 0 && h >= lh {
                        RED
                    } else if hf {
                        BLUE
                    } else {
                        rgb565(0x4D, 0x80, 0xC0)
                    };
                    lcd.fill(bx, BASE - h, if bw >= 4 { bw - 1 } else { bw }, h, c);
                }
                if let Some(i) = has_m {
                    let cx = x0 + (i - a) as i32 * bw + bw / 2;
                    lcd.tri((cx - 3).clamp(x0, x0 + 33), TOP - 8, 7, 1, AMBER);
                }
                if lh > 0 {
                    let mut x = x0;
                    while x < x0 + 40 {
                        lcd.fill(x, BASE - lh, 3, 1, RED);
                        x += 5;
                    }
                }
            });
        }
        // Waterfall (0 3 1 5): one row per sweep, wrapping; the next row's place shows dark.
        let wf = hf || app.set.scope_wf;
        if hf && app.dirty(R_TOP, centre) {
            lcd.fill(0, WF_Y, 240, WF_ROWS * 3, BG);
        }
        let frame = hf.then_some(centre);
        if app.dirty(10, Mix::new().h(&wf).h(&wf.then_some(sweeps)).h(&frame).get()) {
            if wf {
                let row = (sweeps as i32) % WF_ROWS;
                let y = WF_Y + row * 3;
                let palette = theme::get();
                for (i, &v) in bars[..n].iter().enumerate() {
                    let color = if hf { hf_heat(v, palette) } else { heat(v) };
                    lcd.fill(i as i32 * bw, y, bw, 3, color);
                }
                let ny = WF_Y + ((row + 1) % WF_ROWS) * 3;
                lcd.fill(0, ny, 240, 3, BG);
            } else {
                lcd.fill(0, WF_Y, 240, WF_ROWS * 3, BG);
            }
        }
        // The marker box: the marker (or what is being listened to) and its level; the strongest under it.
        let (label, lab_c, f, lv) = match self.listen {
            0 => (&b"Marker"[..], AMBER, bar_freq(centre, step, n, m as i32), bars[m]),
            _ => (&b"Listen"[..], GREEN, self.freq(app), heard.filter(|&i| (0..n as i32).contains(&i)).map_or(0, |i| bars[i as usize])),
        };
        let st = if lv == 0 { Buf::from(b"-") } else { w::s_text(lv as i16 - 160) };
        let mut p = Buf::<40>::new();
        if self.listen != 0 {
            p.push(if open { b"Signal" } else { b"Quiet " });
            p.push(if hf { " \u{b7} Listen only" } else { " \u{b7} PTT talks here" }.as_bytes());
        } else {
            p.push(b"Strongest ");
            match pk {
                Some(i) => p.push(w::freq3(bar_freq(centre, step, n, i as i32)).as_bytes()),
                None => p.push(b"none"),
            }
        }
        let line = app.dirty(12, Mix::new().h(&p).get());
        if app.dirty(11, Mix::new().h(&label).h(&f).h(&st.as_bytes()).get()) {
            // The box whole in bands, with the line under the marker (DISP-25).
            let y = BOX_Y;
            ferrite_ui::canvas::banded(lcd, 6, y, 228, 44, BG, |lcd| {
                lcd.fill_r(6, y, 228, 44, 3, SURFACE);
                lcd.frame_r(6, y, 228, 44, 3, lab_c);
                lcd.text4_rows(15, y + 20, label, &UI_12, lab_c, SURFACE, y + 4, y + 24);
                w::freq(lcd, 66, 180, y + 21, f, &DIG_17, &DIG_17, TEXT, SUB, SURFACE);
                w::field(lcd, 180, 46, y + 4, y + 24, y + 20, st.as_bytes(), const { &w::st(&UI_12B, TEXT, SURFACE, Align::Right) });
                w::field(lcd, 15, 211, BOX_Y + 22, BOX_Y + 39, BOX_Y + 35, p.as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
            });
        } else if line {
            w::field(lcd, 15, 211, BOX_Y + 22, BOX_Y + 39, BOX_Y + 35, p.as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
        }
        // Bars, step, speed, trigger (SCOPE-2).
        let mut t = Buf::<40>::new();
        let d = app.set.scope_dwell.clamp(1, 100);
        let _ = uwrite!(t, "{} bars \u{b7} ", n);
        khz(step as u32, &mut t);
        let _ = uwrite!(t, " \u{b7} {}.{} ms", d / 10, d % 10);
        if app.set.scope_trig != 0 {
            t.push(" \u{b7} ".as_bytes());
            // "Mon" for Monitor: the whole word doesn't fit beside "60 bars" (SCOPE-2).
            t.push(match app.set.scope_trig % 3 { 1 => "Mon", i => SCOPE_TRIGGERS[i as usize] }.as_bytes());
        }
        if app.dirty(13, Mix::new().h(&t).get()) {
            w::field(lcd, 8, 224, INFO_Y, INFO_Y + 17, INFO_Y + 13, t.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
        }
    }

    fn hints(self, _: &App) -> Hints {
        if self.hf {
            return if self.listen == 0 {
                Hints::new(&[(ARROWS_LR, "Marker"), ("#", "Peak"), ("OK", "Tune")])
            } else {
                Hints::new(&[("BACK", "Sweep"), ("OK", "Tune")])
            };
        }
        match self.listen {
            0 => Hints::new(&[(ARROWS_LR, "Marker"), ("SIDE1", "Listen"), ("OK", "Go")]),
            1 => Hints::new(&[("PTT", "Talk"), ("SIDE1", "Stop"), ("OK", "Go")]),
            2 => Hints::new(&[("PTT", "Talk"), ("SIDE2", "Stop"), ("OK", "Go")]),
            _ => Hints::new(&[("PTT", "Talk"), ("BACK", "Stop"), ("OK", "Go")]),
        }
    }
}

/// "800k", "12.5k", "1.4M": 10 Hz units, short.
pub fn khz(f: u32, b: &mut Buf<40>) {
    if f >= 100_000 {
        let _ = uwrite!(b, "{}", f / 100_000);
        if f % 100_000 >= 10_000 {
            let _ = uwrite!(b, ".{}", f % 100_000 / 10_000);
        }
        b.push(b"M");
    } else {
        let _ = uwrite!(b, "{}", f / 100);
        let r = f % 100;
        if r != 0 {
            let _ = uwrite!(b, ".{}", if r.is_multiple_of(10) { r / 10 } else { r });
        }
        b.push(b"k");
    }
}

/// The HF waterfall uses theme roles. Night Red keeps a graded ramp so it can
/// show signal strength as brightness even though its accent roles share a hue.
fn hf_heat(v: u8, palette: u8) -> u16 {
    if v == 0 {
        return BG;
    }
    if palette == theme::NIGHT_RED {
        return heat(v);
    }
    match parts::level_h(v, 8) {
        0 | 1 => SURFACE,
        2 => LINE,
        3 | 4 => BLUE,
        5 | 6 => GREEN,
        _ if palette == theme::CYBERPUNK => VIOLET,
        _ => AMBER,
    }
}

/// Waterfall colour for a level: dark blue through teal and green to amber.
pub fn heat(v: u8) -> u16 {
    match parts::level_h(v, 8) {
        0 | 1 => rgb565(0x0F, 0x2A, 0x45),
        2 => rgb565(0x1A, 0x55, 0x80),
        3 | 4 => rgb565(0x2A, 0x9D, 0xA8),
        5 | 6 => rgb565(0x5C, 0xD0, 0x9A),
        _ => AMBER,
    }
}

/// The home strip's bars (single + scope): 4 px each across 224 px.
pub const STRIP_BARS: usize = 56;
const X0: i32 = 8;
const MID: i32 = STRIP_BARS as i32 / 2;

/// The small scope under the active slot (home, single + scope; HOME-3): bars, the centre in blue, the edges.
pub fn strip(app: &mut App, lcd: &mut impl Panel, region: usize, y: i32, h: i32, centre: Freq10, step: u16, bars: &[u8]) {
    // The bars change every point: each is drawn as background above and colour below, two fills that don't
    // overlap, never blanked first (blanking the strip flickered on the radio, Oct 12, 2026). The frame, axis and
    // labels only when the strip is first drawn or the centre moves.
    // SCOPE-14: input waiting: the strip waits for the next pass (still dirty), so the input is handled first.
    if lcd.input_pending() {
        return;
    }
    let Some(clear) = app.dirty_shape(region, Mix::new().h(&bars).h(&centre).get(), true) else { return };
    if clear {
        lcd.fill(0, y, 240, h + 18, BG);
        lcd.fill(X0, y + h, 224, 1, LINE);
    }
    for (i, &v) in bars.iter().enumerate() {
        let bx = X0 + i as i32 * 4;
        let bh = parts::level_h(v, h).max(2);
        let c = if i as i32 == MID { BLUE } else { DIM };
        lcd.fill(bx, y, 3, h - bh, BG);
        lcd.fill(bx, y + h - bh, 3, bh, c);
    }
    let half = step as u32 * MID as u32;
    let mut a = Buf::<40>::new();
    a.push(b"-");
    khz(half, &mut a);
    let mut b = Buf::<40>::new();
    b.push(b"+");
    khz(half, &mut b);
    w::field(lcd, 8, 60, y + h + 2, y + h + 18, y + h + 14, a.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
    w::field(lcd, 80, 80, y + h + 2, y + h + 18, y + h + 14, w::freq3(centre).as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Center) });
    w::field(lcd, 172, 60, y + h + 2, y + h + 18, y + h + 14, b.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Right) });
}

/// HOME-14: the strip while it can't run (FM broadcast has the receiver: BCR-29): its area blanked once, then only a
/// dim frame, with no grid, bars or labels, so it doesn't look like a scope that is just quiet. Shares the strip's
/// region, so either drawing replaces the other whole.
pub fn strip_off(app: &mut App, lcd: &mut impl Panel, region: usize, y: i32, h: i32) {
    if app.dirty_shape(region, Mix::new().h(&y).h(&h).get(), false).is_none() {
        return;
    }
    lcd.fill(0, y, 240, h + 18, BG);
    lcd.fill(X0, y, 224, 1, LINE);
    lcd.fill(X0, y + h, 224, 1, LINE);
    lcd.fill(X0, y + 1, 1, h - 1, LINE);
    lcd.fill(X0 + 223, y + 1, 1, h - 1, LINE);
}

#[cfg(test)]
mod floor_tests {
    use super::*;

    /// HF-36: the waterfall preserves distinct levels in each display theme.
    #[test]
    fn hf_waterfall_uses_the_theme_palette() {
        let previous = theme::get();
        let levels = [35, 55, 75, 100, 130];
        for palette in [theme::DARK, theme::DAYLIGHT, theme::NIGHT_RED, theme::CYBERPUNK] {
            theme::set(palette);
            assert_eq!(theme::map(hf_heat(0, palette)), theme::map(BG));
            let colors = levels.map(|v| theme::map(hf_heat(v, palette)));
            for (i, &color) in colors.iter().enumerate() {
                assert!(!colors[..i].contains(&color), "theme {palette}: duplicate level {i}");
            }
            if palette == theme::NIGHT_RED {
                assert!(colors.iter().all(|&c| c & 0x07ff == 0), "red only");
                assert!(colors.windows(2).all(|pair| pair[0] < pair[1]), "brighter for stronger signals");
            } else {
                assert_eq!(colors[2], theme::map(BLUE));
                assert_eq!(colors[3], theme::map(GREEN));
                assert_eq!(colors[4], theme::map(if palette == theme::CYBERPUNK { VIOLET } else { AMBER }));
            }
        }
        theme::set(previous);
    }

    /// SCOPE-8: bars stand on the noise floor: the floor is the lowest bar of the sweep (blank points left out),
    /// each bar's height is its level above that, the floor bar still a pixel tall, a strong one capped at the
    /// graph's height; a weak signal over a quiet band stands out as much as a strong one over a noisy band.
    #[test]
    fn bars_on_the_floor() {
        let quiet = [40u8, 41, 40, 0, 46, 40];
        let (floor, pk) = floor_peak(&quiet);
        assert_eq!((floor, pk), (40, Some(4)), "blank points don't set the floor");
        assert_eq!(bar_h(40, floor), 1, "the floor bar is a pixel");
        assert_eq!(bar_h(0, floor), 0, "a blank point has no bar");
        let noisy = [90u8, 91, 90, 96, 90];
        let (nf, _) = floor_peak(&noisy);
        assert_eq!(bar_h(46, floor), bar_h(96, nf), "the same rise above the floor, the same height");
        assert_eq!(bar_h(255, 0), H, "capped at the graph");
    }
}
