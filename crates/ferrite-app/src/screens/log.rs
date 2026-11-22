//! The event log (MENU 0 4 1; manual chapters 24 and 27, INBOX-6 to INBOX-10, MENU-26), and one
//! event's detail (C1, C2). Newest first, unread marked; ◀▶ filter by source with that source's unread count
//! (INBOX-9); OK opens the detail and marks it read; # "Go" tunes the active slot to it (INBOX-10); hold MENU
//! for the log's options (mark all read, clear, decoder settings).

use super::lists::*;
use super::opts::Of;
use super::shared::*;
use super::{Cx, Hints, View};
use crate::chan::ChannelStore;
use crate::data::{Event, Op, Src};
use crate::keys::{KeyEvent, Press};
use crate::{App, Screen};
use ferrite_core::keys::Key;
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;
use ufmt::uwrite;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Log {
    pub cur: Cursor,
    /// 0 every source, else Src::ALL[filter - 1].
    pub filter: u8,
    /// The detail of the selected event (C2).
    pub detail: bool,
}

impl Log {
    pub fn new() -> Self {
        Log { cur: Cursor::default(), filter: 0, detail: false }
    }

    fn src(self) -> Option<Src> {
        self.filter.checked_sub(1).map(|i| Src::ALL[i as usize])
    }
}

/// The filter: which events show.
fn keep<'a>(store: &'a impl ChannelStore, src: Option<Src>) -> impl Fn(u16) -> bool + 'a {
    move |i| src.is_none_or(|s| store.data().event(i).is_some_and(|e| e.src == s))
}

impl View for Log {
    /// KEY-19: ◀▶ filter by source (INBOX-9); in a detail they are the knob.
    fn own_arrows(self) -> bool {
        true
    }

    fn options(self, app: &mut App) -> bool {
        app.open_screen(Of::Log.screen());
        true
    }

    fn name(self) -> &'static str {
        if self.detail { "Event detail" } else { "Event log" }
    }

    fn title(self, _: &App, b: &mut Buf<10>) {
        b.set(if self.detail { b"Event" } else { b"Events" }); // SBAR-5: 8 characters at most
    }

    fn key(self, app: &mut App, e: KeyEvent, store: &impl ChannelStore) {
        use Press::*;
        let d = store.data();
        let n = d.events();
        let k = keep(store, self.src());
        let sel = d.event(self.cur.sel).filter(|_| k(self.cur.sel));
        match (e.key, e.press) {
            (Key::Up | Key::Down, Short | Repeat) => self.knob(app, if e.key == Key::Up { -1 } else { 1 }, store),
            (Key::Left | Key::Right, Short | Repeat) if !self.detail => {
                // INBOX-9: filter by source
                let filter = (self.filter as i32 + if e.key == Key::Right { 1 } else { -1 }).rem_euclid(Src::ALL.len() as i32 + 1) as u8;
                let s = Log { filter, ..self };
                app.screen = Screen::Log(Log { cur: Cursor::first(n, &keep(store, s.src())), ..s });
            }
            (Key::Left | Key::Right, Short | Repeat) => self.knob(app, if e.key == Key::Right { 1 } else { -1 }, store),
            (Key::Menu, Short) if !self.detail && sel.is_some() => {
                d.request(Op::Read(self.cur.sel));
                app.log_unread = app.log_unread.saturating_sub(sel.is_some_and(|e| e.unread) as u8);
                app.screen = Screen::Log(Log { detail: true, ..self });
            }
            // INBOX-10: Go
            (Key::Hash, Short) => match sel {
                Some(e) if e.freq != 0 => app.go_freq(e.freq),
                _ => {}
            },
            (Key::Exit, Short) if self.detail => app.screen = Screen::Log(Log { detail: false, ..self }),
            (Key::Exit, Short) => app.back(),
            _ => {}
        }
    }

    fn knob(self, app: &mut App, d: i32, store: &impl ChannelStore) {
        let n = store.data().events();
        let cur = self.cur.step(d, n, ROWS2, &keep(store, self.src()));
        if self.detail && cur.sel != self.cur.sel {
            store.data().request(Op::Read(cur.sel));
        }
        app.screen = Screen::Log(Log { cur, ..self });
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        let data = cx.store.data();
        app.page(lcd, self.detail);
        if !app.gate((self, data.version(), cx.dev.clock_min)) {
            return;
        }
        if self.detail {
            match data.event(self.cur.sel) {
                Some(e) => detail(app, lcd, &e, cx),
                None => app.empty(lcd, b"No such event", b"The log was cleared"),
            }
            return;
        }
        let n = data.events();
        let k = keep(cx.store, self.src());
        // INBOX-9: unread per source, in the header
        let unread = (0..n).filter(|&i| k(i) && data.event(i).is_some_and(|e| e.unread)).count();
        let shown = (0..n).filter(|&i| k(i)).count();
        let mut l = Buf::<32>::new();
        let _ = uwrite!(l, "{} {}", shown, if shown == 1 { "event" } else { "events" });
        if unread > 0 {
            let _ = uwrite!(l, " \u{b7} {} new", unread);
        }
        let mut r = Buf::<24>::new();
        match self.src() {
            Some(s) => {
                r.push(b"< ");
                r.push(s.name().as_bytes());
                r.push(b" >");
            }
            None => r.set(b"newest first"),
        }
        app.list_head(lcd, l.as_bytes(), r.as_bytes());
        let rows = self.cur.rows(n, ROWS2, &k, |row, i| {
            if let Some(e) = data.event(i) {
                let (mut t, mut sub, mut time, mut f) = (Buf::<24>::new(), Buf::<48>::new(), Buf::<12>::new(), Buf::<16>::new());
                title(&e, &mut t);
                summary(&e, &mut sub);
                clock_text(cx.dev.clock_min, e.ago_s, &mut time);
                mhz_text(e.freq, &mut f);
                let r = Row2 {
                    tag: e.src.name().as_bytes(),
                    title: t.as_bytes(),
                    big: e.src == Src::Aprs,
                    right: time.as_bytes(),
                    sub: sub.as_bytes(),
                    sub_right: f.as_bytes(),
                    dot: e.unread,
                    selected: i == self.cur.sel,
                    sub_c: if matches!(e.src, Src::Same) || is_distress(&e) { AMBER } else { 0 },
                    tag_w: 58,
                    ..Row2::default()
                };
                app.row2(lcd, row, &r);
            }
        });
        app.rows_end(lcd, rows, ROWS2, ROW2_H);
        app.list_cue(lcd, self.cur, n, ROWS2, ROW2_H, rows, &k);
        if rows == 0 {
            app.empty(lcd, b"Nothing logged", if self.filter == 0 { b"Decodes and alerts land here" } else { b"Left and right: other sources" });
        } else {
            app.drawn.hash[R_NOTE] = 0;
        }
    }

    fn hints(self, _: &App) -> Hints {
        if self.detail {
            Hints::new(&[(ARROWS_UD, "Next"), ("#", "Go"), ("BACK", "List")])
        } else {
            Hints::new(&[("OK", "Open"), ("#", "Go"), (ARROWS_LR, "Source")])
        }
    }
}

