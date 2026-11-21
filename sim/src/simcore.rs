//! The interactive simulator without a window: the firmware's own UI (`ferrite-app` drawing through
//! `ferrite-ui`), a fake three-slot radio and the radio's front panel ([`crate::face`]), driven by key, mouse and
//! event calls and stepped by the caller's clock. The desktop window (`cargo simw`, minifb) and the web page
//! (`sim-web`, wasm) are thin shells over it.
//!
//! Time is the caller's: [`SimCore::step`] takes milliseconds since start. The status bar clock comes from
//! [`SimCore::set_clock`] (UTC seconds), which the desktop sets from the PC and the page from the browser.
//!
//! Channels, zones, range presets, settings and slots live in a simulated SPI flash through the firmware's own
//! store (`ferrite-store`), so they survive the power switch; the caller keeps the flash image between runs
//! ([`SimCore::flash_image`]: the desktop in `target/sim/flash.bin`, the page in the browser).

use crate::face::{Face, Part};
use crate::fake::{self, FakeRadio};
use ferrite_app::chan::ChannelStore;
use ferrite_app::data::{Data, MsgState, Src};
use ferrite_app::ports::{Port, Ports, To};
use ferrite_app::sample::Sample;
use ferrite_app::settings::Settings;
use ferrite_core::layout::{LAYOUT_HEADER, MAILBOX};
use ferrite_store::sim::SimFlash;
use ferrite_store::FlashStore;
use ferrite_app::screens::nav::{self, Dest};
use ferrite_app::{App, Screen as AppScreen};
use ferrite_core::freq;
use ferrite_core::keys::Key;
use ferrite_core::msg::{Device, GpsState};
use ferrite_core::VERSION;
use ferrite_ui::boot::{self, BootInfo, Check, State};
use ferrite_ui::fb::FrameBuffer;
use ferrite_app::term::{cmd, Sink};
use ferrite_ui::gfx::Gfx;
use ferrite_ui::panel::{Panel, H, W};

/// The PC keyboard map, as the desktop window has it (the web page uses the same keys).
pub const KEYMAP: &str = "\
keys: arrows = UP/DOWN/LEFT/RIGHT · Enter = MENU/OK · Esc = BACK · 0-9 digits · , = * · . = #
      S = SLOT · V = V/M · Q = SIDE1 · W = SIDE2 · Space = PTT A · [ ] or mouse wheel = knob
      or click the radio's keys (hold the mouse for long presses; hold SIDE1 + wheel = squelch)
      hold a key 0.7 s for its long press (hold S: edit, hold . : lock, hold Esc: home, hold Enter: MENU 0)
      M message · N alert · F5 GPS · F6 Bluetooth · F7 battery · F8 power off (or click VOL: the power switch) · F9 empty battery
      F10 unknown SSB patch · F11 a write from a new device · X crossband (also MENU 0 3 4) · K speaker mic in/out
      L home layout · B a beacon (automatic transmit) · R broadcast FM on the active slot (100.5, again 99.1)
      hold Enter while booting: the settings reset
      F2 redraw flash · F3 mock-up overlay · F12 screenshot · Ctrl-Q quit";

/// The largest whole scale offered.
pub const MAX_SCALE: usize = 2;

/// How long the boot screen shows (ms), and how long "Saving..." shows before the radio goes dark.
const BOOT_MS: u32 = 2000;
const SAVE_MS: u32 = 1500;

/// The frame buffer, plus a record of what each redraw touched, and the text on the screen now (for the
/// end-to-end tests: [`SimCore::screen_text`]): each pixel remembers the string that last painted it, and a
/// string counts as on screen while it still owns at least half of its box.
struct Counting {
    fb: FrameBuffer,
    rects: Vec<(i32, i32, i32, i32)>,
    pixels: u64,
    texts: Vec<(u32, TextItem)>,
    /// Per pixel: the id of the string that painted it last, 0 if anything else did.
    owner: Vec<u32>,
    next_id: u32,
}

/// A string as drawn, and the box it covers (inclusive).
#[derive(Clone, Debug)]
pub struct TextItem {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub s: String,
}

impl Counting {
    fn new() -> Self {
        Counting { fb: FrameBuffer::new(), rects: Vec::new(), pixels: 0, texts: Vec::new(), owner: vec![0; (W * H) as usize], next_id: 1 }
    }

    fn mark(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, id: u32) {
        for y in y0.max(0)..=y1.min(H - 1) {
            for x in x0.max(0)..=x1.min(W - 1) {
                self.owner[(y * W + x) as usize] = id;
            }
        }
    }

    /// The strings still on screen, dropping the ones painted over.
    fn visible(&mut self) -> Vec<TextItem> {
        let mut owned = std::collections::HashMap::<u32, i32>::new();
        for &o in &self.owner {
            if o != 0 {
                *owned.entry(o).or_default() += 1;
            }
        }
        self.texts.retain(|(id, t)| 2 * owned.get(id).copied().unwrap_or(0) >= (t.x1 - t.x0 + 1) * (t.y1 - t.y0 + 1));
        self.texts.iter().map(|(_, t)| t.clone()).collect()
    }
}

impl Panel for Counting {
    fn window(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.rects.push((x0, y0, x1, y1));
        self.fb.window(x0, y0, x1, y1);
        self.mark(x0, y0, x1, y1, 0);
    }
    fn text_hint(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, s: &[u8]) {
        let id = self.next_id;
        self.next_id += 1;
        self.mark(x0, y0, x1, y1, id);
        self.texts.push((id, TextItem { x0, y0, x1, y1, s: String::from_utf8_lossy(s).into_owned() }));
        if self.texts.len() > 4096 {
            self.visible();
        }
    }
    fn push(&mut self, color: u16, count: u32) {
        self.pixels += count as u64;
        self.fb.push(color, count);
    }
    fn push_buf(&mut self, px: &[u16]) {
        self.pixels += px.len() as u64;
        self.fb.push_buf(px);
    }
}

/// What the radio (or the world) would report, as the simulator's event keys send it. Each calls the App hook the
/// firmware calls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// M: an APRS message arrives.
    Message,
    /// N: the fake decoder's SAME warning, as if heard on slot C.
    Alert,
    /// F5: GPS off, searching, fix.
    Gps,
    /// F6: Bluetooth on/off.
    Bluetooth,
    /// F7: battery normal, low, cut-off.
    Battery,
    /// F8, or a click on VOL: the power switch (off: "Saving...", then dark; on: a fresh boot).
    PowerSwitch,
    /// F9: an empty battery.
    EmptyBattery,
    /// F10: an unknown SSB patch at boot.
    SsbPatch,
    /// F11: a write from a new Bluetooth device.
    LinkWrite,
    /// X: the crossband repeater.
    Crossband,
    /// L: the next home layout.
    Layout,
    /// B: a beacon (an automatic transmit).
    Beacon,
    /// D: the LED demo: the bands go quiet and the backlight time runs out now, so the screen stays dark and the
    /// heartbeat shows (a double flash while M's message is unread); D again brings the stations back.
    ScreenOff,
    /// T: a station on the active slot sending another tone than the slot's: the tone hint (TSRCH-13) shows 88.5;
    /// T again, or a retune or new receive tone, clears it.
    ToneHint,
    /// R: the active slot's VFO to 100.5 MHz, a broadcast FM station with the full RDS radio text, so the slot plays
    /// broadcast (BCR-21); R again: 99.1, a station with only its name (BCR-26).
    FmStation,
    /// K: a speaker mic plugged into the side connector, or taken out (SND-12): the Status page's row.
    SpeakerMic,
}

/// A recent redraw region and when it was drawn (ms), for the redraw flash.
struct Flash {
    rect: (i32, i32, i32, i32),
    at: u32,
}

/// The boot checks the simulator shows: all passed.
const CHECKS: [Check; 6] = [
    Check { label: "MCU", value: "AT32F403A", state: State::Ok },
    Check { label: "SPI flash", value: "4 MB", state: State::Ok },
    Check { label: "BK4829 VHF", value: "0x4829", state: State::Ok },
    Check { label: "BK4829 UHF", value: "0x4829", state: State::Ok },
    Check { label: "SI4732", value: "not tested", state: State::Off },
    Check { label: "Calibration", value: "", state: State::Ok },
];

