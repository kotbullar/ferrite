//! # Screens
//!
//! One module per screen (or family of screens), plugged into the App through this registry. A screen is a
//! small `Copy` state struct (its cursor, its page; reset each time it opens) that implements [`View`]: what
//! its keys, the knob and PTT do, how it times out, how it draws, and what its hint bar says. Everything the
//! screens share lives on the [`App`] (slots, toast, undo, commands), in [`crate::settings`] (radio-wide
//! settings, outside the App) and in [`shared`] (region dirty-tracking, pages, status bar, hint bar, list rows,
//! the edit grid, help box), with [`lists`] (group C's list pages), [`forms`] (group A's fields and pickers),
//! [`parts`] (group B's meters and strips) and [`table`] (label + value pages as data) for families of screens.
//!
//! One key is MENU and OK: on every screen a short press is OK (the screen's main action) and holding it opens
//! that screen's options (MENU-25); home is the exception the manual makes (MENU: quick settings, hold:
//! advanced).
//!
//! ## Adding a screen
//!
//! 1. A new file `screens/<name>.rs`: `pub struct Name { .. }` deriving `Clone, Copy, Debug, PartialEq, Eq,
//!    Hash`, and `impl View for Name`. Only `name`, `key`, `render` and `hints` are required; copy
//!    [`coming`] or [`status`] as a start.
//! 2. Here, three lines: `pub mod name;`, a variant `Name(name::Name)` in [`Screen`], and the matching line in
//!    `each!`.
//! 3. In [`nav`]: its [`nav::Dest`] arm in [`nav::enter`] (the placeholder used until then), so every way in
//!    (hold-digit shortcuts, MENU 0 paths, PTT, digits, OK on a row) opens the real screen.
//!
//! A screen changes screens by assigning `app.screen` (moving within itself, or to a screen it knows), by
//! [`App::open`] (anything in the manual, remembered for BACK), or by `app.back()` / `app.go_home()`.
//! Screens own regions `shared::R_*` while they are up; the content area is cleared and every region forgotten
//! when the screen's variant changes.
//!
//! RAM: the [`Screen`] enum is as large as its largest state, and it is held in the App and in the BACK stack
//! (five copies), so keep screen state to a few bytes (8 is the aim; the forms that hold a draft, Save and
//! Tones, are 12-16 for now). Lists read their items on demand (channels from the
//! store, settings from tables), never a copy.

/// DISP-25: `$body` (drawing on `$lcd`) straight onto the panel; or, when `$clear` (a box whose layout changed), off
/// the screen in bands over the box, so its old content goes to the new without a blank between.
macro_rules! redraw_box {
    // The body is compiled once, as one closure on a `Band`: straight to the panel when not clearing (size review #9).
    ($clear:expr, $lcd:ident, ($x:expr, $y:expr, $w:expr, $h:expr, $bg:expr), $body:block) => {{
        let mut draw = |$lcd: &mut ferrite_ui::canvas::Band| $body;
        if $clear {
            ferrite_ui::canvas::banded($lcd, $x, $y, $w, $h, $bg, &mut draw);
        } else {
            draw(&mut ferrite_ui::canvas::direct($lcd));
        }
    }};
}

pub mod activity;
pub mod advanced;
pub mod alert;
pub mod aprs;
pub mod contacts;
pub mod browse;
pub mod chedit;
pub mod coming;
pub mod confirm;
pub mod dtmf;
pub mod edit;
pub mod entry;
pub mod fm;
pub(crate) mod forms;
pub mod gps;
pub mod groups;
pub mod help;
pub mod hf;
pub mod home;
pub mod info;
pub mod keytest;
pub mod layouts;
pub mod link;
pub(crate) mod lists;
pub mod log;
pub mod lowpower;
pub mod msgs;
pub mod nav;
pub mod opts;
pub mod posentry;
pub mod parts;
pub mod pin;
pub mod probe;
pub mod qgrid;
pub mod qmenu;
pub mod quick;
pub mod recall;
pub mod save;
pub mod samecodes;
pub mod autoreply;
pub mod sat;
pub mod scan;
pub mod scanlists;
pub mod scope;
pub mod setup;
pub(crate) mod shared;
pub mod shortcut;
pub mod snake;
pub mod squelch;
pub mod status;
pub(crate) mod table;
pub mod text;
pub mod tones;
pub mod transmit;
pub mod value;
pub mod xb;
pub mod zones;

#[cfg(test)]
mod cov;

