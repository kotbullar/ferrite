//! SSTV (chapter 27a) through the App and a real store: a picture decoded by the codec goes into its slot (header,
//! rows, end), the next slot gets ready, the screen shows it, and a picture written over the link lands in a
//! "to send" slot without stopping the store (SSTV-4.5).

use ferrite_app::chan::ChannelStore;
use ferrite_app::data::NoData;
use ferrite_app::settings::Settings;
use ferrite_app::sstv::{headers, VIEW_CARD};
use ferrite_app::App;
use ferrite_core::sstv::{self, Event, Header, HEADER_LEN, ROW_LEN, WIDTH};
use ferrite_core::usstv;
use ferrite_store::sim::SimFlash;
use ferrite_store::FlashStore;

type Store = FlashStore<SimFlash, NoData>;

fn app() -> App {
    let set: &'static mut Settings = Box::leak(Box::new(Settings::DEFAULT));
    let alert: &'static mut ferrite_app::screens::alert::AlertInfo = Box::leak(Box::new(ferrite_app::screens::alert::AlertInfo::NONE));
    App::new(set, alert)
}

fn mount() -> Store {
    FlashStore::mount(SimFlash::new(), NoData)
}

/// The codec's lines, as the DSP task would hand them over, for a Martin M1 picture of `card`.
struct Collect(Vec<Event>, Vec<[u16; WIDTH]>);
impl usstv::Sink for Collect {
    fn start(&mut self, m: &'static usstv::Mode) {
        self.0.push(Event::Start(m.vis));
        self.1.push([0; WIDTH]);
    }
    fn line(&mut self, y: u16, px: &[u16; WIDTH]) {
        self.0.push(Event::Line(y, 0));
        self.1.push(*px);
    }
    fn end(&mut self, whole: bool) {
        self.0.push(Event::End(whole));
        self.1.push([0; WIDTH]);
    }
}

struct Card;
impl usstv::Picture for Card {
    fn rgb(&mut self, x: usize, y: usize) -> (u8, u8, u8) {
        sstv::test_card(x, y, b"VA7TST\0\0\0\0")
    }
}

fn decoded(vis: u8) -> Collect {
    let mode = usstv::Mode::by_vis(vis).unwrap();
    let mut e = usstv::Encoder::new(mode, 24_000, 8000);
    let mut audio = vec![0i16; 6000];
    while let Some(s) = e.next(&mut Card) {
        audio.push(s);
    }
    audio.extend([0i16; 6000]);
    let mut d = usstv::Decoder::new(24_000);
    let mut c = Collect(Vec::new(), Vec::new());
    d.feed(&audio, &mut c);
    c
}

fn row(st: &Store, slot: u8, y: u16) -> [u8; ROW_LEN] {
    let mut b = [0u8; ROW_LEN];
    assert!(st.data().data_read(sstv::row_addr(slot, y), &mut b));
    b
}

/// SSTV-2, SSTV-4.1, SSTV-4.4: a received picture into the first received slot, its header finished at the end.
#[test]
fn received_picture_is_saved() {
    let st = mount();
    let mut a = app();
    a.sstv_open(&st);
    assert_eq!(a.sstv.view, VIEW_CARD, "nothing received yet: the test card");
    let c = decoded(44);
    for (ev, px) in c.0.iter().zip(&c.1) {
        a.sstv_event(*ev, px, &st);
    }
    let hs = headers(&st);
    let h = hs[1].expect("slot 1 (the first received) holds it");
    assert_eq!((h.kind, h.vis, h.lines, h.complete, h.seq), (sstv::KIND_RX, 44, Some(256), true, 1));
    // the rows are the decoder's, little-endian RGB565
    let (_, px) = c.0.iter().zip(&c.1).find(|(e, _)| matches!(e, Event::Line(100, _))).unwrap();
    let r = row(&st, 1, 100);
    assert_eq!(u16::from_le_bytes([r[0], r[1]]), px[0]);
    assert_eq!(u16::from_le_bytes([r[638], r[639]]), px[319]);
    // a second picture goes into the next slot, numbered after
    for (ev, px) in c.0.iter().zip(&c.1) {
        a.sstv_event(*ev, px, &st);
    }
    let hs = headers(&st);
    assert_eq!(hs[2].map(|h| h.seq), Some(2));
    assert_eq!(a.sstv.view, 2, "the screen shows the newest");
}