/// The simulated flash's saved part ([`SimFlash::image`]): the chip from the layout header to the end of the packet
/// mailbox (settings, channels, zones, the APRS records, waypoints) at the image's start, then the event log
/// (decision 333's layout 3: [`ferrite_store::sim::IMAGE_LEN`] bytes in all). An image of another length (layout
/// 2's) isn't used: the sample data instead.
pub const FLASH_IMAGE: core::ops::Range<u32> = LAYOUT_HEADER.start..MAILBOX.end;

/// What the flash holds at power on.
pub enum Stored {
    /// The look review's sample: zones, channels, presets and settings written to a blank flash.
    Sample,
    /// A saved image ([`SimCore::flash_image`]); an empty one gives a blank flash (setup).
    Image(Vec<u8>),
    /// Blank, as after installing: setup runs (BOOT-12).
    Blank,
}

type Store = FlashStore<SimFlash, Sample>;

pub struct SimCore {
    /// LINK-13.4: the receive audio the simulator sends a listening cable session: a 440 Hz tone, coded as the
    /// radio codes the BK4829s' audio (only while a slot's squelch is open), and the time its samples are made to.
    audio: ferrite_core::rxaudio::Packer,
    audio_ms: u32,
    app: App,
    /// Boxed so it stays put: the App's satellite source reads it (SAT-2, [`power_on`]).
    store: Box<Store>,
    radio: FakeRadio,
    dev: Device,
    lcd: Counting,
    face: Face,
    scale: usize,
    out: Vec<u32>,
    now: u32,
    /// UTC seconds at `now` = 0, for the status bar clock.
    clock0: u64,
    // The power switch: booting until `boot_end`; `off_at` while the app saves before going dark.
    booting: bool,
    boot_end: u32,
    /// BOOT-18: the boot animation still has frames to draw (the logo screen; the owner card stays still).
    boot_anim: bool,
    off_at: Option<u32>,
    off: bool,
    // Parts held by the keyboard and by the mouse; the knob half last clicked and until when it shows; PTT.
    kb_held: Vec<usize>,
    mouse: Option<usize>,
    knob_flash: (i8, u32),
    /// The second the APRS task last ran.
    aprs_s: u32,
    tx: Option<u8>,
    battery: usize,
    layout: u8,
    last_key: String,
    last_px: u64,
    flashes: Vec<Flash>,
    /// The terminal (manual chapter 37) and the link protocol (chapter 29) on the cable and Bluetooth, as the
    /// firmware runs them ([`Ports`]); what each port has been sent that its reader hasn't taken.
    ports: Ports,
    term_out: Vec<u8>,
    /// Bytes from the cable not taken yet: a line waits while the one before it runs (TERM-13).
    term_in: std::collections::VecDeque<u8>,
    /// Bluetooth's bytes both ways (roadmap 3d): a phone app on the BT513's serial characteristic.
    bt_in: std::collections::VecDeque<u8>,
    bt_out: Vec<u8>,
    /// KISS bytes that reached the TNC from either port (KISS-6), for tests.
    pub kiss_in: Vec<u8>,
    /// Every command the App sent the fake radio, when [`SimCore::log_cmds`] is on (tests).
    pub cmds: Vec<ferrite_core::msg::RadioCmd>,
    log_cmds: bool,
    /// Every render pass's flicker and overdraw, when [`SimCore::watch_passes`] is on (tests, DISP-25).
    pub passes: Vec<PassHit>,
    watch: bool,
    /// DISP-31: each render's screen against the same state drawn from scratch, when [`SimCore::check_repaint`] is on
    /// (tests): every difference, as stale pixels the incremental drawing left.
    pub repaint_misses: Vec<RepaintMiss>,
    repaint: bool,
    /// The screen the last watched pass drew.
    last_screen: &'static str,
    last_page: u32,
    /// The theme of the last pass: a new theme redraws everything, as a new screen does (DISP-18, DISP-25).
    last_theme: u8,
    /// The fake GPS module under the power manager (GPS-26 to GPS-33), when asked for ([`SimCore::gps_model`]).
    gps: Option<crate::fakegps::FakeGps>,
    /// 0 6 1 turned the sample's fix off (GPS-3: the fixed position or none stands in until it is on again).
    gps_off: bool,
    /// `--cut-restart` (decision 339): the store's writes queued as on the radio, and the next restart after a raw
    /// write is a power cut: what the Storage task hadn't done yet is lost (the stamp, when Console sent it last).
    cut_restart: bool,
}

/// One render whose screen differed from a fresh repaint of the same state ([`SimCore::check_repaint`]).
#[derive(Clone, Debug)]
pub struct RepaintMiss {
    pub at: u32,
    pub screen: &'static str,
    /// How many pixels differ, and the box around them (x0, y0, x1, y1).
    pub pixels: u32,
    pub area: (u16, u16, u16, u16),
    /// The first eight misses: the screen as drawn and as drawn from scratch (RGB565, row by row).
    pub shots: Option<(Vec<u16>, Vec<u16>)>,
}

/// One render pass as [`SimCore::watch_passes`] records it.
#[derive(Clone, Debug)]
pub struct PassHit {
    pub at: u32,
    /// The screen after the pass, and whether the pass before drew it too, on the same page (a live update, not a
    /// change of screen or page).
    pub screen: &'static str,
    pub live: bool,
    pub report: ferrite_ui::fb::PassReport,
}

/// The terminal's bytes waiting for the cable: room up to [`TERM_BUF`].
struct TermBuf<'a>(&'a mut Vec<u8>);

/// How much drawn output the simulator holds for a slow reader (about two full 80 x 24 screens).
pub const TERM_BUF: usize = 8192;

impl Sink for TermBuf<'_> {
    fn room(&self) -> usize {
        TERM_BUF.saturating_sub(self.0.len())
    }
    fn put(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
}

/// A window position to panel (buffer) pixels: the panel sits 1:1 in the middle of the window.
pub fn to_panel(m: (f32, f32), win: (usize, usize), buf: (usize, usize)) -> (i32, i32) {
    let (ox, oy) = ((win.0 as f32 - buf.0 as f32) / 2.0, (win.1 as f32 - buf.1 as f32) / 2.0);
    ((m.0 - ox).floor() as i32, (m.1 - oy).floor() as i32)
}

/// The largest whole scale whose panel fits the window; 1 if none does. `sizes[i]` is the panel at scale i + 1.
pub fn fit_scale(win: (usize, usize), sizes: &[(usize, usize)]) -> usize {
    sizes.iter().rposition(|&(w, h)| w <= win.0 && h <= win.1).map_or(1, |i| i + 1)
}

/// RGB565 to 0xRRGGBB.
pub fn rgb565_to_rgb(c: u16) -> u32 {
    let r = ((c >> 11) & 31) as u32 * 255 / 31;
    let g = ((c >> 5) & 63) as u32 * 255 / 63;
    let b = (c & 31) as u32 * 255 / 31;
    r << 16 | g << 8 | b
}

fn blend(a: u32, b: u32) -> u32 {
    let ch = |s: u32| ((a >> s & 255) + (b >> s & 255)) / 2;
    ch(16) << 16 | ch(8) << 8 | ch(0)
}

/// The key a front-panel part sends.
fn key_of(p: Part) -> Key {
    match p {
        Part::Key(k) => k,
        Part::Ptt(n) => Key::Ptt(n),
        _ => Key::PTT,
    }
}

fn freq_text(f: freq::Freq10) -> String {
    let mut b = [0u8; 16];
    let n = freq::format(f, &mut b);
    String::from_utf8_lossy(&b[..n]).into_owned()
}

/// The screens [`SimCore::jump`] offers: home, then every destination in the manual's inventory.
pub fn jump_names() -> impl Iterator<Item = String> {
    core::iter::once(String::from("Home")).chain(nav::ALL.iter().map(|d| {
        let (name, _, id) = d.info();
        format!("{id} {name}")
    }))
}