use crate::chan::ChannelStore;
use crate::keys::KeyEvent;
use crate::App;
use ferrite_core::msg::{Device, LiveAll, SLOTS};
use ferrite_ui::gfx::{Gfx as _, BG};
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;
pub use shared::{Cx, Hints};

/// SAFE-14's popup head: home keeps drawing under it (it lasts as long as PTT is held).
pub(crate) const STUCK_HEAD: &[u8] = b"PTT stuck";

/// Which screen is up, with its state. One variant per screen module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Screen {
    /// Home (chapter 6): stateless, so a plain variant.
    Home,
    Edit(edit::Edit),
    Quick(quick::Quick),
    Snake(snake::Snake),
    /// MENU-34: the APRS slot's quick page (hold ABC).
    QGrid(qgrid::QGrid),
    Advanced(advanced::Advanced),
    Status(status::Status),
    Entry(entry::Entry),
    ChEdit(chedit::ChEdit),
    Value(value::Value),
    Tones(tones::Tones),
    Text(text::Text),
    Save(save::Save),
    Confirm(confirm::Confirm),
    Shortcut(shortcut::Shortcut),
    // Group B, radio activity.
    Transmit(transmit::Transmit),
    TxRefused(transmit::TxRefused),
    Emergency(transmit::Emergency),
    Scan(scan::Scan),
    Scope(scope::Scope),
    ToneSearch(probe::ToneSearch),
    Counter(probe::Counter),
    Crossband(xb::Crossband),
    Squelch(squelch::Squelch),
    Group(groups::Group),
    Setup(setup::Setup),
    ExtendTx(setup::ExtendTx),
    Pin(pin::Pin),
    Info(info::Info),
    KeyTest(keytest::KeyTest),
    Link(link::Link),
    Hf(hf::Hf),
    Alert(alert::Alert),
    /// A screen not built yet: its name and "coming" (any [`nav::Dest`] without its own screen).
    Coming(coming::Coming),
    // Group C: lists and data
    Log(log::Log),
    Options(opts::Options),
    Aprs(aprs::Aprs),
    AprsSent(aprs::Sent),
    Msgs(msgs::Msgs),
    Compose(msgs::Compose),
    QuickMsgs(msgs::QuickMsgs),
    Contacts(contacts::Contacts),
    Gps(gps::Gps),
    Browse(browse::Browse),
    Zones(zones::Zones),
    ScanLists(scanlists::ScanLists),
    Recall(recall::Recall),
    PosEntry(posentry::PosEntry),
    SameCodes(samecodes::SameCodes),
    AutoReply(autoreply::AutoReply),
    Dtmf(dtmf::DtmfPad),
    /// Satellite mode (chapter 28a): the pass list, pass page and pointing page.
    Sat(sat::SatScr),
}

/// Runs `$e` with `$s` bound to the screen's state: the one place that lists every variant.
macro_rules! each {
    ($screen:expr, $s:ident => $e:expr) => {
        match $screen {
            Screen::Home => {
                let $s = home::Home;
                $e
            }
            Screen::Edit($s) => $e,
            Screen::Quick($s) => $e,
            Screen::Snake($s) => $e,
            Screen::QGrid($s) => $e,
            Screen::Advanced($s) => $e,
            Screen::Status($s) => $e,
            Screen::Entry($s) => $e,
            Screen::ChEdit($s) => $e,
            Screen::Value($s) => $e,
            Screen::Tones($s) => $e,
            Screen::Text($s) => $e,
            Screen::Save($s) => $e,
            Screen::Confirm($s) => $e,
            Screen::Shortcut($s) => $e,
            Screen::Transmit($s) => $e,
            Screen::TxRefused($s) => $e,
            Screen::Emergency($s) => $e,
            Screen::Scan($s) => $e,
            Screen::Scope($s) => $e,
            Screen::ToneSearch($s) => $e,
            Screen::Counter($s) => $e,
            Screen::Crossband($s) => $e,
            Screen::Squelch($s) => $e,
            Screen::Group($s) => $e,
            Screen::Setup($s) => $e,
            Screen::ExtendTx($s) => $e,
            Screen::Pin($s) => $e,
            Screen::Info($s) => $e,
            Screen::KeyTest($s) => $e,
            Screen::Link($s) => $e,
            Screen::Hf($s) => $e,
            Screen::Alert($s) => $e,
            Screen::Coming($s) => $e,
            Screen::Log($s) => $e,
            Screen::Options($s) => $e,
            Screen::Aprs($s) => $e,
            Screen::AprsSent($s) => $e,
            Screen::Msgs($s) => $e,
            Screen::Compose($s) => $e,
            Screen::QuickMsgs($s) => $e,
            Screen::Contacts($s) => $e,
            Screen::Gps($s) => $e,
            Screen::Browse($s) => $e,
            Screen::Zones($s) => $e,
            Screen::ScanLists($s) => $e,
            Screen::Recall($s) => $e,
            Screen::PosEntry($s) => $e,
            Screen::SameCodes($s) => $e,
            Screen::AutoReply($s) => $e,
            Screen::Dtmf($s) => $e,
            Screen::Sat($s) => $e,
        }
    };
}

