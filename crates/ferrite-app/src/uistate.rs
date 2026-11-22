//! What the screen shows, as text (manual chapter 29, LINK-19; decision 150): the link's `UI` and the terminal's
//! `ui`. A PC script presses keys over the link and reads this back to check the radio's UI as a person would.
//!
//! It is built from the App's own state, the same values the drawing code draws (each screen's rows come from the
//! functions its `render` uses), never from pixels. One line at a time, by number ([`line`]): nothing is held, so
//! it costs no RAM, and a page of an answer is the next lines. The lines, in this order:
//!
//! - `screen ID "Name" title="..."`: the screen (ID is stable: its variant), its name and the status bar's title.
//! - `state frame=N ago=MS dark=0 locked=0 active=A keyed=- remote=0 mode=off fm=0`: redraws that changed
//!   something, the time since the last, the backlight off, the key lock, the active slot, the slot transmitting,
//!   remote PTT, the exclusive mode running (MODE-1: off, scope, strip, tone, counter, fm, hf, xb) and whether
//!   broadcast FM plays.
//! - `bar lock=0 scan=0 watch=0 inbox=0 alert=0 gps=fix bt_on=1 bt=0 battery=80 low=0 cutoff=0 dry=0 lpm=0 vox=0 clock=14:32 utc=0 call=""`: the status bar
//!   (`call` the callsign drawn in the clock's place with no time, SBAR-2); while locked `lockmode=keypad`,
//!   `allbutptt` or `all` follows `lock=1` (KEY-14).
//! - `toast "head" "body"` or `popup "head" "body"` while one shows; `qmenu "PTT A" "Reverse" "off"` per row of the
//!   quick menu while it is up.
//! - The screen's own lines: home's `card` per slot (a broadcast card followed by its `bcast` report: the RDS
//!   text, `stereo`, `dbuv`, `snr`); a list's `row N "name" "value"` (with `sel`, `dot` changed from its default, `grey`), its `help`; an
//!   editor's `value`; a question's `ask`; the squelch screen's `squelch`; tone search's `tsearch` (the tone found,
//!   the slot's signal, the hits) and the counter's `counter` (frequency and level); a card's `hint` while it shows
//!   a tone hint; the PIN entry's `pin` (digits as
//!   stars); FM's `fm`, HF mode's `hf`, the band scope's `scope`, crossband's `xb`
//!   with its `check`s, setup's `setup`, compose's `compose`, the GPS position page's `gps`, `pos` and `near`
//!   (their option pages, grids and tiles as rows); and so on. A screen without its own lines yet shows only
//!   the rest.
//! - `hints "KEY" "label" ...`: the hint bar's pairs (under a toast, what it will show again).
//!
//! [`status_line`] is the compact one-line status (`UI` what=2, `ui status`).
//!
//! Strings are in double quotes, `"` and `\` escaped with `\`; the arrows are `^ v < >`.

use crate::chan::ChannelStore;
use crate::screens::{self, shared::Cx};
use crate::slot::{card_tone, mode_chip, shift_chip, QUICK};
use crate::term::cmd::Line;
use crate::{App, Screen};
use ferrite_core::freq::Freq10;
use ferrite_core::keys::{self as k, Key};
use ferrite_core::msg::{Device, GpsState, LiveAll, SLOTS};
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;
use ufmt::uwrite;

/// Line `i` of the description into `out`; false past the last.
pub fn line<S: ChannelStore>(app: &App, st: &S, dev: &Device, live: &LiveAll, i: u16, out: &mut Line) -> bool {
    out.clear();
    let mut d = Desc { want: i, n: 0, out, got: false, bare: false };
    describe(app, &Cx { live, dev, store: st }, &mut d);
    d.got
}

/// LINK-19.5: line `i` of the readback with the screen as drawn when `texts`: the description without the screen's
/// own lines (its rows, values, help: the drawing says them), then `t` lines (see [`Listener`]); as [`line`] without
/// `texts`, and for home's default layout, which keeps its structured cards (its meters and card shapes aren't text).
pub fn line_all<S: ChannelStore>(app: &mut App, st: &S, dev: &Device, live: &LiveAll, texts: bool, i: u16, out: &mut Line) -> bool {
    // Home's default layout keeps its structured cards; its other two (HOME-3) and Low Power Mode's are sent as drawn
    if !texts || matches!(app.screen, Screen::Home) && app.home_layout() == 0 && !app.lpm_home() {
        return line(app, st, dev, live, i, out);
    }
    out.clear();
    let mut d = Desc { want: i, n: 0, out, got: false, bare: true };
    describe(app, &Cx { live, dev, store: st }, &mut d);
    if d.got {
        return true;
    }
    let base = d.n;
    let mut d = Desc { want: i.wrapping_sub(base), n: 0, out: d.out, got: false, bare: true };
    let mut l = Listener { d: &mut d, k: 0, row: None, open: false, on: false, style: (0, 0, 0), win: (0, 0, 0, 0), bg: ferrite_ui::theme::map_bg(ferrite_ui::gfx::BG) };
    app.render_texts(&mut l, live, dev, st);
    d.got
}

/// LINK-19.5: a `t` line holds one row of the screen: items whose top is inside its first full-height item's line
/// box (give or take [`ROW_PX`]), and thin ones (an edge, a rule) wherever they are, up to about [`LINE_BYTES`]
/// (Live View's LINE_ROOM). By row, so a change on one row (a selection moved) changes its line alone, not every
/// line after it; by each item's exact length (worked out, not written), so the lines break in the same places
/// whether or not this line is the one being written.
const ROW_PX: i32 = 8;
const LINE_BYTES: u32 = 225;
/// An item this thin (a rule, a frame's edge) joins whatever line is open.
const THIN_PX: i32 = 3;
/// A fill smaller than this many pixels (a meter's segment, a dot) isn't sent.
const RECT_MIN_PX: u32 = 64;

/// LINK-19.5: the panel the screen is drawn again into for the readback: nothing is drawn, each text and each
/// coloured fill is written, as it comes, into `t` lines by screen row ([`ROW_PX`]), in drawing order (a later one
/// over an earlier one). A text: `x,y,w,h,font,fg,bg,"text"` (its line box, the font's [`font_id`], RGB565 in hex);
/// a fill not in the screen's background colour: `x,y,w,h,colour`; an icon (a mask: a dot, a symbol) `x,y,w,h,colour,o`. A text's `bg` is `-` when it is the screen's. Nothing is kept but the one line being written.
struct Listener<'a, 'b> {
    d: &'a mut Desc<'b>,
    /// The open line's estimated bytes, and its row (the top and bottom of its first full-height item; None before
    /// one).
    k: u32,
    row: Option<(i32, i32)>,
    open: bool,
    on: bool,
    style: (u8, u16, u16),
    win: (i32, i32, i32, i32),
    bg: u16,
}

impl Listener<'_, '_> {
    /// The next item's place: its box (top `y`, height `h`) and estimated bytes `n`; a new line for another row, or
    /// when this one is full. Some(the line) if it is the one wanted.
    fn item(&mut self, y: i32, h: i32, n: u32) -> Option<&mut Line> {
        let thin = h <= THIN_PX;
        let same = match self.row {
            _ if !self.open => false,
            _ if thin => true,
            None => true,
            Some((t, b)) => y >= t - ROW_PX && y <= b.max(t + ROW_PX),
        };
        if !same || self.k + n > LINE_BYTES {
            (self.row, self.k, self.open) = (None, 0, true);
            self.on = self.d.ln().is_some();
            if self.on {
                self.d.out.push(b"t");
            }
        }
        if !thin && self.row.is_none() {
            self.row = Some((y, y + h));
        }
        self.k += n;
        if self.on {
            self.d.out.push(b" ");
            Some(self.d.out)
        } else {
            None
        }
    }
}

impl Listener<'_, '_> {
    /// A fill (LINK-19.5): an item when it is coloured and big enough. Out of line: the panel's methods are kept in
    /// fast flash by name (memory.x), and this isn't per-pixel work.
    #[inline(never)]
    fn fill_item(&mut self, color: u16, count: u32) {
        let (x0, y0, x1, y1) = self.win;
        let (w, h) = ((x1 - x0 + 1).max(0) as u32, (y1 - y0 + 1).max(0) as u32);
        if color == self.bg || count != w * h || count < RECT_MIN_PX {
            return;
        }
        // ` x,y,w,h,cccc`
        let n = 1 + digits(x0) + digits(y0) + digits(w as i32) + digits(h as i32) + 3 + 5;
        if let Some(l) = self.item(y0, h as i32, n) {
            let _ = uwrite!(l, "{},{},{},{},", x0, y0, w, h);
            hex4(l, color);
        }
    }

    /// An icon (LINK-19.5).
    #[inline(never)]
    fn icon_item(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u16) {
        let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
        // ` x,y,w,h,cccc,o`
        let n = 1 + digits(x0) + digits(y0) + digits(w) + digits(h) + 3 + 5 + 2;
        if let Some(l) = self.item(y0, h, n) {
            let _ = uwrite!(l, "{},{},{},{},", x0, y0, w, h);
            hex4(l, c);
            l.push(b",o");
        }
    }