/// SSTV-4.1: with all four received slots full, the oldest is erased ready for the next picture.
#[test]
fn oldest_received_is_written_over() {
    let st = mount();
    let mut a = app();
    let c = decoded(8);
    a.sstv_open(&st);
    for _ in 0..5 {
        for (ev, px) in c.0.iter().zip(&c.1) {
            a.sstv_event(*ev, px, &st);
            }
    }
    let seqs: Vec<_> = headers(&st)[1..].iter().map(|h| h.map(|h| h.seq)).collect();
    // three received slots: the fourth over the first, the fifth over the second; the oldest left (the third)
    // already erased for the next one
    assert_eq!(seqs, vec![Some(4), Some(5), None]);
}

/// SSTV-4.5: a link write inside the picture slots is a picture's: the store keeps running (no update state).
#[test]
fn console_picture_write_leaves_the_store_running() {
    use ferrite_app::data::DataWrite;
    let st = mount();
    let a0 = sstv::slot_addr(0);
    assert!(st.data().picture_write(DataWrite::Erase(a0)));
    let h = Header { kind: sstv::KIND_SEND, vis: 0, complete: true, lines: Some(256), utc: 0, freq: 0, name: *b"photo\0\0\0\0\0\0\0\0\0\0\0", seq: 0 };
    let mut bytes = [0u8; 64];
    bytes.copy_from_slice(&h.encode()[..HEADER_LEN]);
    assert!(st.data().picture_write(DataWrite::Program { addr: a0, len: 64, bytes }));
    assert_eq!(headers(&st)[0], Some(h));
    assert_eq!(st.data().layout_found(), 0, "not an update");
}

/// SSTV-4.1: a slot with no picture but old bytes in it (the range was free before) is erased before a picture
/// goes in, so the picture isn't programmed over them.
#[test]
fn old_bytes_in_a_slot_are_erased_first() {
    use ferrite_app::data::DataWrite;
    let st = mount();
    // old bytes in slot 1, header sector and pixels, no magic
    for off in [0u32, 4096, 4096 + 100 * 640] {
        let mut bytes = [0x5Au8; 64];
        bytes[0] = 0;
        assert!(st.data().picture_write(DataWrite::Program { addr: sstv::slot_addr(1) + off, len: 64, bytes }));
    }
    let mut a = app();
    a.sstv_open(&st);
    let c = decoded(8);
    for (ev, px) in c.0.iter().zip(&c.1) {
        a.sstv_event(*ev, px, &st);
    }
    let h = headers(&st)[1].expect("the picture in slot 1");
    assert_eq!((h.vis, h.lines, h.complete), (8, Some(240), true));
    let (_, px) = c.0.iter().zip(&c.1).find(|(e, _)| matches!(e, Event::Line(100, _))).unwrap();
    let r = row(&st, 1, 100);
    assert_eq!(u16::from_le_bytes([r[0], r[1]]), px[0], "row 100 is the picture's, not old bytes ANDed in");
}

/// SSTV-2.6: a picture holds the active slot's squelch open (its monitor) until it ends; a monitor the user latched
/// is left alone.
#[test]
fn a_picture_holds_the_squelch_open() {
    use ferrite_core::msg::RadioCmd;
    let st = mount();
    let mut a = app();
    a.sstv_open(&st);
    while a.pop_cmd().is_some() {}
    let c = decoded(8);
    let mut on = false;
    let mut off = false;
    for (ev, px) in c.0.iter().zip(&c.1) {
        a.sstv_event(*ev, px, &st);
        while let Some(cmd) = a.pop_cmd() {
            match cmd {
                RadioCmd::Monitor { slot: 0, on: true } => on = true,
                RadioCmd::Monitor { slot: 0, on: false } => off = on,
                _ => {}
            }
        }
    }
    assert!(on && off, "held open at the start, closed at the end");
}

/// The readback's lines (LINK-19) as text.
fn readback(a: &App, st: &Store) -> Vec<String> {
    let (dev, live) = (ferrite_core::msg::Device::default(), ferrite_core::msg::LiveAll::default());
    let mut l = ferrite_app::term::cmd::Line::new();
    (0..).map_while(|i| ferrite_app::uistate::line(a, st, &dev, &live, i, &mut l).then(|| String::from_utf8_lossy(l.as_bytes()).into_owned())).collect()
}

fn sstv_line(a: &App, st: &Store) -> String {
    readback(a, st).into_iter().find(|t| t.starts_with("sstv")).expect("an sstv line")
}