/// What a screen does. Methods take the state by value (it is a few bytes) and the App; a screen that moves
/// its cursor writes its new state back to `app.screen`.
pub trait View: Copy {
    /// The screen's name: the simulator's title, and the status bar's unless [`View::title`] says otherwise.
    fn name(self) -> &'static str;

    /// Its mock-up in `docs/img/screens`, for the simulator's overlay (F3).
    fn mockup(self) -> Option<&'static str> {
        None
    }

    /// The status bar's screen name (SBAR-5): 8 characters or fewer; screens whose name is longer have a
    /// short one here or their own `title`.
    fn title(self, _app: &App, b: &mut Buf<10>) {
        b.set(short_title(self.name()).as_bytes());
    }

    /// Idle time after which it goes home: menus use the menu timeout (MENU-28); None stays.
    fn timeout(self) -> Option<u32> {
        None
    }

    /// False for a screen that draws its own top bar instead of the status bar (setup).
    fn bar(self) -> bool {
        true
    }

    /// True while nothing else may happen: hold BACK and hold # come to the screen as keys, and it never times
    /// out (the PIN at power on, power off, a link write).
    fn modal(self) -> bool {
        false
    }

    /// A key went down or up, before any press is decided; `code` is the matrix code when the firmware knows it
    /// ([`App::raw_key`]). Only the key test listens.
    fn raw(self, _app: &mut App, _key: Option<ferrite_core::keys::Key>, _code: Option<u8>, _down: bool) {}

    /// Hold MENU (MENU-25): open this screen's options (the shared options page, [`opts`], or the screen's own
    /// options page drawn the same way) and return true; false when it has none, and a toast says "No options
    /// here". Called by the App, so every screen answers hold MENU the same way.
    fn options(self, _app: &mut App) -> bool {
        false
    }

    /// Hold MENU is this screen's own key, and comes to [`View::key`]: home (advanced settings, MENU-2), the
    /// alert (the log), the key test (a key under test), PIN entry (OK however long it is held).
    fn own_hold_menu(self) -> bool {
        false
    }

    /// ◀▶ have a job of their own here (KEY-19); otherwise they are the knob, ◀ = -1, ▶ = +1.
    fn own_arrows(self) -> bool {
        false
    }

    /// SIDE1 has a job of its own here (home's squelch and clarifier shortcuts, HF mode, the squelch screen, the
    /// crossband repeater, the key test); otherwise it is the monitor key (SQL-9.3: [`App::monitor_key`]).
    fn own_side1(self) -> bool {
        false
    }

    /// A short, long or repeat press (after the App's own: hold BACK, hold #, the lock, hold MENU).
    fn key(self, app: &mut App, e: KeyEvent, store: &impl ChannelStore);

    /// The knob, +1 per click clockwise.
    fn knob(self, _app: &mut App, _d: i32, _store: &impl ChannelStore) {}

    /// PTT down or up. By default, down transmits from home (KEY-7: from any screen, going home first).
    fn ptt(self, app: &mut App, down: bool) {
        if down {
            nav::transmit(app);
        }
    }

    /// Time passed; the Radio task's latest snapshot is in `live`.
    fn tick(self, _app: &mut App, _live: &LiveAll) {}

    /// Draws what changed (HOME-7): each region through `app.dirty(region, hash)`.
    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>);

    /// The hint bar (KEY-9), while no toast covers it.
    fn hints(self, app: &App) -> Hints;
}