    /// A text (LINK-19.5).
    #[inline(never)]
    fn text_item(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, s: &[u8]) {
        let (f, fg, bg) = self.style;
        let own = bg == self.bg;
        // ` x,y,w,h,f,ffff,bbbb,"text"` (the text as `q` writes it, escapes and all)
        let n = 1 + digits(x0) + digits(y0) + digits(x1 - x0 + 1) + digits(y1 - y0 + 1) + digits(f as i32) + 5 + 4 + 1 + if own { 1 } else { 4 } + 1 + quoted_len(s);
        if let Some(l) = self.item(y0, y1 - y0 + 1, n) {
            let _ = uwrite!(l, "{},{},{},{},{},", x0, y0, x1 - x0 + 1, y1 - y0 + 1, f);
            hex4(l, fg);
            l.push(b",");
            if own {
                l.push(b"-"); // the screen's own background
            } else {
                hex4(l, bg);
            }
            l.push(b",");
            q(l, s);
        }
    }
}

impl ferrite_ui::panel::Panel for Listener<'_, '_> {
    fn window(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.win = (x0, y0, x1, y1);
    }

    fn push(&mut self, color: u16, count: u32) {
        self.fill_item(color, count);
    }

    fn push_buf(&mut self, _px: &[u16]) {}

    fn mask_hint(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: u16) {
        self.icon_item(x0, y0, x1, y1, c);
    }

    fn text_style(&mut self, f: &'static ferrite_ui::font4::Font4, fg: u16, bg: u16) {
        self.style = (font_id(f), fg, bg);
    }

    fn text_hint(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, s: &[u8]) {
        self.text_item(x0, y0, x1, y1, s);
    }

    fn offscreen(&self) -> bool {
        true // the screens' regions draw straight through, no bands (ferrite_ui::canvas)
    }

    fn text_only(&self) -> bool {
        true
    }
}

/// The digits of `n` written in decimal (a minus counted).
fn digits(n: i32) -> u32 {
    let (mut d, mut v) = (if n < 0 { 2 } else { 1 }, n.unsigned_abs() / 10);
    while v > 0 {
        (d, v) = (d + 1, v / 10);
    }
    d
}

/// The bytes [`q`] writes for `s`, quotes included.
fn quoted_len(s: &[u8]) -> u32 {
    2 + s.iter().map(|&c| if c == b'"' || c == b'\\' { 2 } else { 1 }).sum::<u32>()
}

/// RGB565 as four hex digits.
fn hex4(l: &mut Line, c: u16) {
    const H: &[u8; 16] = b"0123456789abcdef";
    l.push(&[H[(c >> 12) as usize], H[(c >> 8 & 15) as usize], H[(c >> 4 & 15) as usize], H[(c & 15) as usize]]);
}

/// LINK-19.5: a font as the readback names it: 0 UI 12, 1 UI 12 bold, 2 UI 15 bold, 3 name 22, 4 name 26,
/// 5 digits 17, 6 digits 26, 7 digits 28, 8 digits 46, 9 the wordmark 40; 15 another.
#[inline(never)]
fn font_id(f: &'static ferrite_ui::font4::Font4) -> u8 {
    use ferrite_ui::assets::*;
    let all: [&ferrite_ui::font4::Font4; 10] = [&UI_12, &UI_12B, &UI_15B, &NAME_22, &NAME_26, &DIG_17, &DIG_26, &DIG_28, &DIG_46, &WORD_40];
    all.iter().position(|a| core::ptr::eq(*a, f)).map_or(15, |i| i as u8)
}

/// One line of the description is wanted: the others are counted, not written.
struct Desc<'a> {
    want: u16,
    n: u16,
    out: &'a mut Line,
    got: bool,
    /// Without the screen's own lines (LINK-19.5: the screen as drawn follows).
    bare: bool,
}

impl Desc<'_> {
    /// The next line: Some(its buffer) if it is the one wanted.
    fn ln(&mut self) -> Option<&mut Line> {
        let me = self.n == self.want;
        self.n += 1;
        if me {
            self.got = true;
            Some(self.out)
        } else {
            None
        }
    }
}

/// A string in double quotes: `"` and `\` escaped, the font's arrows as `^ v < >`, other control bytes as `?`.
fn q(l: &mut Line, s: &[u8]) {
    l.push(b"\"");
    for &c in s {
        match c {
            b'"' | b'\\' => l.push(&[b'\\', c]),
            0x18 => l.push(b"^"),
            0x19 => l.push(b"v"),
            0x1A => l.push(b">"),
            0x1B => l.push(b"<"),
            c if c < 0x20 => l.push(b"?"),
            c => l.push(&[c]),
        }
    }
    l.push(b"\"");
}

/// ` key=` then a quoted string.
fn kq(l: &mut Line, key: &str, s: &[u8]) {
    l.push(b" ");
    l.push(key.as_bytes());
    l.push(b"=");
    q(l, s);
}

/// ` key=n`.
fn kn(l: &mut Line, key: &str, n: u32) {
    let _ = uwrite!(l, " {}={}", key, n);
}

/// MHz with five decimals (10 Hz steps): "146.52000".
fn mhz(l: &mut Line, f: Freq10) {
    let frac = f % 100_000;
    let _ = uwrite!(l, "{}.", f / 100_000);
    let mut d = 10_000;
    while d > 0 {
        l.push(&[b'0' + (frac / d % 10) as u8]);
        d /= 10;
    }
}

fn letter(s: u8) -> u8 {
    b'A' + s
}

/// ` keyed=A remote=0 mode=off`: the slot transmitting (`-` none), remote PTT and the exclusive mode (MODE-1).
/// A data key-up (APRS, KISS: the UI's own `keyed` is voice only) counts too, from the Radio task's live state
/// (LINK-19), so a test sees and times every transmission.
fn keyed_remote_mode(app: &App, live: &LiveAll, l: &mut Line) {
    let keyed = app.radio.keyed.or(live.tx.keyed.then_some(live.tx.slot));
    match keyed {
        Some(s) if (s as usize) < SLOTS => {
            let _ = uwrite!(l, " keyed={}", letter(s) as char);
        }
        Some(_) => l.push(b" keyed=HF"),
        None => l.push(b" keyed=-"),
    }
    kn(l, "remote", app.remote_keyed() as u32);
    use screens::activity::Mode;
    l.push(b" mode=");
    l.push(match app.radio.mode {
        Mode::Off => b"off".as_slice(),
        Mode::Scope => b"scope",
        Mode::Strip => b"strip",
        Mode::Tone => b"tone",
        Mode::Counter => b"counter",
        Mode::Fm => b"fm",
        Mode::Hf => b"hf",
        Mode::Xb => b"xb",
        Mode::Sat => b"sat",
    });
}

/// The compact status (LINK-19, `UI` what=2; the terminal's `ui status`): one short line for a slow link
/// (Bluetooth, LINK-36), the facts tests poll, the same values as the full readback:
/// `st scr=Home act=A keyed=- remote=0 mode=off A=RX/20 B=heard/12 C=-/0 xb=0`. Per slot its card's state word
/// (`-` for idle, spaces as `_`: `RX_muted`) and its meter; `xb=1,STATE,PASSES` while the repeater runs. About
/// 70-85 bytes, 140 at the very most: always one answer.
pub fn status_line(app: &App, live: &LiveAll, l: &mut Line) {
    l.clear();
    l.push(b"st scr=");
    l.push(id(app.screen).as_bytes());
    let _ = uwrite!(l, " act={}", letter(app.active) as char);
    keyed_remote_mode(app, live, l);
    for s in 0..SLOTS as u8 {
        let _ = uwrite!(l, " {}=", letter(s) as char);
        match app.state(s, live).0 {
            "idle" => l.push(b"-"),
            w => {
                for &c in w.as_bytes() {
                    l.push(&[if c == b' ' { b'_' } else { c }]);
                }
            }
        }
        let _ = uwrite!(l, "/{}", w::meter_level(live.slots[s as usize].rssi));
    }
    match live.mode {
        ferrite_core::msg::ModeLive::Xb(x) if screens::xb::running(app) => {
            let _ = uwrite!(l, " xb=1,{},{}", x.state, x.passes);
        }
        _ if screens::xb::running(app) => l.push(b" xb=1,0,0"),
        _ => l.push(b" xb=0"),
    }
}