impl SimCore {
    /// A radio switched on now (the boot screen), the panel at a whole `scale`, on the sample data; `owner_card`
    /// boots with the sample owner card (MENU 0 7 3 3).
    pub fn new(scale: usize, owner_card: bool) -> Self {
        Self::with_flash(scale, owner_card, Stored::Sample)
    }

    /// Decision 339's check for Ferrite Console (`--cut-restart`): the store's writes queued as on the radio (the
    /// Storage task's turn at the start of each step), and the next restart after a raw write cuts the power, so a
    /// stamp queued by the link but not yet written is lost. One restart only; after it, writes are done at once.
    pub fn cut_next_restart(&mut self) {
        self.cut_restart = true;
        self.store.set_deferred(true);
    }

    /// A radio switched on now with the flash holding `stored` (see [`Stored`]).
    pub fn with_flash(scale: usize, owner_card: bool, stored: Stored) -> Self {
        let scale = scale.clamp(1, MAX_SCALE);
        let nor = match &stored {
            Stored::Image(img) if img.len() == ferrite_store::sim::IMAGE_LEN => SimFlash::from_image(img),
            _ => SimFlash::new(),
        };
        // a kept image from another layout (its length): the sample, as if none was kept
        let sample = matches!(&stored, Stored::Sample) || matches!(&stored, Stored::Image(img) if !img.is_empty() && img.len() != ferrite_store::sim::IMAGE_LEN);
        let stored = if sample { Stored::Sample } else { stored };
        let store = Box::new(FlashStore::mount(nor, Sample::new()));
        if matches!(stored, Stored::Sample) && store.first_boot() {
            seed(&store);
        }
        let mut radio = FakeRadio::new();
        let app = power_on(&store, &mut radio);
        if matches!(stored, Stored::Sample) {
            // The mock-ups' C: scanning airband, a few hits in.
            radio.cmd(ferrite_core::msg::RadioCmd::Scan { slot: 2, on: true });
            radio.live.slots[2].hits = 3;
        }
        let face = Face::new(scale);
        let out = vec![0u32; face.w * face.h];
        let mut s = SimCore {
            audio: ferrite_core::rxaudio::Packer::new(),
            audio_ms: 0,
            app,
            store,
            radio,
            dev: fake::device_at(0),
            lcd: Counting::new(),
            face,
            scale,
            out,
            now: 0,
            clock0: 0,
            booting: true,
            boot_end: BOOT_MS,
            boot_anim: false,
            off_at: None,
            off: false,
            kb_held: Vec::new(),
            mouse: None,
            knob_flash: (0, 0),
            aprs_s: u32::MAX,
            tx: None,
            battery: 0,
            layout: 0,
            last_key: String::from("-"),
            last_px: 0,
            flashes: Vec::new(),
            ports: {
                let mut p = Ports::new();
                p.set_cable_max_baud(921_600); // a pseudo-terminal has no rate
                p
            },
            term_out: Vec::new(),
            term_in: std::collections::VecDeque::new(),
            bt_in: std::collections::VecDeque::new(),
            bt_out: Vec::new(),
            kiss_in: Vec::new(),
            cmds: Vec::new(),
            log_cmds: false,
            passes: Vec::new(),
            watch: false,
            repaint_misses: Vec::new(),
            repaint: false,
            last_screen: "",
            last_page: 0,
            last_theme: 0,
            gps: None,
            gps_off: false,
            cut_restart: false,
        };
        if owner_card {
            ferrite_app::sample::owner_card(s.app.settings_mut());
        }
        s.draw_boot();
        s.last_px = s.lcd.pixels;
        s
    }

    /// The saved part of the flash ([`FLASH_IMAGE`]), for the caller to keep.
    pub fn flash_image(&self) -> Vec<u8> {
        self.store.nor().image()
    }

    /// The flash changed since the last call (time to save the image).
    pub fn flash_changed(&mut self) -> bool {
        core::mem::take(&mut self.store.nor().dirty)
    }

    /// Settings and slots saved now (the caller is about to quit).
    pub fn flush(&mut self) {
        if !self.booting {
            self.store.flush(&self.app);
        }
    }

    /// The boot screen from the app's settings, as the firmware draws it: the owner card if 0 7 3 3 says so.
    fn draw_boot(&mut self) {
        let set = *self.app.settings();
        ferrite_ui::theme::set(set.theme); // the boot screen in the saved theme (0 7 1)
        let info = BootInfo { version: VERSION, callsign: set.boot_callsign(), line: set.boot_line(), owner: set.boot_owner(), checks: &CHECKS, note: None };
        // BOOT-18: the logo screen animates as on the radio (no sound: the simulator has no DAC).
        self.boot_anim = !boot::shows_card(&info);
        if self.boot_anim {
            boot::draw_at(&mut self.lcd, &info, 0);
        } else {
            boot::draw(&mut self.lcd, &info);
        }
    }

    /// BOOT-18: the boot animation's frame for now, the last one the still screen.
    fn boot_frame(&mut self) {
        let t = self.now.saturating_sub(self.boot_end.saturating_sub(BOOT_MS));
        let set = *self.app.settings();
        let info = BootInfo { version: VERSION, callsign: set.boot_callsign(), line: set.boot_line(), owner: set.boot_owner(), checks: &CHECKS, note: None };
        boot::frame(&mut self.lcd, &info, t);
        self.boot_anim = t < boot::ANIM_MS;
    }

    /// The status bar clock: `utc_secs` is UTC now (seconds since 1970).
    pub fn set_clock(&mut self, utc_secs: u64) {
        self.clock0 = utc_secs.saturating_sub(self.now as u64 / 1000);
        self.store.set_time(utc_secs as u32); // the log's and messages' times in UTC from now
    }

    pub fn scale(&self) -> usize {
        self.scale
    }

    /// The panel's size in pixels at the current scale.
    pub fn size(&self) -> (usize, usize) {
        (self.face.w, self.face.h)
    }

    /// The screen's top-left inside the panel, in panel pixels.
    pub fn screen_origin(&self) -> (usize, usize) {
        (self.face.sx, self.face.sy)
    }

    /// A new whole scale (1 to [`MAX_SCALE`]); held parts are forgotten.
    pub fn set_scale(&mut self, scale: usize) {
        let scale = scale.clamp(1, MAX_SCALE);
        if scale != self.scale {
            self.scale = scale;
            self.face = Face::new(scale);
            self.out = vec![0u32; self.face.w * self.face.h];
            self.kb_held.clear();
            self.mouse = None;
        }
    }

    fn live(&self) -> bool {
        !self.booting
    }

    /// Whether MENU is held, by key or mouse.
    fn menu_held(&self) -> bool {
        self.face.find(Key::Menu).is_some_and(|i| self.kb_held.contains(&i) || self.mouse == Some(i))
    }