impl Screen {
    pub fn name(self) -> &'static str {
        each!(self, s => s.name())
    }

    pub fn mockup(self) -> Option<&'static str> {
        each!(self, s => s.mockup())
    }

    pub(crate) fn title(self, app: &App, b: &mut Buf<10>) {
        each!(self, s => s.title(app, b))
    }

    pub(crate) fn timeout(self) -> Option<u32> {
        each!(self, s => s.timeout())
    }

    pub(crate) fn key(self, app: &mut App, e: KeyEvent, store: &impl ChannelStore) {
        each!(self, s => s.key(app, e, store))
    }

    pub(crate) fn options(self, app: &mut App) -> bool {
        each!(self, s => s.options(app))
    }

    pub(crate) fn own_hold_menu(self) -> bool {
        each!(self, s => s.own_hold_menu())
    }

    pub(crate) fn own_arrows(self) -> bool {
        each!(self, s => s.own_arrows())
    }

    pub(crate) fn own_side1(self) -> bool {
        each!(self, s => s.own_side1())
    }

    pub(crate) fn bar(self) -> bool {
        each!(self, s => s.bar())
    }

    pub(crate) fn modal(self) -> bool {
        each!(self, s => s.modal())
    }

    pub(crate) fn raw(self, app: &mut App, key: Option<ferrite_core::keys::Key>, code: Option<u8>, down: bool) {
        each!(self, s => s.raw(app, key, code, down))
    }

    pub(crate) fn knob(self, app: &mut App, d: i32, store: &impl ChannelStore) {
        each!(self, s => s.knob(app, d, store))
    }

    pub(crate) fn ptt(self, app: &mut App, down: bool) {
        each!(self, s => s.ptt(app, down))
    }

    pub(crate) fn tick(self, app: &mut App, live: &LiveAll) {
        each!(self, s => s.tick(app, live))
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, cx: &Cx<impl ChannelStore>) {
        each!(self, s => s.render(app, lcd, cx))
    }

    pub(crate) fn hints(self, app: &App) -> Hints {
        each!(self, s => s.hints(app))
    }
}

impl App {
    /// LINK-19.5: the screen's content drawn again, all of it, as a new screen draws it, into `lcd` (the readback's
    /// text listener); what the drawing keeps of what is on the panel ([`crate::Drawn`]) is put back after, so the
    /// panel's next render goes on as if this never ran. One copy: the screens draw through a [`ferrite_ui::canvas`]
    /// band, as on the panel.
    #[inline(never)]
    pub(crate) fn render_texts<S: ChannelStore>(&mut self, lcd: &mut dyn Panel, live: &LiveAll, dev: &Device, store: &S) {
        let before = self.drawn;
        self.drawn.hash = [0; shared::REGIONS];
        self.drawn.meter = [None; SLOTS];
        let cx = Cx { live, dev, store };
        let screen = self.screen;
        screen.render(self, &mut ferrite_ui::canvas::direct(lcd), &cx);
        self.drawn = before;
    }

    /// Draws what changed since the last call: the status bar, the screen, then the hint bar or a toast.
    pub fn render(&mut self, lcd: &mut impl Panel, live: &LiveAll, dev: &Device, store: &impl ChannelStore) {
        // LINK-19's readback counts the redraws that changed something (what was drawn, as the regions record it).
        let before = (self.drawn.screen, self.drawn.sbar, self.drawn.hash, self.drawn.meter, self.toast.is_some());
        self.render_all(lcd, live, dev, store);
        if before != (self.drawn.screen, self.drawn.sbar, self.drawn.hash, self.drawn.meter, self.toast.is_some()) {
            self.frames = self.frames.wrapping_add(1);
            self.drawn_at = self.now;
        }
    }