fn describe<S: ChannelStore>(app: &App, cx: &Cx<S>, d: &mut Desc) {
    let sc = app.screen;
    if let Some(l) = d.ln() {
        l.push(b"screen ");
        l.push(if app.lpm_home() { b"LowPower".as_slice() } else { id(sc).as_bytes() }); // PWR-24.7
        l.push(b" ");
        q(l, sc.name().as_bytes());
        let mut t = Buf::<10>::new();
        sc.title(app, &mut t);
        kq(l, "title", t.as_bytes());
    }
    if let Some(l) = d.ln() {
        l.push(b"state");
        kn(l, "frame", app.frames);
        kn(l, "ago", app.now.wrapping_sub(app.drawn_at));
        kn(l, "dark", app.dark() as u32);
        kn(l, "locked", app.locked() as u32);
        let _ = uwrite!(l, " active={}", letter(app.active) as char);
        keyed_remote_mode(app, cx.live, l);
        kn(l, "fm", app.radio.fm_on as u32);
        kn(l, "leave", app.lpm_leave_progress().unwrap_or(0) as u32); // PWR-24.4: the SIDE1 + SIDE2 hold, thousandths
    }
    // LINK-13.4: receive audio's blocks made, dropped in the radio's queue, and the times one waited for room in the
    // cable's ring (decision 290; once any).
    // TERM-32.1, LINK-13.8 (decision 294): PA2's counts at a rail, the last second's peak and the stream's gain.
    if cx.dev.audio[0] != 0 || cx.dev.audio[3] != 0 {
        if let Some(l) = d.ln() {
            l.push(b"audio");
            kn(l, "made", cx.dev.audio[0]);
            kn(l, "dropped", cx.dev.audio[1]);
            kn(l, "waits", cx.dev.audio[2]);
            kn(l, "clips", cx.dev.audio[3]);
            kn(l, "peak", cx.dev.audio[4]);
            kn(l, "gain", cx.dev.audio[5]);
        }
    }
    // APRS-56: the packet decoder's counts (once it has heard anything): decoded, missed (bad FCS with an AX.25
    // address field), noise, FX.25 not corrected, dropped.
    let dc = cx.dev.decode;
    if dc[..4].iter().chain(&dc[7..]).any(|&n| n != 0) {
        if let Some(l) = d.ln() {
            l.push(b"decode");
            kn(l, "frames", dc[0]);
            kn(l, "missed", dc[1]);
            kn(l, "noise", dc[2]);
            kn(l, "fx25_failed", dc[3]);
            kn(l, "dropped", dc[4]);
            // APRS-56.4: bench builds only (decision 363); a tx build leaves them 0 and they aren't printed
            if dc[5..].iter().any(|&n| n != 0) {
                kn(l, "capture_ms", dc[5]); // PWR-26.9.1: squelch open to capture, last and longest
                kn(l, "capture_max", dc[6]);
                kn(l, "captures", dc[7]); // APRS-56.4: the path's later stages
                kn(l, "heard", dc[8]);
                kn(l, "late", dc[9]); // capture halves the DSP task took too late: audio lost
                kn(l, "wake_max_us", dc[10]); // a half's wait for the DSP task, and the task's work on it (8 ms halves)
                kn(l, "work_max_us", dc[11]);
                kn(l, "radio_max_us", dc[12]); // the executors that preempt it: one run each, longest
                kn(l, "input_max_us", dc[13]);
                kn(l, "long_runs", dc[14]);
                kn(l, "sq_lag_ms", dc[15]); // the last capture's timeline: squelch lag, the tick then, first data
                kn(l, "tick_ms", dc[16]);
                kn(l, "first_data_ms", dc[17]);
            }
        }
    }
    // SAFE-8.2: this start followed a fault (its address) or a hang, and the restarts in a row.
    if cx.dev.restart[0] != 0 {
        if let Some(l) = d.ln() {
            l.push(b"restart");
            if cx.dev.restart[0] == 1 {
                l.push(b" after=hang");
            } else {
                l.push(b" after=fault");
                let _ = uwrite!(l, " pc=0x{:x}", cx.dev.restart[0]);
            }
            kn(l, "in_a_row", cx.dev.restart[1]);
        }
    }
    if let Some(l) = d.ln() {
        let b = app.status_bar(cx.live, cx.dev);
        l.push(b"bar");
        kn(l, "lock", b.locked as u32);
        // KEY-14: what the lock covers, while locked (`lock=` stays 0/1)
        if let Some(m) = app.lock_mode() {
            l.push(match m {
                crate::settings::Lock::Keypad => b" lockmode=keypad".as_slice(),
                crate::settings::Lock::AllButPtt => b" lockmode=allbutptt",
                crate::settings::Lock::All => b" lockmode=all",
            });
        }
        kn(l, "scan", b.scan as u32);
        kn(l, "watch", b.watch as u32);
        kn(l, "inbox", b.inbox as u32);
        kn(l, "alert", b.alert as u32);
        l.push(match b.gps {
            GpsState::Off => b" gps=off".as_slice(),
            GpsState::Searching => b" gps=searching",
            GpsState::Fix => b" gps=fix",
        });
        kn(l, "bt_on", b.bt_on as u32); // SBAR-8: on (a grey icon), and connected (blue)
        kn(l, "bt", b.bt as u32);
        kn(l, "battery", cx.dev.battery_pct as u32);
        kn(l, "low", b.battery_low as u32);
        kn(l, "cutoff", b.battery_cutoff as u32);
        kn(l, "dry", b.dry as u32); // TX-48, SBAR-22
        kn(l, "lpm", b.lpm as u32); // PWR-24, SBAR-24
        kn(l, "vox", b.vox as u32); // SBAR-25
        if b.sat != 0 {
            // SAT-20: the pass alarm's mark: minutes to the rise, or up
            let m = b.sat & ferrite_ui::widgets::SAT_UP;
            if m == ferrite_ui::widgets::SAT_UP {
                kq(l, "sat", b"up");
            } else {
                kn(l, "sat", m as u32);
            }
        }
        // PWR-26.6: the MCU clock (MHz, 0 in the simulators), switches since power on, the longest interrupts-held
        // time (us), and the governor's reasons (clocktree::why bits).
        let m = cx.dev.mcu;
        kn(l, "mcu", m.mhz as u32);
        kn(l, "switches", m.switches);
        kn(l, "held_us", m.held_us);
        kn(l, "why", m.why as u32);
        // SELF-9.1, SELF-9.3: the CPU over the last 10 s, awake at 24 MHz and at the build's speed, the time at 24
        for (k, v) in [("cpu", cx.dev.cpu_pct), ("cpu24", cx.dev.cpu_speeds[0]), ("cpu120", cx.dev.cpu_speeds[1]), ("at24", cx.dev.cpu_speeds[2])] {
            if let Some(v) = v {
                kn(l, k, v as u32);
            }
        }
        match b.clock {
            Some(m) => {
                let _ = uwrite!(l, " clock={}:{}{}", m / 60, (m % 60) / 10, m % 10);
            }
            None => l.push(b" clock=-"),
        }
        kn(l, "utc", b.utc as u32);
        l.push(b" call=");
        q(l, b.call.as_bytes());
    }
    if let Some(t) = app.toast {
        if let Some(l) = d.ln() {
            l.push(if t.center { b"popup ".as_slice() } else { b"toast " });
            q(l, t.head.as_bytes());
            l.push(b" ");
            q(l, t.body.as_bytes());
        }
    }
    if app.qmenu.is_some() {
        for (key, name, v, _, _) in screens::qmenu::lines(app) {
            if let Some(l) = d.ln() {
                l.push(b"qmenu ");
                q(l, key.as_bytes());
                l.push(b" ");
                q(l, name.as_bytes());
                l.push(b" ");
                q(l, v.as_bytes());
            }
        }
    }
    if !d.bare {
        body(app, cx, d);
    }
    if let Some(l) = d.ln() {
        l.push(b"hints");
        for (k, v) in sc.hints(app).as_slice() {
            l.push(b" ");
            q(l, k.as_bytes());
            l.push(b" ");
            q(l, v.as_bytes());
        }
    }
}

/// A list row: `row N "name" "value"` with `sel`, `dot`, `grey` when they apply. N is the number the row shows (0
/// for none).
fn row(d: &mut Desc, num: u8, name: &[u8], value: &[u8], sel: bool, dot: bool, grey: bool) {
    if let Some(l) = d.ln() {
        let _ = uwrite!(l, "row {} ", num);
        q(l, name);
        l.push(b" ");
        q(l, value);
        for (on, w) in [(sel, " sel"), (dot, " dot"), (grey, " grey")] {
            if on {
                l.push(w.as_bytes());
            }
        }
    }
}

/// `help "where" "text"`: the help box.
fn help(d: &mut Desc, at: &[u8], text: &[u8]) {
    if let Some(l) = d.ln() {
        l.push(b"help ");
        q(l, at);
        l.push(b" ");
        q(l, text);
    }
}

/// A label + value page ([`screens::table`]), row `sel` selected.
fn table<S: ChannelStore>(app: &App, cx: &Cx<S>, t: &screens::table::Table<()>, sel: Option<u8>, d: &mut Desc) {
    let ctx = screens::table::Ctx { app, live: cx.live, dev: cx.dev, x: &() };
    for (i, (label, val)) in t.rows.iter().enumerate() {
        let mut b = Buf::<40>::new();
        val(&ctx, &mut b);
        row(d, 0, label.as_bytes(), b.as_bytes(), sel == Some(i as u8), false, false);
    }
}