    /// Advance to `now_ms` (since start): boot and power-off timing, the fake radio following the app's commands,
    /// the app's tick and redraw.
    pub fn step(&mut self, now_ms: u32) {
        self.now = now_ms;
        ferrite_core::watch::set_aprs_first(self.app.settings().aprs_share == 0); // WATCH-17.3, as the UI task
        let now = now_ms;
        self.lcd.rects.clear();
        self.lcd.pixels = 0;
        if self.off_at.is_some_and(|t| now >= t) {
            // Saved: the radio is off and the screen dark until the switch is turned on again.
            (self.off_at, self.off, self.booting) = (None, true, true);
            self.lcd.fill(0, 0, W, H, 0);
        }
        if self.booting && !self.off && self.boot_anim {
            self.boot_frame();
        }
        if self.booting && !self.off && now > self.boot_end {
            self.booting = false;
            // As the firmware does: setup on a first boot (BOOT-12); MENU held through the boot screen: the
            // settings reset (PIN-6, BOOT-14); else the PIN if one is set.
            if self.store.first_boot() {
                self.app.first_boot();
            } else if self.menu_held() {
                self.app.power_on_reset();
            } else {
                self.app.ask_pin();
            }
        }
        if self.live() {
            let (app, radio, store, dev) = (&mut self.app, &mut self.radio, &*self.store, &mut self.dev);
            let (log, cmds) = (self.log_cmds, &mut self.cmds);
            let mut send = |radio: &mut FakeRadio, c| {
                if log {
                    cmds.push(c);
                }
                radio.cmd(c);
            };
            while let Some(c) = app.pop_cmd() {
                send(radio, c);
            }
            while let Some(w) = app.pop_write() {
                store.apply(w);
            }
            radio.step(now as f32 / 1000.0, app.active());
            if let Some(a) = radio.take_same() {
                app.same_alert(&a.header, a.slot, a.freq); // WX-4: the decoder's alert, over any screen
                let name = ferrite_core::same::event_name(&a.header.event).unwrap_or("Alert");
                store.log(&log_event(Src::Same, a.slot, a.freq, name.as_bytes(), &a.header.event)); // INBOX-6
            }
            // The pager decoder (DEC-4, DEC-18): the fake system's pages while a slot sits on 0 4 2's frequency.
            if let Some(p) = radio.take_page(app.pager_wanted(&radio.live)) {
                if let Some(e) = app.pager_hit(&p, &radio.live) {
                    store.log(&e);
                }
            }
            // The fake link (the LinkWrite event): a write session, 2 % a step, then ends; BACK cancels it (LINK-9).
            if dev.link.write_pct.is_some() && app.take_link_answer().is_some() {
                dev.link.write_pct = None;
            }
            if let Some(p) = dev.link.write_pct {
                if p >= 100 {
                    dev.link.write_pct = None;
                    app.link_write_done();
                } else {
                    dev.link.write_pct = Some(p + 2);
                }
            }
            // The APRS task, once a second (APRS-15 to APRS-20): arrivals notify, packets key C for a moment.
            if now / 1000 != self.aprs_s {
                self.aprs_s = now / 1000;
                let relay_ok = ferrite_core::txplan::relay_ok(app.tuned_freq(app.active()), app.aprs_freq());
                // APRS-29: beacons alone run the beacon's timer too (the card's countdown).
                let n = store.extra.aprs_tick(self.aprs_s, app.aprs_on() || app.aprs_holder() == Some(ferrite_app::aprs::Holder::Beacon), relay_ok);
                if let Some((from, text)) = n.inbound {
                    app.notify(from.as_bytes(), text.as_bytes(), false); // APRS-6
                    store.message_in(true, from.as_bytes(), text.as_bytes(), 0);
                    store.log(&log_event(Src::Aprs, 2, app.aprs_freq(), from.as_bytes(), text.as_bytes())); // INBOX-6
                }
                for (peer, text, st) in n.states.into_iter().flatten() {
                    store.message_state(peer.as_bytes(), text.as_bytes(), st);
                }
                if let Some(what) = n.tx {
                    if ferrite_app::screens::transmit::auto_tx(app, 2, what.as_bytes()) {
                        radio.burst(2);
                    }
                }
            }
            // GPS-23: a clock set by hand at 0 6 6 (the sim has no GPS time to take over)
            if let Some(u) = app.take_clock_set() {
                self.clock0 = (u as u64).saturating_sub(now as u64 / 1000);
                store.set_time(u);
            }
            let secs = self.clock0 + now as u64 / 1000;
            // GPS-3 as on the radio: the sample's fix while the GPS is on (0 6 1, or Auto while something needs it); off, the fixed position stands in
            // (0 6 9), else there is none. So a test gives the simulator a place with `set 0 6 1 Off`, `set 0 6 9`.
            // (A fix a test set itself is left alone while the GPS is on.)
            // GPS-1.5: Auto's look, as the firmware's UI task takes it; Low Power Mode left aside here, as before.
            ferrite_app::gpsdata::auto_step(app, store.data(), false, now);
            let gps_on = ferrite_app::gpsdata::cfg(app.settings(), app.gps_auto().powered()).on;
            let set = app.settings();
            if !gps_on {
                let fix = set.fixed_on.then_some(ferrite_app::data::Fix { pos: set.fixed, three_d: false, alt_m: 0, speed_kmh: 0, heading: 0, hdop_x10: 0, used: 0, age_s: ferrite_app::gpsdata::FIXED_AGE, utc_s: 0, phone: false });
                if store.extra.fix.get() != fix {
                    store.extra.fix.set(fix);
                }
                self.gps_off = true;
            } else if self.gps_off {
                store.extra.fix.set(Sample::sample_fix());
                self.gps_off = false;
            }
            // As the firmware's GPS task: local time by 0 6 3's zone, `Z` when that is UTC (SBAR-2, GPS-8).
            let (min, utc) = ferrite_app::gpsdata::local_clock(secs as u32, app.settings().tz);
            (dev.clock_min, dev.utc) = (Some(min), utc);
            // As the firmware's UI task: Low Power Mode's Auto (PWR-24.3) gets the battery once there is a reading.
            if dev.battery_mv != 0 {
                app.battery(dev.battery_pct);
            }
            // Satellite mode (SAT-5): the clock (a fixed day when the simulator's isn't set) and the fix or fixed position.
            let sat_t = if secs < 1_700_000_000 { ferrite_app::sat::SIM_T0 + now / 1000 } else { secs as u32 };
            let here = store.extra.fix.get().map(|f| (f.pos.lat, f.pos.lon, f.alt_m));
            app.sat_clock(Some(sat_t), true, here);
            app.tick(now, &radio.live, store);
            while let Some(c) = app.pop_cmd() {
                send(radio, c);
            }
            // The GPS module under the power manager, as the firmware's GPS task runs it (GPS-26).
            if let Some(g) = &mut self.gps {
                let d = ferrite_app::gpsdata::demand(app, store.data(), false, now);
                g.step(now, &d, radio.live.tx.keyed);
                (dev.gps, dev.sats) = g.state(now, d.on);
                dev.gps_phase = g.phase(now);
            }
            while let Some(w) = app.pop_write() {
                store.apply(w);
            }
            store.autosave(now, app);
            if self.watch {
                self.lcd.fb.begin_pass();
            }
            app.render(&mut self.lcd, &radio.live, dev, store);
            if self.repaint && !app.redraw_deferred() {
                // DISP-31: the same state drawn from scratch must give the same screen.
                let before: Vec<u16> = (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).map(|(x, y)| self.lcd.fb.at(x, y)).collect();
                app.redraw_all();
                app.render(&mut self.lcd, &radio.live, dev, store);
                let (mut n, mut b) = (0u32, (u16::MAX, u16::MAX, 0u16, 0u16));
                for y in 0..H {
                    for x in 0..W {
                        if before[(y * W + x) as usize] != self.lcd.fb.at(x, y) {
                            n += 1;
                            b = (b.0.min(x as u16), b.1.min(y as u16), b.2.max(x as u16), b.3.max(y as u16));
                        }
                    }
                }
                if n > 0 {
                    if std::env::var("FERRITE_REPAINT_DEBUG").is_ok() {
                        eprintln!("REPAINT miss at {now}: screen {} toast_up {} lock {}", app.screen().name(), app.toast_up(), app.locked());
                    }
                    let shots = (self.repaint_misses.len() < 8).then(|| (before, (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).map(|(x, y)| self.lcd.fb.at(x, y)).collect()));
                    self.repaint_misses.push(RepaintMiss { at: now, screen: app.screen().name(), pixels: n, area: b, shots });
                }
            }
            if self.watch {
                let screen = app.screen().name();
                let page = app.page_shown();
                let theme = app.settings().theme;
                let live = screen == self.last_screen && page == self.last_page && theme == self.last_theme;
                let report = self.lcd.fb.end_pass(live);
                if report.writes > 0 {
                    self.passes.push(PassHit { at: now, screen, live, report });
                }
                (self.last_screen, self.last_page, self.last_theme) = (screen, page, theme);
            }
            dev.bt = self.ports.bt_connected(app, now);
            self.ports.poll(app, store, dev, &radio.live, now, &mut TermBuf(&mut self.term_out), &mut TermBuf(&mut self.bt_out));
            // LINK-13.4: receive audio while the cable's session listens (the fake radio's: a tone)
            if self.ports.audio_wanted(now) != 0 {
                let mut blocks = std::vec::Vec::new();
                while self.audio_ms < now {
                    for k in 0..24u32 {
                        let t = (self.audio_ms * 24 + k) as f32 / 24_000.0;
                        let x = (8_000.0 * (2.0 * core::f32::consts::PI * 440.0 * t).sin()) as i16;
                        self.audio.push(x, |b| blocks.push(*b));
                    }
                    self.audio_ms += 1;
                }
                for b in &blocks {
                    self.ports.audio(b, &radio.live, now, &mut TermBuf(&mut self.term_out));
                }
            } else {
                self.audio_ms = now;
            }
            if let Some(r) = self.ports.debug_req() {
                self.ports.debug_answer(radio.debug(r)); // TERM-21: the fake radio answers at once
            }
            let _ = self.ports.take_baud(); // a pseudo-terminal keeps no rate
            if let Some(p) = self.ports.progress() {
                dev.link.write_pct = Some(p);
            }
            while let Some(w) = app.pop_write() {
                store.apply(w);
            }
        }
        if self.live() && (!self.term_in.is_empty() || !self.bt_in.is_empty() || self.ports.busy()) {
            self.term_pump(); // a load or a dump goes on between frames
        }
        if self.ports.take_reboot() {
            self.restart(); // LINK-24: REBOOT's answer has gone out; the radio restarts
        }
        // The Storage task's turn (`--cut-restart` only: the store's writes are done at once otherwise), after the
        // restart: what the link queued (in this step or fed before it) is still pending when the power goes.
        while let Some(j) = self.store.next_job() {
            ferrite_store::block(self.store.run(j));
        }
        if self.lcd.pixels > 0 {
            self.last_px = self.lcd.pixels;
        }
        self.flashes.extend(self.lcd.rects.iter().map(|&rect| Flash { rect, at: now }));
        self.flashes.retain(|f| now.wrapping_sub(f.at) < 250);
    }