/// SSTV-3.1: the readback's `sstv` line: the picture shown and what it is, the Send page's mode, a picture coming in.
#[test]
fn readback_says_what_the_sstv_screen_shows() {
    use ferrite_core::keys::Key;
    let st = mount();
    let mut a = app();
    let mut t = 1000;
    let mut press = |a: &mut App, k: Key| {
        a.key_down(k, t, &st);
        a.key_up(k, t + 50, &st);
        t += 200;
    };
    for k in [Key::Menu, Key::Digit(0), Key::Digit(3), Key::Digit(7)] {
        press(&mut a, k); // MENU 0 3 7 (SSTV-1)
    }
    assert!(matches!(a.screen(), ferrite_app::screens::Screen::Sstv(_)), "MENU 0 3 7 opens SSTV");
    a.sstv_prepare(&st);
    let t = sstv_line(&a, &st);
    assert!(t.contains("view=\"card\"") && t.contains("page=\"main\"") && !t.contains(" rx="), "{t}");
    // a picture coming in: its mode, slot and lines so far; the screen shows its slot
    let c = decoded(8);
    let n = c.0.iter().position(|e| matches!(e, Event::Line(19, _))).unwrap();
    for (ev, px) in c.0.iter().zip(&c.1).take(n + 1) {
        a.sstv_event(*ev, px, &st);
    }
    let t = sstv_line(&a, &st);
    assert!(t.contains("view=1 slot=\"rx\" mode=\"Robot 36\" lines=\"open\""), "{t}");
    assert!(t.contains(" rx=\"Robot 36\" rx_slot=1 rx_lines=20") && t.contains(" held=1"), "{t}");
    for (ev, px) in c.0.iter().zip(&c.1).skip(n + 1) {
        a.sstv_event(*ev, px, &st);
    }
    let t = sstv_line(&a, &st);
    assert!(t.contains("lines=240 complete=1") && !t.contains(" rx=") && !t.contains("held"), "{t}");
    // the Send page (OK), the knob's next mode
    press(&mut a, Key::Menu);
    a.knob(1, 5000, &st);
    let t = sstv_line(&a, &st);
    assert!(t.contains("page=\"send\" send=\"Martin M1\""), "{t}");
}

/// The SSTV screen's three lines under the picture (SSTV-3), as text.
fn info(a: &App, st: &Store) -> [String; 3] {
    use ferrite_ui::text::Buf;
    let (mut l1, mut l2, mut l3) = (Buf::<40>::new(), Buf::<40>::new(), Buf::<40>::new());
    ferrite_app::screens::sstv::Sstv::info(a, st, None, &mut l1, &mut l2, &mut l3);
    [l1, l2, l3].map(|l| String::from_utf8_lossy(l.as_bytes()).into_owned())
}