/// What each screen adds: the same values its `render` draws.
fn body<S: ChannelStore>(app: &App, cx: &Cx<S>, d: &mut Desc) {
    use screens::*;
    match app.screen {
        Screen::Home => {
            for s in 0..SLOTS as u8 {
                card(app, cx, s, d);
                bcast_line(app, cx, s, d);
            }
            aprs_card_line(app, cx, d);
            aprs_sq_line(app, cx, d);
        }
        Screen::Quick(qk) => {
            if quick::fm_rows(app) {
                // BCR-28, BCR-31: FM's own rows, then the saved stations and More
                for (i, (name, ..)) in fm::OPTIONS.iter().enumerate() {
                    let mut v = Buf::<24>::new();
                    let changed = fm::option_text(app, cx.dev, i as u8, &mut v);
                    row(d, i as u8 + 1, name.as_bytes(), v.as_bytes(), i as u8 == qk.row, changed, false);
                }
                let n = fm::OPTIONS.len() as u8;
                row(d, n + 1, b"Saved stations", b">", qk.row == n, false, false);
                row(d, 0, b"More", b">", qk.row > n, false, false);
                return;
            }
            for (i, item) in QUICK.into_iter().enumerate() {
                let v = quick::row_value(app, item);
                if i as u8 == quick::RF_GAIN_ROW {
                    row(d, 10, b"RF gain", app.rf_gain_text(), qk.row == quick::RF_GAIN_ROW, app.rf_gain != Some(0), false);
                    row(d, 0, item.name().as_bytes(), v.as_bytes(), qk.row == quick::MORE_ROW, false, false);
                } else {
                    row(d, i as u8 + 1, item.name().as_bytes(), v.as_bytes(), i as u8 == qk.row, !app.is_default(item), app.greyed(item).is_some());
                }
            }
        }
        Screen::Advanced(a) => {
            if let Some(l) = d.ln() {
                l.push(b"recent");
                for r in app.recent.iter().flatten() {
                    l.push(b" ");
                    q(l, r.label().as_bytes());
                }
                if a.row >= advanced::RECENT {
                    kn(l, "sel", (a.row - advanced::RECENT) as u32 + 1);
                }
            }
            for (i, (name, _)) in advanced::GROUPS.iter().enumerate() {
                let v = advanced::group_value(app, cx.store.count(), cx.dev, i);
                row(d, i as u8 + 1, name.as_bytes(), v.as_bytes(), i as u8 == a.row, false, false);
            }
        }
        Screen::Group(g) => {
            for i in 0..g.items().len() as u8 {
                let (name, v, dot, grey) = g.row(app, cx.dev, cx.store, i);
                row(d, i + 1, name.as_bytes(), v.as_bytes(), i == g.row, dot, grey);
            }
            let (path, h) = g.help(app);
            help(d, path.as_bytes(), h.load().as_bytes());
        }
        Screen::QGrid(g) => {
            for (i, (label, p)) in g.page.cells().iter().enumerate() {
                let (v, dot, grey) = screens::qgrid::QGrid::value(app, cx.dev, p);
                row(d, i as u8 + 1, label.as_bytes(), v.as_bytes(), i as u8 == g.cell, dot, grey);
            }
            let (path, h) = g.help(app);
            help(d, path.as_bytes(), h.load().as_bytes());
        }
        Screen::Options(o) => {
            let all = opts::rows(o.of);
            for (i, (label, act, _)) in all.iter().enumerate() {
                let v: &[u8] = if matches!(act, opts::Act::Open(_)) { b">" } else { b"" };
                row(d, i as u8 + 1, label.as_bytes(), v, i as u8 == o.row, false, false);
            }
            row(d, 0, b"Full menu", b">", false, false, false);
            let (label, _, h) = all[o.row as usize % all.len()];
            let h = crate::text::load(h);
            if o.confirm {
                ask(d, label.as_bytes(), b"?", h.as_bytes());
            } else {
                help(d, opts::title(o.of), h.as_bytes());
            }
        }
        Screen::Status(s) => table(app, cx, &status::PAGE, Some(s.row), d),
        Screen::Link(l) if l.page == link::LinkPage::Status => table(app, cx, &link::STATUS, None, d),
        Screen::Info(i) => {
            let mut b = Buf::<40>::new();
            if let Some((head, text, _)) = info::message(i.page, cx.dev, app.transferring(), &mut b) {
                if let Some(l) = d.ln() {
                    l.push(b"message ");
                    q(l, head.as_bytes());
                    l.push(b" ");
                    q(l, b.as_bytes());
                    l.push(b" ");
                    q(l, text.as_bytes());
                }
            } else if !matches!(i.page, info::Page::Help(_)) {
                let p = info::def(i.page);
                let t = screens::table::Table { rows: p.rows, go: &[], y: 0, h: 0, style: w::KV_STATUS };
                table(app, cx, &t, None, d);
                if !matches!(p.note, info::Note::None) {
                    help(d, p.title.as_bytes(), info::note_text(i.page, p.note).as_bytes()); // the page's note under its rows
                }
            }
        }
        Screen::Value(v) => {
            if let Some(l) = d.ln() {
                let (cur, _) = value::cur(app, v.item);
                let mut t = Buf::<16>::new();
                value::text_of(app, v.item, cur, &mut t);
                l.push(b"value ");
                q(l, v.item.name().as_bytes());
                l.push(b" ");
                q(l, t.as_bytes());
                if let Some(why) = app.greyed(v.item) {
                    kq(l, "grey", why.as_bytes());
                }
            }
        }
        Screen::Confirm(c) => {
            let (title, goes, kept, _) = confirm::words(c.ask);
            if let Some(l) = d.ln() {
                l.push(b"ask ");
                q(l, title.as_bytes());
                kq(l, "goes", goes.map(crate::text::load).as_ref().map_or(&b""[..], |t| t.as_bytes()));
                kq(l, "kept", crate::text::load(kept).as_bytes());
            }
        }
        Screen::Coming(c) => {
            if let Some(l) = d.ln() {
                let mut h = Buf::<32>::new();
                c.dest.heading(&mut h);
                l.push(b"coming ");
                q(l, h.as_bytes());
            }
        }
        Screen::TxRefused(t) => {
            if let Some(l) = d.ln() {
                l.push(b"refused");
                slot_word(l, t.slot);
                kq(l, "why", transmit::short(t.why).as_bytes());
                kn(l, "beeps", app.refusal_beeps as u32); // SND-4: refusal beeps sent since power-on
                if let Some((path, name, _)) = transmit::fix(app, t.why) {
                    kq(l, "fix", path.as_bytes()); // TX-20: the menu path that fixes it, and what it is
                    kq(l, "fixes", name.as_bytes());
                }
            }
        }
        Screen::Transmit(t) => {
            if let Some(l) = d.ln() {
                l.push(b"transmit");
                slot_word(l, t.slot);
                kn(l, "remote", app.remote_keyed() as u32);
                kn(l, "dial", t.dial as u32);
                kn(l, "secs", app.now.wrapping_sub(app.radio.t0) / 1000);
                kn(l, "dry", app.dry_tx() as u32); // TX-48: "Dry TX: no RF" shown
            }
        }
        Screen::Emergency(e) => {
            if let Some(l) = d.ln() {
                l.push(if e.aprs { b"aprs" as &[u8] } else { b"emergency" });
                slot_word(l, e.slot);
            }
        }
        Screen::Entry(e) => {
            // LINK-19.4: what the entry screen draws: the digits, the slot and what they are for, the rows under them,
            // the amber note and the help (the screen's own `Entry::info`, so the two can't differ).
            let info = e.info(app, cx.store, cx.dev);
            if let Some(l) = d.ln() {
                l.push(b"entry ");
                q(l, &e.buf[..e.n as usize % (e.buf.len() + 1)]);
                slot_word(l, app.active);
                kq(l, "what", e.what());
                if !info.note.is_empty() {
                    kq(l, "note", info.note);
                }
            }
            for (label, v, _) in info.rows.iter().filter(|r| !r.0.is_empty()) {
                row(d, 0, label, v.as_bytes(), false, false, false);
            }
            help(d, b"", crate::text::load(e.tip()).as_bytes());
        }
        Screen::Zones(z) => zones_lines(app, cx, z, d),
        Screen::Text(_) => {
            if let Some(l) = d.ln() {
                l.push(b"text ");
                q(l, app.text.as_bytes());
            }
        }
        Screen::Hf(h) => hf_lines(app, cx, h, d),
        Screen::Sat(s) => sat_lines(app, &s, d),
        Screen::Pin(p) => {
            // PIN-2, PIN-3: the heading, a star per digit typed (never the digits), the wait or attempts left
            if let Some(l) = d.ln() {
                let (head, n, note) = p.shown(app);
                l.push(b"pin ");
                q(l, head.as_bytes());
                kq(l, "typed", &b"******"[..(n as usize).min(6)]);
                kq(l, "note", note.as_bytes());
            }
        }
        Screen::ToneSearch(ts) => {
            // TSRCH-2: the tone found ("" while searching), the slot's signal, the question, the last hits
            if let Some(l) = d.ln() {
                let mut t = Buf::<16>::new();
                crate::slot::tone_text(ts.shown(app, cx.live), &mut t);
                l.push(b"tsearch");
                slot_word(l, ts.slot);
                kq(l, "found", t.as_bytes());
                let lv = cx.live.slots[ts.slot as usize];
                kn(l, "open", lv.squelch_open as u32);
                kq(l, "s", w::s_text(lv.rssi).as_bytes());
                kn(l, "ask", ts.ask_tx as u32);
                l.push(b" hits=");
                let mut hits = Buf::<64>::new();
                for h in app.radio.hits.iter().filter(|h| h.what != 0) {
                    crate::slot::tone_text(h.what as u16, &mut t);
                    if !hits.is_empty() {
                        hits.push(b",");
                    }
                    hits.push(t.as_bytes());
                }
                q(l, hits.as_bytes());
            }
        }
        Screen::Counter(c) => {
            // TSRCH-6, TSRCH-11: the frequency (0 while listening) and level shown, live or the last hit's, held
            if let Some(l) = d.ln() {
                let (f, rssi, now) = c.shown(app, cx.live);
                l.push(b"counter freq=");
                mhz(l, f);
                kq(l, "s", if f == 0 { Buf::new() } else { w::s_text(rssi) }.as_bytes());
                kn(l, "meter", if f == 0 { 0 } else { w::meter_level(rssi) as u32 });
                kn(l, "live", now as u32);
                kn(l, "hold", c.hold as u32);
            }
        }
        Screen::Squelch(sq) => {
            // SQL-7, SQL-12: the level, open or closed, the live signal and the noise floor beside it
            if let Some(l) = d.ln() {
                let lv = cx.live.slots[app.active as usize];
                let _ = uwrite!(l, "squelch level={}", app.sq_level(app.active));
                kn(l, "open", lv.squelch_open as u32);
                kq(l, "s", w::s_text(lv.rssi).as_bytes());
                kq(l, "floor", sq.floor_text(lv.rssi, lv.squelch_open).as_bytes());
            }
        }
        Screen::Scope(sc) => {
            if let Some(l) = d.ln() {
                l.push(b"scope centre=");
                mhz(l, app.radio.centre);
                kn(l, "step", scope::step_of(app) as u32 * 10);
                let n = scope::bars_n(app);
                kn(l, "bars", n as u32);
                if let ferrite_core::msg::ModeLive::Scope { bars, n: swept, sweeps, .. } = cx.live.mode {
                    let swept = (swept as usize).min(n);
                    kn(l, "swept", swept as u32);
                    kn(l, "blank", bars[..swept].iter().filter(|&&b| b == 0).count() as u32);
                    kn(l, "sweeps", sweeps as u32);
                }
                l.push(b" marker=");
                mhz(l, scope::bar_freq(app.radio.centre, scope::step_of(app), n, sc.marker as i32));
                kn(l, "listen", sc.listen as u32);
            }
        }
        Screen::Crossband(c) => xb_lines(app, cx, c, d),
        Screen::Setup(s) => {
            let mut rows = [(Buf::<20>::new(), Buf::<32>::new(), false); 10];
            let mut k = 0;
            let (page, of, head, note) = s.shown(app, cx.store.count(), &mut |name, v, sel| {
                if k < rows.len() {
                    rows[k] = (Buf::from(name), Buf::from(v), sel);
                    k += 1;
                }
            });
            if let Some(l) = d.ln() {
                let _ = uwrite!(l, "setup page={} of={}", page, of);
                kq(l, "head", head.as_bytes());
            }
            for (name, v, sel) in &rows[..k] {
                row(d, 0, name.as_bytes(), v.as_bytes(), *sel, false, false);
            }
            if !note.is_empty() {
                help(d, b"summary", note.as_bytes());
            }
        }
        Screen::Compose(c) => {
            if let Some(l) = d.ln() {
                let (to, count, text, field) = c.shown(app, cx.store);
                l.push(b"compose");
                kq(l, "to", to.as_bytes());
                kq(l, "count", count.as_bytes());
                kq(l, "text", text.as_bytes());
                kn(l, "field", field as u32);
            }
            // APRS-38: the address book on an empty recipient, its highlighted row and group
            if c.book_up(cx.store) {
                let data = cx.store.data();
                let b = crate::screens::contacts::Book::new(data);
                let at = (c.quick as u16).min(b.len() - 1);
                if let (Some(l), Some((g, call, sub))) = (d.ln(), b.row(data, at)) {
                    l.push(b"book");
                    kq(l, "group", crate::screens::contacts::GROUPS[g].as_bytes());
                    kq(l, "sel", call.as_bytes());
                    kq(l, "sub", sub.as_bytes());
                    kn(l, "rows", b.len() as u32);
                }
            }
            if let Some(k) = crate::screens::contacts::service(cx.store.data().draft().to.as_bytes()) {
                if let Some(l) = d.ln() {
                    l.push(b"service");
                    kq(l, "format", crate::screens::contacts::SERVICES[k].2.as_bytes());
                }
            }
        }
        Screen::Gps(g) if g.page == gps::Page::Position => {
            let p = gps::position_shown(app, cx.store.data(), cx.dev);
            if let Some(l) = d.ln() {
                l.push(b"gps ");
                q(l, p.status.as_bytes());
                kq(l, "age", p.age.as_bytes());
            }
            if let Some((a, b, tiles)) = &p.fix {
                if let Some(l) = d.ln() {
                    l.push(b"pos ");
                    q(l, a.as_bytes());
                    l.push(b" ");
                    q(l, b.as_bytes());
                }
                for (name, v) in gps::tile_names().zip(tiles) {
                    row(d, 0, name, v.as_bytes(), false, false, false);
                }
                if let Some(l) = d.ln() {
                    l.push(b"near ");
                    match &p.near {
                        Some((name, dist, deg)) => {
                            q(l, name.as_bytes());
                            l.push(b" ");
                            q(l, dist.as_bytes());
                            kn(l, "deg", *deg as u32);
                        }
                        None => l.push(b"\"\" \"None: # marks one here\""),
                    }
                }
            }
        }
        // APRS-53: the Sent page's rows on the page: `row 0 "4 min ago" "by VE3RSB-1"`, `grey` when not heard back
        Screen::AprsSent(st) => {
            let data = cx.store.data();
            st.cur.rows(data.beacons_sent(), screens::lists::ROWS2, &|_| true, |_, i| {
                if let Some(e) = data.beacon_sent(i) {
                    let (ago, by, heard) = aprs::sent_row(&e);
                    row(d, 0, ago.as_bytes(), by.as_bytes(), i == st.cur.sel, false, !heard);
                }
            });
        }
        _ => {}
    }
}

