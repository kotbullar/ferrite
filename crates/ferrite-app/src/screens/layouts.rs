//! Home's other two layouts (manual chapter 6, HOME-3; HomeB and HomeC mock-ups), chosen with MENU 0 7 2. The
//! keys are home's in every layout (HOME-3, NAV-6); only the drawing differs.
//!
//! - Single + scope: the active slot with its settings, S-meter and a scope strip around it; the other slots
//!   one line each.
//! - Watch + activity: three equal cards, each with its last minute of activity (`RadioUi::act`).

use super::activity::ACT_BUCKETS;
use super::scope;
use super::shared::*;
use super::Cx;
use crate::chan::ChannelStore;
use crate::slot::{band_name, card_tone, mode_chip, shift_text, Item, STEPS};
use crate::App;
use ferrite_core::msg::{LiveAll, ModeLive, RadioCmd, SCOPE_BARS, SLOTS};
use ferrite_ui::assets::*;
use ferrite_ui::canvas;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;

/// The strip's step: 4 kHz, so 56 bars cover about ±112 kHz.
pub const STRIP_STEP: u16 = 400;

/// The strip's start (MODE-2, from [`App::reconcile_mode`]): stock's 10 ms a point (dwell is in 0.1 ms; at 1 ms
/// the strip starved the UI on the radio, Oct 12, 2026) around the active frequency.
pub(crate) fn send_strip(app: &mut App, f: ferrite_core::freq::Freq10) {
    app.out.push(RadioCmd::ScopeCfg { bars: scope::STRIP_BARS as u8, dwell: 100, strip: true, hf: false });
    app.out.push(RadioCmd::Sweep { centre: f, step: STRIP_STEP, on: true });
}