fn is_distress(e: &Event) -> bool {
    e.src == Src::Dsc && e.x == 3
}

/// The row's title: who or what.
fn title(e: &Event, b: &mut Buf<24>) {
    b.clear();
    match e.src {
        Src::Aprs | Src::Ais | Src::Gps | Src::Sota | Src::Pota => b.push(e.name.as_bytes()),
        Src::Pocsag => {
            let _ = uwrite!(b, "Capcode {}", e.id);
        }
        Src::Same => b.push(same(e.text.as_bytes()).event.as_bytes()),
        Src::Dsc => {
            let _ = uwrite!(b, "MMSI {}", e.id);
        }
        Src::Dtmf => b.push(e.text.as_bytes()),
        Src::Selcall => {
            let _ = uwrite!(b, "{} {}", Txt(e.name.as_bytes()), e.id);
        }
    }
}

/// The second line: what it said.
fn summary(e: &Event, b: &mut Buf<48>) {
    b.clear();
    match e.src {
        Src::Same => {
            let s = same(e.text.as_bytes());
            let _ = uwrite!(b, "{} areas \u{b7} {}", s.areas, Txt(s.purge.as_bytes()));
        }
        Src::Ais => {
            let _ = uwrite!(b, "{}\u{b0} \u{b7} {}.{} kn", e.x, e.y / 10, e.y % 10);
        }
        Src::Dtmf => {
            let _ = uwrite!(b, "Slot {}", w::LETTERS[e.slot as usize % 3] as char);
        }
        _ => b.push(e.text.as_bytes()),
    }
}

/// A SAME header, decoded: `ZCZC-ORG-EEE-PSSCCC(-PSSCCC)*+TTTT-JJJHHMM-LLLLLLLL-`.
struct Same<'a> {
    org: &'a str,
    event: &'a str,
    first_area: &'a [u8],
    areas: usize,
    purge: Buf<12>,
    issued: Buf<24>,
    sender: &'a [u8],
}