/// HF mode (HF-15): `hf loading=N` while the SSB patch loads; its options page as rows; else `hf freq=MHz
/// band="31 m" rx="SI4732" mode="AM" step="5 kHz" bw="6 kHz" bfo="" signal="SNR 12 dB" entry="" tx=0 seek=0`.
/// Satellite mode (chapter 28a): `sat state=ready done=4 all=4 passes=25`, the list's rows on the page
/// (`row N "ISS" "15:02 72° 11"`, the selected one `sel`; " bell=5" with an alarm), or on a pass `pass "ISS"
/// down=437.8042 up=145.9886 tone=67.0 hd=0 trim=0 tp=1 power=0 uhf_only=0 up_now=1`; the options (SAT-22) as
/// numbered rows, as the options pages give theirs.
fn sat_lines(app: &App, s: &screens::sat::SatScr, d: &mut Desc) {
    use ferrite_core::sat::data::State;
    let sh = screens::sat::shown_list(app, s);
    if let Some(l) = d.ln() {
        l.push(b"sat");
        let st: &[u8] = match sh.state {
            State::Ready => b"ready",
            State::Busy => b"busy",
            State::NoSats => b"no-sats",
            State::NoPosition => b"no-position",
            State::NoClock => b"no-clock",
        };
        kq(l, "state", st);
        kn(l, "done", sh.done as u32);
        kn(l, "all", sh.all as u32);
        kn(l, "passes", sh.rows as u32);
        kn(l, "alarm", app.sat.alarm_lead as u32);
        kn(l, "picker", s.picker as u32);
    }
    // SAT-22: the satellite's options, as the options pages give theirs
    if s.opt != 0 {
        for (i, (name, _)) in screens::sat::OPT_ROWS.iter().enumerate() {
            let mut v = Buf::<16>::new();
            screens::sat::opt_value(app, s.sat, i as u8 + 1, &mut v);
            row(d, i as u8 + 1, name.as_bytes(), v.as_bytes(), i as u8 + 1 == s.opt, false, false);
        }
        row(d, 0, b"Full menu", b">", false, false, false);
        return;
    }
    if screens::sat::page_id(s) == "SatList" || screens::sat::page_id(s) == "SatAlarm" {
        for i in sh.top as usize..(sh.top as usize + 11).min(sh.rows as usize) {
            let (mut n, mut v) = (Buf::<16>::new(), Buf::<32>::new());
            if screens::sat::row_text(app, s, i, &mut n, &mut v) {
                row(d, i as u8 + 1, n.as_bytes(), v.as_bytes(), i == sh.sel as usize, false, false);
            }
        }
        return;
    }
    let Some((p, sat, t, pr)) = app.sat_cur() else { return };
    let now = app.sat_now().unwrap_or(0);
    if let Some(l) = d.ln() {
        l.push(b"pass ");
        q(l, sat.name());
        if let Some((down, up)) = screens::sat::pass_freqs(app) {
            l.push(b" down=");
            mhz(l, down);
            l.push(b" up=");
            mhz(l, up);
        }
        let _ = uwrite!(l, " tone={}.{}", t.tone / 10, t.tone % 10);
        kn(l, "hd", pr.hd as u32);
        let _ = uwrite!(l, " trim={}", pr.trim as i32 * 10);
        kn(l, "tp", pr.tp as u32 + 1);
        kn(l, "power", pr.power as u32);
        kn(l, "uhf_only", pr.uhf_only as u32);
        kn(l, "up_now", p.up(now) as u32);
        kn(l, "max", p.max_el as u32);
    }
}