/// SSTV-2.5, SSTV-3, SSTV-3.1 (issue 181): # picks the receive mode and the screen says so: Robot 36, Martin M1,
/// Scottie S1, PD90, PD120, then Auto, each press the next; the mode picked waits for its sync ("waiting for sync",
/// no picture yet) and stays picked while it waits; a pick # leaves before its sync gives no toast and no picture
/// (the older pictures stay); a picked mode's picture ends back on Auto. Every line fits the screen's 224 px.
#[test]
fn hash_picks_the_receive_mode() {
    use ferrite_core::keys::Key;
    use ferrite_ui::assets::{UI_12, UI_12B};
    use ferrite_ui::gfx::text4_width;
    let st = mount();
    let mut a = app();
    let mut t = 1000;
    let mut press = |a: &mut App, k: Key| {
        a.key_down(k, t, &st);
        a.key_up(k, t + 50, &st);
        t += 200;
    };
    for k in [Key::Menu, Key::Digit(0), Key::Digit(3), Key::Digit(7)] {
        press(&mut a, k);
    }
    a.sstv_prepare(&st);
    // a picture received first (slot 1), so a pick # leaves has one to keep
    let c = decoded(8);
    for (ev, px) in c.0.iter().zip(&c.1) {
        a.sstv_event(*ev, px, &st);
    }
    assert_eq!(a.sstv.view, 1);
    let fits = |l: &[String; 3]| {
        assert!(text4_width(l[0].as_bytes(), &UI_12B) <= 216, "{l:?}");
        assert!(text4_width(l[1].as_bytes(), &UI_12) <= 216 && text4_width(l[2].as_bytes(), &UI_12) <= 216, "{l:?}");
    };
    let l = info(&a, &st);
    assert!(l[2].starts_with("Listening on A, "), "before # the slot listened on: {l:?}");
    // # Robot 36: the decoder told (the UI task passes `force` on), the line says it; the decoder starts the
    // picture waiting for its sync
    press(&mut a, Key::Hash);
    assert_eq!(a.sstv.force.take(), Some(8));
    a.sstv_event(Event::Start(8), &[0; WIDTH], &st);
    let l = info(&a, &st);
    fits(&l);
    assert_eq!(l[0], "Robot 36, waiting for sync", "{l:?}");
    assert!(l[1].starts_with("Listening on A, "), "{l:?}");
    assert_eq!(l[2], "Mode: Robot 36 (# next)");
    assert!(sstv_line(&a, &st).contains(" forced=\"Robot 36\""));
    // # again: Martin M1; the Robot 36 wait the decoder ends has no lines: no toast, no picture, the older one shown
    press(&mut a, Key::Hash);
    assert_eq!(a.sstv.force.take(), Some(44));
    a.sstv_event(Event::End(false), &[0; WIDTH], &st);
    let lost = |a: &App| readback(a, &st).iter().any(|l| l.contains("Signal lost"));
    assert!(!lost(&a), "no \"Signal lost\" for a wait that never started");
    assert_eq!(headers(&st)[2], None, "the wait's slot erased again");
    assert_eq!(a.sstv.view, 1, "the picture received shown again");
    a.sstv_event(Event::Start(44), &[0; WIDTH], &st);
    let l = info(&a, &st);
    assert_eq!((l[0].as_str(), l[2].as_str()), ("Martin M1, waiting for sync", "Mode: Martin M1 (# next)"));
    assert_eq!(headers(&st)[1].map(|h| h.vis), Some(8), "the older picture kept");
    // on through the others to Auto (the decoder stopped: VIS again)
    for (vis, name) in [(60, "Scottie S1"), (99, "PD90"), (95, "PD120")] {
        press(&mut a, Key::Hash);
        assert_eq!(a.sstv.force.take(), Some(vis));
        a.sstv_event(Event::End(false), &[0; WIDTH], &st);
        a.sstv_event(Event::Start(vis), &[0; WIDTH], &st);
        let l = info(&a, &st);
        fits(&l);
        assert_eq!(l[2], format!("Mode: {name} (# next)"));
        assert_eq!(l[0], format!("{name}, waiting for sync"));
    }
    press(&mut a, Key::Hash);
    assert_eq!(a.sstv.force.take(), Some(0), "Auto: the decoder stopped");
    a.sstv_event(Event::End(false), &[0; WIDTH], &st);
    assert!(!lost(&a));
    let l = info(&a, &st);
    assert_eq!(l[2], "Mode: Auto (# next)");
    assert!(!sstv_line(&a, &st).contains("forced"));
    // # Robot 36 and a picture in that mode: it ends back on Auto, saved with its toast
    press(&mut a, Key::Hash);
    assert_eq!(a.sstv.force.take(), Some(8));
    for (ev, px) in c.0.iter().zip(&c.1) {
        a.sstv_event(*ev, px, &st);
    }
    assert_eq!(a.sstv.forced_vis(), None);
    assert_eq!(info(&a, &st)[2], "Mode: Auto (# next)");
    assert!(readback(&a, &st).iter().any(|l| l == "toast \"Picture saved\" \"Robot 36\""), "{:?}", readback(&a, &st));
}

#[test]
fn iss_pd120_audio_is_detected_and_saved_in_color() {
    let st = mount();
    let mut a = app();
    a.sstv_open(&st);
    a.sstv_iss(&st);
    assert!(a.sstv.iss);
    let c = decoded(95); // PD120, with the VIS header.
    for (event, pixels) in c.0.iter().zip(&c.1) {
        a.sstv_event(*event, pixels, &st);
    }
    let h = headers(&st)[sstv::FIRST_RX as usize].expect("ISS picture");
    assert_eq!((h.vis, h.freq, h.lines, h.complete), (95, 14_580_000, Some(248), true));
    let (_, pixels) = c.0.iter().zip(&c.1).find(|(event, _)| matches!(event, Event::Line(100, _))).unwrap();
    let stored = row(&st, sstv::FIRST_RX, 100);
    assert!(stored.chunks_exact(2).zip(pixels).all(|(raw, &px)| u16::from_le_bytes([raw[0], raw[1]]) == px));
}