/// SAME event codes the detail names; others show their code.
const SAME_EVENTS: [(&[u8], &str); 14] = [
    (b"TOR", "Tornado Warning"),
    (b"TOA", "Tornado Watch"),
    (b"SVR", "Severe Thunderstorm Warning"),
    (b"SVA", "Severe Thunderstorm Watch"),
    (b"FFW", "Flash Flood Warning"),
    (b"FLW", "Flood Warning"),
    (b"WSW", "Winter Storm Warning"),
    (b"BZW", "Blizzard Warning"),
    (b"HUW", "Hurricane Warning"),
    (b"SPS", "Special Weather Statement"),
    (b"EAN", "Emergency Action Notification"),
    (b"CEM", "Civil Emergency Message"),
    (b"RWT", "Required Weekly Test"),
    (b"RMT", "Required Monthly Test"),
];

fn same(h: &[u8]) -> Same<'_> {
    let mut s = Same { org: "", event: "Alert", first_area: b"", areas: 0, purge: Buf::new(), issued: Buf::new(), sender: b"" };
    let (head, tail) = match h.iter().position(|&c| c == b'+') {
        Some(p) => (&h[..p], &h[p + 1..]),
        None => (h, &b""[..]),
    };
    for (i, part) in head.split(|&c| c == b'-').enumerate() {
        match i {
            0 => {}
            1 => {
                s.org = match part {
                    b"WXR" => "Weather service",
                    b"CIV" => "Civil authority",
                    b"EAS" => "Broadcast station",
                    b"PEP" => "Primary entry point",
                    _ => "",
                }
            }
            2 => s.event = SAME_EVENTS.iter().find(|e| e.0 == part).map_or("Alert", |e| e.1),
            _ => {
                if s.areas == 0 {
                    s.first_area = part;
                }
                s.areas += 1;
            }
        }
    }
    let mut t = tail.split(|&c| c == b'-');
    if let Some(p) = t.next().filter(|p| p.len() == 4) {
        let v = |a: &[u8]| a.iter().fold(0u32, |n, &c| n * 10 + c.wrapping_sub(b'0') as u32);
        let (hh, mm) = (v(&p[..2]), v(&p[2..]));
        let _ = match (hh, mm) {
            (0, m) => uwrite!(s.purge, "{} min", m),
            (h, 0) => uwrite!(s.purge, "{} h", h),
            (h, m) => uwrite!(s.purge, "{} h {}", h, m),
        };
    }
    if let Some(p) = t.next().filter(|p| p.len() == 7) {
        let _ = uwrite!(s.issued, "{}:{} UTC, day {}", Txt(&p[3..5]), Txt(&p[5..7]), Txt(&p[..3]));
    }
    s.sender = t.next().unwrap_or(b"");
    s
}