fn hf_lines<S: ChannelStore>(app: &App, cx: &Cx<S>, h: screens::hf::Hf, d: &mut Desc) {
    let v = h.shown(app, cx.live);
    if let Some(pct) = v.loading {
        if let Some(l) = d.ln() {
            l.push(b"hf");
            kn(l, "loading", pct as u32);
        }
        return;
    }
    if let Some((sel, vals)) = &v.menu {
        for (i, (name, val)) in screens::hf::option_names().zip(vals).enumerate() {
            row(d, i as u8 + 1, name.as_bytes(), val.as_bytes(), i as u8 == *sel, false, false);
        }
        row(d, 0, b"Full menu", b">", false, false, false);
        return;
    }
    if let Some(l) = d.ln() {
        l.push(b"hf freq=");
        mhz(l, app.settings().hf.freq);
        kq(l, "band", v.band.as_bytes());
        kq(l, "rx", v.rx.as_bytes());
        kq(l, "mode", v.mode.as_bytes());
        kq(l, "step", v.step.as_bytes());
        kq(l, "bw", v.bw.as_bytes());
        kq(l, "bfo", v.bfo.as_bytes());
        kq(l, "signal", v.signal.as_bytes());
        kq(l, "entry", v.entry.as_bytes());
        kn(l, "tx", v.tx as u32);
        kn(l, "seek", v.seeking as u32);
    }
}

/// The crossband repeater (XB-2, XB-17): `xb running=0 cell=N`, each side (`row 0 "Side 1" "A 145.49500"`), the
/// grid as rows and XB-2's checks (`check ok|warn|fail "text"`); running, `xb running=1 state=N by=WHO side1=A
/// side2=B pass=S passes=N listen=0|1 ab=N ba=N` (XB-20.6, XB-15, XB-16: the strip's columns each way) and the four
/// cells as rows.
fn xb_lines<S: ChannelStore>(app: &App, cx: &Cx<S>, c: screens::xb::Crossband, d: &mut Desc) {
    use screens::xb;
    if xb::running(app) {
        let x = match cx.live.mode {
            ferrite_core::msg::ModeLive::Xb(x) => x,
            _ => Default::default(),
        };
        let cfg = app.settings().xb;
        if let Some(l) = d.ln() {
            // XB-20.6: who started it; XB-15: SIDE1 listening; XB-16: the strip's columns each way
            l.push(b"xb running=1");
            kn(l, "state", x.state as u32);
            let _ = uwrite!(l, " by={} side1={} side2={}", xb::by_word(app.radio.xb_by), letter(cfg.side1) as char, letter(cfg.side2) as char);
            kn(l, "pass", x.pass_s as u32);
            kn(l, "passes", x.passes as u32);
            kn(l, "listen", (app.radio.xb_by & xb::LISTEN != 0) as u32);
            kn(l, "ab", x.strip.iter().filter(|&&v| v > 0).count() as u32);
            kn(l, "ba", x.strip.iter().filter(|&&v| v < 0).count() as u32);
        }
        for (name, v) in xb::CELLS.iter().zip(xb::cells(&cfg, &x)) {
            row(d, 0, name, v.as_bytes(), false, false, false);
        }
        return;
    }
    let (sides, grid, checks) = xb::start_shown(app, cx.dev.battery_pct);
    if let Some(l) = d.ln() {
        l.push(b"xb running=0");
        kn(l, "cell", c.cell as u32);
    }
    for (i, s) in sides.iter().enumerate() {
        if let Some(l) = d.ln() {
            let _ = uwrite!(l, "row 0 \"Side {}\" \"{} ", i + 1, letter(*s) as char);
            mhz(l, app.slot(*s).freq());
            l.push(b"\"");
            if c.cell == i as u8 {
                l.push(b" sel");
            }
        }
    }
    for (i, (name, v)) in xb::grid_names().iter().zip(&grid).enumerate() {
        row(d, 0, name.as_bytes(), v.as_bytes(), c.cell as usize == 2 + i, false, false);
    }
    for (k, t) in &checks {
        if let Some(l) = d.ln() {
            l.push(["check ok ", "check warn ", "check fail "][*k as usize % 3].as_bytes());
            q(l, t.as_bytes());
        }
    }
}

/// The zones page (CHAN-27 to CHAN-30): `head`, the Nearby row and the zone rows on the page (`row 0 "name" "value"`
/// with `kind=nearby` or `kind=zone` and `chips="A SCAN HID"`), the preview (`preview "NAME" "state"`, a `pvrow
/// "name" "place" "freq"` per channel, `pvfoot "..."`); the zone editor's rows as a list's.
fn zones_lines<S: ChannelStore>(app: &App, cx: &Cx<S>, z: screens::zones::Zones, d: &mut Desc) {
    use screens::zones::*;
    let st = cx.store;
    if let Some(r) = z.edit() {
        for i in 0..EDITOR_ROWS {
            let (label, v, dot) = editor_row(st, z.cur.sel, i);
            row(d, 0, label.as_bytes(), v.as_bytes(), i as u8 == r, dot, false);
        }
        return;
    }
    if let Some(l) = d.ln() {
        l.push(b"head ");
        q(l, head_text(st).as_bytes());
    }
    let near = nearby_now(app, st);
    let on_near = z.on_nearby() && near.is_some();
    if let Some(n) = near {
        if let Some(l) = d.ln() {
            let mut p = Buf::<12>::new();
            crate::screens::lists::place_text(n.m, n.deg, app.settings().units, &mut p);
            l.push(b"row 0 \"Nearby\" ");
            q(l, p.as_bytes());
            if on_near {
                l.push(b" sel");
            }
            l.push(b" kind=nearby");
        }
    }
    let (n, local) = list_n(app, st);
    let keep = keep(st, local);
    let vis = zone_rows(near.is_some());
    let cur = z.cur.shown(n, vis, &keep);
    cur.rows(n, vis, &keep, |_, i| {
        let zone = zid(st, i);
        let r = ZoneRow::of(app, st, zone);
        if let Some(l) = d.ln() {
            l.push(b"row 0 ");
            q(l, r.name.as_bytes());
            let _ = uwrite!(l, " \"{}\"", r.count);
            if !on_near && i == cur.sel {
                l.push(b" sel");
            }
            if r.count == 0 {
                l.push(b" grey");
            }
            l.push(if zone == crate::local::LOCAL { b" kind=local" } else { b" kind=zone" }); // CHAN-34
            let mut c = Buf::<16>::new();
            r.chip_words(&mut c);
            kq(l, "chips", c.as_bytes());
        }
    });
    let p = if on_near {
        preview_nearby(app, st)
    } else if (0..n).any(keep) {
        preview_zone(app, st, zid(st, cur.sel))
    } else {
        return;
    };
    if let Some(l) = d.ln() {
        l.push(b"preview ");
        q(l, p.head.as_bytes());
        l.push(b" ");
        q(l, p.state.as_bytes());
    }
    for r in p.rows.iter().flatten() {
        if let Some(l) = d.ln() {
            l.push(b"pvrow ");
            q(l, r.name.as_bytes());
            l.push(b" ");
            q(l, r.place.as_bytes());
            l.push(b" ");
            q(l, r.freq.as_bytes());
        }
    }
    if let Some(l) = d.ln() {
        l.push(b"pvfoot ");
        q(l, p.foot.as_bytes());
    }
}

/// A question with its options in the hint bar: `ask "title" ...`.
fn ask(d: &mut Desc, what: &[u8], tail: &[u8], text: &[u8]) {
    if let Some(l) = d.ln() {
        l.push(b"ask ");
        let mut t = Buf::<48>::new();
        t.push(what);
        t.push(tail);
        q(l, t.as_bytes());
        kq(l, "text", text);
    }
}

/// ` slot=A` (or HF).
fn slot_word(l: &mut Line, s: u8) {
    if (s as usize) < SLOTS {
        let _ = uwrite!(l, " slot={}", letter(s) as char);
    } else {
        l.push(b" slot=HF");
    }
}