    /// The whole panel as 0xRRGGBB pixels, `size()` wide and tall: the front panel, then the screen scaled up,
    /// with an optional mock-up blended over it (240 x 320 RGB) and optional redraw outlines.
    pub fn compose(&mut self, show_flash: bool, mockup: Option<&[u32]>) -> &[u32] {
        let mut pressed = self.kb_held.clone();
        pressed.extend(self.mouse);
        let knob = if self.now < self.knob_flash.1 { self.knob_flash.0 } else { 0 };
        // The LEDs as the firmware drives them (decision 63: RX, TX and the dark-screen heartbeat).
        let leds = if self.live() { self.app.leds(&self.radio.live, &self.dev) } else { Default::default() };
        // A dark screen (backlight off) shows dimmed, so the sim stays readable.
        let dark = self.live() && self.app.dark();
        let (face, lcd, flashes, scale) = (&self.face, &self.lcd, &self.flashes, self.scale);
        face.draw(&mut self.out, &pressed, knob, leds.green, leds.red, scale);
        face.put_screen(&mut self.out, scale, |x, y| {
            let mut c = rgb565_to_rgb(lcd.fb.at(x, y));
            if dark {
                c = (c >> 2) & 0x3F3F3F;
            }
            if let Some(m) = mockup {
                c = blend(c, m[(y * W + x) as usize]);
            }
            if show_flash
                && flashes.iter().any(|f| {
                    let (x0, y0, x1, y1) = f.rect;
                    (x == x0 || x == x1) && (y0..=y1).contains(&y) || (y == y0 || y == y1) && (x0..=x1).contains(&x)
                })
            {
                c = 0xFF00FF;
            }
            c
        });
        &self.out
    }

    /// A logical key down or up, from the keyboard (or the page's key buttons): lights its part on the panel.
    pub fn key(&mut self, key: Key, down: bool) {
        let part = self.face.find(key);
        if down {
            self.last_key = format!("{key:?}");
            if let Some(i) = part {
                if !self.kb_held.contains(&i) {
                    self.kb_held.push(i);
                }
            }
            if let Key::Ptt(n) = key {
                self.tx = Some(if n < 3 { n } else { 0 });
            }
            if self.live() {
                self.app.key_down(key, self.now, &*self.store);
            }
        } else {
            if let Some(i) = part {
                self.kb_held.retain(|&h| h != i);
            }
            if matches!(key, Key::Ptt(_)) {
                self.tx = None;
            }
            if self.live() {
                self.app.key_up(key, self.now, &*self.store);
            }
        }
    }

    /// The channel knob: one step down (-1) or up (+1).
    pub fn knob(&mut self, d: i8) {
        self.knob_flash = (d, self.now + 150);
        if self.live() {
            self.app.knob(d, self.now, &*self.store);
        }
    }

    /// The mouse at panel pixel (x, y), button `down` or not. Down on a key is its key down until the button comes
    /// up; down on the knob steps it by half; down on VOL flips the power switch.
    pub fn mouse(&mut self, x: i32, y: i32, down: bool) {
        if down && self.mouse.is_none() {
            let Some(i) = self.face.hit(x, y) else { return };
            let h = self.face.hits[i];
            match h.part {
                Part::Power => self.power_switch(),
                Part::Knob => {
                    self.knob(if x < h.x + h.w / 2 { -1 } else { 1 });
                    self.mouse = Some(i);
                }
                Part::Key(_) | Part::Ptt(_) => {
                    if let Part::Ptt(n) = h.part {
                        self.tx = Some(n);
                    }
                    let key = key_of(h.part);
                    self.last_key = format!("{key:?} (mouse)");
                    if self.live() {
                        self.app.key_down(key, self.now, &*self.store);
                    }
                    self.mouse = Some(i);
                }
            }
        } else if !down {
            if let Some(i) = self.mouse.take() {
                let part = self.face.hits[i].part;
                if let Part::Ptt(_) = part {
                    self.tx = None;
                }
                if matches!(part, Part::Key(_) | Part::Ptt(_)) && self.live() {
                    self.app.key_up(key_of(part), self.now, &*self.store);
                }
            }
        }
    }

    /// Bytes from the terminal program on the cable (ignored while booting or off).
    pub fn term_feed(&mut self, bytes: &[u8]) {
        if !self.live() {
            return;
        }
        self.term_in.extend(bytes);
        self.term_pump();
    }

    /// Feeds the waiting bytes while the terminal takes them, running each whole line as it comes (a load runs
    /// line by line); stops when the output is full or after a few hundred lines, to go on next frame.
    fn term_pump(&mut self) {
        for _ in 0..400 {
            for port in [Port::Cable, Port::Bt] {
                while self.ports.ready_for(port) {
                    let q = if port == Port::Cable { &mut self.term_in } else { &mut self.bt_in };
                    let Some(b) = q.pop_front() else { break };
                    match self.ports.route(&self.app, port, b, self.now) {
                        To::Kiss if self.kiss_in.len() < 4096 => self.kiss_in.push(b),
                        To::Kiss => {}
                        to => self.ports.feed(&mut self.app, &*self.store, &self.dev, &self.radio.live, to, b, self.now),
                    }
                    while let Some(w) = self.app.pop_write() {
                        self.store.apply(w); // one channel write per key
                    }
                }
            }
            self.ports.poll(&mut self.app, &*self.store, &self.dev, &self.radio.live, self.now, &mut TermBuf(&mut self.term_out), &mut TermBuf(&mut self.bt_out));
            if let Some(r) = self.ports.debug_req() {
                self.ports.debug_answer(self.radio.debug(r));
            }
            while let Some(w) = self.app.pop_write() {
                self.store.apply(w);
            }
            if self.term_in.is_empty() && self.bt_in.is_empty() && self.ports.ready() || self.term_out.len() >= TERM_BUF / 2 || self.bt_out.len() >= TERM_BUF / 2 {
                break;
            }
        }
    }

    /// Everything as the terminal's `dump` writes it (TERM-13): text, one command per line, ending with `end`.
    pub fn dump_text(&self) -> Vec<u8> {
        let (mut p, mut l, mut v) = (cmd::DumpPos::default(), cmd::Line::new(), Vec::new());
        while cmd::dump_line(&self.app, &*self.store, &self.dev, &mut p, &mut l) {
            v.extend_from_slice(l.as_bytes());
            v.push(b'\n');
        }
        v
    }