/// C2: the event's fields by type (chapter 27, "Event detail").
fn detail(app: &mut App, lcd: &mut impl Panel, e: &Event, cx: &Cx<impl ChannelStore>) {
    let (mut t, mut time, mut age) = (Buf::<24>::new(), Buf::<12>::new(), Buf::<12>::new());
    title(e, &mut t);
    clock_text(cx.dev.clock_min, e.ago_s, &mut time);
    age_text(e.ago_s, &mut age);
    let top = Row2 { tag: e.src.name().as_bytes(), title: t.as_bytes(), big: e.src == Src::Aprs, right: time.as_bytes(), sub: e.src_text(), tag_w: 58, ..Row2::default() };
    if app.dirty(R_TOP, Mix::new().any(&top).get()) {
        row2(lcd, 22, &top, w::Cue::default());
    }
    let mut rows: [(&str, Buf<40>); 7] = Default::default();
    let mut n = 0;
    let mut add = |label: &'static str, v: &[u8]| {
        if n < rows.len() {
            rows[n] = (label, Buf::from(v));
            n += 1;
        }
    };
    let mut b = Buf::<40>::new();
    let mut body: &[u8] = b"";
    match e.src {
        Src::Aprs => {
            add("From", e.name.as_bytes());
            body = e.text.as_bytes();
        }
        Src::Pocsag => {
            add("Baud", ferrite_ui::v!(b, "{}", e.x));
            add("Function", ferrite_ui::v!(b, "{} ({})", e.y, (b'A' + (e.y as u8 & 3)) as char));
            body = e.text.as_bytes();
        }
        Src::Same => {
            let s = same(e.text.as_bytes());
            // The title holds the event; repeat it in full only when the title had to cut it.
            if text4_width(s.event.as_bytes(), &UI_12B) > 124 {
                add("Event", s.event.as_bytes());
            }
            if s.areas > 1 {
                add("Areas", ferrite_ui::v!(b, "{} and {} more", Txt(s.first_area), s.areas - 1));
            } else {
                add("Areas", s.first_area); // one area: no "and 0 more"
            }
            add("For", s.purge.as_bytes());
            add("Issued", s.issued.as_bytes());
            add("From", ferrite_ui::v!(b, "{} {}", s.org, Txt(s.sender)));
            // The raw ZCZC header is not shown: every field of it is in the rows above.
        }
        Src::Dsc => {
            add("Category", ["Routine", "Safety", "Urgency", "Distress"][e.x as usize & 3].as_bytes());
            add("MMSI", ferrite_ui::v!(b, "{}", e.id));
            add("Call", e.text.as_bytes());
        }
        Src::Ais => {
            add("Vessel", e.name.as_bytes());
            add("MMSI", ferrite_ui::v!(b, "{}", e.id));
            add("Course", ferrite_ui::v!(b, "{}\u{b0} \u{b7} {}.{} kn", e.x, e.y / 10, e.y % 10));
        }
        Src::Dtmf => {
            add("Sequence", e.text.as_bytes());
            add("Slot", &[w::LETTERS[e.slot as usize % 3]]);
        }
        Src::Selcall => {
            add("Format", e.name.as_bytes());
            add("Unit", ferrite_ui::v!(b, "{}", e.id));
            add("Sent", e.text.as_bytes());
        }
        Src::Sota | Src::Pota => {
            add("Callsign", e.name.as_bytes());
            add("Reference", e.text.as_bytes());
        }
        Src::Gps => {
            add("Waypoint", e.name.as_bytes());
            add("Arrival", e.text.as_bytes());
        }
    }
    if let Some(p) = e.pos {
        let mut pb = Buf::<32>::new();
        pos_text(p, app.set.coord_fmt, &mut pb);
        add("Position", pb.as_bytes());
        if let Some(f) = cx.store.data().fix() {
            let (m, deg) = dist_bearing(f.pos, p);
            let mut d = Buf::<12>::new();
            dist_text(m, &mut d);
            add("Distance", ferrite_ui::v!(b, "{} {} {}\u{b0}", Txt(d.as_bytes()), compass_name(deg), deg));
        }
    }
    if e.freq != 0 {
        // a waypoint arrival has no frequency
        let mut f = Buf::<16>::new();
        mhz_text(e.freq, &mut f);
        add("Freq", f.as_bytes());
    }
    add("Heard", if e.ago_s == crate::data::AGO_UNKNOWN { &b"Before restart"[..] } else { ferrite_ui::v!(b, "{} ago", Txt(age.as_bytes())) }); // the clock time is in the header
    let mut y = 58;
    for (i, (label, v)) in rows.iter().enumerate().take(n) {
        let c = if i == 0 && is_distress(e) { RED } else { TEXT };
        y = app.kv(lcd, i, y, 0, &w::KV_DETAIL, label.as_bytes(), v.as_bytes(), c, false);
    }
    // The message itself, wrapped, under the rows.
    let y = y + 6;
    if app.dirty(R_NOTE, Mix::new().h(&body).h(&y).get()) {
        ferrite_ui::canvas::banded(lcd, 0, y - 6, 240, w::HINT_Y - y + 6, BG, |lcd| {
            wrap(lcd, 8, y, 224, w::HINT_Y - 2, body, TEXT);
        });
    }
}

impl Event {
    /// The detail's sub line: the source's long name.
    fn src_text(&self) -> &'static [u8] {
        match self.src {
            Src::Aprs => b"APRS message",
            Src::Pocsag => b"POCSAG pager",
            Src::Same => b"Weather alert (SAME)",
            Src::Dsc => b"Marine DSC",
            Src::Ais => b"AIS position",
            Src::Dtmf => b"DTMF sequence",
            Src::Selcall => b"Selcall",
            Src::Gps => b"Waypoint arrival",
            Src::Sota | Src::Pota => b"Activation contact",
        }
    }
}

/// Text wrapped on words in the box from (x, y) to (x + w, y_end), 17 px lines; returns the lines used.
pub(crate) fn wrap(lcd: &mut impl Panel, x: i32, y: i32, w: i32, y_end: i32, text: &[u8], c: u16) -> i32 {
    let mut rest = text;
    let mut line = 0;
    while !rest.is_empty() && y + (line + 1) * 17 <= y_end {
        let mut cut = rest.len();
        if text4_width(rest, &UI_12) > w {
            cut = rest.iter().enumerate().filter(|&(i, &ch)| ch == b' ' && text4_width(&rest[..i], &UI_12) <= w).map(|(i, _)| i).next_back().unwrap_or(0);
            if cut == 0 {
                cut = w::fit(rest, &UI_12, w).len().max(1);
            }
        }
        lcd.text4(x, y + 13 + line * 17, &rest[..cut], &UI_12, c, BG);
        rest = rest[cut..].strip_prefix(b" ").unwrap_or(&rest[cut..]);
        line += 1;
    }
    line
}