    fn render_all(&mut self, lcd: &mut impl Panel, live: &LiveAll, dev: &Device, store: &impl ChannelStore) {
        if ferrite_ui::theme::get() != self.set.theme {
            // 0 7 1 changed (or the settings were loaded): everything again, in the new colours.
            ferrite_ui::theme::set(self.set.theme);
            self.drawn.screen = None;
            self.drawn.sbar = None;
        }
        let id = core::mem::discriminant(&self.screen);
        let cleared = self.drawn.screen != Some(id);
        if cleared {
            // DISP-29: the content area's background goes in with the screen's first drawing, where nothing else
            // paints (below)
            self.drawn.hash = [0; shared::REGIONS];
            self.drawn.meter = [None; SLOTS];
            self.drawn.screen = Some(id);
            self.drawn.popup = 0; // a popup went with the content too
            qmenu::unseen(self); // the quick menu's box went with the content: drawn again whole over the screen
        }
        self.note_device(dev);
        let t0 = ferrite_ui::prof::now();
        if self.screen.bar() && !self.lpm_home() {
            let sb = self.status_bar(live, dev);
            if self.drawn.sbar != Some(sb) {
                sb.draw(lcd, self.drawn.sbar.as_ref());
                self.drawn.sbar = Some(sb);
            }
        } else {
            self.drawn.sbar = None; // the screen draws its own; the status bar comes back whole
        }
        ferrite_ui::prof::add(ferrite_ui::prof::SBAR, t0);
        let popup_gone = self.drawn.popup_gone;
        if popup_gone {
            self.drawn.popup = 0;
        }
        // (Not SAFE-14's "PTT stuck": it stays up while PTT is held, and home's RX and TX must keep showing.)
        // HOME-8: a shortcut's popup on home, already on the screen: home waits under it as under the quick menu
        // (it redrew under the popup every frame and the popup over it again, 24 KB each, Nov 9, 2026); the first
        // frame draws home first, so what the shortcut changed shows at once.
        let popup_frozen = !cleared
            && !popup_gone
            && self.screen == Screen::Home
            && self.drawn.popup != 0
            && self.toast.is_some_and(|t| t.center && t.head.as_bytes() != STUCK_HEAD);
        if popup_gone && !cleared && self.screen == Screen::Home && home::uncover(self, lcd, w::POPUP_BOX) {
            // HOME-8: the popup went from home's cards: only the cards under it are drawn again (DISP-25)
            self.drawn.popup_gone = false;
        } else if popup_gone && !cleared {
            // The popup went: clear its box and let every region draw again over it.
            let (x, y, w_, h) = w::POPUP_BOX;
            lcd.fill(x, y, w_, h, BG);
            self.drawn.hash = [0; shared::REGIONS];
            self.drawn.meter = [None; SLOTS];
            self.drawn.popup_gone = false;
        } else if popup_gone {
            self.drawn.popup_gone = false; // the whole area is new anyway
        }
        let cx = Cx { live, dev, store };
        if self.drawn.qmenu_gone {
            qmenu::repaint(self, lcd, &cx); // HOME-17: the box went: home again where it was, straight from the box
        }
        // HOME-17: while the quick menu's box is up the screen under it waits, so nothing draws through the box
        // (DISP-25); a cleared screen (or one under a popup that just went) is drawn whole first, the box over it.
        let t0 = ferrite_ui::prof::now();
        if cleared {
            // DISP-29: a new screen (or the panel woken) is drawn once, recorded, with the background only where its
            // content doesn't paint: clearing the area first sent most pixels twice. A screen too big to record (or
            // the bench's `lcd paint old`, TERM-40) clears and draws as before, from the state before the recording.
            let before = self.drawn;
            let screen = self.screen;
            let (x, y, h) = (0, w::SBAR_H, w::HINT_Y - w::SBAR_H);
            if !ferrite_ui::canvas::fresh(lcd, x, y, 240, h, BG, &mut |b| screen.render(self, b, &cx)) {
                self.drawn = before;
                lcd.fill(x, y, 240, h, BG);
                self.screen.render(self, &mut ferrite_ui::canvas::direct(lcd), &cx);
            }
        } else if popup_gone || !(qmenu::frozen(self) || popup_frozen) {
            // through a band that passes straight on, so each screen's drawing is compiled once (as `fresh`'s)
            self.screen.render(self, &mut ferrite_ui::canvas::direct(lcd), &cx);
        }
        ferrite_ui::prof::add(ferrite_ui::prof::SCREEN, t0);
        let t0 = ferrite_ui::prof::now();
        self.render_hint(lcd);
        qmenu::draw(self, lcd);
        // A shortcut's popup sits over the screen: drawn after it every time, as the screen may have drawn under it.
        if let Some(t) = self.toast.filter(|t| t.center) {
            let h = shared::Mix::new().b(t.head.as_bytes()).b(t.body.as_bytes()).get() | 1;
            // on home: once, then only when its text changes (home waits under it); elsewhere every time, as the
            // screen may have drawn under it
            if self.screen != Screen::Home || self.drawn.popup != h || !popup_frozen {
                w::popup(lcd, t.head.as_bytes(), t.body.as_bytes());
            }
            self.drawn.popup = if self.screen == Screen::Home { h } else { 0 };
        } else {
            self.drawn.popup = 0;
        }
        ferrite_ui::prof::add(ferrite_ui::prof::HINT, t0);
    }
}

/// A screen name of more than 8 characters, shortened for the status bar (SBAR-5); others as they are.
pub(crate) fn short_title(name: &str) -> &str {
    match name {
        "Extend TX" => "Extend",
        "Shortcut map" => "Shortcut",
        "HF recent" => "Recent",
        "Calibrate" => "Trims",
        "SSB patch" => "SSB",
        "Add device" => "Device",
        "Self-test" => "Checks",
        "Home keys" => "Keys", // HELP-3
        "Side keys" => "Side+PTT",
        "Menu numbers" => "Menu nos", // HELP-5
        n => n,
    }
}