/// Single + scope (HomeB).
pub fn single(app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
    let live = cx.live;
    let a = app.active;
    let lv = live.slots[a as usize];
    let f = app.shown_freq(a, live);
    let (state, sc) = app.state(a, live);
    // TSRCH-13: a tone hint takes the state's place, in a wider field; so does a long state (BCR-15).
    let mut ht = Buf::<24>::new();
    if wide_state(state, &lv, app.slots[a as usize].cfg().rx_tone, &mut ht) {
    } else if state.len() > 8 {
        ht.set(state.as_bytes());
    }
    let mut h = Buf::<32>::new();
    app.head_text(a, live, cx.store, &mut h);
    if app.dirty(1, Mix::new().h(&a).h(&h).h(&state).h(&ht).get()) {
        w::badge(lcd, 8, 25, a, true, BG);
        let ww = (text4_width(ht.as_bytes(), &UI_12) + 2).max(HEARD_W);
        // HOME-4.3: the header takes what the state leaves (a 16-character zone name fits), at least 140 px
        let hw = if ht.is_empty() { (232 - 6 - super::home::head_right_w(true, state, &Buf::new()) - 26).max(140) } else { 232 - ww - 26 };
        w::field(lcd, 26, hw, 22, 40, 36, w::fit(h.as_bytes(), &UI_12, hw), const { &w::st(&UI_12, SUB, BG, Align::Left) });
        canvas::banded(lcd, 26 + hw, 22, 240 - 26 - hw, 20, BG, |lcd| {
            if !ht.is_empty() {
                w::field(lcd, 232 - ww, ww, 22, 40, 36, ht.as_bytes(), &w::st(&UI_12, sc, BG, Align::Right));
            } else if state == "RX muted" {
                // HOME-15.3: the RX tag, "Muted" left of it
                w::tag(lcd, 232, 24, b"RX", GREEN, BG);
                // to x 204, 4 px short of the tag: with the header at its 140 px least, to 200 cut "Muted" (35 px in 34)
                w::field(lcd, 26 + hw, 204 - 26 - hw, 22, 40, 36, b"Muted", &w::st(&UI_12, sc, BG, Align::Right));
            } else if matches!(state, "RX" | "TX" | "MON" | "FM") {
                w::tag(lcd, 232, 24, state.as_bytes(), sc, BG);
            } else if state != "idle" {
                w::field(lcd, 26 + hw, 206 - hw, 22, 40, 36, state.as_bytes(), &w::st(&UI_12, sc, BG, Align::Right));
            }
        });
    }
    // Channel mode: the name large, the frequency small under it (HOME-4).
    let name = app.card_name(a, live, cx.store);
    if let Some(clear) = app.dirty_shape(2, Mix::new().h(&f).h(&name).get(), name.is_empty()) {
        redraw_box!(clear, lcd, (8, 42, 224, 52, BG), {
            if name.is_empty() {
                w::freq_centred(lcd, 8, 232, 88, f, &DIG_46, &DIG_28, FREQ, SUB, BG); // HOME-3: centred
            } else {
                // 26 px when the whole name fits, else 22 px (HOME-4.3).
                let nb = name.as_bytes();
                let (font, base) = if text4_width(nb, &NAME_26) <= 224 { (&NAME_26, 70) } else { (&NAME_22, 68) };
                w::field(lcd, 8, 224, 43, 76, base, w::fit(nb, font, 224), &w::st(font, TEXT, BG, Align::Left));
                w::freq(lcd, 8, 232, 92, f, &DIG_17, &DIG_17, SUB, SUB, BG);
            }
        });
    }
    // The settings, label over value (HOME-4): mode, tone, power; step, offset, squelch.
    let cfg = *app.slot(a).cfg();
    let mut v = [Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new()];
    mode_chip(&cfg, &mut v[0]);
    card_tone(&cfg, &mut v[1]);
    if v[1].is_empty() {
        v[1].set(b"None");
    }
    v[2].set(super::transmit::power_name(app, a).as_bytes());
    let st = STEPS[cfg.step as usize % STEPS.len()].1;
    match st.strip_suffix(" kHz") {
        Some(k) => {
            v[3].set(k.as_bytes());
            v[3].push(b"k");
        }
        None => v[3].set(st.as_bytes()),
    }
    shift_text(&cfg, false, &mut v[4]);
    if v[4].is_empty() {
        v[4].set(b"Simplex");
    }
    let mut sq = Buf::<16>::new();
    app.item_text(Item::Squelch, false, &mut sq);
    v[5] = sq;
    const LABELS: [&str; 6] = ["Mode", "Tone", "Power", "Step", "Offset", "Squelch"];
    let bcast = super::home::card_kind(app, a) == 2;
    // BCR-26: the broadcast slot's report (signal in dBuV, SNR, stereo pilot, RDS) while it plays.
    let (ps, rt, rssi, snr, stereo) = match live.mode {
        ModeLive::Fm { ps, rt, rssi, snr, stereo, .. } if bcast && app.bcast_slot() == Some(a) && app.radio.fm_on => (ps, rt, rssi, snr, stereo),
        _ => ([b' '; 8], [0; 64], 0, 0, false),
    };
    if bcast {
        // The signal and SNR with the stereo pilot under the frequency, as the cards and the FM screen show them;
        // then the RDS text wrapped in the settings' place (three lines hold the 64 characters).
        let mut sig = Buf::<24>::new();
        if rssi > 0 {
            let _ = ufmt::uwrite!(sig, "{} dBuV \u{b7} SNR {} dB", rssi, snr);
        }
        if app.dirty(4, Mix::new().h(&sig).h(&stereo).h(&2u8).get()) {
            canvas::banded(lcd, 8, 96, 224, 18, BG, |lcd| {
                w::field(lcd, 8, 160, 96, 114, 110, sig.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
                if stereo {
                    w::tag(lcd, 232, 97, b"STEREO", SUB, BG);
                }
            });
        }
        let text = crate::bcast::rds_text(&ps, &rt);
        if app.dirty(3, Mix::new().h(&text).h(&2u8).get()) {
            canvas::banded(lcd, 8, 114, 224, 52, BG, |lcd| {
                let t: &[u8] = if text.is_empty() { b"No RDS" } else { text };
                w::wrap(lcd, 8, 224, 114, 166, 128, 15, t, &UI_12, if text.is_empty() { DIM } else { TEXT }, BG);
            });
        }
        for i in 5..9 {
            app.dirty(i, Mix::new().h(&2u8).get()); // drawn again when the slot leaves the band
        }
    }
    if app.dirty(13, Mix::new().h(&bcast).get()) && !bcast {
        lcd.fill(8, 96, 224, 70, BG); // the broadcast's rows gone whole, gaps between the fields too
    }
    for (i, l) in LABELS.iter().enumerate() {
        let (x, y) = (8 + (i as i32 % 3) * 76, 96 + (i as i32 / 3) * 36);
        if !bcast && app.dirty(3 + i, Mix::new().h(&v[i]).get()) {
            w::field(lcd, x, 74, y, y + 16, y + 12, l.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
            w::field(lcd, x, 74, y + 16, y + 34, y + 30, v[i].as_bytes(), const { &w::st(&UI_12B, TEXT, BG, Align::Left) });
        }
    }
    // S-meter with its scale. A broadcast slot's is the SI4732's signal on the FM screen's scale (0-70 dBuV, as
    // the cards), green while it plays (BCR-26); the BK4829's RSSI means nothing there.
    let (lit, on) = if bcast {
        ((rssi as u32 * w::METER_SEGS as u32 / 70).min(w::METER_SEGS as u32) as u8, app.bcast_heard(live) && app.bcast_slot() == Some(a))
    } else {
        (w::meter_level(lv.rssi), lv.squelch_open)
    };
    let col = if on { GREEN } else { DIM };
    // One meter, the active slot's: what another slot's left there is drawn over whole (a slot change).
    for o in (0..SLOTS).filter(|&o| o != a as usize) {
        app.drawn.meter[o] = None;
    }
    // SQL-12.3: 0 7 3 1 dBm shows the level as a number over the meter and its scale.
    let dbm = app.set.smeter_dbm && !bcast;
    if dbm {
        app.drawn.meter[a as usize] = None; // the bars come back whole
        let t = w::dbm_text(lv.rssi);
        if app.dirty(9, Mix::new().h(&t).h(&on).h(&true).get()) {
            w::field(lcd, 8, 226, 166, 196, 188, t.as_bytes(), &w::st(&UI_15B, if on { GREEN } else { TEXT }, BG, Align::Left));
        }
    }
    if !dbm {
        let old = app.drawn.meter[a as usize].filter(|&(_, o)| o == on).map(|(l, _)| l);
        if app.drawn.meter[a as usize] != Some((lit, on)) {
            if old.is_none() {
                canvas::banded(lcd, 8, 166, 226, 4, BG, |_| {}); // what the dBm number left above the bars
            }
            w::meter(lcd, 8, 170, old, lit, col, BG);
            app.drawn.meter[a as usize] = Some((lit, on));
        }
        let s = if bcast { Buf::new() } else { w::s_text(lv.rssi) };
        if app.dirty(9, Mix::new().h(&s).h(&bcast).get()) {
            canvas::banded(lcd, 8, 178, 226, 18, BG, |lcd| {
                if bcast {
                    for (i, t) in ["0", "10", "20", "30", "40", "50", "60"].iter().enumerate() {
                        lcd.text4_rows(8 + i as i32 * 200 / 7, 192, t.as_bytes(), &UI_12, SUB, BG, 180, 196); // dBuV
                    }
                } else {
                    for (i, t) in ["1", "3", "5", "7", "9", "+20", "+40"].iter().enumerate() {
                        let x = 8 + [0, 20, 40, 60, 80, 120, 160][i];
                        lcd.text4_rows(x, 192, t.as_bytes(), &UI_12, SUB, BG, 180, 196);
                    }
                    w::field(lcd, 208, 26, 178, 196, 192, s.as_bytes(), const { &w::st(&UI_12B, TEXT, BG, Align::Right) });
                }
            });
        }
    }
    // The scope strip (SCOPE strip from the sweep the tick keeps running).
    let (centre, bars) = match live.mode {
        ModeLive::Scope { centre, bars, .. } => (centre, bars),
        _ => (f, [0; SCOPE_BARS]),
    };
    // HOME-13: the strip and the other slots' lines wait while the knob turns.
    if !app.held(10) {
        if bcast || app.strip_blocked() {
            scope::strip_off(app, lcd, 10, 200, 34); // HOME-14: the broadcast slot is active (or needs the relay)
        } else {
            scope::strip(app, lcd, 10, 200, 34, centre, STRIP_STEP, &bars[..scope::STRIP_BARS]);
        }
    }
    // The other slots, a line each.
    for (i, o) in (0..SLOTS as u8).filter(|&o| o != a).enumerate() {
        let (st, c) = app.state(o, live);
        let sl = app.slot(o);
        let mut t = Buf::<40>::new();
        t.push(w::freq3(app.shown_freq(o, live)).as_bytes());
        t.push(" \u{b7} ".as_bytes());
        match sl.ch.filter(|_| sl.on_channel()) {
            Some(ch) => t.push(ch.name()),
            None => t.push(band_name(sl.freq()).as_bytes()),
        }
        let y = 256 + 21 * i as i32;
        let st = if st.len() > 9 { "relay" } else { st }; // "Watch off: relay" (BCR-15) in the line's room
        if !app.held(11 + i) && app.dirty(11 + i, Mix::new().h(&o).h(&t).h(&st).get()) {
            w::badge(lcd, 8, y + 2, o, false, BG);
            // a long channel name cut on purpose, to the room the state leaves
            w::field(lcd, 28, 140, y, y + 18, y + 14, w::fit(t.as_bytes(), &UI_12, 140), const { &w::st(&UI_12, TEXT, BG, Align::Left) });
            w::field(lcd, 168, 64, y, y + 18, y + 14, st.as_bytes(), &w::st(&UI_12, c, BG, Align::Right)); // "scanning" whole
        }
    }
}

/// Watch + activity (HomeC): three equal cards, each with its last minute.
pub fn activity(app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
    let live: &LiveAll = cx.live;
    for s in 0..SLOTS as u8 {
        let p = s as usize;
        let (y, h) = (25 + 82 * s as i32, 76);
        let lv = live.slots[p];
        let (state, sc) = app.state(s, live);
        let active = s == app.active;
        // HOME-13: while the knob turns only the active card follows, and its last minute waits too.
        if !active && app.held(R_FRAME + p) {
            continue;
        }
        let border = if active { BLUE } else if matches!(state, "RX" | "RX muted") { GREEN } else { LINE }; // HOME-15.3
        let geom = Mix::new().h(&s).h(&2u8).get() | 1;
        let drawn_before = app.drawn.hash[R_FRAME + p] != 0;
        if !app.dirty(R_FRAME + p, Mix::new().h(&s).h(&border).get()) {
            // unchanged
        } else if drawn_before && app.drawn.card[p] == geom {
            w::card_border(lcd, 6, y, 228, h, border, active, BG); // the border's colour alone (DISP-25)
        } else {
            app.drawn.card[p] = geom;
            lcd.fill(6, y, 228, h, BG);
            lcd.fill_r(6, y, 228, h, 3, SURFACE);
            w::card_border(lcd, 6, y, 228, h, border, active, BG);
            for r in [R_HEAD, R_FREQ, R_INFO, R_SUB] {
                app.drawn.hash[r + p] = 0;
            }
            app.drawn.hash[16 + p] = 0;
        }
        let mut head = Buf::<32>::new();
        app.head_text(s, live, cx.store, &mut head);
        let mut stt = Buf::<24>::new();
        stt.set(state.as_bytes());
        if state == "RX" {
            stt.push(b" ");
            stt.push(w::s_text(lv.rssi).as_bytes());
        } else {
            wide_state(state, &lv, app.slots[s as usize].cfg().rx_tone, &mut stt); // TSRCH-13, DEC-6.1: in a wider field
        }
        if app.dirty(R_HEAD + p, Mix::new().h(&head).h(&stt).h(&active).get()) {
            w::badge(lcd, 14, y + 5, s, active, BG);
            let f = if state == "RX" { &UI_12B } else { &UI_12 };
            // HOME-4.3: the state's own width (at least 40 px), so a zone name in the header has the rest
            let sw = if state == "heard" || state == "DTMF" || state.len() > 8 { (text4_width(stt.as_bytes(), &UI_12) + 2).max(HEARD_W) } else { (text4_width(stt.as_bytes(), f) + 6).clamp(40, 72) };
            w::field(lcd, 34, 192 - sw, y + 4, y + 21, y + 17, w::fit(head.as_bytes(), &UI_12, 192 - sw), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
            w::field(lcd, 226 - sw, sw, y + 4, y + 21, y + 17, stt.as_bytes(), &w::st(f, sc, SURFACE, Align::Right));
        }
        let f = app.shown_freq(s, live);
        // Channel mode: the name at the left, the frequency small at the right (HOME-4).
        let name = app.card_name(s, live, cx.store);
        let sl = *app.slot(s);
        let mut right = Buf::<16>::new();
        if lv.scanning {
            let _ = ufmt::uwrite!(right, "{} hits", lv.hits);
        } else if !name.is_empty() {
            right.set(w::freq3(f).as_bytes());
        }
        let (x, c, fo) = if name.is_empty() { (140, TEXT, &UI_12B) } else { (170, SUB, &UI_12) };
        let info = app.dirty(R_INFO + p, Mix::new().h(&right).get());
        if app.dirty(R_FREQ + p, Mix::new().h(&f).h(&name).get()) {
            // Off-screen, sent once with the text at its right, which shares the rows (DISP-25).
            canvas::banded(lcd, 15, y + 22, 211, 24, SURFACE, |b| {
                if name.is_empty() {
                    w::freq(b, 15, 140, y + 44, f, &DIG_26, &DIG_17, TEXT, SUB, SURFACE);
                } else {
                    let nb = name.as_bytes();
                    let font = if text4_width(nb, &NAME_22) <= 150 { &NAME_22 } else { &UI_15B };
                    w::field(b, 15, 150, y + 22, y + 46, y + 42, w::fit(nb, font, 150), &w::st(font, TEXT, SURFACE, Align::Left));
                }
                w::field(b, x, 226 - x, y + 26, y + 44, y + 42, right.as_bytes(), &w::st(fo, c, SURFACE, Align::Right));
            });
        } else if info {
            w::field(lcd, x, 226 - x, y + 26, y + 44, y + 42, right.as_bytes(), &w::st(fo, c, SURFACE, Align::Right));
        }
        let cfg = *sl.cfg();
        // The value line in fixed columns (HOME-4): mode, tone, power pinned right; a scan's mode and last hit.
        let mut cols = [Buf::<16>::new(), Buf::<16>::new(), Buf::<16>::new()];
        if lv.scanning {
            cols[0].set(crate::slot::MODES[cfg.mode as usize % crate::slot::MODES.len()].as_bytes());
            if lv.hits > 0 && lv.last_hit != f {
                cols[1].set(b"last ");
                let hn = app.hit_name(s, live, cx.store); // HOME-4.1: a channel scan's hit by name
                if hn.is_empty() {
                    cols[1].push(w::freq3(lv.last_hit).as_bytes());
                } else {
                    cols[1].push(hn.as_bytes());
                }
            }
        } else if super::home::card_kind(app, s) == 2 {
            // BCR-26: FM, as much of the RDS text as fits, the stereo pilot
            cols[0].set(b"FM");
            if let ModeLive::Fm { ps, rt, stereo, .. } = live.mode {
                if app.bcast_slot() == Some(s) && app.radio.fm_on {
                    cols[1].set(w::fit(crate::bcast::rds_text(&ps, &rt), &UI_12, 104));
                    cols[2].set(if stereo { b"STEREO".as_slice() } else { b"" });
                }
            }
        } else {
            mode_chip(&cfg, &mut cols[0]);
            card_tone(&cfg, &mut cols[1]);
            cols[2].set(super::transmit::power_name(app, s).as_bytes());
        }
        if app.dirty(R_SUB + p, Mix::new().h(&cols).get()) {
            canvas::banded(lcd, 15, y + 46, 211, 15, SURFACE, |b| {
                w::field(b, 15, 36, y + 46, y + 61, y + 58, cols[0].as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
                w::field(b, 51, 120, y + 46, y + 61, y + 58, cols[1].as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Left) });
                w::field(b, 156, 70, y + 46, y + 61, y + 58, cols[2].as_bytes(), const { &w::st(&UI_12, SUB, SURFACE, Align::Right) });
            });
        }
        // The last minute: a bar per 1.2 s bucket heard, newest at the right, on a dotted baseline.
        let act = app.radio.act[p];
        if !app.held(16 + p) && app.dirty(16 + p, Mix::new().h(&act).get()) {
            let (by, bh) = (y + 62, 10);
            canvas::banded(lcd, 14, by, 212, bh + 1, SURFACE, |b| {
                for i in 0..ACT_BUCKETS as i32 {
                    let x = 18 + (ACT_BUCKETS as i32 - 1 - i) * 6 + 10;
                    if act & 1 << i != 0 {
                        b.fill(x, by, 4, bh, w::slot_colour(s));
                    } else {
                        b.fill(x, by + bh - 1, 2, 1, DIM);
                    }
                }
            });
        }
    }
}