/// Home's card for slot `s`, as [`screens::home`] draws it (HOME-4): the header, the state, the frequency or
/// the channel's name, the chips and the S-meter.
/// APRS-17.4 (decision 313): the large APRS card's rows on their own line while C is the APRS slot: `aprs_card`
/// with `priority=` (beside the state while C shares the VHF chip, APRS-21), `r1=` – `r3=` (the station rows as
/// "call|age|distance"), `unread=` and `from=` (APRS-17.2.4), `fate=` (APRS-17.2.3), `bottom=` (the beacon row),
/// `nofix=1` (APRS-17.2.5). The card line is near its room.
fn aprs_card_line<S: ChannelStore>(app: &App, cx: &Cx<S>, d: &mut Desc) {
    let Some(k) = app.aprs_card(cx.store.data()) else { return };
    let Some(l) = d.ln() else { return };
    l.push(b"aprs_card");
    if let Some((t, _)) = app.aprs_priority() {
        kq(l, "priority", t.as_bytes());
    }
    for (i, r) in k.rows[..k.rows_n as usize].iter().enumerate() {
        let mut b = Buf::<40>::new();
        let _ = uwrite!(b, "{}|{}|{}", screens::lists::Txt(r.call.as_bytes()), screens::lists::Txt(r.age.as_bytes()), screens::lists::Txt(r.right.as_bytes()));
        kq(l, ["r1", "r2", "r3"][i], b.as_bytes());
    }
    if k.unread > 0 {
        kn(l, "unread", k.unread as u32);
        kq(l, "from", k.unread_from.as_bytes());
    }
    if !k.fate.is_empty() {
        kq(l, "fate", k.fate.as_bytes());
    }
    kq(l, "bottom", k.bottom.as_bytes());
    if k.nofix {
        kn(l, "nofix", 1);
    }
}

/// SQL-19.4: `aprs_sq opens=N short=N spikes=N settle=N`, the APRS squelch's counts since power-on, on its own
/// line (the card C line is near Live View's room), while C is the APRS slot.
fn aprs_sq_line<S: ChannelStore>(app: &App, cx: &Cx<S>, d: &mut Desc) {
    if app.aprs_holder().is_none() {
        return;
    }
    let Some(l) = d.ln() else { return };
    let q = cx.live.aprs_sq;
    l.push(b"aprs_sq");
    kn(l, "opens", q.opens as u32);
    kn(l, "short", q.short as u32);
    kn(l, "spikes", q.spikes as u32);
    kn(l, "settle", q.settle as u32);
}

fn card<S: ChannelStore>(app: &App, cx: &Cx<S>, s: u8, d: &mut Desc) {
    let Some(l) = d.ln() else { return };
    let lv = cx.live.slots[s as usize];
    let cfg = *app.slot(s).cfg();
    let _ = uwrite!(l, "card {}", letter(s) as char);
    kn(l, "active", (s == app.active) as u32);
    let mut head = Buf::<32>::new();
    app.head_text(s, cx.live, cx.store, &mut head);
    kq(l, "head", head.as_bytes());
    let state = app.state(s, cx.live).0;
    kq(l, "state", state.as_bytes());
    // TSRCH-13, DEC-6.1: the tone hint or the DTMF heard as the card draws it ("Heard 88.5", "DTMF 101#")
    let mut h = Buf::<24>::new();
    if screens::shared::wide_state(state, &lv, cfg.rx_tone, &mut h) {
        kq(l, "hint", h.as_bytes());
    }
    l.push(b" freq=");
    mhz(l, app.shown_freq(s, cx.live));
    kq(l, "name", app.card_name(s, cx.live, cx.store).as_bytes());
    if lv.hits > 0 {
        kn(l, "hits", lv.hits as u32); // LINK-19.4: the scan's hits, as the card shows them ("4 hits")
    }
    // LINK-19.4, HOME-4.1: the scan's last hit as the value line draws it ("last 119.500" or the channel's name)
    if lv.scanning && lv.hits > 0 && lv.last_hit != app.shown_freq(s, cx.live) {
        let hn = app.hit_name(s, cx.live, cx.store);
        if hn.is_empty() {
            kq(l, "lasthit", w::freq3(lv.last_hit).as_bytes());
        } else {
            kq(l, "lasthit", hn.as_bytes());
        }
    }
    let kind = screens::home::card_kind(app, s);
    if kind == 1 {
        // APRS-17: the APRS card shows no mode, tone or shift chips, but the last heard and the beacon lines and
        // the stations heard; C's own VFO (AM, say) is put aside meanwhile, so its chips would mislead.
        let data = cx.store.data();
        let mut last = Buf::<80>::new();
        match app.aprs_card(data).filter(|k| k.holder == crate::aprs::Holder::Mode) {
            // LINK-19.4: APRS mode's bottom line as drawn (the last heard since power-on, APRS-17.2)
            Some(k) => last.push(k.line.as_bytes()),
            None => crate::aprs::last_text(data, &mut last),
        }
        let mut beacon = Buf::<28>::new();
        app.beacon_text(data, &mut beacon);
        kq(l, "last", last.as_bytes());
        kq(l, "beacon", beacon.as_bytes());
        let (boot, hour, _) = app.heard_counts(data);
        kn(l, "heard", boot as u32); // APRS-17.2: the card's counts, since power-on and in the last hour
        kn(l, "hour", hour as u32);
        let digi = data.beacon().digi;
        if !digi.is_empty() {
            kq(l, "digi", digi.as_bytes()); // APRS-36
        }
        // APRS-17 (decision 181): what holds C, the beacon power, the beacon's countdown "m:ss" or why there is
        // none (`next`, only while automatic beacons are on); KISS's port and counts; beacons' bottom line.
        // Short, as the line holds 256 bytes and `last` may take 80.
        if let Some(k) = app.aprs_card(data) {
            l.push(b" holder=");
            l.push(k.holder.word().as_bytes());
            kq(l, "power", k.power.as_bytes());
            if !k.next.is_empty() {
                kq(l, "next", k.next.as_bytes());
            }
            match k.holder {
                crate::aprs::Holder::Kiss => {
                    let (sent, heard) = data.kiss_counts();
                    kq(l, "port", k.right.as_bytes());
                    kn(l, "sent", sent as u32);
                    kn(l, "frames", heard as u32);
                }
                crate::aprs::Holder::Beacon => kq(l, "line", k.line.as_bytes()),
                // LINK-19.4: which count the small card shows now (APRS-17.2: "14 heard", 1 "3 /hr")
                crate::aprs::Holder::Mode => kn(l, "turn", 0), // APRS-17.2.1: the small card no longer flips
            }
            if app.aprs_shared().is_some() {
                // 1 APRS first, 2 A·B first (APRS-21, WATCH-17.3; the header says which, `aprs_card priority=`)
                kn(l, "shared", 1 + (app.settings().aprs_share != 0) as u32);
            }
        }
    }
    let (mut m, mut t, mut o) = (Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new());
    mode_chip(&cfg, &mut m);
    card_tone(&cfg, &mut t);
    shift_chip(&cfg, &mut o);
    if kind != 1 {
        kq(l, "mode", m.as_bytes());
        kq(l, "tone", t.as_bytes());
        kq(l, "shift", o.as_bytes());
        // HOME-4.2: the chip as drawn, "No TX" where PTT can never transmit
        kq(l, "power", crate::screens::transmit::power_name(app, s).as_bytes());
    }
    // SQL-12.3: the card's signal as drawn, S-units or dBm (0 7 3 1)
    if app.set.smeter_dbm {
        kq(l, "s", w::dbm_text(lv.rssi).as_bytes());
    } else {
        kq(l, "s", w::s_text(lv.rssi).as_bytes());
    }
    kn(l, "meter", w::meter_level(lv.rssi) as u32);
    kn(l, "rx", lv.squelch_open as u32);
    kn(l, "scan", lv.scanning as u32);
    l.push(match kind {
        1 => b" kind=aprs".as_slice(),
        2 => b" kind=broadcast",
        _ => b" kind=slot",
    });
}

/// A broadcast card's report after its `card` line (BCR-26): `bcast A "RDS text" stereo=1 dbuv=48 snr=24` (the
/// text empty: "No RDS"; while another slot plays it, where it plays).
fn bcast_line<S: ChannelStore>(app: &App, cx: &Cx<S>, s: u8, d: &mut Desc) {
    if screens::home::card_kind(app, s) != 2 {
        return;
    }
    let Some(l) = d.ln() else { return };
    let (stereo, rssi, snr, text, _, _) = screens::home::bcast_shown(app, cx.live, s);
    let _ = uwrite!(l, "bcast {} ", letter(s) as char);
    q(l, text.as_bytes());
    kn(l, "stereo", stereo as u32);
    kn(l, "dbuv", rssi as u32);
    kn(l, "snr", snr as u32);
}

