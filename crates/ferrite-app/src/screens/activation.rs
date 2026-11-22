//! SOTA/POTA contact logging (MENU 0 3 8). Each contact is saved in the radio's
//! event log and made available to Console through LOG_READ.

use super::shared::*;
use super::text::{self, TextFor};
use super::{Cx, Hints, View};
use crate::chan::ChannelStore;
use crate::data::{Event, Src};
use crate::keys::{KeyEvent, Press};
use crate::{App, Screen};
use ferrite_core::keys::Key;
use ferrite_ui::assets::*;
use ferrite_ui::gfx::*;
use ferrite_ui::panel::Panel;
use ferrite_ui::text::Buf;
use ferrite_ui::widgets as w;

pub(crate) struct Session {
    pub(crate) reference: Buf<16>,
}

impl Session {
    pub(crate) const fn new() -> Self {
        Self { reference: Buf::new() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Activation;

impl View for Activation {
    fn name(self) -> &'static str {
        "SOTA / POTA"
    }

    fn title(self, _: &App, title: &mut Buf<10>) {
        title.set(b"Activate");
    }

    fn key(self, app: &mut App, e: KeyEvent, store: &impl ChannelStore) {
        if e.press != Press::Short {
            return;
        }
        match e.key {
            Key::Digit(1) => text::open(app, TextFor::ActivationRef, store),
            Key::Menu | Key::Digit(2) if !app.activation.reference.is_empty() => {
                text::open(app, TextFor::ActivationCall, store);
            }
            Key::Menu | Key::Digit(2) => text::open(app, TextFor::ActivationRef, store),
            Key::Digit(3) => {
                let mut log = super::log::Log::new();
                if let Some(source) = reference_source(app.activation.reference.as_bytes()) {
                    log.filter = source as u8 + 1;
                }
                app.open_screen(Screen::Log(log));
            }
            Key::Exit => app.back(),
            _ => {}
        }
    }

    fn render(self, app: &mut App, lcd: &mut impl Panel, _: &Cx<impl ChannelStore>) {
        let reference = app.activation.reference;
        let slot = app.active;
        let freq = super::transmit::tx_freq(app, slot);
        let mode = app.tuned_cfg(slot).mode;
        if !app.dirty(R_PAGE, Mix::new().h(&reference).h(&freq).h(&mode).h(&slot).get()) {
            return;
        }
        let source = reference_source(reference.as_bytes());
        let title = source.map_or(b"Activation" as &[u8], |s| s.name().as_bytes());
        let reference = if reference.is_empty() { b"Set reference" as &[u8] } else { reference.as_bytes() };
        lcd.fill_r(8, 31, 224, 70, 4, SURFACE);
        w::field(lcd, 17, 206, 36, 55, 51, title, const { &w::st(&UI_12B, BLUE, SURFACE, Align::Left) });
        w::field(lcd, 17, 206, 56, 91, 80, reference, const { &w::st(&UI_15B, TEXT, SURFACE, Align::Left) });
        let mut current = Buf::<40>::new();
        let _ = ufmt::uwrite!(current, "{}  ", w::LETTERS[slot as usize] as char);
        current.push(w::freq3(freq).as_bytes());
        current.push(b" MHz TX  ");
        current.push(crate::slot::MODES.get(mode as usize).unwrap_or(&"").as_bytes());
        w::field(lcd, 16, 208, 108, 130, 124, current.as_bytes(), const { &w::st(&UI_12, SUB, BG, Align::Left) });
        for (i, (key, label)) in [("1", "Reference"), ("2", "Log contact"), ("3", "Radio log")].into_iter().enumerate() {
            let y = 144 + i as i32 * 36;
            lcd.fill_r(8, y, 224, 32, 3, SURFACE);
            w::key_label_in(lcd, 16, y + 21, key, label, (BLUE, TEXT, SURFACE), true);
        }
        w::field(lcd, 16, 208, 262, 283, 276, b"Export in Console's Event Log", const { &w::st(&UI_12, SUB, BG, Align::Left) });
    }

    fn hints(self, app: &App) -> Hints {
        Hints::new(&[("OK", if app.activation.reference.is_empty() { "Reference" } else { "Log" }), ("BACK", "Back")])
    }
}

fn reference_source(reference: &[u8]) -> Option<Src> {
    let dash = reference.iter().rposition(|&b| b == b'-')?;
    let (prefix, number) = (&reference[..dash], &reference[dash + 1..]);
    let valid_part = |s: &[u8]| !s.is_empty() && s.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if number.is_empty() || !number.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if let Some(slash) = prefix.iter().position(|&b| b == b'/') {
        (valid_part(&prefix[..slash]) && valid_part(&prefix[slash + 1..])).then_some(Src::Sota)
    } else {
        valid_part(prefix).then_some(Src::Pota)
    }
}

pub(crate) fn keep(app: &mut App, target: TextFor, value: &[u8], store: &impl ChannelStore) {
    if target == TextFor::ActivationRef {
        if reference_source(value).is_none() {
            return app.show_toast(b"Reference", b"Check the format", false);
        }
        app.activation.reference = Buf::from(value);
        return app.back();
    }
    if !value.iter().any(u8::is_ascii_uppercase) || !value.iter().any(u8::is_ascii_digit)
        || !value.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-'))
    {
        return app.show_toast(b"Callsign", b"Enter a valid call", false);
    }
    let reference = app.activation.reference.as_bytes();
    let Some(src) = reference_source(reference) else { return app.back() };
    let slot = app.active;
    let event = Event {
        src,
        ago_s: 0,
        freq: super::transmit::tx_freq(app, slot),
        slot,
        unread: false,
        id: 0,
        x: u16::from(app.tuned_cfg(slot).mode),
        y: 0,
        pos: None,
        name: Buf::from(value),
        text: Buf::from(reference),
    };
    if store.data().append_activation(&event) {
        app.back();
        app.show_toast(b"Contact logged", value, false);
    } else {
        app.show_toast(b"Not logged", b"Storage unavailable", false);
    }
}