    /// A dump read back as the terminal's `load` reads it, line by line up to `end` (TERM-13): what each refused
    /// line was refused for, then the count.
    pub fn load_text(&mut self, text: &[u8]) -> String {
        let (mut n, mut ok, mut bad, mut report) = (0u32, 0u32, 0u32, String::new());
        if !self.live() {
            return String::from("The radio is off or starting: switch it on first.\n");
        }
        for line in text.split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.iter().filter(|b| !b.is_ascii_whitespace()).eq(b"end".iter()) {
                break;
            }
            n += 1;
            let r = if line.len() > cmd::LINE_MAX {
                Err(cmd::Msg::from(b"too long: 255 characters at most"))
            } else {
                cmd::run(&mut self.app, &*self.store, &self.dev, line, true)
            };
            match r {
                Ok(cmd::Done::Nothing) => {}
                Ok(_) => ok += 1,
                Err(m) => {
                    bad += 1;
                    report.push_str(&format!("line {n}: {}\n", String::from_utf8_lossy(m.as_bytes())));
                }
            }
            while let Some(w) = self.app.pop_write() {
                self.store.apply(w);
            }
            while let Some(c) = self.app.pop_cmd() {
                self.radio.cmd(c);
            }
        }
        report.push_str(&format!("{n} lines read: {ok} applied, {bad} refused.\n"));
        report
    }

    /// Bytes from a phone on Bluetooth (roadmap 3d): the terminal, the link or KISS, as the radio routes them.
    pub fn bt_feed(&mut self, bytes: &[u8]) {
        if !self.live() {
            return;
        }
        self.bt_in.extend(bytes);
        self.term_pump();
    }

    /// What went out on Bluetooth since the last call.
    pub fn bt_take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bt_out)
    }

    /// What the terminal drew since the last call, for the cable.
    pub fn term_take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.term_out)
    }

    /// The pending output, without taking it (the web page reads it in place).
    pub fn term_pending(&self) -> &[u8] {
        &self.term_out
    }

    /// Drops what [`SimCore::term_pending`] showed.
    pub fn term_clear(&mut self) {
        self.term_out.clear();
    }

    /// The terminal's size from outside (the web page's pane); a terminal program answers the size question itself.
    pub fn term_size(&mut self, cols: u16, rows: u16) {
        self.ports.term.set_size(cols, rows);
    }

    /// The power switch on the volume knob (PE0): off saves ("Saving...", PWR-17) then goes dark; on is a fresh
    /// boot.
    fn power_switch(&mut self) {
        if self.off {
            // On: RAM is gone, so the app starts again from what the flash kept.
            let flash = std::mem::replace(&mut *self.store, FlashStore::mount(SimFlash::new(), Sample::new())).into_parts().0;
            *self.store = FlashStore::mount(flash, Sample::new()); // the same place: the App's satellite source reads it
            self.radio = FakeRadio::new();
            self.app = power_on(&self.store, &mut self.radio);
            (self.off, self.boot_end) = (false, self.now + BOOT_MS);
            self.ports = Ports::new(); // the cable and Bluetooth start over too: Enter opens the terminal again
            self.ports.set_cable_max_baud(921_600);
            self.term_out.clear();
            self.term_in.clear();
            self.bt_in.clear();
            self.bt_out.clear();
            self.draw_boot();
            self.last_key = String::from("power on");
        } else if self.booting {
            (self.off, self.booting) = (true, true);
            self.lcd.fill(0, 0, W, H, 0);
            self.last_key = String::from("power off");
        } else if self.off_at.is_none() {
            // PWR-17: "Saving...", the pending writes, then off.
            self.app.power_off();
            self.store.flush(&self.app);
            self.off_at = Some(self.now + SAVE_MS);
            self.last_key = String::from("power off");
        }
    }

    /// LINK-24's restart, as the firmware's REBOOT does it: what is pending is written, then a fresh boot from
    /// what the flash kept (RAM gone, the link sessions start over). The stock bootloader's 200 ms listen has
    /// nothing to stand in for: the boot screen follows at once. What the cable was already sent stays queued.
    fn restart(&mut self) {
        // `--cut-restart`, once: a power cut instead, after a raw write (nothing pending is written)
        let cut = self.cut_restart && self.store.raw();
        if !self.booting && !cut {
            self.store.flush(&self.app);
            while let Some(j) = self.store.next_job() {
                ferrite_store::block(self.store.run(j));
            }
        }
        if cut {
            self.cut_restart = false;
        }
        let flash = std::mem::replace(&mut *self.store, FlashStore::mount(SimFlash::new(), Sample::new())).into_parts().0;
        *self.store = FlashStore::mount(flash, Sample::new()); // the same place: the App's satellite source reads it
        self.radio = FakeRadio::new();
        self.app = power_on(&self.store, &mut self.radio);
        (self.off, self.off_at, self.booting, self.boot_end) = (false, None, true, self.now + BOOT_MS);
        self.ports = Ports::new();
        self.ports.set_cable_max_baud(921_600);
        self.term_in.clear();
        self.bt_in.clear();
        self.draw_boot();
        self.last_key = String::from(if cut { "power cut" } else { "restart" });
    }

    /// One of the simulator's stand-ins for what the radio would report (most are ignored while booting or off).
    pub fn event(&mut self, e: Event) {
        let live = self.live();
        let app = &mut self.app;
        match e {
            Event::Message if live => app.notify_message(b"VE3KPW-9", b"On my way"),
            Event::Alert if live => {
                // The fake decoder's SAME warning now, as if heard on slot C (it also comes by itself 8 s after a
                // slot settles on a weather channel).
                if let Some(h) = ferrite_core::same::parse(fake::SAME_HEADER) {
                    app.same_alert(&h, 2, 16_255_000);
                }
            }
            Event::Crossband if live => app.open(Dest::Crossband),
            Event::Layout if live => {
                self.layout = (self.layout + 1) % 3;
                app.set_layout(self.layout);
            }
            Event::Beacon if live => {
                // The checks of an automatic transmission, then a short burst on the active slot, as the APRS
                // task's beacons do.
                let a = app.active();
                if ferrite_app::screens::transmit::auto_tx(app, a, b"Beacon") {
                    self.radio.burst(a);
                }
            }
            Event::Gps => {
                self.dev.gps = match self.dev.gps {
                    GpsState::Off => GpsState::Searching,
                    GpsState::Searching => GpsState::Fix,
                    GpsState::Fix => GpsState::Off,
                }
            }
            Event::FmStation if live => {
                let a = app.active();
                let f = if app.slot(a).freq() == freq::mhz(100) + 50_000 { freq::mhz(99) + 10_000 } else { freq::mhz(100) + 50_000 };
                app.set_vfo(a, f);
            }
            Event::ToneHint if live => {
                let a = app.active();
                self.radio.hint = if self.radio.hint.is_some() { None } else { Some((a, 885)) };
            }
            Event::ScreenOff if live => {
                self.radio.quiet = !self.radio.quiet;
                if self.radio.quiet {
                    app.backlight_out();
                }
            }
            Event::Bluetooth => self.dev.bt = !self.dev.bt,
            Event::SpeakerMic => self.dev.spkmic = !self.dev.spkmic,
            Event::Battery => {
                self.battery = (self.battery + 1) % 3;
                let d = &mut self.dev;
                (d.battery_low, d.battery_cutoff, d.battery_pct, d.battery_mv) =
                    [(false, false, 82, 7700), (true, false, 15, 7000), (true, true, 3, 6600)][self.battery];
            }
            Event::PowerSwitch => self.power_switch(),
            Event::EmptyBattery if live => app.battery_empty(),
            Event::SsbPatch if live => app.ssb_patch_found(),
            Event::LinkWrite if live => {
                // A web app writing (LINK-9): no approval since decision 104, so straight to the progress screen.
                self.dev.link.write_pct = Some(0);
                app.link_write_progress();
            }
            _ => {}
        }
    }

    /// Opens screen `i` of [`jump_names`] (0 = home), from home, so BACK returns there.
    /// Record every render pass's flicker and overdraw in [`SimCore::passes`] (tests, DISP-25).
    /// DISP-31: draw every frame twice, the second time from scratch, and record where they differ
    /// ([`SimCore::repaint_misses`]). Slower; tests only.
    pub fn check_repaint(&mut self, on: bool) {
        self.repaint = on;
    }

    pub fn watch_passes(&mut self, on: bool) {
        self.watch = on;
        self.passes.clear();
        self.lcd.fb.clear_heat();
    }

    /// Flickering passes per pixel since [`SimCore::watch_passes`] (row-major, 240 x 320).
    pub fn flicker_heat(&self) -> Vec<u32> {
        self.lcd.fb.heat().map(|h| h.to_vec()).unwrap_or_default()
    }

    pub fn jump(&mut self, i: usize) {
        if !self.live() {
            return;
        }
        self.app.go_home();
        if let Some(&d) = i.checked_sub(1).and_then(|i| nav::ALL.get(i)) {
            self.app.open(d);
        }
    }

    fn screen(&self) -> Option<AppScreen> {
        self.live().then(|| self.app.screen())
    }

    /// The screen on show: "Boot" while booting, "Off" while off.
    pub fn screen_name(&self) -> &'static str {
        if self.off {
            return "Off";
        }
        self.screen().map_or("Boot", |s| s.name())
    }

    /// The look-review mock-up for the screen on show (a placeholder has none: the boot one stands in).
    pub fn mockup_name(&self) -> &'static str {
        self.screen().map_or(Some("BootA.png"), |s| s.mockup()).unwrap_or("BootA.png")
    }

    /// One line of state: screen, last key, TX, the active slot and frequency, lock, the last redraw's pixels.
    pub fn status(&self) -> String {
        let a = self.app.active();
        format!(
            "{} · key {} · {}{} {} · last redraw {} px{}",
            self.screen_name(),
            self.last_key,
            self.tx.map(|n| format!("TX {} · ", ["A", "B", "C"][n as usize % 3])).unwrap_or_default(),
            ["A", "B", "C"][a as usize],
            freq_text(self.app.slot(a).freq()),
            self.last_px,
            if self.app.locked() { " · locked" } else { "" },
        )
    }

    /// Pixels pushed by the last redraw that drew anything.
    pub fn last_px(&self) -> u64 {
        self.last_px
    }

    /// Booting or off: the app is not running.
    pub fn booting(&self) -> bool {
        self.booting
    }

    // ---- For the end-to-end and screenshot tests (sim/tests) ----

    /// The App, as the firmware's UI task holds it.
    pub fn app(&self) -> &App {
        &self.app
    }
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }
    /// The fake radio (its live snapshot, traffic switch).
    pub fn radio(&self) -> &FakeRadio {
        &self.radio
    }
    pub fn radio_mut(&mut self) -> &mut FakeRadio {
        &mut self.radio
    }
    /// The store on the simulated SPI flash.
    pub fn store(&self) -> &FlashStore<SimFlash, Sample> {
        &self.store
    }
    /// The fake GPS module run by the power manager (GPS-26 to GPS-33): `Some(indoor)` starts it (or moves it
    /// indoors or out), None stops it (the status bar back to F5's state).
    pub fn gps_model(&mut self, indoor: Option<bool>) {
        match indoor {
            Some(i) => self.gps.get_or_insert_with(crate::fakegps::FakeGps::new).indoor = i,
            None => self.gps = None,
        }
    }

    pub fn gps(&self) -> Option<&crate::fakegps::FakeGps> {
        self.gps.as_ref()
    }

    /// What the radio reports (battery, GPS, Bluetooth...).
    pub fn device(&self) -> &Device {
        &self.dev
    }
    /// `--sats N`: N satellites (the sample over and over), so a test can watch the pass list fill in (SAT-6).
    pub fn many_sats(&mut self, n: usize) {
        let s = ferrite_app::sat::sample();
        let v: Vec<ferrite_core::sat::Sat> = (0..n.min(32)).map(|i| ferrite_core::sat::Sat { id: s[i % 4].id + i as u32 * 100_000, ..s[i % 4] }).collect();
        let list = ferrite_core::sat::List(Box::leak(v.into_boxed_slice()));
        self.app.sat_attach(Box::leak(Box::new(ferrite_core::sat::data::Live::new(list))));
    }

    pub fn device_mut(&mut self) -> &mut Device {
        &mut self.dev
    }
    /// The LEDs as the firmware drives them now: (green, red).
    pub fn leds(&self) -> (bool, bool) {
        if !self.live() {
            return (false, false);
        }
        let l = self.app.leds(&self.radio.live, &self.dev);
        (l.green, l.red)
    }
    /// Record every command the App sends the radio in [`SimCore::cmds`].
    pub fn log_cmds(&mut self, on: bool) {
        self.log_cmds = on;
    }
    /// The text on the screen now, as drawn (each string with its box), top to bottom, left to right.
    pub fn screen_texts(&mut self) -> Vec<TextItem> {
        let mut v = self.lcd.visible();
        v.sort_by_key(|t| (t.y0, t.x0));
        v
    }
    /// The screen's text as lines: strings whose boxes overlap vertically joined left to right with a space.
    pub fn screen_text(&mut self) -> String {
        let mut lines: Vec<(i32, i32, Vec<TextItem>)> = Vec::new();
        for t in self.screen_texts() {
            match lines.iter_mut().find(|(y0, y1, _)| t.y0 <= *y1 && *y0 <= t.y1) {
                Some(l) => l.2.push(t),
                None => lines.push((t.y0, t.y1, vec![t])),
            }
        }
        lines.sort_by_key(|l| l.0);
        lines
            .into_iter()
            .map(|(_, _, mut items)| {
                items.sort_by_key(|t| t.x0);
                items.into_iter().map(|t| t.s).collect::<Vec<_>>().join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// The UI readback (LINK-19, `ui`), as the link sends it.
    pub fn readback(&self) -> String {
        let (mut out, mut l, mut i) = (String::new(), ferrite_app::term::cmd::Line::new(), 0);
        while ferrite_app::uistate::line(&self.app, &*self.store, &self.dev, &self.radio.live, i, &mut l) {
            out.push_str(&String::from_utf8_lossy(l.as_bytes()));
            out.push('\n');
            i += 1;
        }
        out
    }

    /// Bluetooth as if at its unlocked rate (LINK-25.1): the Live View's faster cap (LINK-19.5).
    pub fn set_bt_fast(&mut self, fast: bool) {
        self.ports.set_bt_fast(fast);
    }

    /// LINK-19.5: the readback with the screen's texts, as a session that asked for them gets it (`UI` what=3).
    pub fn readback_texts(&mut self) -> String {
        let (mut out, mut l, mut i) = (String::new(), ferrite_app::term::cmd::Line::new(), 0);
        while ferrite_app::uistate::line_all(&mut self.app, &*self.store, &self.dev, &self.radio.live, true, i, &mut l) {
            out.push_str(&String::from_utf8_lossy(l.as_bytes()));
            out.push('\n');
            i += 1;
        }
        out
    }

    /// The App's bytes: a test that drawing the screen again for the readback changed nothing (LINK-19.5).
    pub fn app_bytes(&self) -> Vec<u8> {
        // SAFETY: reading the App's own bytes, no more; it is plain data (the comparison is only ever with itself).
        unsafe { core::slice::from_raw_parts((&self.app as *const ferrite_app::App).cast::<u8>(), core::mem::size_of::<ferrite_app::App>()) }.to_vec()
    }

    /// The screen alone, 240 x 320, 0xRRGGBB (not dimmed when dark): for the goldens.
    pub fn screen_rgb(&self) -> Vec<u32> {
        let mut v = Vec::with_capacity((W * H) as usize);
        for y in 0..H {
            for x in 0..W {
                v.push(rgb565_to_rgb(self.lcd.fb.at(x, y)));
            }
        }
        v
    }
    /// Whether the screen is dark (backlight off).
    pub fn dark(&self) -> bool {
        self.live() && self.app.dark()
    }
}

/// The settings from the flash (the defaults on a first boot), kept for the program's life as the firmware keeps
/// its own; the App on them with its slots restored, and its start-up commands sent to the fake radio.
fn power_on(store: &Store, radio: &mut FakeRadio) -> App {
    let set: &'static mut Settings = Box::leak(Box::new(Settings::DEFAULT));
    store.load_settings(set);
    let alert = Box::leak(Box::new(ferrite_app::screens::alert::AlertInfo::NONE));
    let mut app = App::new(set, alert);
    // Satellite mode: the satellites in the store (SAT-2, the sample's seeded), predicted on the host as on the
    // radio (SAT-6).
    // SAFETY: the store is boxed in the SimCore with this App, and replaced only together with it (a restart makes a
    // new App on the new store); nothing reads the old App's source after.
    let st: &'static Store = unsafe { &*(store as *const Store) };
    let sats = ferrite_core::sat::data::Live::new(ferrite_store::sats::StoreSats(st));
    app.sat_attach(Box::leak(Box::new(sats)));
    store.restore(&mut app);
    while let Some(c) = app.pop_cmd() {
        radio.cmd(c);
    }
    app
}

/// An event for the log, heard now.
fn log_event(src: Src, slot: u8, freq: freq::Freq10, name: &[u8], text: &[u8]) -> ferrite_app::data::Event {
    use ferrite_ui::text::Buf;
    ferrite_app::data::Event { src, ago_s: 0, freq, slot, unread: true, id: 0, x: 0, y: 0, pos: None, name: Buf::from(name), text: Buf::from(text) }
}

/// The look review's sample written to a blank flash: its zones, channels (with positions) and range presets,
/// then the sample settings and slots (B on "Club repeater", C scanning airband, watch A+B+C).
fn seed(store: &Store) {
    let sample = Sample::new();
    for z in 0..sample.zones() {
        let name = sample.zone_name(z).unwrap_or_default();
        store.add_zone(name.as_bytes(), &sample.zone_cfg(z));
    }
    for i in 0..sample.count() {
        if let Some(c) = sample.by_number(i) {
            store.put_with_position(c, sample.position(c.num));
        }
    }
    for i in 0..sample.presets() {
        if let Some(p) = sample.preset(i) {
            store.add_preset(&p);
            // LINK-58.1: and its skipped frequencies, so no entry is left blank (erased flash)
            let mut f = [0; ferrite_app::data::SKIP_CHUNK];
            let n = (p.skips as usize).min(f.len());
            for (j, v) in f.iter_mut().enumerate().take(n) {
                *v = sample.preset_skip(i, j as u8).unwrap_or(0);
            }
            if n > 0 {
                store.request(ferrite_app::data::Op::PresetSkips { preset: i, at: 0, count: n as u8, f });
            }
        }
    }
    // Satellite mode's sample (SAT-2): data.ferritefw.dev's fixture as Console would write it, with the declination
    // at the sample's fix.
    store.request(ferrite_app::data::Op::ClearSats);
    for (k, s) in ferrite_app::sat::sample().iter().enumerate() {
        store.sat_stage(s);
        store.request(ferrite_app::data::Op::PutSat(k as u8));
    }
    let info = ferrite_core::sat::wire::Info { decl: Some(ferrite_app::sat::SIM_DECL), stamp: 1_793_500_000, written: ferrite_app::sat::SIM_T0 };
    store.request(ferrite_app::data::Op::SatInfo(Some(info)));
    // The event log, messages and waypoints, oldest first, each as old as the sample says.
    for i in (0..sample.events()).rev() {
        if let Some(e) = sample.event(i) {
            store.log(&e);
        }
    }
    for i in (0..sample.messages()).rev() {
        if let Some(m) = sample.message(i) {
            store.message_in(m.inbound, m.peer.as_bytes(), m.text.as_bytes(), m.ago_s);
            match m.state {
                MsgState::Read => {
                    store.request(ferrite_app::data::Op::MessageRead(0));
                }
                MsgState::Unread | MsgState::Pending => {}
                s => {
                    if let MsgState::Retrying(n) = s {
                        for k in 1..n {
                            store.message_state(m.peer.as_bytes(), m.text.as_bytes(), if k == 1 { MsgState::Pending } else { MsgState::Retrying(k) });
                        }
                    }
                    store.message_state(m.peer.as_bytes(), m.text.as_bytes(), s);
                }
            }
        }
    }
    for i in (0..sample.waypoints()).rev() {
        if let Some(w) = sample.waypoint(i) {
            store.add_waypoint(&w);
        }
    }
    let mut radio = FakeRadio::new();
    let mut app = fake::app();
    fake::sample(&mut app, store, &mut radio);
    store.save(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &mut SimCore, from: u32, to: u32) {
        for t in (from..=to).step_by(20) {
            s.step(t);
        }
    }

    /// LINK-58.1: the seeded sample's range presets hold their skipped frequencies, each within its range: none
    /// left blank (erased flash), so `what` 11 sends what the sample has.
    #[test]
    fn seeded_preset_skips_have_no_blank_entries() {
        let s = SimCore::new(1, false);
        let mut seen = 0;
        for i in 0..s.store.presets() {
            let p = s.store.preset(i).unwrap();
            for j in 0..p.skips {
                let f = s.store.preset_skip(i, j).unwrap();
                assert!((p.start..=p.end).contains(&f), "preset {i} skip {j}: {f:#x}");
                seen += 1;
            }
        }
        assert!(seen > 0, "the sample has some");
    }

    #[test]
    fn the_flash_keeps_settings_through_the_power_switch_and_the_image() {
        let mut s = SimCore::new(1, false);
        run(&mut s, 0, 2500);
        assert_eq!(s.screen_name(), "Home");
        assert!(s.store.count() > 10, "the sample's channels are in the flash");
        s.event(Event::Layout);
        run(&mut s, 2520, 5000); // saved 2 s after the change
        let layout = s.app.settings().layout;
        assert_eq!(layout, 1);
        s.event(Event::PowerSwitch);
        run(&mut s, 5020, 7000);
        assert_eq!(s.screen_name(), "Off");
        s.event(Event::PowerSwitch);
        run(&mut s, 7020, 9500);
        assert_eq!(s.screen_name(), "Home");
        assert_eq!(s.app.settings().layout, layout, "kept through the power switch");
        assert_eq!(s.app.slot(1).ch.map(|c| c.num), Some(11), "B still on Club repeater");
        // The image, as the desktop and the page keep it.
        let img = s.flash_image();
        let mut t = SimCore::with_flash(1, false, Stored::Image(img));
        run(&mut t, 0, 2500);
        assert_eq!(t.app.settings().layout, layout);
    }

    #[test]
    fn a_blank_flash_runs_setup() {
        let mut s = SimCore::with_flash(1, false, Stored::Blank);
        run(&mut s, 0, 2500);
        assert!(matches!(s.app.screen(), AppScreen::Setup(_)), "{}", s.screen_name());
        assert_eq!(s.store.count(), 0);
    }

    /// A zone scan on a channel slot runs in the (fake) Radio task from the set the UI streams over and the
    /// frequencies it answers for: it visits the zone's channels and nothing else (SCAN-3, decision 86).
    #[test]
    fn a_zone_scan_walks_the_zone_in_the_radio() {
        let mut s = SimCore::new(1, false);
        run(&mut s, 0, 2500);
        let mut t = 2520;
        while s.app.active() != 1 {
            s.key(ferrite_core::keys::Key::Slot, true);
            s.key(ferrite_core::keys::Key::Slot, false);
            run(&mut s, t, t + 100);
            t += 120;
        }
        let zone = s.app.slot(1).zone;
        let zone_freqs: std::vec::Vec<u32> = (0..s.store.zone_len(zone)).filter_map(|i| s.store.zone_at(zone, i)).filter(|c| !c.skip() && !c.is_hf()).map(|c| c.rx).collect();
        s.key(ferrite_core::keys::Key::Side(2), true);
        s.key(ferrite_core::keys::Key::Side(2), false);
        let mut seen = std::vec::Vec::new();
        for k in 0..400 {
            s.step(t + k * 20);
            let f = s.radio.live.slots[1].freq;
            if !seen.contains(&f) {
                seen.push(f);
            }
        }
        assert!(s.radio.live.slots[1].scanning, "still scanning");
        assert!(seen.len() >= 2.min(zone_freqs.len()), "moved through the zone: {seen:?}");
        for f in &seen {
            assert!(zone_freqs.contains(f), "{f} is not one of the zone's channels {zone_freqs:?}");
        }
    }
}