/// The screen's stable id: its [`Screen`] variant.
pub fn id(s: Screen) -> &'static str {
    match s {
        Screen::Home => "Home",
        Screen::Edit(_) => "Edit",
        Screen::QGrid(_) => "AprsQuick",
        Screen::Quick(_) => "Quick",
        Screen::Snake(_) => "Snake",
        Screen::Activation(_) => "Activation",
        Screen::Advanced(_) => "More",
        Screen::Status(_) => "Status",
        Screen::Entry(_) => "Entry",
        Screen::ChEdit(_) => "ChEdit",
        Screen::Value(_) => "Value",
        Screen::Tones(_) => "Tones",
        Screen::Text(_) => "Text",
        Screen::Save(_) => "Save",
        Screen::Confirm(_) => "Confirm",
        Screen::Shortcut(_) => "Shortcut",
        Screen::Transmit(_) => "Transmit",
        Screen::TxRefused(_) => "TxRefused",
        Screen::Emergency(_) => "Emergency",
        Screen::Scan(_) => "Scan",
        Screen::Scope(_) => "Scope",
        Screen::ToneSearch(_) => "ToneSearch",
        Screen::Counter(_) => "Counter",
        Screen::Crossband(_) => "Crossband",
        Screen::Squelch(_) => "Squelch",
        Screen::Group(_) => "Group",
        Screen::Setup(_) => "Setup",
        Screen::ExtendTx(_) => "ExtendTx",
        Screen::Pin(_) => "Pin",
        Screen::Info(_) => "Info",
        Screen::KeyTest(_) => "KeyTest",
        Screen::Link(_) => "Link",
        Screen::Hf(_) => "Hf",
        Screen::Alert(_) => "Alert",
        Screen::Coming(_) => "Coming",
        Screen::Log(_) => "Log",
        Screen::Options(_) => "Options",
        Screen::Aprs(_) => "Aprs",
        Screen::AprsSent(_) => "AprsSent",
        Screen::Msgs(_) => "Msgs",
        Screen::Compose(_) => "Compose",
        Screen::QuickMsgs(_) => "QuickMsgs",
        Screen::Contacts(_) => "Contacts",
        Screen::Gps(_) => "Gps",
        Screen::Browse(_) => "Browse",
        Screen::Zones(_) => "Zones",
        Screen::ScanLists(_) => "ScanLists",
        Screen::Recall(_) => "Recall",
        Screen::PosEntry(_) => "PosEntry",
        Screen::SameCodes(_) => "SameCodes",
        Screen::AutoReply(_) => "AutoReply",
        Screen::Dtmf(_) => "Dtmf",
        Screen::Sat(s) => screens::sat::page_id(&s),
    }
}

// ---- Key names (the terminal's `key`, `ui keys`; tools/ferrite_link.py has the same) ----

/// A key's name: MENU (OK too), BACK, UP, DOWN, LEFT, RIGHT, SLOT, VM, 0-9, STAR, HASH, SIDE1, SIDE2.
pub fn key_name(code: u8) -> &'static str {
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match k::key(code) {
        Some(Key::Menu) => "MENU",
        Some(Key::Exit) => "BACK",
        Some(Key::Up) => "UP",
        Some(Key::Down) => "DOWN",
        Some(Key::Left) => "LEFT",
        Some(Key::Right) => "RIGHT",
        Some(Key::Slot) => "SLOT",
        Some(Key::VfoMem) => "VM",
        Some(Key::Digit(d)) => DIGITS[d as usize % 10],
        Some(Key::Star) => "STAR",
        Some(Key::Hash) => "HASH",
        Some(Key::Side(1)) => "SIDE1",
        Some(Key::Side(_)) => "SIDE2",
        Some(Key::Ptt(_)) | None => "",
    }
}

/// Other names a key answers to.
const ALIASES: [(&str, u8); 8] = [("OK", 6), ("EXIT", 9), ("*", 13), ("#", 21), ("PF1", 0), ("PF2", 1), ("ABC", 3), ("V/M", 2)];

/// A key's stock code by name, any case.
pub fn key_code(name: &[u8]) -> Option<u8> {
    let is = |n: &str| n.as_bytes().eq_ignore_ascii_case(name);
    (0..=ferrite_core::link::key::LAST_CODE).find(|&c| is(key_name(c))).or_else(|| ALIASES.iter().find(|(n, _)| is(n)).map(|a| a.1))
}

/// `ui keys`, line `i`: each key's name, its stock code and its other names; then the knob.
pub fn key_line(i: u16, out: &mut Line) -> bool {
    out.clear();
    let codes = ferrite_core::link::key::LAST_CODE as u16 + 1;
    if i == codes {
        out.push(b"knob +N|-N (1-20 steps; + clockwise)");
        return true;
    }
    if i > codes {
        return false;
    }
    let c = i as u8;
    let _ = uwrite!(out, "{} {}", key_name(c), c);
    for (n, a) in ALIASES {
        if a == c {
            out.push(b" ");
            out.push(n.as_bytes());
        }
    }
    true
}

/// LINK-19.6: line `i` of what the radio knows about itself (`ui radio`, the link's `UI` what=4), for Ferrite
/// Console's debug info: the build, the MCU, the SPI flash and the store's mount, calibration, the chips, what is
/// stored, power, GPS, Bluetooth and the last restart. No callsign, position or other personal data. False past the
/// last line.
pub fn radio_line<S: ChannelStore>(app: &App, st: &S, dev: &Device, i: u16, l: &mut Line) -> bool {
    use ferrite_core::msg::{CalFault, CalState};
    let t = &dev.selftest;
    l.clear();
    match i {
        0 => {
            l.push(b"firmware version=");
            l.push(ferrite_core::VERSION.as_bytes());
            kn(l, "tx", dev.tx_built as u32);
            kn(l, "bench", dev.bench as u32);
            kn(l, "dry", app.dry_tx() as u32);
            kn(l, "region", app.set.region as u32);
        }
        1 => {
            let _ = uwrite!(l, "mcu idcode=0x{:x}", t.idcode);
            kn(l, "ok", t.mcu_ok as u32);
            l.push(b" uid=");
            q(l, &ferrite_core::uid::text(t.uid));
            kn(l, "mhz", dev.mcu.mhz as u32);
            if let Some(c) = dev.mcu_temp {
                let _ = uwrite!(l, " temp={}", c);
            }
            kn(l, "stack_free", dev.stack_free);
            kn(l, "ram_statics", dev.ram_statics);
        }
        2 => {
            let _ = uwrite!(l, "flash id=0x{:x}", t.flash_id);
            kn(l, "mb", t.flash_mb as u32);
            l.push(match t.store {
                1 => b" store=mounted".as_slice(),
                2 => b" store=first_boot",
                3 => b" store=FAILED",
                4 => b" store=other_layout",
                _ => b" store=unknown",
            });
            let own = ferrite_core::layout::LAYOUT_VERSION as u32;
            kn(l, "layout", own);
            kn(l, "found", match st.data().layout_found() {
                0 => own,
                v => v as u32,
            });
        }
        3 => {
            l.push(match t.cal {
                CalState::Unknown => b"cal state=unknown".as_slice(),
                CalState::Ok => b"cal state=ok",
                CalState::Blank => b"cal state=blank",
                CalState::Invalid => b"cal state=invalid",
                CalState::CrcMismatch => b"cal state=crc_mismatch",
            });
            l.push(match t.cal_fault {
                CalFault::None => b" fault=none".as_slice(),
                CalFault::PaCurve => b" fault=pa_curve",
                CalFault::LevelOrder => b" fault=level_order",
                CalFault::Windows => b" fault=windows",
                CalFault::Trim => b" fault=trim",
                CalFault::ToneGain => b" fault=tone_gain",
                CalFault::Unchecked => b" fault=unchecked",
            });
            let _ = uwrite!(l, " at={},{},{}", t.cal_at[0], t.cal_at[1], t.cal_at[2]);
            let _ = uwrite!(l, " battery_trim={}", t.battery_trim);
        }
        4 => {
            let _ = uwrite!(l, "chips bk4829=0x{:x}", t.chips);
            kn(l, "si4732", t.si4732 as u32);
            l.push(b" si_rev=");
            let rev = t.si_rev.map(|c| if c.is_ascii_graphic() { c } else { b'.' });
            q(l, &rev);
            kn(l, "ssb_patch", t.ssb_patch as u32);
        }
        5 => {
            l.push(b"data");
            kn(l, "channels", st.count() as u32);
            kn(l, "zones", st.zones() as u32);
            kn(l, "presets", st.data().presets() as u32);
            kn(l, "lists_gen", crate::link::lists_gen(st));
        }
        6 => {
            l.push(b"power");
            kn(l, "mv", dev.battery_mv as u32);
            kn(l, "pct", dev.battery_pct as u32);
            kn(l, "low", dev.battery_low as u32);
            kn(l, "cutoff", dev.battery_cutoff as u32);
            kn(l, "spkmic", dev.spkmic as u32);
        }
        7 => {
            l.push(match dev.gps {
                GpsState::Off => b"gps state=off".as_slice(),
                GpsState::Searching => b"gps state=searching",
                GpsState::Fix => b"gps state=fix",
            });
            kn(l, "sats", dev.sats as u32);
            kn(l, "clock", dev.clock_min.is_some() as u32);
            kn(l, "bt_on", app.bt_on() as u32);
            kn(l, "bt", dev.bt as u32);
        }
        8 => {
            l.push(b"restart");
            l.push(match dev.restart[0] {
                0 => b" after=start".as_slice(),
                1 => b" after=hang",
                _ => b" after=fault",
            });
            if dev.restart[0] > 1 {
                let _ = uwrite!(l, " pc=0x{:x}", dev.restart[0]);
            }
            kn(l, "in_a_row", dev.restart[1]);
        }
        _ => return false,
    }
    true
}
