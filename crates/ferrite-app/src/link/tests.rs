//! The link session against the sample store: what a PC tool sees on the cable.
#![cfg_attr(not(feature = "line-cmds"), allow(unused))] // helpers for the tests gated on the line commands

use super::*;
use crate::keys::Press;
use crate::sample::Sample;
use crate::screens::link::LinkPage;
use crate::Screen;
use ferrite_core::keys::Key;
use ferrite_core::msg::Device;
use std::vec::Vec;

/// The cable's other end: frames out, answers back.
struct Pc {
    app: App,
    st: Sample,
    link: Session,
    now: u32,
    seq: u8,
    /// What the firmware tells the session (the build, LINK-35).
    dev: Device,
}

struct Wire(Vec<u8>);

impl Sink for Wire {
    fn room(&self) -> usize {
        4096
    }
    fn put(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
}

impl Pc {
    fn new(trusted: bool) -> Self {
        let mut app = App::new(crate::tests::set(), crate::tests::alert());
        while app.pop_cmd().is_some() {}
        Pc { app, st: Sample::new(), link: Session::new(trusted), now: 1000, seq: 0, dev: Device::default() }
    }

    /// Sends one request, polls until its answer: (status, the rest of the payload).
    fn ask(&mut self, ty: u8, payload: &[u8]) -> (u8, Vec<u8>) {
        self.seq = self.seq.wrapping_add(1);
        let mut w = [0u8; MAX_WIRE];
        let n = wire::build(ty, self.seq, payload, &mut w).unwrap();
        self.raw(&w[..n]);
        let mut out = Wire(Vec::new());
        for _ in 0..4 {
            self.now += 10;
            self.link.poll(&mut self.app, &self.st, &self.dev, &LiveAll::default(), self.now, &mut out);
            while let Some(wr) = self.app.pop_write() {
                self.st.apply(wr);
            }
            if !out.0.is_empty() {
                break;
            }
        }
        assert_eq!(out.0.last(), Some(&0), "one delimited answer");
        let k = out.0.len() - 1;
        let f = wire::parse(&mut out.0[..k]).unwrap();
        assert_eq!((f.ty, f.seq), (ty | wire::ANSWER, self.seq));
        (f.payload[0], f.payload[1..].to_vec())
    }

    fn raw(&mut self, b: &[u8]) {
        for &c in b {
            assert!(self.link.ready());
            self.link.feed(c, self.now);
        }
    }

    /// A whole record: the settings and APRS by `READ` in parts, a channel, zone or preset by `READ_MANY` of its
    /// index alone (LINK-21, LINK-40).
    fn read(&mut self, w: u8, idx: u16) -> Result<Vec<u8>, u8> {
        if !matches!(w, what::CHANNEL | what::ZONE | what::PRESET) {
            return self.read_parts(w, idx);
        }
        let mut p = std::vec![w];
        p.extend_from_slice(&idx.to_le_bytes());
        p.extend_from_slice(&1u16.to_le_bytes());
        let (s, a) = self.ask(msg::READ_MANY, &p);
        if s != 0 {
            return Err(s);
        }
        match a.get(2..5) {
            Some(h) if u16::from_le_bytes([h[0], h[1]]) == idx => Ok(a[5..5 + h[2] as usize].to_vec()),
            _ => Err(Status::NotFound as u8),
        }
    }

    /// A whole record by `READ`, in parts.
    fn read_parts(&mut self, w: u8, idx: u16) -> Result<Vec<u8>, u8> {
        let mut rec = Vec::new();
        loop {
            let off = rec.len() as u16;
            let mut p = std::vec![w];
            p.extend_from_slice(&idx.to_le_bytes());
            p.extend_from_slice(&off.to_le_bytes());
            p.push(200);
            let (s, a) = self.ask(msg::READ, &p);
            if s != 0 {
                return Err(s);
            }
            let total = u16::from_le_bytes([a[0], a[1]]) as usize;
            rec.extend_from_slice(&a[2..]);
            if rec.len() >= total {
                return Ok(rec);
            }
        }
    }

    /// A whole record: a channel, zone or preset by `WRITE_MANY` of it alone, (status, a refusal's reason);
    /// anything else by `WRITE` in parts of `part` bytes, the last part's answer (LINK-21, LINK-41).
    fn write(&mut self, w: u8, idx: u16, rec: &[u8], part: usize) -> (u8, Vec<u8>) {
        if matches!(w, what::CHANNEL | what::ZONE | what::PRESET) {
            let (s, a) = self.write_many(w, &[(idx, rec.to_vec())]);
            return (s, a.get(1..).unwrap_or_default().to_vec());
        }
        self.write_parts(w, idx, rec, part)
    }

    /// A whole record by `WRITE`, in parts of `part` bytes; the last part's answer.
    fn write_parts(&mut self, w: u8, idx: u16, rec: &[u8], part: usize) -> (u8, Vec<u8>) {
        let mut last = (0, Vec::new());
        for (k, chunk) in rec.chunks(part).enumerate() {
            let mut p = std::vec![w];
            p.extend_from_slice(&idx.to_le_bytes());
            p.extend_from_slice(&((k * part) as u16).to_le_bytes());
            p.extend_from_slice(&(rec.len() as u16).to_le_bytes());
            p.extend_from_slice(chunk);
            last = self.ask(msg::WRITE, &p);
            if last.0 != 0 {
                return last;
            }
        }
        last
    }

    fn key(&mut self, k: Key) {
        self.app.key_down(k, self.now, &self.st);
        self.now += 100;
        self.app.key_up(k, self.now, &self.st);
        let _ = Press::Short;
    }
}

/// LINK-1, LINK-2, LINK-35.1: HELLO and INFO answer (INFO ending with the unique ID); every frame is acknowledged
/// with its type and seq.
#[test]
fn hello_and_info() {
    let mut pc = Pc::new(true);
    let (s, a) = pc.ask(msg::HELLO, &[PROTOCOL, 0, 0, 0, 0, 0, 0, 0, 0, b'p', b'c']);
    assert_eq!(s, Status::Ok as u8);
    assert_eq!(&a[..3], &[PROTOCOL, MAX_PAYLOAD as u8, 1], "protocol, max payload, may write");
    assert_eq!(&a[4..4 + a[3] as usize], VERSION.as_bytes());
    pc.dev.selftest.uid = [0x3F2A_91C0, 0x7D44_0E18, 0xA2B6_5C93];
    let (s, a) = pc.ask(msg::INFO, &[]);
    assert_eq!(s, 0);
    let v = a[0] as usize;
    assert_eq!(u16::from_le_bytes([a[1 + v], a[2 + v]]), pc.st.count());
    // LINK-35.1: the 12 bytes before the layouts (LINK-62) are the unique ID as the MCU stores it
    assert_eq!(a[a.len() - 14..a.len() - 2], ferrite_core::uid::bytes(pc.dev.selftest.uid));
    assert_eq!(pc.link.hellos(), 1);
    assert!(pc.link.active(pc.now));
    assert!(!pc.link.active(pc.now + ACTIVE_MS));
}

/// LINK-35: HELLO's answer ends with the build after the version, INFO's after the callsign: 0 receive, 1 tx,
/// 3 tx with Dry TX on (TX-48), from the firmware's features (`Device::tx_built`) and the switch, with bit 7 set when
/// the bench commands are in (`Device::bench`, TERM-38): the tx-bench build answers 0x83 from power-on (TX-48.1).
/// 2 is the retired tx-dry build's (decision 193), still named for an older radio.
/// LINK-35.2: HELLO ends with the capability byte (decision 385: compression, audio, the line commands), the layouts
/// and the lists generation.
#[test]
fn hello_and_info_give_the_build() {
    use ferrite_core::link::build;
    for (tx, on, bench, b) in [
        (false, false, false, 0u8),
        (true, false, false, 1),
        (false, false, true, 0x80),
        (true, true, false, 3),     // TX-48: a tx build with Dry TX on
        (true, true, true, 0x83),   // TX-48.1: the tx-bench build as it starts
        (true, false, true, 0x81),  // ... with Dry TX turned off by hand
        (false, true, false, 0),    // no transmit: nothing to hold
    ] {
        let mut pc = Pc::new(true);
        (pc.dev.tx_built, pc.dev.bench) = (tx, bench);
        pc.app.set_dry_tx(on);
        assert_eq!(build::dry(b), tx && on, "dry {b:#x}");
        let (_, a) = pc.ask(msg::HELLO, &[PROTOCOL]);
        let v = 4 + a[3] as usize;
        let cmds = if cfg!(feature = "line-cmds") { ferrite_core::link::caps::LINE_CMDS } else { 0 };
        let g = super::lists_gen(&pc.st).to_le_bytes();
        assert_eq!(a[v..], [b, ferrite_core::link::caps::AUDIO | cmds, 3, 3, g[0], g[1], g[2], g[3]], "HELLO, tx {tx} on {on} bench {bench}; LINK-35.2's caps (decision 385: no LZ, not asked; AUDIO, the cable; the line commands by build), then the layouts (LINK-62) and the lists hash (LINK-71)");
        let (_, a) = pc.ask(msg::INFO, &[]);
        let v = 1 + a[0] as usize + 8;
        let c = v + 1 + a[v] as usize;
        assert_eq!(a[c], b, "INFO");
        assert_eq!(a[c + 1..c + 13], [0; 12], "LINK-35.1: the unique ID follows, zeros until read");
        assert_eq!(a[c + 13..], [3, 3], "LINK-62: the layouts, this firmware's and the chip's");
    }
    assert_eq!(build::name(2), "tx-dry");
    assert_eq!(build::name(0x82), "tx-dry");
    assert!(build::bench(0x83) && !build::bench(1));
    assert_eq!(build::name(0x83), "tx+dry");
    assert!(build::dry(3) && build::dry(0x83) && build::dry(0x82) && !build::dry(1) && !build::dry(0x81) && !build::dry(0));
}

/// A damaged frame, an unknown type and a short payload are answered, not dropped.
#[test]
fn errors_are_answered() {
    let mut pc = Pc::new(true);
    assert_eq!(pc.ask(0x5F, &[]).0, Status::UnknownType as u8);
    assert_eq!(pc.ask(msg::READ, &[2]).0, Status::BadArg as u8);
    let mut w = [0u8; MAX_WIRE];
    let n = wire::build(msg::INFO, 9, &[1, 2, 3], &mut w).unwrap();
    w[4] ^= 0x10;
    pc.raw(&w[..n]);
    let mut out = Wire(Vec::new());
    pc.link.poll(&mut pc.app, &pc.st, &Device::default(), &LiveAll::default(), 2000, &mut out);
    let k = out.0.len() - 1;
    let f = wire::parse(&mut out.0[..k]).unwrap();
    assert_eq!((f.ty, f.seq, f.payload[0]), (msg::INFO | wire::ANSWER, 9, Status::BadFrame as u8));
}

/// LINK-2, LINK-5, CHAN-31: a channel reads, writes back changed (a name mapped to the name set), and a bad one is
/// refused with the reason.
#[test]
fn channel_round_trip_and_checks() {
    let mut pc = Pc::new(true);
    let c = pc.st.by_number(0).unwrap();
    let mut rec = pc.read(what::CHANNEL, c.num).unwrap();
    assert_eq!(&rec[8..8 + c.name().len()], c.name());
    rec[8] = b'Z';
    assert_eq!(pc.write(what::CHANNEL, c.num, &rec, 240).0, 0);
    assert_eq!(pc.st.get(c.num).unwrap().name[0], b'Z');
    // CHAN-31: printable characters outside the name set are mapped to '-', not refused
    rec[8..11].copy_from_slice(b"@*?");
    assert_eq!(pc.write(what::CHANNEL, c.num, &rec, 240).0, 0);
    assert_eq!(&pc.st.get(c.num).unwrap().name[..3], b"---");
    let mut bad = rec.clone();
    bad[4..8].copy_from_slice(&70_000_000u32.to_le_bytes()); // 700 MHz: no chip receives it
    let (s, why) = pc.write(what::CHANNEL, c.num, &bad, 240);
    assert_eq!(s, Status::Invalid as u8);
    assert!(std::str::from_utf8(&why).unwrap().contains("756-1300 MHz"), "{why:?}");
    bad[4..8].copy_from_slice(&1_000_000u32.to_le_bytes()); // 10 MHz: an HF channel, but NFM isn't HF mode's
    let (s, why) = pc.write(what::CHANNEL, c.num, &bad, 240);
    assert_eq!(s, Status::Invalid as u8);
    assert!(std::str::from_utf8(&why).unwrap().contains("an HF channel is AM"), "{why:?}");
    assert_eq!(pc.read(what::CHANNEL, 999).unwrap_err(), Status::NotFound as u8);
    assert_eq!(pc.ask(msg::DELETE, &[what::CHANNEL, (c.num & 255) as u8, (c.num >> 8) as u8]).0, 0);
    assert!(pc.st.get(c.num).is_none());
}

/// LINK-3: nothing but the four records is written; there is no address-based write at all.
#[test]
fn only_records_are_written() {
    let mut pc = Pc::new(true);
    let (s, _) = pc.write(9, 0, &[0; 8], 240);
    assert_eq!(s, Status::Refused as u8);
}

/// LINK-21, LINK-40, LINK-41: `READ` and `WRITE` take the settings and the APRS record only; a channel, zone or
/// preset (one frame each) goes by `READ_MANY`/`WRITE_MANY`: `READ` of one answers `bad argument` (as an unknown
/// kind), `WRITE` `refused` (LINK-3), changing nothing.
#[test]
fn read_write_are_for_large_records() {
    let mut pc = Pc::new(true);
    let c = pc.st.by_number(0).unwrap();
    let rec = pc.read(what::CHANNEL, c.num).unwrap();
    for w in [what::CHANNEL, what::ZONE, what::PRESET] {
        assert_eq!(pc.read_parts(w, 0), Err(Status::BadArg as u8), "READ of {w}");
    }
    let mut new = rec.clone();
    new[7] = b'Z';
    assert_eq!(pc.write_parts(what::CHANNEL, c.num, &new, 240).0, Status::Refused as u8);
    assert_eq!(pc.read(what::CHANNEL, c.num).unwrap(), rec, "unchanged");
    assert!(pc.read_parts(what::SETTINGS, 0).is_ok());
    assert!(pc.read_parts(what::APRS, 0).is_ok());
}

/// LINK-5, LINK-7: the settings read without the PIN, write back in parts, and the protected ones
/// (region, Extend TX, PIN, cable port) keep the radio's values; the callsign is written, in capitals (LINK-26).
#[test]
fn settings_round_trip_keeps_protected() {
    let mut pc = Pc::new(true);
    pc.app.set.pin = 1234;
    pc.app.set.pin_len = 4;
    let rec = pc.read(what::SETTINGS, 0).unwrap();
    assert!(rec.len() <= RECORD_MAX, "{} bytes", rec.len());
    let mut s = Settings::DEFAULT;
    s.load(&mut Rd { b: &rec, n: 0 });
    assert_eq!(s.pin, 0, "masked");
    s.theme = 2;
    s.region = 3;
    s.extend_tx = true;
    s.cable_port = 2;
    s.callsign = crate::settings::Text::EMPTY;
    for &b in b"va7fer" {
        s.callsign.push(b);
    }
    let mut out = std::vec![0u8; RECORD_MAX];
    let mut w = Window { b: &mut out, from: 0, n: 0 };
    s.save(&mut w);
    let n = w.n;
    let region = pc.app.set.region;
    assert_eq!(pc.write(what::SETTINGS, 0, &out[..n], 100).0, 0);
    assert_eq!(pc.app.set.theme, 2, "changed");
    assert_eq!(pc.app.set.callsign.as_bytes(), b"VA7FER", "the callsign is written, in capitals (LINK-26)");
    assert_eq!((pc.app.set.region, pc.app.set.extend_tx, pc.app.set.cable_port), (region, false, 0), "kept");
    assert_eq!(pc.app.set.pin, 1234, "kept, not zeroed by the masked read");
}

/// PWR-24.1 (decision 328): a settings write sets Low Power Mode (0 7 7 2) and its Auto level (0 7 7 3) as the menu
/// does: On goes in at once, Off comes out; the KISS port stays the radio's (LINK-5, KISS-7).
#[test]
fn settings_write_sets_low_power_mode() {
    let mut pc = Pc::new(true);
    let rec = pc.read(what::SETTINGS, 0).unwrap();
    let mut s = Settings::DEFAULT;
    s.load(&mut Rd { b: &rec, n: 0 });
    let put = |pc: &mut Pc, s: &Settings| {
        let mut out = std::vec![0u8; RECORD_MAX];
        let mut w = Window { b: &mut out, from: 0, n: 0 };
        s.save(&mut w);
        let n = w.n;
        assert_eq!(pc.write(what::SETTINGS, 0, &out[..n], 100).0, 0);
    };
    let kiss = pc.app.set.kiss_port;
    (s.lpm, s.lpm_below, s.kiss_port) = (1, 30, 2);
    put(&mut pc, &s);
    assert_eq!((pc.app.set.lpm, pc.app.set.lpm_below, pc.app.set.kiss_port), (1, 30, kiss));
    assert!(pc.app.low_power(), "On: in at once");
    s.lpm = 0;
    put(&mut pc, &s);
    assert_eq!(pc.app.set.lpm, 0);
    assert!(!pc.app.low_power(), "Off: out");
}

/// LINK-7, LINK-30, LINK-31: with no PIN lock, a Bluetooth session reads and writes like the cable, with no
/// prompt on the radio. With the PIN lock on, HELLO says AUTH first and every other request is refused until
/// AUTH gives the radio's PIN; wrong PINs count toward the radio's own 30 s wait (PIN-3), shown on its screen.
#[test]
fn bluetooth_pin_only() {
    let mut pc = Pc::new(false);
    let c = pc.st.by_number(0).unwrap();
    let rec = pc.read(what::CHANNEL, c.num).unwrap();
    assert_eq!(pc.ask(msg::HELLO, &[PROTOCOL]).1[2], 1, "no PIN lock: may go on");
    assert_eq!(pc.write(what::CHANNEL, c.num, &rec, 240).0, 0, "no PIN lock: writes go ahead");
    assert!(!matches!(pc.app.screen(), Screen::Link(_)), "no prompt on the radio");
    // the PIN lock on
    let mut bt = Pc::new(false);
    (bt.app.set.pin, bt.app.set.pin_len, bt.app.set.pin_mode) = (1234, 4, 1);
    assert_eq!(bt.ask(msg::HELLO, &[PROTOCOL]).1[2], 0, "AUTH first");
    assert_eq!(bt.read(what::CHANNEL, c.num), Err(Status::NotAuthorised as u8), "not even reads");
    assert_eq!(bt.ask(msg::INFO, &[]).0, Status::NotAuthorised as u8);
    assert_eq!(bt.ask(msg::AUTH, b"0000").0, Status::Refused as u8, "wrong");
    assert_eq!(bt.app.set.pin_misses, 1, "the radio's own counter");
    assert_eq!(bt.ask(msg::AUTH, b"01234").0, Status::Refused as u8, "length counts");
    assert_eq!(bt.ask(msg::AUTH, b"9999").0, Status::Refused as u8, "third miss");
    assert!(bt.app.set.pin_wait != 0, "the radio's 30 s wait started");
    assert_eq!(bt.ask(msg::AUTH, b"1234").0, Status::Busy as u8, "even the right PIN waits");
    bt.app.set.pin_wait = 0;
    assert_eq!(bt.ask(msg::AUTH, b"1234").0, 0);
    assert_eq!(bt.app.set.pin_misses, 0, "a right PIN clears the count");
    assert_eq!(bt.ask(msg::HELLO, &[PROTOCOL]).1[2], 1);
    assert_eq!(bt.write(what::CHANNEL, c.num, &rec, 240).0, 0, "writes after AUTH, no prompt");
    // a new connection asks again
    bt.link.set_port(false, 115_200);
    assert_eq!(bt.ask(msg::INFO, &[]).0, Status::NotAuthorised as u8, "a new connection gives it again");
    // the cable never asks
    let mut cable = Pc::new(true);
    (cable.app.set.pin, cable.app.set.pin_len, cable.app.set.pin_mode) = (1234, 4, 1);
    assert_eq!(cable.ask(msg::HELLO, &[PROTOCOL]).1[2], 1);
    assert_eq!(cable.write(what::CHANNEL, c.num, &rec, 240).0, 0, "the cable is trusted");
}

/// LINK-9: BEGIN shows progress; BACK cancels the session's writes until END.
#[test]
fn write_session_progress_and_cancel() {
    let mut pc = Pc::new(true);
    let c = pc.st.by_number(0).unwrap();
    let rec = pc.read(what::CHANNEL, c.num).unwrap();
    assert_eq!(pc.ask(msg::BEGIN, &2u16.to_le_bytes()).0, 0);
    assert!(matches!(pc.app.screen(), Screen::Link(l) if l.page == LinkPage::Progress));
    assert_eq!(pc.link.progress(), Some(0));
    assert_eq!(pc.write(what::CHANNEL, c.num, &rec, 240).0, 0);
    assert_eq!(pc.link.progress(), Some(50));
    pc.key(Key::Exit);
    assert_eq!(pc.write(what::CHANNEL, c.num, &rec, 240).0, Status::Cancelled as u8);
    assert_eq!(pc.ask(msg::END, &[]).0, 0);
    assert_eq!(pc.link.progress(), None);
    assert_eq!(pc.write(what::CHANNEL, c.num, &rec, 240).0, 0, "a new session writes again");
}

/// LINK-11: BAUD takes only a listed rate the UART reaches.
#[test]
fn baud_within_the_uart() {
    let mut pc = Pc::new(true);
    pc.link.set_max_baud(230_400);
    assert_eq!(pc.ask(msg::BAUD, &921_600u32.to_le_bytes()).0, Status::Refused as u8);
    assert_eq!(pc.ask(msg::BAUD, &100_000u32.to_le_bytes()).0, Status::Refused as u8);
    assert_eq!(pc.ask(msg::BAUD, &230_400u32.to_le_bytes()).0, 0);
    assert_eq!(pc.link.take_baud(), Some(230_400));
    assert_eq!(pc.link.take_baud(), None);
}

/// LINK-24.1, LINK-24.2, LINK-24.3: REBOOT on the cable is answered, then handed to the caller once; never while
/// transmitting; on Bluetooth with the PIN lock on, only after AUTH (LINK-30).
#[test]
fn reboot_to_the_bootloader() {
    let mut pc = Pc::new(true);
    pc.app.radio.keyed = Some(0);
    assert_eq!(pc.ask(msg::REBOOT, &[]).0, Status::Busy as u8, "not while transmitting");
    assert!(!pc.link.take_reboot());
    pc.app.radio.keyed = None;
    assert_eq!(pc.ask(msg::REBOOT, &[]).0, 0);
    assert!(pc.link.take_reboot());
    assert!(!pc.link.take_reboot(), "once");
    let mut bt = Pc::new(false);
    (bt.app.set.pin, bt.app.set.pin_len, bt.app.set.pin_mode) = (1234, 4, 1);
    assert_eq!(bt.ask(msg::REBOOT, &[]).0, Status::NotAuthorised as u8);
    assert!(!bt.link.take_reboot());
    assert_eq!(bt.ask(msg::AUTH, b"1234").0, 0);
    assert_eq!(bt.ask(msg::REBOOT, &[]).0, 0);
    assert!(bt.link.take_reboot());
}

/// TERM-2: the cable port setting and the first bytes decide.
#[test]
fn cable_routing() {
    assert_eq!(route(0, false, b'\r'), Route::Term);
    assert_eq!(route(0, false, 0), Route::Link);
    assert_eq!(route(0, true, b'\r'), Route::Link, "a link session keeps the port");
    assert_eq!(route(1, false, b'\r'), Route::Link);
    assert_eq!(route(2, true, 0), Route::Term);
}

/// Decision 386: 0 9 2 3 has no KISS any more (0 5 8 is KISS's one switch); a radio saved with it (3) comes back
/// with KISS on the cable by 0 5 8 and the cable port on Auto, as it behaved.
#[test]
fn cable_port_kiss_moves_to_kiss_tnc() {
    let mut s = crate::settings::Settings::DEFAULT;
    s.cable_port = 3;
    s.sanitize();
    assert_eq!((s.cable_port, s.kiss_port), (0, 1));
    assert!(s.kiss_on_cable());
    s.cable_port = 9;
    s.sanitize();
    assert_eq!(s.cable_port, crate::settings::Settings::DEFAULT.cable_port);
}

// ---- Remote PTT (LINK-20) ----

use crate::remote::RemoteEnd;
use ferrite_core::keys::{PTT_A, PTT_B};
use ferrite_core::link::{key, Ptt};
use ferrite_core::msg::{LiveAll, RadioCmd};

impl Pc {
    /// A transmit build with a region set, as a person's PTT would key.
    fn tx() -> Self {
        let mut pc = Pc::new(true);
        pc.app.radio.tx_built = true;
        pc.app.set_region(Some(ferrite_core::txplan::Region::Us), false);
        while pc.app.pop_cmd().is_some() {}
        pc
    }

    fn ptt(&mut self, p: Ptt) -> (u8, Vec<u8>) {
        let mut b = [0u8; 3];
        let n = p.encode(&mut b);
        self.ask(msg::KEY, &b[..n])
    }

    /// Time passes on the radio: the App's ticks, and the session polled.
    fn pass(&mut self, ms: u32, live: &LiveAll) {
        let end = self.now + ms;
        while self.now < end {
            self.now += 50;
            self.app.tick(self.now, live, &self.st);
            self.link.poll(&mut self.app, &self.st, &Device::default(), &LiveAll::default(), self.now, &mut Wire(Vec::new()));
        }
    }

    fn cmds(&mut self) -> Vec<RadioCmd> {
        core::iter::from_fn(|| self.app.pop_cmd()).collect()
    }
}

fn keyed(c: &[RadioCmd]) -> bool {
    c.iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. }))
}

fn unkeyed(c: &[RadioCmd]) -> bool {
    c.iter().any(|c| matches!(c, RadioCmd::Transmit { on: false, .. } | RadioCmd::TxCut { .. }))
}

/// LINK-20, TX-47: unkeyed as a cut (no end tone, CW ID or carrier hold), not as a PTT let go.
fn cut(c: &[RadioCmd]) -> bool {
    c.iter().any(|c| matches!(c, RadioCmd::TxCut { .. })) && !c.iter().any(|c| matches!(c, RadioCmd::Transmit { on: false, .. }))
}

fn text(a: &[u8]) -> &str {
    core::str::from_utf8(a).unwrap()
}

/// LINK-20.1, LINK-20.4, LINK-20.6, LINK-20.12: KEY on keys the active slot through the PTT path (the Transmit screen,
/// marked Remote), keepalives hold it, KEY off unkeys; then a keepalive says it isn't keyed, and KEY off is always ok.
#[test]
fn remote_ptt_keys_and_unkeys() {
    let mut pc = Pc::tx();
    let (s, a) = pc.ptt(Ptt::On(key::ACTIVE));
    assert_eq!((s, a.as_slice()), (Status::Ok as u8, &[0u8][..]), "the active slot, A, keyed");
    let c = pc.cmds();
    assert!(c.iter().any(|c| matches!(c, RadioCmd::TxPlan { confirmed: false, .. })), "never a confirmed emergency plan");
    assert!(c.contains(&RadioCmd::Transmit { slot: 0, on: true }));
    assert!(matches!(pc.app.screen(), Screen::Transmit(_)) && pc.app.remote_keyed());
    for _ in 0..6 {
        pc.pass(500, &LiveAll::default());
        assert_eq!(pc.ptt(Ptt::Alive).0, Status::Ok as u8, "keepalives hold it");
    }
    assert!(!unkeyed(&pc.cmds()), "3 s keyed with keepalives");
    assert_eq!(pc.ptt(Ptt::Off).0, Status::Ok as u8);
    let c = pc.cmds();
    assert!(c.contains(&RadioCmd::Transmit { slot: 0, on: false }), "KEY off: a PTT let go (the hold, TX-47)");
    assert!(!c.iter().any(|c| matches!(c, RadioCmd::TxCut { .. })));
    assert_eq!(pc.app.screen(), Screen::Home);
    let (s, a) = pc.ptt(Ptt::Alive);
    assert_eq!((s, text(&a)), (Status::Invalid as u8, "not keyed"));
    assert_eq!(pc.ptt(Ptt::Off).0, Status::Ok as u8, "off again is fine");
    // PTT B's slot; C only while it is the active slot.
    assert_eq!(pc.ptt(Ptt::On(1)), (0, std::vec![1]));
    assert!(pc.cmds().contains(&RadioCmd::Transmit { slot: 1, on: true }));
    assert_eq!(pc.ptt(Ptt::On(1)).0, 0, "a repeated key-up is a keepalive");
    assert_eq!(pc.ptt(Ptt::On(0)).0, Status::Invalid as u8, "one transmission at a time");
    pc.ptt(Ptt::Off);
    let (s, a) = pc.ptt(Ptt::On(2));
    assert_eq!(s, Status::Invalid as u8, "C isn't active: {}", text(&a));
    assert!(!keyed(&pc.cmds()));
    assert_eq!(pc.ask(msg::KEY, &[key::PTT, key::ON]).0, Status::BadArg as u8, "ON needs its slot");
    for bad in [&[0x50u8, 1][..], &[22, 2], &[24, 2], &[25, 1], &[26, 1], &[6, 9], &[6], &[6, 2, 0]] {
        assert_eq!(pc.ask(msg::KEY, bad).0, Status::BadArg as u8, "{bad:?}: not a key (the PTTs are LINK-20's alone)");
    }
}

/// LINK-20.4: no keepalive for 2 s, no transmit, and the carrier drops at once (a cut, TX-47.3: no 400 ms hold); the
/// PC hears why.
#[test]
fn remote_ptt_keepalive_timeout() {
    let mut pc = Pc::tx();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    pc.cmds();
    pc.pass(1800, &LiveAll::default());
    assert!(pc.app.remote_keyed() && !unkeyed(&pc.cmds()), "1.8 s: still keyed");
    pc.pass(300, &LiveAll::default());
    assert!(!pc.app.remote_keyed());
    assert!(cut(&pc.cmds()), "past 2 s: unkeyed as a cut");
    assert!(!pc.app.tx_cut);
    assert_eq!(pc.app.screen(), Screen::Home);
    let (s, a) = pc.ptt(Ptt::Alive);
    assert_eq!((s, text(&a)), (Status::Invalid as u8, RemoteEnd::Keepalive.text()));
}

/// LINK-20.5, LINK-28: the session closing or moving port ends a remote transmission, as a cut (TX-47.4).
#[test]
fn remote_ptt_ends_with_the_session() {
    let mut pc = Pc::tx();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    pc.cmds();
    pc.link.set_port(false, 115_200); // Bluetooth took the session, or the cable took it back
    pc.pass(50, &LiveAll::default());
    assert!(cut(&pc.cmds()) && !pc.app.remote_keyed());
    assert_eq!(text(&pc.ptt(Ptt::Alive).1), RemoteEnd::Session.text());
}

/// LINK-20.5: a new `HELLO` on the session that keyed is the session starting over: the transmission ends at once,
/// as a cut (no wait for the keepalive to lapse, no 400 ms hold).
#[test]
fn remote_ptt_ends_with_a_new_hello() {
    let mut pc = Pc::tx();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    pc.cmds();
    pc.pass(500, &LiveAll::default());
    assert_eq!(pc.ask(msg::HELLO, &[PROTOCOL]).0, Status::Ok as u8);
    assert!(!pc.app.remote_keyed(), "unkeyed by the HELLO itself");
    assert!(cut(&pc.cmds()), "as a cut");
    assert_eq!(text(&pc.ptt(Ptt::Alive).1), RemoteEnd::Session.text());
}

fn refusal_beeps(v: &[RadioCmd]) -> usize {
    v.iter().filter(|c| matches!(c, RadioCmd::Beep { hz, .. } if *hz == crate::REFUSED_HZ)).count()
}

/// SND-4, LINK-20.3: a refused remote key-up beeps as a PTT's refusal does (the beep was missing for both: the
/// radio's PE4 never came on, Oct 15, 2026), once, and the refused screen's readback counts it (`beeps=`); a
/// refusal by the Radio task after the key-up beeps too.
#[test]
fn remote_keyup_refusal_beeps() {
    let mut pc = Pc::tx();
    let a0 = pc.app.active as usize;
    (pc.app.slots[a0].vfo, pc.app.slots[a0].vfo_mode) = (ferrite_core::freq::mhz(160), true);
    pc.cmds();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, Status::Invalid as u8);
    assert_eq!(refusal_beeps(&pc.cmds()), 1, "one refusal beep");
    assert_eq!(pc.app.refusal_beeps, 1);
    let mut pc = Pc::tx();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    pc.cmds();
    let mut live = LiveAll::default();
    live.tx.refused = Some(ferrite_core::txplan::Refusal::Hot);
    pc.pass(100, &live);
    assert!(matches!(pc.app.screen(), Screen::TxRefused(_)));
    assert_eq!(refusal_beeps(&pc.cmds()), 1, "the Radio task's refusal beeps once");
}

/// LINK-20.2, LINK-20.7: the radio's own refusals come back as `invalid` with the reason; the Radio task's stops too.
#[test]
fn remote_ptt_refusals_pass_through() {
    let mut pc = Pc::new(true); // a receive-only build
    let (s, a) = pc.ptt(Ptt::On(key::ACTIVE));
    assert_eq!((s, text(&a)), (Status::Invalid as u8, "receive-only build"));
    assert!(!keyed(&pc.cmds()) && !pc.app.remote_keyed());
    // outside the band plan
    let mut pc = Pc::tx();
    let a0 = pc.app.active as usize;
    (pc.app.slots[a0].vfo, pc.app.slots[a0].vfo_mode) = (ferrite_core::freq::mhz(160), true);
    let (s, a) = pc.ptt(Ptt::On(key::ACTIVE));
    assert_eq!((s, text(&a)), (Status::Invalid as u8, "outside the band plan"));
    assert!(matches!(pc.app.screen(), Screen::TxRefused(_)), "the radio shows it as for PTT");
    // keyed, then the Radio task stops it (the transmitter too hot): the next keepalive says so
    let mut pc = Pc::tx();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    let mut live = LiveAll::default();
    live.tx.refused = Some(ferrite_core::txplan::Refusal::Hot);
    pc.pass(100, &live);
    assert!(!pc.app.remote_keyed());
    assert_eq!(text(&pc.ptt(Ptt::Alive).1), "transmitter cooling");
    // the transmit timeout (TX-30.3)
    let mut pc = Pc::tx();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    let limit = pc.app.set.tx_timeout_s() * 1000;
    let mut t = 0;
    while t < limit + 500 {
        pc.pass(500, &LiveAll::default());
        pc.ptt(Ptt::Alive);
        t += 500;
    }
    assert!(!pc.app.remote_keyed());
    assert_eq!(text(&pc.ptt(Ptt::Alive).1), RemoteEnd::Timeout.text());
}

/// LINK-20.9, LINK-20.10, TX-35.1, TX-35.5: a remote key-up never transmits on an emergency frequency, never opens the
/// second-PTT prompt, and never answers one a person opened.
#[test]
fn remote_ptt_never_on_an_emergency_frequency() {
    let mut pc = Pc::tx();
    pc.app.set_region(Some(ferrite_core::txplan::Region::Us), true);
    let a0 = pc.app.active as usize;
    (pc.app.slots[a0].vfo, pc.app.slots[a0].vfo_mode) = (ferrite_core::freq::mhz(406) + 2_500, true);
    pc.cmds();
    let (s, a) = pc.ptt(Ptt::On(key::ACTIVE));
    assert_eq!(s, Status::Invalid as u8);
    assert!(text(&a).starts_with("emergency frequency"));
    assert!(!pc.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. } | RadioCmd::TxPlan { .. })));
    assert!(!matches!(pc.app.screen(), Screen::Emergency(_)), "no prompt");
    // A person's first PTT opens the prompt; a remote KEY on slot B doesn't confirm it.
    pc.app.key_down(Key::Ptt(PTT_A), pc.now, &pc.st);
    pc.app.key_up(Key::Ptt(PTT_A), pc.now + 100, &pc.st);
    assert!(matches!(pc.app.screen(), Screen::Emergency(_)));
    assert_eq!(pc.ptt(Ptt::On(1)).0, Status::Invalid as u8);
    assert!(!pc.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. } | RadioCmd::TxPlan { .. })));
}

/// APRS-21.3: a remote key-up on A on the APRS frequency while C is the APRS slot is refused (data only: PTT on
/// the radio, twice), and opens no prompt.
#[test]
fn remote_ptt_never_on_the_aprs_frequency() {
    let mut pc = Pc::tx();
    pc.app.set_region(Some(ferrite_core::txplan::Region::Us), true);
    pc.app.set_aprs_mode(true);
    let a0 = pc.app.active as usize;
    (pc.app.slots[a0].vfo, pc.app.slots[a0].vfo_mode) = (pc.app.aprs_freq(), true);
    pc.cmds();
    let (s, a) = pc.ptt(Ptt::On(key::ACTIVE));
    assert_eq!(s, Status::Invalid as u8);
    assert!(text(&a).starts_with("APRS frequency"), "{}", text(&a));
    assert!(!pc.cmds().iter().any(|c| matches!(c, RadioCmd::Transmit { on: true, .. } | RadioCmd::TxPlan { .. })));
    assert!(!matches!(pc.app.screen(), Screen::Emergency(_)), "no prompt");
}

/// LINK-20.8: the radio's operator wins. A local PTT during a remote transmission ends it and keys nothing until let
/// go; a remote key-up is refused while a local PTT is held; after, PTT keys as usual.
#[test]
fn remote_ptt_local_ptt_wins() {
    let mut pc = Pc::tx();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    pc.cmds();
    pc.app.key_down(Key::PTT, pc.now, &pc.st); // the side connector's, the same key the remote pressed
    let c = pc.cmds();
    assert!(unkeyed(&c) && !keyed(&c), "unkeyed, and the press keys nothing");
    assert!(!pc.app.remote_keyed());
    assert_eq!(text(&pc.ptt(Ptt::Alive).1), RemoteEnd::LocalPtt.text());
    let (s, a) = pc.ptt(Ptt::On(key::ACTIVE));
    assert_eq!((s, text(&a)), (Status::Invalid as u8, "PTT held on the radio"));
    pc.app.key_up(Key::PTT, pc.now + 200, &pc.st);
    assert!(pc.cmds().is_empty(), "its release does nothing either");
    // PTT B held: the remote waits; once let go, a person's PTT keys as usual.
    pc.app.key_down(Key::Ptt(PTT_B), pc.now, &pc.st);
    assert!(keyed(&pc.cmds()));
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, Status::Invalid as u8);
    pc.app.key_up(Key::Ptt(PTT_B), pc.now + 300, &pc.st);
    assert!(unkeyed(&pc.cmds()));
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0, "the remote again");
    pc.ptt(Ptt::Off);
}

/// LINK-20.15, TX-43.1: `KEY` is answered without room for the largest answer and with the store busy (on the cable its
/// answers wait in the send ring while keyed), so keepalives keep coming in.
#[test]
fn remote_ptt_answered_with_a_full_ring() {
    struct Small(Vec<u8>);
    impl Sink for Small {
        fn room(&self) -> usize {
            16usize.saturating_sub(self.0.len())
        }
        fn put(&mut self, b: &[u8]) {
            assert!(b.len() <= self.room(), "never cut");
            self.0.extend_from_slice(b);
        }
    }
    let mut pc = Pc::tx();
    let mut b = [0u8; 3];
    let n = Ptt::On(key::ACTIVE).encode(&mut b);
    let mut w = [0u8; MAX_WIRE];
    let k = wire::build(msg::KEY, 1, &b[..n], &mut w).unwrap();
    pc.raw(&w[..k]);
    let mut out = Small(Vec::new());
    pc.link.poll(&mut pc.app, &pc.st, &Device::default(), &LiveAll::default(), pc.now, &mut out);
    assert!(pc.app.remote_keyed() && !out.0.is_empty(), "answered with 16 bytes of room");
    // a full ring: the keepalive still counts, its answer is dropped whole
    let n = Ptt::Alive.encode(&mut b);
    let k = wire::build(msg::KEY, 2, &b[..n], &mut w).unwrap();
    pc.now += 1500;
    pc.raw(&w[..k]);
    pc.link.poll(&mut pc.app, &pc.st, &Device::default(), &LiveAll::default(), pc.now, &mut Small(std::vec![0; 16]));
    assert!(pc.link.ready(), "handled");
    pc.now += 1000;
    pc.app.tick(pc.now, &LiveAll::default(), &pc.st);
    assert!(pc.app.remote_keyed(), "the keepalive counted: 1 s since it");
}

/// LINK-20.14, LINK-30: on Bluetooth with a PIN lock, `KEY` only after `AUTH`.
#[test]
fn remote_ptt_needs_the_pin_on_bluetooth() {
    let mut bt = Pc::tx();
    bt.link = Session::new(false);
    (bt.app.set.pin, bt.app.set.pin_len, bt.app.set.pin_mode) = (1234, 4, 1);
    assert_eq!(bt.ptt(Ptt::On(key::ACTIVE)).0, Status::NotAuthorised as u8);
    assert!(!keyed(&bt.cmds()));
    assert_eq!(bt.ask(msg::AUTH, b"1234").0, 0);
    assert_eq!(bt.ptt(Ptt::On(key::ACTIVE)).0, 0);
    assert!(keyed(&bt.cmds()));
}

// ---- SLOT (LINK-33) ----

/// LINK-33: `SLOT` reads a slot, changes it through the keypad's paths (as the terminal's `slot`, TERM-35) and
/// answers it as it is after; a refusal is `invalid` with the reason and changes nothing.
#[test]
fn slot_read_and_set() {
    use ferrite_core::link::slot as m;
    let mut pc = Pc::new(true);
    let (s, a) = pc.ask(msg::SLOT, &[1]);
    assert_eq!((s, a.len(), a[0], a[1]), (0, 19, 1, 0), "slot B, not active");
    assert_eq!(u16::from_le_bytes([a[2], a[3]]), 0xFFFF, "on its VFO");
    let mut p = std::vec![1, m::FREQ | m::MODE | m::POWER | m::TX_TONE | m::ACTIVE];
    p.extend_from_slice(&(ferrite_core::freq::mhz(146) + 94_000).to_le_bytes());
    p.extend_from_slice(&[1, 0]);
    p.extend_from_slice(&1000u16.to_le_bytes());
    let (s, a) = pc.ask(msg::SLOT, &p);
    assert_eq!(s, 0);
    assert_eq!(u32::from_le_bytes([a[4], a[5], a[6], a[7]]), ferrite_core::freq::mhz(146) + 94_000);
    assert_eq!((a[1], a[8], a[9], u16::from_le_bytes([a[12], a[13]])), (1, 1, 0, 1000), "active, NFM, low, tone sent");
    assert_eq!(pc.app.active(), 1);
    // refused: nothing changes
    let mut p = std::vec![1, m::FREQ | m::MODE];
    p.extend_from_slice(&ferrite_core::freq::mhz(7).to_le_bytes());
    p.push(0);
    let (s, a) = pc.ask(msg::SLOT, &p);
    assert_eq!(s, Status::Invalid as u8);
    assert!(text(&a).contains("HF mode"));
    assert_eq!(pc.app.slot_state(1).mode, 1, "the mode not changed either");
    assert_eq!(pc.ask(msg::SLOT, &[3]).0, Status::BadArg as u8);
    assert_eq!(pc.ask(msg::SLOT, &[0, m::FREQ, 1, 2]).0, Status::BadArg as u8, "a field cut short");
    assert_eq!(pc.ask(msg::SLOT, &[0, 0x80]).0, Status::BadArg as u8);
    // OFFSET: a -0.6 MHz shift, then simplex (the shift cleared from the PC), then a split; split byte 2 is bad
    let off = |v: i32, split: u8| {
        let mut p = std::vec![1, m::OFFSET];
        p.extend_from_slice(&v.to_le_bytes());
        p.push(split);
        p
    };
    let (s, a) = pc.ask(msg::SLOT, &off(-60_000, 0));
    assert_eq!((s, i32::from_le_bytes([a[14], a[15], a[16], a[17]]), a[18] & m::SPLIT), (0, -60_000, 0));
    let (s, a) = pc.ask(msg::SLOT, &off(0, 0));
    assert_eq!((s, i32::from_le_bytes([a[14], a[15], a[16], a[17]])), (0, 0), "simplex");
    let tx = (ferrite_core::freq::mhz(147) + 50_000) as i32;
    let (s, a) = pc.ask(msg::SLOT, &off(tx, 1));
    assert_eq!((s, i32::from_le_bytes([a[14], a[15], a[16], a[17]]), a[18] & m::SPLIT), (0, tx, m::SPLIT));
    assert_eq!(pc.ask(msg::SLOT, &off(0, 2)).0, Status::BadArg as u8);
}

// ---- STATE and TUNE (LINK-67, LINK-68) ----

/// LINK-67: `STATE` answers the whole record: the version, the active slot, then each slot's flags, channel,
/// frequency and name as the slot is.
#[test]
fn state_record() {
    use ferrite_core::link::state as m;
    let mut pc = Pc::new(true);
    let (s, a) = pc.ask(msg::STATE, &[]);
    assert_eq!((s, a.len(), a[0], a[1]), (0, m::LEN, m::VERSION, pc.app.active()));
    assert_eq!(pc.ask(msg::STATE, &[1]).0, Status::BadArg as u8);
    // slot B to a channel: its part says so, with the channel's number, zone, frequency and name
    let c = (0..pc.st.count()).find_map(|i| pc.st.by_number(i)).filter(|c| !c.is_hf()).unwrap();
    let mut p = std::vec![1, ferrite_core::link::tune::CHANNEL];
    p.extend_from_slice(&c.num.to_le_bytes());
    assert_eq!(pc.ask(msg::TUNE, &p).0, 0);
    let (_, a) = pc.ask(msg::STATE, &[]);
    let b = &a[m::RADIO_LEN + m::SLOT_LEN..m::RADIO_LEN + 2 * m::SLOT_LEN];
    assert_eq!(b[0] & m::ON_CHANNEL, m::ON_CHANNEL);
    assert_eq!((u16::from_le_bytes([b[1], b[2]]), u16::from_le_bytes([b[3], b[4]])), (c.num, c.zone));
    assert_eq!(u32::from_le_bytes([b[5], b[6], b[7], b[8]]), c.rx);
    assert_eq!(i16::from_le_bytes([b[11], b[12]]), m::NO_SIGNAL, "squelch shut: no signal");
    let name = &b[13..13 + m::NAME_LEN];
    assert_eq!(&name[..c.name().len().min(m::NAME_LEN)], &c.name()[..c.name().len().min(m::NAME_LEN)]);
    // the radio's part: the position the store has (the sample's fix)
    let f = crate::data::Data::fix(pc.st.data()).unwrap();
    assert_eq!((i32::from_le_bytes([a[8], a[9], a[10], a[11]]), i32::from_le_bytes([a[12], a[13], a[14], a[15]])), (f.pos.lat, f.pos.lon));
}

/// LINK-68: `TUNE` moves a slot as home's keys do with it active (a channel by number, ▲▼, ◀▶, a zone, SIDE2's
/// scan), leaves the active slot as it was (a scan's slot becomes active), and answers as `SLOT` reads it; refusals change nothing.
#[test]
fn tune_by_command() {
    use ferrite_core::link::tune as t;
    let mut pc = Pc::new(true);
    let tune = |pc: &mut Pc, s: u8, op: u8, arg: &[u8]| {
        let mut p = std::vec![s, op];
        p.extend_from_slice(arg);
        pc.ask(msg::TUNE, &p)
    };
    let active = pc.app.active();
    let other = (active + 1) % 3;
    // ▲ twice on a VFO: two of its steps up
    let f0 = pc.app.slot_state(other).freq;
    let (s, a) = tune(&mut pc, other, t::STEP, &[2]);
    assert_eq!((s, a.len(), a[0]), (0, 19, other));
    let f1 = u32::from_le_bytes([a[4], a[5], a[6], a[7]]);
    assert!(f1 > f0, "{f0} -> {f1}");
    assert_eq!(pc.app.active(), active, "the active slot stays");
    let (_, a) = tune(&mut pc, other, t::STEP, &[(-2i8) as u8]);
    assert_eq!(u32::from_le_bytes([a[4], a[5], a[6], a[7]]), f0, "and back");
    // a channel by number, then ▲ to the next in its zone, then zones
    let c = (0..pc.st.count()).find_map(|i| pc.st.by_number(i)).filter(|c| !c.is_hf()).unwrap();
    let (s, a) = tune(&mut pc, other, t::CHANNEL, &c.num.to_le_bytes());
    assert_eq!((s, u16::from_le_bytes([a[2], a[3]])), (0, c.num));
    if pc.st.zone_len(c.zone) > 1 {
        let (s, a) = tune(&mut pc, other, t::STEP, &[1]);
        assert_eq!(s, 0);
        assert_ne!(u16::from_le_bytes([a[2], a[3]]), c.num, "the next channel");
    }
    let (s, _) = tune(&mut pc, other, t::ZONE, &0u16.to_le_bytes());
    assert_eq!(s, 0);
    assert_eq!(pc.app.slots[other as usize].zone, 0);
    if pc.st.zones() > 1 {
        assert_eq!(tune(&mut pc, other, t::GROUP, &[1]).0, 0);
        assert_ne!(pc.app.slots[other as usize].zone, 0, "the next zone");
    }
    // refusals: an empty number, no such zone; nothing changes
    let before = pc.app.slot_state(other);
    let (s, a) = tune(&mut pc, other, t::CHANNEL, &3999u16.to_le_bytes());
    assert_eq!(s, Status::Invalid as u8);
    assert!(text(&a).contains("no channel"), "{}", text(&a));
    let (s, _) = tune(&mut pc, other, t::ZONE, &500u16.to_le_bytes());
    assert_eq!(s, Status::Invalid as u8);
    assert_eq!(pc.app.slot_state(other), before);
    // SIDE2: the scan starts (the scan screen up; the Radio task reports it scanning), the slot refuses steps, then
    // stops
    assert_eq!(tune(&mut pc, other, t::SCAN, &[1]).0, 0);
    assert!(matches!(pc.app.screen, Screen::Scan(_)), "the scan screen, as SIDE2 opens it");
    assert_eq!(pc.app.active(), other, "a scan's slot becomes the active one (SIDE2 scans the active slot)");
    pc.app.scan_mask |= 1 << other;
    let (s, a) = tune(&mut pc, other, t::STEP, &[1]);
    assert_eq!(s, Status::Invalid as u8);
    assert!(text(&a).contains("scanning"));
    while pc.app.pop_cmd().is_some() {}
    assert_eq!(tune(&mut pc, other, t::SCAN, &[0]).0, 0);
    let mut stopped = false;
    while let Some(c) = pc.app.pop_cmd() {
        stopped |= c == ferrite_core::msg::RadioCmd::Scan { slot: other, on: false };
    }
    assert!(stopped, "the Radio task told to stop (it reports the slot scanning no more)");
    assert_eq!(pc.app.active(), other, "and stays active");
    // malformed
    for p in [&[other, t::STEP, 0][..], &[other, t::STEP, 101], &[other, t::GROUP, 21], &[other, t::SCAN, 2], &[other, t::CHANNEL, 1], &[3, t::STEP, 1], &[other, 9, 1], &[other]] {
        assert_eq!(pc.ask(msg::TUNE, p).0, Status::BadArg as u8, "{p:?}");
    }
}

/// LINK-68: a scan started by `TUNE` while another screen is in use leaves that screen up (the scan screen opens
/// only over home); in channel-only mode only the active slot can be tuned (PIN-5), as `SLOT` refuses the others.
#[test]
fn tune_keeps_the_screen_and_channel_only() {
    use ferrite_core::link::tune as t;
    let mut pc = Pc::new(true);
    let other = (pc.app.active() + 1) % 3;
    let up = Screen::Status(crate::screens::status::Status { row: 0, from_menu: true });
    pc.app.screen = up;
    assert_eq!(pc.ask(msg::TUNE, &[other, t::SCAN, 1]).0, 0);
    assert_eq!(pc.app.screen, up, "the screen in use stays up");
    assert_eq!(pc.app.active(), other, "the scan's slot still becomes the active one");
    let mut pc = Pc::new(true);
    pc.app.set.chan_only = true;
    let active = pc.app.active();
    let (s, a) = pc.ask(msg::TUNE, &[(active + 1) % 3, t::STEP, 1]);
    assert_eq!(s, Status::Invalid as u8);
    assert!(text(&a).contains("PIN-5"), "{}", text(&a));
}

/// LINK-68: `TUNE` op 6 sets a slot on, muted or off as \* does (HOME-15, HOME-15.1), any slot, the active one
/// kept; `STATE` shows it (64 muted, 128 off); off leaves the watch (WATCH-2.1). The APRS slot mutes (APRS-2's
/// "Mute speaker for APRS" follows) but is never off; 3 or more is a bad argument.
#[test]
fn tune_mute_and_off() {
    use ferrite_core::link::{state as m, tune as t};
    use ferrite_core::msg::RadioCmd;
    let mut pc = Pc::new(true);
    let active = pc.app.active();
    let b = (active + 1) % 3;
    let flags = |pc: &mut Pc, s: u8| pc.ask(msg::STATE, &[]).1[m::RADIO_LEN + s as usize * m::SLOT_LEN];
    while pc.app.pop_cmd().is_some() {}
    let (s, a) = pc.ask(msg::TUNE, &[b, t::QUIET, 1]);
    assert_eq!((s, a[0]), (0, b), "answered as SLOT reads it");
    assert_eq!(flags(&mut pc, b) & (m::MUTED | m::OFF), m::MUTED);
    assert_eq!(pc.app.active(), active, "the active slot stays");
    assert_eq!(pc.ask(msg::TUNE, &[b, t::QUIET, 2]).0, 0);
    assert_eq!(flags(&mut pc, b) & (m::MUTED | m::OFF), m::OFF);
    let mut sent_off = false;
    while let Some(c) = pc.app.pop_cmd() {
        sent_off |= c == RadioCmd::SlotOff(1 << b);
        if let RadioCmd::Watch(w) = c {
            assert_eq!(w & 1 << b, 0, "an off slot leaves the watch");
        }
    }
    assert!(sent_off, "the Radio task told");
    assert_eq!(pc.ask(msg::TUNE, &[b, t::QUIET, 0]).0, 0);
    assert_eq!(flags(&mut pc, b) & (m::MUTED | m::OFF), 0);
    for q in [3u8, 255] {
        assert_eq!(pc.ask(msg::TUNE, &[b, t::QUIET, q]).0, Status::BadArg as u8);
    }
    assert_eq!(pc.ask(msg::TUNE, &[b, t::QUIET]).0, Status::BadArg as u8);
    // the APRS slot (C in APRS mode): muted turns on "Mute speaker for APRS"; off is refused, nothing changed
    pc.app.set.aprs_mode = true;
    assert!(pc.app.aprs_slot(2));
    assert_eq!(pc.ask(msg::TUNE, &[2, t::QUIET, 1]).0, 0);
    assert!(pc.app.set.aprs_mute);
    let (s, a) = pc.ask(msg::TUNE, &[2, t::QUIET, 2]);
    assert_eq!(s, Status::Invalid as u8);
    assert!(text(&a).contains("never off"), "{}", text(&a));
    assert_eq!(flags(&mut pc, 2) & (m::MUTED | m::OFF), m::MUTED);
    assert_eq!(pc.ask(msg::TUNE, &[2, t::QUIET, 0]).0, 0);
    assert!(!pc.app.set.aprs_mute);
}

/// LINK-69: `BEACON` sends a beacon now as the radio's Beacon now does, with its checks: refused with APRS and
/// beacons off, while KISS holds the APRS slot, and without a callsign; nothing asked of the APRS task then.
#[test]
fn beacon_from_the_link() {
    let mut pc = Pc::new(true);
    let (s, a) = pc.ask(msg::BEACON, &[]);
    assert_eq!((s, text(&a)), (Status::Invalid as u8, "APRS and beacons off"));
    assert!(!pc.st.beacon_asked());
    pc.app.set_aprs_mode(true);
    assert_eq!(pc.ask(msg::BEACON, &[]).0, 0);
    assert!(pc.st.beacon_asked(), "the APRS task asked for a beacon");
    assert_eq!(pc.ask(msg::BEACON, &[1]).0, Status::BadArg as u8);
    (pc.app.set.bt, pc.app.set.kiss_port) = (true, 2);
    let (s, a) = pc.ask(msg::BEACON, &[]);
    assert_eq!((s, text(&a)), (Status::Invalid as u8, "KISS on: the app sends"));
}

// ---- CMD and MORE (LINK-34) ----

impl Pc {
    /// A terminal command over the link: every part's text joined, or the refusal (status, text).
    fn cmd(&mut self, line: &str) -> Result<std::string::String, (u8, std::string::String)> {
        let (s, a) = self.ask(msg::CMD, line.as_bytes());
        if s != 0 {
            return Err((s, text(&a).into()));
        }
        let mut all: std::string::String = text(&a[1..]).into();
        let mut more = a[0];
        let mut parts = 0;
        while more == 1 {
            let (s, a) = self.ask(msg::MORE, &[]);
            assert_eq!(s, 0);
            assert!(a.len() < MAX_PAYLOAD);
            assert!(a.len() == 1 || a.last() == Some(&b'\n'), "whole lines only");
            all.push_str(text(&a[1..]));
            more = a[0];
            parts += 1;
            assert!(parts < 2000);
        }
        Ok(all)
    }
}

/// LINK-34: a terminal command line over the link runs as in the terminal (TERM-12): its answer line, a list
/// paged over as many `MORE`s as it takes in whole lines, the terminal's refusals as `invalid` with its text.
#[cfg(feature = "line-cmds")]
#[test]
fn terminal_commands_over_the_link() {
    let mut pc = Pc::new(true);
    let help = pc.cmd("help").unwrap();
    assert!(help.starts_with("Commands:") && help.contains("  slot, slot A") && help.lines().count() > 20, "{help}");
    let s = pc.cmd("slot B 146.94 nfm").unwrap();
    assert!(s.starts_with("B  vfo 146.94000 mode=NFM"), "{s}");
    assert_eq!(pc.app.slot_state(1).mode, 1, "the same change as the terminal's");
    assert_eq!(pc.cmd("slot").unwrap().lines().count(), 3);
    assert!(pc.cmd("set 0 7 1 Dark").is_ok());
    assert!(pc.cmd("get 0 7 1").unwrap().contains("Dark"));
    let (s, why) = pc.cmd("frobnicate").unwrap_err();
    assert_eq!((s, why.contains("no such command")), (Status::Invalid as u8, true), "{why}");
    let (s, why) = pc.cmd("slot A 7.1").unwrap_err();
    assert!(s == Status::Invalid as u8 && why.contains("HF mode"), "{why}");
    assert_eq!(pc.cmd("load").unwrap_err().0, Status::Invalid as u8, "load stays the terminal's");
    let dump = pc.cmd("dump").unwrap();
    assert!(dump.lines().any(|l| l.starts_with("ch ")) && dump.lines().any(|l| l.starts_with("set ")), "the whole dump");
    assert_eq!(pc.ask(msg::MORE, &[]), (0, std::vec![0]), "nothing running");
}

#[cfg(feature = "bench")]
/// LINK-34, TERM-21: a debug command over the link asks the firmware as the terminal's does; MORE says "still
/// working" until the answer comes, then gives its lines. A new CMD ends one still running.
#[test]
fn debug_commands_over_the_link() {
    let mut pc = Pc::new(true);
    let (s, a) = pc.ask(msg::CMD, b"bk 0 30");
    assert_eq!((s, a.as_slice()), (0, &[1u8][..]), "waiting for the firmware");
    let r = pc.link.debug_req().expect("the request for the Radio task");
    assert_eq!(r, DebugReq::BkRead { chip: 0, reg: 0x30 });
    assert!(pc.link.debug_req().is_none(), "once");
    assert_eq!(pc.ask(msg::MORE, &[]).1, std::vec![1], "still waiting");
    pc.link.debug_answer(DebugAns::Reg { chip: 0, reg: 0x30, val: 0xBFF1 });
    let (_, a) = pc.ask(msg::MORE, &[]);
    assert_eq!(a[0], 0);
    assert!(text(&a[1..]).to_ascii_lowercase().contains("bff1"), "{}", text(&a[1..]));
    // ended by the next command
    pc.ask(msg::CMD, b"bk 0 31");
    assert!(pc.link.debug_req().is_some());
    assert!(pc.cmd("slot A").is_ok());
    assert!(pc.link.debug_req().is_none());
    // refused before anything is sent, as in the terminal (TERM-23: registers to 0x7F)
    assert_eq!(pc.ask(msg::CMD, b"bk 0 99").0, Status::Invalid as u8);
}

/// LINK-34, TERM-38: over the link a user build refuses the bench commands as its terminal does (Invalid, with the
/// reason), sends nothing to the firmware, and still answers `gps` (the last fix).
#[cfg(not(feature = "bench"))]
#[cfg(feature = "line-cmds")]
#[test]
fn bench_commands_refused_over_the_link() {
    let mut pc = Pc::new(true);
    for c in ["txtest suite", "bk 0 30", "mem 20000000", "gps standby 60"] {
        let (s, why) = pc.cmd(c).unwrap_err();
        assert!(s == Status::Invalid as u8 && why.contains("not in this build: a bench build has it"), "{c}: {why}");
        assert!(pc.link.debug_req().is_none(), "{c}");
    }
    pc.ask(msg::CMD, b"gps");
    assert_eq!(pc.link.debug_req(), Some(DebugReq::Gga));
}

/// LINK-34, LINK-30: on Bluetooth with a PIN lock, commands only after AUTH, as the terminal's PIN prompt.
#[cfg(feature = "line-cmds")]
#[test]
fn terminal_commands_need_the_pin_on_bluetooth() {
    let mut bt = Pc::new(false);
    (bt.app.set.pin, bt.app.set.pin_len, bt.app.set.pin_mode) = (1234, 4, 1);
    assert_eq!(bt.ask(msg::CMD, b"slot").0, Status::NotAuthorised as u8);
    assert_eq!(bt.ask(msg::MORE, &[]).0, Status::NotAuthorised as u8);
    assert_eq!(bt.ask(msg::AUTH, b"1234").0, 0);
    assert!(bt.cmd("slot").is_ok());
}

// ---- Remote keys and the UI readback (LINK-19) ----

use crate::uistate::{key_code, key_line};

impl Pc {
    /// `KEY` for a keypad key by name: (status, text).
    fn press(&mut self, name: &str, action: u8) -> (u8, std::string::String) {
        let (s, a) = self.ask(msg::KEY, &[key_code(name.as_bytes()).unwrap(), action]);
        (s, text(&a).into())
    }

    /// Several short presses; each must be ok.
    fn keys(&mut self, names: &str) {
        for n in names.split(' ') {
            assert_eq!(self.press(n, key::SHORT), (0, std::string::String::new()), "{n}");
        }
    }

    /// `UI`: the whole text, over as many `MORE`s as it takes.
    fn ui(&mut self) -> std::string::String {
        self.ui_what(0)
    }

    /// `UI` with its what byte (3: with the screen as drawn, LINK-19.5).
    fn ui_what(&mut self, w: u8) -> std::string::String {
        let (s, a) = self.ask(msg::UI, &[w]);
        assert_eq!(s, 0);
        let mut all: std::string::String = text(&a[1..]).into();
        let mut more = a[0];
        while more == 1 {
            let (s, a) = self.ask(msg::MORE, &[]);
            assert!(s == 0 && (a.len() == 1 || a.last() == Some(&b'\n')), "whole lines");
            all.push_str(text(&a[1..]));
            more = a[0];
        }
        all
    }

    fn lines(&mut self, kind: &str) -> Vec<std::string::String> {
        self.ui().lines().filter(|l| l.split(' ').next() == Some(kind)).map(|l| l.into()).collect()
    }
}

/// TX-48: the link's remote keys reach 0 8 8 5 but never change Dry TX (OK, #), on or off; the bar's readback
/// says `dry=` either way.
#[test]
fn remote_keys_never_change_dry_tx() {
    let mut pc = Pc::new(true);
    pc.app.radio.tx_built = true;
    pc.keys("MENU 0 8 8 5 OK");
    assert_eq!(std::format!("{:?}", pc.app.screen()), "Group(Group { g: 8, sub: 8, row: 4 })", "on Dry TX's row");
    assert!(!pc.app.dry_tx(), "OK over the link: still off");
    assert!(pc.lines("bar")[0].contains(" dry=0"));
    pc.app.set_dry_tx(true);
    pc.keys("OK #");
    assert!(pc.app.dry_tx(), "OK and # over the link: still on");
    assert!(pc.lines("bar")[0].contains(" dry=1"));
}

/// LINK-19: every front key and side key by stock's key code under its name; the PTTs have no name.
#[test]
fn remote_key_names() {
    let want = [
        ("MENU", Key::Menu), ("OK", Key::Menu), ("BACK", Key::Exit), ("exit", Key::Exit), ("UP", Key::Up), ("DOWN", Key::Down),
        ("LEFT", Key::Left), ("RIGHT", Key::Right), ("SLOT", Key::Slot), ("VM", Key::VfoMem), ("STAR", Key::Star), ("*", Key::Star),
        ("HASH", Key::Hash), ("#", Key::Hash), ("SIDE1", Key::Side(1)), ("PF2", Key::Side(2)), ("0", Key::Digit(0)), ("9", Key::Digit(9)),
    ];
    for (n, k) in want {
        assert_eq!(key_code(n.as_bytes()).and_then(ferrite_core::keys::key), Some(k), "{n}");
    }
    assert_eq!(key_code(b"PTT"), None);
    let mut l = crate::term::cmd::Line::new();
    let names: Vec<_> = (0..).map_while(|i| key_line(i, &mut l).then(|| std::string::String::from(text(l.as_bytes())))).collect();
    assert_eq!(names.len(), 23, "22 keys and the knob: {names:?}");
    for c in 0..22u8 {
        assert!(ferrite_core::keys::key(c).is_some_and(|k| !matches!(k, Key::Ptt(_))), "code {c} is a key, not a PTT");
    }
}

/// LINK-19: remote keys go through the keypad's path: MENU opens quick settings, digits and the knob move through
/// the menus, BACK goes back; the readback shows each screen, its rows and the selected one.
#[test]
fn remote_keys_drive_the_menus() {
    let mut pc = Pc::new(true);
    assert!(pc.ui().starts_with("screen Home \"Home\""));
    assert_eq!(pc.lines("card").len(), 3, "a card per slot");
    pc.keys("MENU");
    assert!(matches!(pc.app.screen(), Screen::Quick(_)));
    let rows = pc.lines("row");
    assert_eq!(rows.len(), 11, "nine numbered settings, RF gain and 0 More: {rows:?}");
    assert_eq!(rows[9], "row 10 \"RF gain\" \"Auto\"");
    assert!(rows[0].starts_with("row 1 \"Squelch\"") && rows[0].contains(" sel"), "{rows:?}");
    pc.keys("0 7");
    let ui = pc.ui();
    assert!(ui.starts_with("screen Group \"Settings group\" title=\"Display\""), "{ui}");
    assert!(ui.contains("row 1 \"Theme\" \"Dark\" sel") && ui.contains("help \"MENU 0 7 1\""), "{ui}");
    assert_eq!(pc.ask(msg::KEY, &[key::KNOB, 1]).0, 0);
    assert!(pc.ui().contains("row 1 \"Theme\" \"Daylight\" sel dot"), "the knob changed it");
    assert_eq!(pc.ask(msg::KEY, &[key::KNOB, (-1i8) as u8]).0, 0);
    pc.keys("DOWN");
    let sel: Vec<_> = pc.lines("row").into_iter().filter(|r| r.contains(" sel")).collect();
    assert_eq!(sel.len(), 1);
    assert!(sel[0].starts_with("row 2 \"Layout\""), "{sel:?}");
    pc.keys("BACK");
    assert!(matches!(pc.app.screen(), Screen::Advanced(_)));
    assert_eq!(pc.ask(msg::KEY, &[key::KNOB, 0]).0, Status::Invalid as u8, "no steps");
    assert_eq!(pc.ask(msg::KEY, &[key::KNOB, 21]).0, Status::Invalid as u8, "20 at most");
}

/// LINK-19, KEY-3, KEY-13, KEY-14.4: a long press is held to the long-press time and let go by the radio (keys
/// answer `busy` meanwhile); hold # locks, and 0 then chooses All but PTT (the readback's `lockmode=`); a locked radio takes remote keys as it takes the radio's (the toast, nothing
/// else); down and up hold a key, and a session that moves port lets go of what it held.
#[test]
fn remote_long_press_lock_and_hold() {
    let mut pc = Pc::new(true);
    assert_eq!(pc.press("HASH", key::LONG).0, 0);
    assert_eq!(pc.press("MENU", key::SHORT).0, Status::Busy as u8, "the long press is still held");
    assert!(!pc.app.locked(), "not before 700 ms");
    pc.pass(800, &LiveAll::default());
    assert!(pc.app.locked() && pc.ui().contains("state ") && pc.ui().contains(" locked=1"));
    assert!(pc.ui().contains("bar lock=1 lockmode=keypad ") && pc.ui().contains("popup \"Keypad locked\""), "KEY-13");
    pc.keys("0");
    assert!(pc.ui().contains("bar lock=1 lockmode=allbutptt "), "KEY-13: a remote 0 chooses as the radio's");
    pc.keys("MENU");
    assert_eq!(pc.app.screen(), Screen::Home, "KEY-14.4: locked");
    assert!(pc.ui().contains("toast \"Locked:\""), "{}", pc.ui());
    assert_eq!(pc.press("HASH", key::LONG).0, 0);
    pc.pass(800, &LiveAll::default());
    assert!(!pc.app.locked());
    // Down, held past the long press (hold MENU on home: advanced settings, MENU-2), then up.
    assert_eq!(pc.press("MENU", key::DOWN).0, 0);
    assert_eq!(pc.press("MENU", key::DOWN).0, 0, "down again: still the one press");
    pc.pass(800, &LiveAll::default());
    assert_eq!(pc.press("MENU", key::UP).0, 0);
    assert!(matches!(pc.app.screen(), Screen::Advanced(_)), "{:?}", pc.app.screen());
    assert_eq!(pc.press("UP", key::UP).0, 0, "up for a key not held: nothing");
    // A key held when the session moves port is let go.
    pc.keys("BACK");
    assert_eq!(pc.press("VM", key::DOWN).0, 0);
    assert_ne!(pc.app.remote_keys_held(), 0);
    pc.link.set_port(true, 115_200);
    pc.pass(100, &LiveAll::default());
    assert_eq!(pc.app.remote_keys_held(), 0, "released with the session");
}

/// LINK-19 (safety): a remote key never keys the transmitter. The PTT codes aren't keys; the dialer's Send, a
/// PTT path or a message from a remote key is refused with a toast; and while the radio transmits, remote keys
/// (DTMF digits then) are refused.
#[test]
fn remote_keys_never_transmit() {
    let mut pc = Pc::tx();
    for c in [24u8, 25, 26] {
        assert_eq!(pc.ask(msg::KEY, &[c, key::SHORT]).0, Status::BadArg as u8);
    }
    pc.keys("MENU 0 8 5 1");
    assert!(matches!(pc.app.screen(), Screen::Dtmf(_)), "{:?}", pc.app.screen());
    pc.keys("1 2 3");
    pc.cmds();
    pc.keys("MENU"); // Send
    let c = pc.cmds();
    assert!(!keyed(&c) && !c.iter().any(|c| matches!(c, RadioCmd::DtmfSeq { .. } | RadioCmd::DtmfSend { .. })), "{c:?}");
    assert!(pc.ui().contains("toast \"Remote keys\" \"never transmit\""), "{}", pc.ui());
    // A person's OK on the same dialer sends.
    pc.key(Key::Menu);
    assert!(keyed(&pc.cmds()), "the keypad's Send keys");
    pc.key(Key::Ptt(PTT_A)); // a PTT ends the dialled transmission (TX-25)
    pc.cmds();
    // While remote PTT transmits, remote keys (DTMF digits on the Transmit screen) are refused.
    pc.app.go_home();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    pc.cmds();
    let (s, why) = pc.press("5", key::SHORT);
    assert_eq!(s, Status::Invalid as u8);
    assert!(why.contains("transmitting"), "{why}");
    assert_eq!(pc.ask(msg::KEY, &[key::KNOB, 1]).0, Status::Invalid as u8);
    assert!(!pc.cmds().iter().any(|c| matches!(c, RadioCmd::Dtmf { .. })), "no DTMF");
    assert!(pc.ui().contains("transmit slot=A remote=1"));
    pc.ptt(Ptt::Off);
    assert!(!pc.app.remote_in, "never left on");
}

/// LINK-19, DISP-15.1 (decision 162): a remote key on a dark screen lights it and acts (the PC or phone can't see
/// the screen is dark), unlike the radio's own keys.
#[test]
fn remote_key_wakes_and_acts() {
    let mut pc = Pc::new(true);
    pc.app.set.backlight = 1;
    pc.app.backlight_out();
    pc.pass(100, &LiveAll::default());
    assert!(pc.app.dark());
    assert!(pc.ui().contains(" dark=1"));
    pc.keys("MENU");
    assert!(!pc.app.dark());
    assert!(matches!(pc.app.screen(), Screen::Quick(_)), "woken and acted");
}

/// LINK-19, TERM-37: the readback's lines on several screens: the screen line first, the hints last, the status
/// bar, a value editor and a question; long ones page over `MORE`, the same text as the terminal's `ui`; the
/// terminal's `key` and `knob`.
#[test]
fn ui_readback_screens() {
    let mut pc = Pc::new(true);
    let ui = pc.ui();
    let lines: Vec<_> = ui.lines().collect();
    assert!(lines[0].starts_with("screen Home") && lines[1].starts_with("state frame=") && lines[2].starts_with("bar lock=0"));
    assert!(lines.last().unwrap().starts_with("hints \"Hold ABC\""), "{ui}");
    assert!(lines[3].starts_with("card A active=1 head=\"VFO") && lines[3].contains(" freq=146.52000 "), "{ui}");
    assert!(ui.len() > MAX_PAYLOAD, "home is longer than one answer, so it pages");
    let mut term = std::string::String::new();
    let mut l = crate::term::cmd::Line::new();
    let mut i = 0;
    while crate::term::cmd::list_line(&pc.app, &pc.st, &pc.dev, &LiveAll::default(), crate::term::cmd::List::Ui, i, &mut l) {
        term.push_str(text(l.as_bytes()));
        term.push('\n');
        i += 1;
    }
    assert_eq!(term, ui, "the terminal's ui is the same text");
    // A value editor (MENU 1 OK: squelch).
    pc.keys("MENU OK");
    let ui = pc.ui();
    assert!(ui.starts_with("screen Value") && ui.contains("value \"Squelch\" \""), "{ui}");
    // A question: the zones list's delete asks; here the reset (MENU 0 9 7 2).
    pc.app.go_home();
    pc.keys("MENU 0 9 7 2");
    let ui = pc.ui();
    assert!(ui.starts_with("screen Confirm") && ui.contains("ask \"Reset the radio?\""), "{ui}");
    assert!(ui.contains("hints \"OK\" \"Next\" \"BACK\" \"Cancel\""), "{ui}");
    pc.keys("BACK");
    pc.app.go_home();
    // The terminal's commands do the same (here over CMD, LINK-34).
    if !cfg!(feature = "line-cmds") {
        return; // the rest uses the line commands (decision 347)
    }
    assert!(pc.cmd("ui").unwrap().starts_with("screen Home"));
    assert!(pc.cmd("ui keys").unwrap().contains("MENU 6 OK\n"));
    assert_eq!(pc.cmd("key back long").unwrap(), "key BACK long\n");
    pc.pass(800, &LiveAll::default());
    assert!(pc.cmd("key PTT").is_err());
    assert_eq!(pc.cmd("knob +3").unwrap(), "knob +3\n");
    assert!(pc.cmd("knob 0").is_err() && pc.cmd("knob -21").is_err());
}

// ---- The crossband repeater from the link (XB-20, decision 161) ----

use crate::screens::activity::Mode;
use crate::screens::xb::{XbLog, BY_CMD, BY_KEYS, BY_REMOTE, XB_CMD, XB_KEY};

impl Pc {
    fn xb_running(&self) -> bool {
        self.app.radio.mode == Mode::Xb
    }

    fn xb_cmd(c: &[RadioCmd]) -> Option<bool> {
        c.iter().rev().find_map(|c| match c {
            RadioCmd::Crossband(x) => Some(x.is_some()),
            _ => None,
        })
    }
}

/// XB-20.1, XB-20.4, XB-20.6, LINK-34: `xb start` over the link opens the repeater's screen from wherever the radio
/// is (home under it) and starts it as # does; the readback says it runs and who started it; `xb` tells; `xb stop`
/// stops it; each is logged with who and why. Refusals say why and start nothing.
#[cfg(feature = "line-cmds")]
#[test]
fn xb_command_starts_and_stops_the_repeater() {
    let mut pc = Pc::tx();
    let before = *pc.app.settings();
    pc.keys("MENU 0 7"); // somewhere in the menus
    assert_eq!(pc.cmd("xb").unwrap(), "Crossband: off (A 146.52000 <-> B 443.47500)\n");
    assert_eq!(pc.cmd("xb stop").unwrap_err().1, "xb stop: the repeater isn't running");
    let s = pc.cmd("xb start").unwrap();
    assert!(s.starts_with("Crossband started: A 146.52000 <-> B "), "{s}");
    assert!(pc.xb_running() && matches!(pc.app.screen(), Screen::Crossband(_)), "its screen is up: {:?}", pc.app.screen());
    assert_eq!(Pc::xb_cmd(&pc.cmds()), Some(true), "the Radio task told to start");
    assert_eq!(pc.app.take_xb_log(), Some(XbLog { on: true, by: BY_CMD | crate::remote::BY_CABLE, why: 0 }));
    let ui = pc.ui();
    assert!(ui.starts_with("screen Crossband"), "{ui}");
    assert!(ui.contains("xb running=1 state=0 by=cmd:cable side1=A side2=B pass=0 passes=0 listen=0 ab=0 ba=0"), "{ui}");
    assert!(ui.contains("hints \"Any key\" \"Stop\" \"SIDE1\" \"Listen\""), "{ui}");
    assert!(pc.cmd("xb").unwrap().starts_with("Crossband: running, A 146.52000 <-> B "), "xb tells");
    assert!(pc.cmd("xb start").unwrap_err().1.contains("already running"));
    assert_eq!(pc.cmd("xb stop").unwrap(), "Crossband stopped\n");
    assert!(!pc.xb_running());
    assert_eq!(Pc::xb_cmd(&pc.cmds()), Some(false), "the Radio task told to stop");
    assert_eq!(pc.app.take_xb_log(), Some(XbLog { on: false, by: BY_CMD | crate::remote::BY_CABLE, why: XB_CMD }));
    assert!(pc.ui().contains("xb running=0"), "back on the start page");
    // XB-20.7, XB-1: nothing of it is kept in the settings (never on at power-up).
    assert_eq!(*pc.app.settings(), before);
    // XB-2 through the command: refused with the start page's reason (here the region forbids transmitting).
    pc.app.set_region(None, false);
    let (s, why) = pc.cmd("xb start").unwrap_err();
    assert!(s == Status::Invalid as u8 && why.starts_with("xb start: can't start: "), "{why}");
    assert!(!pc.xb_running() && pc.app.take_xb_log().is_none(), "nothing started, nothing logged");
    pc.app.set_region(Some(ferrite_core::txplan::Region::Us), false);
    // Not while transmitting (remote PTT here), nor in channel-only mode.
    pc.app.go_home();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    assert!(pc.cmd("xb start").unwrap_err().1.contains("transmitting"));
    pc.ptt(Ptt::Off);
    pc.app.set.chan_only = true;
    assert!(pc.cmd("xb start").unwrap_err().1.contains("channel-only"));
    pc.app.set.chan_only = false;
    // A receive-only build: refused as # is (decision 93).
    pc.app.radio.tx_built = false;
    assert!(pc.cmd("xb start").unwrap_err().1.contains("can't start"));
    assert!(!pc.xb_running());
}

/// XB-20.2, LINK-19: remote keys start the repeater with # on its start page, as the radio's keys do (the one thing
/// a remote key may start that transmits); XB-20.5: any remote key but SIDE1 then stops it, and does nothing else.
#[test]
fn remote_keys_start_and_stop_the_repeater() {
    let mut pc = Pc::tx();
    pc.keys("MENU 0 3 4");
    assert!(matches!(pc.app.screen(), Screen::Crossband(_)));
    pc.keys("#");
    assert!(pc.xb_running(), "{}", pc.ui());
    assert!(pc.ui().contains(" by=keys:cable "), "{}", pc.ui());
    assert_eq!(pc.app.take_xb_log(), Some(XbLog { on: true, by: BY_REMOTE | crate::remote::BY_CABLE, why: 0 }));
    // SIDE1 held listens (XB-15) and doesn't stop it.
    assert_eq!(pc.press("SIDE1", key::DOWN).0, 0);
    assert!(pc.xb_running() && pc.ui().contains(" listen=1 "), "{}", pc.ui());
    let c = pc.cmds();
    assert!(c.contains(&RadioCmd::Monitor { slot: 0, on: true }) && c.contains(&RadioCmd::Monitor { slot: 1, on: true }), "{c:?}");
    assert_eq!(pc.press("SIDE1", key::UP).0, 0);
    assert!(pc.ui().contains(" listen=0 ") && pc.cmds().contains(&RadioCmd::Monitor { slot: 0, on: false }));
    // A digit stops it; nothing else happens (no frequency entry, the start page stays up).
    pc.keys("5");
    assert!(!pc.xb_running());
    assert!(matches!(pc.app.screen(), Screen::Crossband(_)), "{:?}", pc.app.screen());
    assert_eq!(pc.app.take_xb_log(), Some(XbLog { on: false, by: BY_REMOTE | crate::remote::BY_CABLE, why: XB_KEY }));
    assert!(pc.ui().contains("toast \"Repeater stopped\" \"from the link\""), "{}", pc.ui());
}

/// XB-20.5: a key on the radio stops a repeater the link started, lit or dark, locked or not, and the press does
/// nothing else; PTT talks through it (XB-14) and SIDE1 listens (XB-15) without stopping it.
#[cfg(feature = "line-cmds")]
#[test]
fn a_radio_key_stops_a_remote_repeater() {
    let mut pc = Pc::tx();
    for (locked, dark) in [(false, false), (true, false), (false, true), (true, true)] {
        pc.app.go_home();
        pc.cmd("xb start").unwrap();
        assert!(pc.xb_running());
        pc.app.take_xb_log();
        if locked {
            pc.app.lock = Some(crate::settings::Lock::Keypad);
        }
        if dark {
            pc.app.set.backlight = 1;
            pc.app.backlight_out();
            pc.pass(100, &LiveAll::default());
            assert!(pc.app.dark());
        }
        pc.key(Key::Side(1));
        assert!(pc.xb_running(), "SIDE1 doesn't stop it (locked {locked}, dark {dark})");
        pc.cmds();
        pc.key(Key::Ptt(PTT_A));
        assert!(pc.xb_running(), "PTT talks through it (XB-14)");
        pc.cmds();
        pc.key(Key::Menu);
        assert!(!pc.xb_running(), "MENU stops it (locked {locked}, dark {dark})");
        assert_eq!(pc.app.take_xb_log(), Some(XbLog { on: false, by: BY_KEYS, why: XB_KEY }));
        assert!(matches!(pc.app.screen(), Screen::Crossband(_)), "the press did nothing else: {:?}", pc.app.screen());
        assert!(!pc.app.dark(), "and woke the screen");
        pc.app.lock = None;
        pc.app.set.backlight = 0;
    }
}

/// XB-20.3, LINK-30: on Bluetooth with a PIN lock, `xb start` and remote keys only after AUTH; `xb start` counts as a
/// write and `xb` alone as a read (TERM-25).
#[cfg(feature = "line-cmds")]
#[test]
fn xb_over_bluetooth_needs_the_pin() {
    let mut bt = Pc::new(false);
    bt.app.radio.tx_built = true;
    bt.app.set_region(Some(ferrite_core::txplan::Region::Us), false);
    (bt.app.set.pin, bt.app.set.pin_len, bt.app.set.pin_mode) = (1234, 4, 1);
    assert_eq!(bt.ask(msg::CMD, b"xb start").0, Status::NotAuthorised as u8);
    assert_eq!(bt.ask(msg::KEY, &[key_code(b"#").unwrap(), key::SHORT]).0, Status::NotAuthorised as u8);
    assert!(bt.app.radio.mode != Mode::Xb);
    assert_eq!(bt.ask(msg::AUTH, b"1234").0, 0);
    bt.app.go_home();
    assert!(bt.cmd("xb start").is_ok());
    assert_eq!(bt.app.take_xb_log(), Some(XbLog { on: true, by: BY_CMD | crate::remote::BY_BT, why: 0 }));
    assert!(bt.ui().contains(" by=cmd:bt "));
    assert!(crate::term::cmd::writes(b"xb start") && crate::term::cmd::writes(b"xb stop") && !crate::term::cmd::writes(b"xb"));
}

// ---- The compact status (LINK-19, UI what=2) ----

/// The full readback's and the compact status's lines for `live` (as the terminal's `ui` and `ui status`).
fn full_and_status(pc: &Pc, live: &LiveAll) -> (std::string::String, std::string::String) {
    use crate::term::cmd::{list_line, Line, List};
    let mut l = Line::new();
    let (mut full, mut i) = (std::string::String::new(), 0);
    while list_line(&pc.app, &pc.st, &pc.dev, live, List::Ui, i, &mut l) {
        full.push_str(text(l.as_bytes()));
        full.push('\n');
        i += 1;
    }
    assert!(list_line(&pc.app, &pc.st, &pc.dev, live, List::UiStatus, 0, &mut l));
    let st = std::string::String::from(text(l.as_bytes()));
    assert!(!list_line(&pc.app, &pc.st, &pc.dev, live, List::UiStatus, 1, &mut l), "one line");
    (full, st)
}

/// Each slot's state word and meter in the status line are its home card's `state="..."` and `meter=` (`-` for
/// idle, spaces as `_`); the line stays short (here at most 90 bytes; 140 at the very most: one answer).
fn status_matches_cards(full: &str, st: &str) {
    assert!(st.len() <= 90, "{} bytes: {st}", st.len());
    let kv: std::collections::HashMap<&str, &str> = st.split(' ').skip(1).filter_map(|w| w.split_once('=')).collect();
    for card in full.lines().filter(|l| l.starts_with("card ")) {
        let s = &card[5..6];
        let state = card.split("state=\"").nth(1).unwrap().split('"').next().unwrap();
        let meter = card.split(" meter=").nth(1).unwrap().split(' ').next().unwrap();
        let want = std::format!("{}/{}", if state == "idle" { "-".into() } else { state.replace(' ', "_") }, meter);
        assert_eq!(kv[s], want, "slot {s}: {card}\n{st}");
    }
    let state = full.lines().find(|l| l.starts_with("state ")).unwrap();
    for k in ["keyed", "remote", "mode"] {
        assert!(state.contains(&std::format!(" {k}={}", kv[k])), "{k}: {state}\n{st}");
    }
    assert!(state.contains(&std::format!(" active={}", kv["act"])), "{state}\n{st}");
    assert!(full.starts_with(&std::format!("screen {} ", kv["scr"])), "{st}");
}

/// LINK-19 (what=2), TERM-37: the compact status is one short line in one answer (no MORE), the terminal's
/// `ui status` the same; its values are the full readback's: the screen, the active slot, keyed, remote PTT, the
/// mode, each slot's card state word and meter (RX, heard, MON, off, idle as `-`), and the repeater's state and
/// passes while it runs.
#[test]
fn ui_status_line() {
    let mut pc = Pc::tx();
    let (s, a) = pc.ask(msg::UI, &[2]);
    assert_eq!((s, a[0]), (0, 0), "one answer, nothing more");
    let line = text(&a[1..]);
    assert_eq!(line, "st scr=Home act=A keyed=- remote=0 mode=off A=-/20 B=-/20 C=off/20 xb=0\n", "LiveAll::default(): rssi 0 dBm, a full meter");
    if cfg!(feature = "line-cmds") {
        assert_eq!(pc.cmd("ui status").unwrap(), line, "the terminal's ui status");
    }
    let (full, st) = full_and_status(&pc, &LiveAll::default());
    status_matches_cards(&full, &st);
    // Receiving on A, a tone heard on B, C monitored.
    let mut live = LiveAll::default();
    live.slots[0].squelch_open = true;
    live.slots[0].rssi = -60;
    live.slots[1].heard_tone = 885;
    live.slots[1].rssi = -100;
    live.slots[2].rssi = -130;
    pc.app.monitor = 1 << 2;
    let (full, st) = full_and_status(&pc, &live);
    assert!(st.contains(" A=RX/17 ") && st.contains(" B=heard/") && st.contains(" C=MON/0 "), "{st}");
    status_matches_cards(&full, &st);
    pc.app.monitor = 0;
    // The crossband repeater running: its state and passes.
    if !cfg!(feature = "line-cmds") {
        return; // the rest starts the repeater with a line command (decision 347)
    }
    assert!(pc.cmd("xb start").is_ok());
    live.mode = ferrite_core::msg::ModeLive::Xb(ferrite_core::msg::XbLive { state: 1, passes: 3, ..Default::default() });
    let (full, st) = full_and_status(&pc, &live);
    assert!(st.starts_with("st scr=Crossband ") && st.contains(" mode=xb ") && st.ends_with(" xb=1,1,3"), "{st}");
    status_matches_cards(&full, &st);
    assert!(full.contains("xb running=1 state=1 ") && full.contains(" passes=3 "), "{full}");
    let (s, _) = pc.ask(msg::UI, &[5]);
    assert_eq!(s, Status::BadArg as u8);
}

/// LINK-19.6: `UI` what=4 is the radio's own report for Console's debug info: the flash chip and the store's
/// mount (a user's radio whose writes didn't stay, Nov 19, 2026), calibration, the chips, what is stored, and no
/// callsign.
#[test]
fn ui_radio_report() {
    let mut pc = Pc::new(true);
    pc.app.set.set_callsign(b"VE7ABC");
    let mut dev = Device::default();
    dev.selftest.flash_id = 0xEF_4016;
    dev.selftest.flash_mb = 4;
    dev.selftest.store = 3;
    dev.selftest.chips = 3;
    let mut all = std::string::String::new();
    let mut i = 0;
    let mut l = crate::term::cmd::Line::new();
    while crate::uistate::radio_line(&pc.app, &pc.st, &dev, i, &mut l) {
        all.push_str(core::str::from_utf8(l.as_bytes()).unwrap());
        all.push('\n');
        i += 1;
    }
    assert!(all.starts_with("firmware version="), "{all}");
    assert!(all.contains("flash id=0xef4016 mb=4 store=FAILED layout="), "{all}");
    assert!(all.contains("cal state=unknown") && all.contains("chips bk4829=0x3 ") && all.contains("data channels="), "{all}");
    assert!(all.contains("restart after=start in_a_row=0"), "{all}");
    assert!(!all.contains("VE7ABC"), "no callsign: {all}");
    let (s, _) = pc.ask(msg::UI, &[4]);
    assert_eq!(s, Status::Ok as u8);
}

/// LINK-19, APRS-17.4: the APRS card's readback gives what the card shows (the last heard line, the beacon line, the
/// stations heard), not the mode, tone and shift of C's own VFO put aside (it read mode="AM", Oct 15, 2026).
#[test]
fn ui_aprs_card_reads_its_text() {
    let mut pc = Pc::tx();
    pc.app.set_aprs_mode(true);
    let (full, _) = full_and_status(&pc, &LiveAll::default());
    let c = full.lines().find(|l| l.starts_with("card C ")).unwrap();
    assert!(c.contains(" kind=aprs") && c.contains(" last=\"") && c.contains(" beacon=\"") && c.contains(" heard="), "{c}");
    assert!(!c.contains(" mode=") && !c.contains(" tone="), "{c}");
}

/// LINK-19: a data key-up (APRS, KISS) shows as keyed in the readback, from the Radio task's live state: the UI's
/// own `keyed` is voice only, so a test couldn't see a KISS frame's carrier (Oct 15, 2026).
#[test]
fn ui_status_shows_data_key_ups() {
    let pc = Pc::tx();
    let mut live = LiveAll::default();
    live.tx.keyed = true;
    live.tx.slot = 2;
    let (full, st) = full_and_status(&pc, &live);
    assert!(st.contains(" keyed=C remote=0 "), "{st}");
    assert!(full.contains(" keyed=C "), "{full}");
}

/// LINK-37 (decision 175): while the radio transmits, remote keys and knob steps are refused `invalid` (the screen
/// stays the Transmit screen, nothing tuned); remote PTT's own keepalive and KEY off go on; after the unkey keys work.
#[test]
fn remote_keys_wait_for_the_unkey() {
    let mut pc = Pc::tx();
    assert_eq!(pc.ptt(Ptt::On(key::ACTIVE)).0, 0);
    pc.cmds();
    let (s, why) = pc.press("MENU", key::SHORT);
    assert_eq!((s, why.as_str()), (Status::Invalid as u8, "transmitting: remote keys wait for the unkey"));
    let (s, a) = pc.ask(msg::KEY, &[key::KNOB, 1]);
    assert_eq!((s, text(&a)), (Status::Invalid as u8, "transmitting: remote keys wait for the unkey"));
    assert!(matches!(pc.app.screen(), Screen::Transmit(_)));
    assert!(!pc.cmds().iter().any(|c| matches!(c, RadioCmd::TuneSlot { .. })), "nothing tuned");
    assert_eq!(pc.ptt(Ptt::Alive).0, Status::Ok as u8, "the keepalive goes on");
    assert_eq!(pc.ptt(Ptt::Off).0, Status::Ok as u8);
    assert_eq!(pc.press("BACK", key::SHORT).0, Status::Ok as u8, "after the unkey keys work");
}

/// LINK-5 (MENU 0 6 3): a settings record written over the link keeps its coordinate format; it isn't one of the
/// protected settings.
#[test]
fn settings_write_keeps_coord_format() {
    let mut pc = Pc::new(true);
    let rec = pc.read(what::SETTINGS, 0).unwrap();
    let mut s = Settings::DEFAULT;
    s.load(&mut Rd { b: &rec, n: 0 });
    for f in 0..crate::settings::COORD_FORMATS.len() as u8 {
        s.coord_fmt = f;
        let mut out = std::vec![0u8; RECORD_MAX];
        let mut w = Window { b: &mut out, from: 0, n: 0 };
        s.save(&mut w);
        let n = w.n;
        assert_eq!(pc.ask(msg::BEGIN, &1u16.to_le_bytes()).0, 0);
        assert_eq!(pc.write(what::SETTINGS, 0, &out[..n], 100).0, 0);
        assert_eq!(pc.ask(msg::END, &[]).0, 0);
        assert_eq!(pc.app.set.coord_fmt, f, "written");
        let back = pc.read(what::SETTINGS, 0).unwrap();
        let mut b = Settings::DEFAULT;
        b.load(&mut Rd { b: &back, n: 0 });
        assert_eq!(b.coord_fmt, f, "reads back");
    }
}

/// VOX-10, LINK-34: CW ID turned on with no callsign, over the link's `CMD` (the terminal's `set`), says what the
/// radio's knob says ("set a callsign first"), not a bare "one of Off"; Off still sets, and with a callsign it
/// turns on.
#[cfg(feature = "line-cmds")]
#[test]
fn cw_id_without_callsign_says_why() {
    let mut pc = Pc::new(true);
    assert!(pc.app.set.callsign.as_bytes().is_empty());
    let (s, a) = pc.ask(msg::CMD, b"set 0 8 6 2 On unkey");
    assert_eq!(s, Status::Invalid as u8);
    assert_eq!(std::str::from_utf8(&a).unwrap(), "CW ID: set a callsign first");
    assert_eq!(pc.ask(msg::CMD, b"set 0 8 6 2 Off").0, 0, "Off is what it is");
    pc.app.set.set_callsign(b"N0CALL");
    assert_eq!(pc.ask(msg::CMD, b"set 0 8 6 2 On unkey").0, 0);
    assert_eq!(pc.app.set.cw_id, 2);
}

impl Pc {
    /// `READ_MANY` over every index of `w`: (index, record) for each one held, and the requests it took.
    fn read_all(&mut self, w: u8, count: u16) -> (Vec<(u16, Vec<u8>)>, usize) {
        let (mut got, mut asks, mut first) = (Vec::new(), 0, 0u16);
        loop {
            let mut p = std::vec![w];
            p.extend_from_slice(&first.to_le_bytes());
            p.extend_from_slice(&count.saturating_sub(first).to_le_bytes());
            let (s, a) = self.ask(msg::READ_MANY, &p);
            asks += 1;
            assert_eq!(s, 0);
            let next = u16::from_le_bytes([a[0], a[1]]);
            let mut r = &a[2..];
            while !r.is_empty() {
                let (i, l) = (u16::from_le_bytes([r[0], r[1]]), r[2] as usize);
                got.push((i, r[3..3 + l].to_vec()));
                r = &r[3 + l..];
            }
            assert!(next > first || next >= count, "it moves on");
            if next >= count {
                return (got, asks);
            }
            first = next;
        }
    }

    /// `WRITE_MANY`: (index, record) pairs in one frame; (status, the rest).
    fn write_many(&mut self, w: u8, recs: &[(u16, Vec<u8>)]) -> (u8, Vec<u8>) {
        let mut p = std::vec![w];
        for (i, r) in recs {
            p.extend_from_slice(&i.to_le_bytes());
            p.push(r.len() as u8);
            p.extend_from_slice(r);
        }
        self.ask_long(msg::WRITE_MANY, &p)
    }

    /// As [`Pc::ask`], polling as long as the radio takes (a `WRITE_MANY` applies a record a pass).
    fn ask_long(&mut self, ty: u8, payload: &[u8]) -> (u8, Vec<u8>) {
        self.seq = self.seq.wrapping_add(1);
        let mut w = [0u8; MAX_WIRE];
        let n = wire::build(ty, self.seq, payload, &mut w).unwrap();
        self.raw(&w[..n]);
        let mut out = Wire(Vec::new());
        for _ in 0..64 {
            self.now += 10;
            self.link.poll(&mut self.app, &self.st, &self.dev, &LiveAll::default(), self.now, &mut out);
            while let Some(wr) = self.app.pop_write() {
                self.st.apply(wr);
            }
            if !out.0.is_empty() {
                break;
            }
        }
        let k = out.0.len() - 1;
        let f = wire::parse(&mut out.0[..k]).unwrap();
        assert_eq!((f.ty, f.seq), (ty | wire::ANSWER, self.seq));
        (f.payload[0], f.payload[1..].to_vec())
    }
}

/// A channel record for number `n` (0-based) at `mhz10` (10 Hz units), named `name`.
pub(crate) fn chan_rec(n: u16, rx: u32, name: &[u8]) -> Vec<u8> {
    let mut c = Channel { num: n, zone: NO_ZONE, rx, name: [0; 16], cfg: SlotCfg::default(), flags: 0, tones: 0 };
    c.set_name(name);
    let mut out = std::vec![0u8; 64];
    let mut w = Window { b: &mut out, from: 0, n: 0 };
    channel_save(&c, &mut w);
    let n = w.n;
    out.truncate(n);
    out
}

/// LINK-39: `USED` gives a bit for every channel number, set where a channel is, in parts of 1,880 from a first index
/// (4,000 channels: three asks); zones and presets all set.
#[test]
fn used_maps_channels() {
    let mut pc = Pc::new(true);
    let mut bits = std::vec::Vec::new();
    let mut first = 0u16;
    let mut asks = 0;
    while first < MAX_CHANNELS {
        let mut p = std::vec![what::CHANNEL];
        p.extend_from_slice(&first.to_le_bytes());
        let (s, a) = pc.ask(msg::USED, &p);
        assert_eq!(s, 0);
        assert_eq!((u16::from_le_bytes([a[0], a[1]]), u16::from_le_bytes([a[2], a[3]])), (MAX_CHANNELS, first));
        let k = (MAX_CHANNELS - first).min(crate::link::USED_MAX);
        assert_eq!(a.len(), 4 + k.div_ceil(8) as usize);
        bits.extend((0..k).map(|i| a[4 + i as usize / 8] >> (i % 8) & 1 != 0));
        first += k;
        asks += 1;
    }
    assert_eq!(asks, 3);
    let mut n = 0;
    for i in 0..MAX_CHANNELS {
        assert_eq!(bits[i as usize], pc.st.get(i).is_some(), "channel {i}");
        n += bits[i as usize] as u16;
    }
    assert_eq!(n, pc.st.count());
    assert_eq!(pc.ask(msg::USED, &[what::CHANNEL]).1[2..4], [0, 0], "no first index: 0");
    let (s, a) = pc.ask(msg::USED, &[what::ZONE]);
    assert_eq!((s, u16::from_le_bytes([a[0], a[1]])), (0, pc.st.zones()));
    assert!((0..pc.st.zones()).all(|i| a[4 + i as usize / 8] >> (i % 8) & 1 != 0));
    assert_eq!(pc.ask(msg::USED, &[what::CHANNEL, 0xA1, 0x0F]).0, Status::BadArg as u8, "past the last");
    assert_eq!(pc.ask(msg::USED, &[what::SETTINGS]).0, Status::BadArg as u8);
    assert_eq!(pc.ask(msg::USED, &[]).0, Status::BadArg as u8);
}

/// LINK-40: `READ_MANY` gives every channel, zone and preset one at a time does, byte for byte, whole records several to a
/// frame, empty numbers skipped; the next index goes on where it stopped.
#[test]
fn read_many_matches_read() {
    let mut pc = Pc::new(true);
    for (w, count) in [(what::CHANNEL, MAX_CHANNELS), (what::ZONE, pc.st.zones()), (what::PRESET, pc.st.data().presets() as u16)] {
        let (got, asks) = pc.read_all(w, count);
        let want: Vec<(u16, Vec<u8>)> = (0..count).filter_map(|i| pc.read(w, i).ok().map(|r| (i, r))).collect();
        assert_eq!(got, want, "what {w}");
        if w == what::CHANNEL {
            assert!(asks <= want.len().div_ceil(5), "{asks} requests for {} channels", want.len());
        }
    }
    // A range: only what's in it; the end past the last channel is cut.
    let mut p = std::vec![what::CHANNEL];
    p.extend_from_slice(&(MAX_CHANNELS - 10).to_le_bytes());
    p.extend_from_slice(&100u16.to_le_bytes());
    let (s, a) = pc.ask(msg::READ_MANY, &p);
    assert_eq!((s, u16::from_le_bytes([a[0], a[1]])), (0, MAX_CHANNELS));
    assert_eq!(pc.ask(msg::READ_MANY, &[what::SETTINGS, 0, 0, 1, 0]).0, Status::BadArg as u8);
}

/// LINK-41, LINK-22, LINK-9: `WRITE_MANY` applies several channels from one frame, each with a `WRITE`'s checks,
/// and counts each on the progress screen; a refusal stops it there, saying how many went before it and why;
/// a malformed frame changes nothing.
#[test]
fn write_many_channels() {
    let mut pc = Pc::new(true);
    let base = pc.st.free_num(500).unwrap();
    let recs: Vec<(u16, Vec<u8>)> = (0..5).map(|k| (base + k, chan_rec(base + k, 14_652_000 + k as u32 * 2500, b"MANY"))).collect();
    assert_eq!(pc.ask(msg::BEGIN, &5u16.to_le_bytes()).0, 0);
    let (s, a) = pc.write_many(what::CHANNEL, &recs);
    assert_eq!((s, a.as_slice()), (0, &[5u8][..]));
    assert_eq!(pc.link.progress(), Some(100), "each counted (LINK-9)");
    assert_eq!(pc.ask(msg::END, &[]).0, 0);
    for (i, r) in &recs {
        assert_eq!(pc.read(what::CHANNEL, *i).unwrap(), *r, "channel {i} written");
    }
    // The third refused (no such zone): two applied, the reason, the rest not.
    let mut bad = recs.clone();
    for (k, (_, r)) in bad.iter_mut().enumerate() {
        r[11] = b'0' + k as u8; // the names change: MANY0, MANY1, ...
    }
    bad[2].1[2] = 200; // zone 200
    let (s, a) = pc.write_many(what::CHANNEL, &bad);
    assert_eq!(s, Status::Invalid as u8);
    assert_eq!(a[0], 2, "two went before it");
    assert_eq!(std::str::from_utf8(&a[1..]).unwrap(), "zone: no such zone");
    assert_eq!(pc.read(what::CHANNEL, bad[1].0).unwrap(), bad[1].1);
    assert_eq!(pc.read(what::CHANNEL, bad[3].0).unwrap(), recs[3].1, "not reached");
    // Malformed (a length past the end): nothing at all.
    let mut p = std::vec![what::CHANNEL];
    p.extend_from_slice(&bad[0].0.to_le_bytes());
    p.push(60);
    p.extend_from_slice(&bad[0].1);
    assert_eq!(pc.ask(msg::WRITE_MANY, &p).0, Status::BadArg as u8);
    assert_eq!(pc.ask(msg::WRITE_MANY, &[what::SETTINGS, 0, 0, 1, 0]).0, Status::BadArg as u8);
    assert_eq!(pc.ask(msg::WRITE_MANY, &[what::CHANNEL]).0, Status::BadArg as u8);
}

/// LINK-41.1: a store that takes queued writes gets a `WRITE_MANY` frame's records in one pass, straight into its
/// queue (the App's own write slot stays free), and the answer goes at once.
#[test]
fn write_many_holds_the_port() {
    {
        // LINK-41.1: the sample store queues (applies) them at once: the answer comes from the first pass
        let mut pc = Pc::new(true);
        let base = pc.st.free_num(600).unwrap();
        let mut p = std::vec![what::CHANNEL];
        for k in 0..3u16 {
            let r = chan_rec(base + k, 43_500_000, b"FAST");
            p.extend_from_slice(&(base + k).to_le_bytes());
            p.push(r.len() as u8);
            p.extend_from_slice(&r);
        }
        let mut w = [0u8; MAX_WIRE];
        let n = wire::build(msg::WRITE_MANY, 8, &p, &mut w).unwrap();
        pc.raw(&w[..n]);
        let mut out = Wire(Vec::new());
        pc.link.poll(&mut pc.app, &pc.st, &pc.dev, &LiveAll::default(), pc.now, &mut out);
        let k = out.0.len() - 1;
        let f = wire::parse(&mut out.0[..k]).unwrap();
        assert_eq!((f.ty, f.payload), (msg::WRITE_MANY | wire::ANSWER, &[0u8, 3][..]), "the whole frame in one pass");
        assert!(pc.app.pop_write().is_none() && pc.st.get(base + 2).is_some(), "straight to the store");
        assert!(pc.link.ready() && !pc.link.writing());
    }
}

/// LINK-42, INBOX-6, INBOX-11: `LOG_READ` gives the event log newest first, in parts, from a sequence number, with
/// each event's age, source, frequency, flags, position, name and text as the log screen has them.
#[test]
fn log_read_newest_first() {
    use ferrite_core::link::{LOG_FIXED, LOG_POS, LOG_UNREAD};
    let mut pc = Pc::new(true);
    let kept = pc.st.data().events();
    assert!(kept >= 3, "the sample has events");
    let (mut before, mut seen, mut asks) = (u32::MAX, Vec::new(), 0);
    loop {
        let mut p = before.to_le_bytes().to_vec();
        p.push(2); // at most 2 a request, so it takes parts
        let (s, a) = pc.ask(msg::LOG_READ, &p);
        assert_eq!(s, 0);
        asks += 1;
        let top = u32::from_le_bytes([a[0], a[1], a[2], a[3]]);
        assert_eq!((top, u16::from_le_bytes([a[4], a[5]])), (kept as u32, kept));
        let n = a[6];
        assert!(n <= 2);
        let mut r = &a[7..];
        for _ in 0..n {
            let seq = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
            let ago = u32::from_le_bytes([r[4], r[5], r[6], r[7]]);
            let (src, slot, flags) = (r[8], r[9], r[10]);
            let freq = u32::from_le_bytes([r[11], r[12], r[13], r[14]]);
            let id = u32::from_le_bytes([r[15], r[16], r[17], r[18]]);
            let mut k = LOG_FIXED;
            let pos = (flags & LOG_POS != 0).then(|| {
                k += 8;
                crate::data::Pos { lat: i32::from_le_bytes([r[23], r[24], r[25], r[26]]), lon: i32::from_le_bytes([r[27], r[28], r[29], r[30]]) }
            });
            let name = r[k + 1..k + 1 + r[k] as usize].to_vec();
            k += 1 + r[k] as usize;
            let text = r[k + 1..k + 1 + r[k] as usize].to_vec();
            k += 1 + r[k] as usize;
            let i = (kept as u32 - 1 - seq) as u16;
            let e = pc.st.data().event(i).unwrap();
            assert_eq!((ago, src, slot, freq, id, pos), (e.ago_s, e.src as u8, e.slot, e.freq, e.id, e.pos), "event {i}");
            assert_eq!(flags & LOG_UNREAD != 0, e.unread);
            assert_eq!((name.as_slice(), text.as_slice()), (e.name.as_bytes(), e.text.as_bytes()));
            seen.push(seq);
            before = seq;
            r = &r[k..];
        }
        assert!(r.is_empty());
        if n == 0 {
            break;
        }
    }
    let want: Vec<u32> = (0..kept as u32).rev().collect();
    assert_eq!(seen, want, "newest first, each once");
    assert_eq!(asks, (kept as usize).div_ceil(2) + 1);
    // As many as fit: one request has them all when they are short, and none from below the oldest.
    let (_, a) = pc.ask(msg::LOG_READ, &u32::MAX.to_le_bytes());
    assert!(a[6] >= 2);
    assert_eq!(pc.ask(msg::LOG_READ, &0u32.to_le_bytes()).1[6], 0);
    assert_eq!(pc.ask(msg::LOG_READ, &[1, 2]).0, Status::BadArg as u8);
}

/// APRS-42: the contacts read in callsign order as record 6, and are written by callsign: a new one added, a
/// saved one renamed, flag bit 0 deletes; a bad callsign is refused with its reason.
#[test]
fn contacts_over_the_link() {
    let mut pc = Pc::new(true);
    let n = pc.st.data().contacts();
    let (got, _) = pc.read_all(what::CONTACT, n);
    assert_eq!(got.len(), n as usize);
    assert_eq!(&got[0].1[..6], b"VA3RTX");
    assert_eq!(got[0].1.len(), 23);
    let rec = |call: &[u8], name: &[u8], flags: u8| {
        let mut r = std::vec![0u8; 23];
        r[..call.len()].copy_from_slice(call);
        r[10..10 + name.len()].copy_from_slice(name);
        r[22] = flags;
        r
    };
    let (s, _) = pc.write_many(what::CONTACT, &[(0xFFFF, rec(b"VE3NEW", b"New one", 0)), (0xFFFF, rec(b"VA3RTX", b"Ronald", 0))]);
    assert_eq!(s, 0);
    assert_eq!(pc.st.data().contact_of(b"VE3NEW").unwrap().name.as_bytes(), b"New one");
    assert_eq!(pc.st.data().contact_of(b"VA3RTX").unwrap().name.as_bytes(), b"Ronald");
    assert_eq!(pc.st.data().contacts(), n + 1);
    assert_eq!(pc.write_many(what::CONTACT, &[(0, rec(b"VE3NEW", b"", 1))]).0, 0);
    assert!(pc.st.data().contact_of(b"VE3NEW").is_none());
    let (s, a) = pc.write_many(what::CONTACT, &[(0, rec(b"ve3x", b"", 0))]);
    assert_eq!(s, Status::Invalid as u8);
    assert!(std::str::from_utf8(&a[1..]).unwrap().starts_with("callsign"));
}

/// BACKUP-7, LINK-8: STOCK_RESTORE's START on the cable is taken by the caller (no answer from the session); never
/// on Bluetooth, while transmitting, or as any other op.
#[test]
fn stock_restore_start_on_the_cable_only() {
    use ferrite_core::restore::{ERASE, START};
    let mut pc = Pc::new(true);
    pc.app.radio.keyed = Some(0);
    assert_eq!(pc.ask(msg::STOCK_RESTORE, &[START]).0, Status::Busy as u8, "not while transmitting");
    pc.app.radio.keyed = None;
    assert_eq!(pc.ask(msg::STOCK_RESTORE, &[ERASE, 0, 0, 0, 0]).0, Status::BadArg as u8, "START first");
    assert_eq!(pc.link.take_restore(), None);
    pc.seq = 41;
    let mut w = [0u8; MAX_WIRE];
    let n = wire::build(msg::STOCK_RESTORE, 42, &[START], &mut w).unwrap();
    pc.raw(&w[..n]);
    let mut out = Wire(Vec::new());
    pc.link.poll(&mut pc.app, &pc.st, &pc.dev, &LiveAll::default(), pc.now + 10, &mut out);
    assert!(out.0.is_empty(), "the caller answers");
    assert_eq!(pc.link.take_restore(), Some(42));
    assert_eq!(pc.link.take_restore(), None, "once");
    assert!(pc.link.ready(), "the port takes the next frame");
    let mut bt = Pc::new(false);
    assert_eq!(bt.ask(msg::STOCK_RESTORE, &[START]).0, Status::Refused as u8, "never over Bluetooth");
    assert_eq!(bt.link.take_restore(), None);
}

/// LINK-59, LINK-60: raw data: the shapes and ranges are checked before the store is asked; writes from the cable
/// only; a store without a flash (the RAM sample) has nothing to read or write.
#[test]
fn raw_data_shapes_ranges_and_ports() {
    use ferrite_core::link::data::{ERASE, PROGRAM, READ, STAMP};
    let mut pc = Pc::new(true);
    assert_eq!(pc.ask(msg::DATA_READ, &[READ, 0, 0, 0, 0, 16]).0, Status::NotFound as u8, "no flash here");
    assert_eq!(pc.ask(msg::DATA_READ, &[READ, 0, 0xF0, 0, 0, 16]).0, Status::Refused as u8, "calibration");
    assert_eq!(pc.ask(msg::DATA_READ, &[READ, 0, 0, 0, 0, 0]).0, Status::BadArg as u8, "nothing to read");
    assert_eq!(pc.ask(msg::DATA_READ, &[9]).0, Status::BadArg as u8);
    assert_eq!(pc.ask(msg::DATA_WRITE, &[ERASE, 0x10, 0, 0, 0]).0, Status::BadArg as u8, "not a sector");
    assert_eq!(pc.ask(msg::DATA_WRITE, &[ERASE, 0, 0, 0x30, 0]).0, Status::Refused as u8, "the update area");
    assert_eq!(pc.ask(msg::DATA_WRITE, &[PROGRAM, 0, 0, 1, 0]).0, Status::BadArg as u8, "no bytes");
    assert_eq!(pc.ask(msg::DATA_WRITE, &[ERASE, 0, 0, 1, 0]).0, Status::Busy as u8, "no store to queue it");
    assert_eq!(pc.ask(msg::DATA_WRITE, &[STAMP, 4, 0]).0, Status::Invalid as u8);
    let mut bt = Pc::new(false);
    assert_eq!(bt.ask(msg::DATA_WRITE, &[ERASE, 0, 0, 1, 0]).0, Status::Refused as u8, "never over Bluetooth");
    assert_eq!(bt.ask(msg::DATA_READ, &[READ, 0, 0, 0, 0, 16]).0, Status::NotFound as u8, "reads go on over Bluetooth");
}

/// UPD-16.2: `DATA_WRITE` `PART` names the part the installer writes now, for the Updating page; nothing is written.
#[test]
fn raw_data_part_mark() {
    use ferrite_core::link::data::PART;
    let mut pc = Pc::new(true);
    assert_eq!(pc.ask(msg::DATA_WRITE, &[PART, 2, 4]).0, Status::Ok as u8);
    assert_eq!(pc.app.update_part, (2, 4));
    for bad in [&[PART, 0, 4][..], &[PART, 5, 4], &[PART, 1]] {
        assert_eq!(pc.ask(msg::DATA_WRITE, bad).0, Status::BadArg as u8, "{bad:?}");
    }
    assert_eq!(pc.app.update_part, (2, 4));
    use ferrite_core::link::data::{CRC, ERASE};
    pc.ask(msg::DATA_READ, &[CRC, 0, 0, 9, 0, 16, 0, 0, 0]);
    assert!(pc.app.update_checking, "a read-back: checking");
    pc.ask(msg::DATA_WRITE, &[ERASE, 0, 0, 9, 0]);
    assert!(!pc.app.update_checking, "a write: writing");
    let mut bt = Pc::new(false);
    assert_eq!(bt.ask(msg::DATA_WRITE, &[PART, 1, 1]).0, Status::Refused as u8, "the cable only, as every raw write");
}

/// CHAN-12.1: a zone deleted over the link leaves no slot in it, as the radio's own delete does, so a later edit
/// of the slot's channel doesn't write the old zone back.
#[test]
fn zone_deleted_over_the_link_leaves_no_slot_in_it() {
    let mut pc = Pc::new(true);
    assert!(pc.st.zones() > 0);
    pc.app.slots[0].zone = 0;
    if let Some(c) = &mut pc.app.slots[0].ch {
        c.zone = 0;
    }
    assert_eq!(pc.ask(msg::DELETE, &[what::ZONE, 0, 0]).0, 0);
    assert_eq!(pc.app.slots[0].zone, crate::chan::NO_ZONE);
    assert!(pc.app.slots[0].ch.is_none_or(|c| c.zone == crate::chan::NO_ZONE));
}

/// LINK-50: `STREAM` answers an inner request's answers back to back (`STREAM_MORE` until the last, `STREAM`), the
/// same answers that inner request asked again from where each left off would give; a new request ends it; another
/// inner type is `bad argument`.
#[test]
fn stream_answers_back_to_back() {
    let mut pc = Pc::new(true);
    for k in 0..40u16 {
        let num = pc.st.free_num(100 + k).unwrap();
        let r = chan_rec(num, 43_500_000 + k as u32 * 2_500, b"STREAM");
        let mut p = std::vec![what::CHANNEL];
        p.extend_from_slice(&num.to_le_bytes());
        p.push(r.len() as u8);
        p.extend_from_slice(&r);
        assert_eq!(pc.ask(msg::WRITE_MANY, &p).0, 0);
    }
    // the reference: READ_MANY asked again and again
    let (mut refs, mut first) = (Vec::new(), 0u16);
    loop {
        let mut p = std::vec![what::CHANNEL];
        p.extend_from_slice(&first.to_le_bytes());
        p.extend_from_slice(&1000u16.to_le_bytes());
        let (s, a) = pc.ask(msg::READ_MANY, &p);
        assert_eq!(s, 0);
        let next = u16::from_le_bytes([a[0], a[1]]);
        refs.push(a);
        if next <= first || next >= 1000 {
            break;
        }
        first = next;
    }
    // one STREAM of it
    let mut p = std::vec![msg::READ_MANY, what::CHANNEL];
    p.extend_from_slice(&0u16.to_le_bytes());
    p.extend_from_slice(&1000u16.to_le_bytes());
    let mut w = [0u8; MAX_WIRE];
    let n = wire::build(msg::STREAM, 77, &p, &mut w).unwrap();
    pc.raw(&w[..n]);
    let mut out = Wire(Vec::new());
    for _ in 0..64 {
        pc.now += 10;
        pc.link.poll(&mut pc.app, &pc.st, &pc.dev, &LiveAll::default(), pc.now, &mut out);
    }
    let mut got = Vec::new();
    for (k, f) in out.0.split_mut(|&b| b == 0).filter(|f| !f.is_empty()).enumerate() {
        let f = wire::parse(f).unwrap();
        assert_eq!(f.seq, 77u8.wrapping_add(k as u8), "numbered from the request's seq");
        got.push((f.ty, f.payload[0], f.payload[1..].to_vec()));
    }
    assert!(got.len() > 1, "more than one frame");
    let (last, more) = got.split_last().unwrap();
    assert!(more.iter().all(|g| g.0 == msg::STREAM_MORE | wire::ANSWER) && last.0 == msg::STREAM | wire::ANSWER);
    assert!(got.iter().all(|g| g.1 == 0));
    let streamed: Vec<u8> = got.iter().flat_map(|g| g.2[2..].to_vec()).collect();
    let asked: Vec<u8> = refs.iter().flat_map(|a| a[2..].to_vec()).collect();
    assert_eq!(streamed, asked, "the same records");
    // a UI readback streams too, and a new request ends a stream
    let (s, ui) = pc.ask(msg::UI, &[]);
    assert_eq!(s, 0);
    let n = wire::build(msg::STREAM, 78, &[msg::UI], &mut w).unwrap();
    pc.raw(&w[..n]);
    let mut out = Wire(Vec::new());
    pc.link.poll(&mut pc.app, &pc.st, &pc.dev, &LiveAll::default(), pc.now, &mut out);
    let k = out.0.iter().position(|&b| b == 0).unwrap();
    let f = wire::parse(&mut out.0[..k]).unwrap();
    assert_eq!(f.payload[2..], ui[1..], "the first page is UI's");
    assert_eq!(pc.ask(msg::INFO, &[]).0, 0, "a new request is answered: the stream is over");
    assert_eq!(pc.ask(msg::STREAM, &[msg::WRITE_MANY]).0, Status::BadArg as u8);
}

/// LINK-19.5: `UI` what=3 gives the screen as drawn (`t` lines: its texts, with place, font and colours, its coloured
/// fills and icons) in place of the screen's own lines, on a screen other than home's cards; what=0 never has them,
/// and home's default layout sends none (its cards are structured).
#[test]
fn ui_texts_are_the_screen_as_drawn() {
    let mut pc = Pc::new(true);
    let home = pc.ui_what(3);
    assert!(!home.lines().any(|l| l.starts_with("t ")), "home: cards, no t lines\n{home}");
    pc.keys("MENU 0");
    let plain = pc.ui();
    let texts = pc.ui_what(3);
    assert!(!plain.lines().any(|l| l.starts_with("t ")), "{plain}");
    // the screen, state, bar and hints as what=0 gives them; the screen's own lines (its rows) left to the drawing
    assert_eq!(texts.lines().next(), plain.lines().next(), "the screen line");
    assert!(plain.lines().any(|l| l.starts_with("row ")) && !texts.lines().any(|l| l.starts_with("row ")), "{texts}");
    assert!(texts.lines().any(|l| l.starts_with("hints ")), "{texts}");
    let t: std::vec::Vec<&str> = texts.lines().filter(|l| l.starts_with("t ")).collect();
    assert!(!t.is_empty(), "{texts}");
    assert!(t.iter().any(|l| l.contains(",\"")), "texts with their place and style: {t:?}");
}

/// Zones, presets, deletes and contacts at their edges.
mod more;

/// Quick messages, waypoints and DTMF memories (LINK-54 – LINK-56).
mod lists;

/// LINK-53 (decision 325): channel positions by number. `READ_MANY` of positions lists only the channels that have
/// one (lat, lon: i32, 1e-7 degree); `WRITE_MANY` sets one (8 bytes), clears one (0 bytes), refuses one out of
/// range with its reason (CHAN-2's checks, as the terminal's `pos=`) and a record of another length as malformed;
/// a number with no channel changes nothing. `USED` of positions is refused (the channels' map is `USED` of
/// channels). The channel itself is kept as it was.
#[test]
fn channel_positions_by_number() {
    let mut pc = Pc::new(true);
    let mut p = std::vec![what::POSITION];
    p.extend_from_slice(&0u16.to_le_bytes());
    p.extend_from_slice(&1000u16.to_le_bytes());
    let (s, a) = pc.ask(msg::READ_MANY, &p);
    assert_eq!(s, 0);
    let mut got = Vec::new();
    let mut r = &a[2..];
    while r.len() >= 3 {
        let (i, l) = (u16::from_le_bytes([r[0], r[1]]), r[2] as usize);
        assert_eq!(l, 8);
        let (lat, lon) = (i32::from_le_bytes(r[3..7].try_into().unwrap()), i32::from_le_bytes(r[7..11].try_into().unwrap()));
        got.push((i, lat, lon));
        r = &r[3 + l..];
    }
    let want: Vec<(u16, i32, i32)> = (0..1000).filter_map(|n| pc.st.position(n).filter(|_| pc.st.get(n).is_some()).map(|p| (n, p.lat, p.lon))).collect();
    assert!(!want.is_empty());
    assert_eq!(got, want, "every channel with a position, in number order");
    // set one on a channel without
    let c = (0..1000).filter_map(|n| pc.st.get(n)).find(|c| pc.st.position(c.num).is_none()).unwrap();
    let mut rec = 454_000_000i32.to_le_bytes().to_vec();
    rec.extend_from_slice(&(-757_000_000i32).to_le_bytes());
    assert_eq!(pc.write_many(what::POSITION, &[(c.num, rec.clone())]), (0, std::vec![1]));
    assert_eq!(pc.st.position(c.num), Some(crate::data::Pos { lat: 454_000_000, lon: -757_000_000 }));
    assert_eq!(pc.st.get(c.num), Some(c), "the channel as it was");
    let mut one = std::vec![what::POSITION];
    one.extend_from_slice(&c.num.to_le_bytes());
    one.extend_from_slice(&1u16.to_le_bytes());
    let (_, a) = pc.ask(msg::READ_MANY, &one);
    assert_eq!(&a[5..13], rec.as_slice(), "read back");
    // out of range: refused with the reason, nothing changed
    for (lat, lon) in [(900_000_001i32, 0i32), (0, -1_800_000_001)] {
        let mut bad = lat.to_le_bytes().to_vec();
        bad.extend_from_slice(&lon.to_le_bytes());
        let (s, a) = pc.write_many(what::POSITION, &[(c.num, bad)]);
        assert_eq!(s, Status::Invalid as u8);
        assert!(std::str::from_utf8(&a[1..]).unwrap().contains("latitude -90 to 90"), "{a:?}");
    }
    assert_eq!(pc.write_many(what::POSITION, &[(c.num, std::vec![0; 5])]).0, Status::BadArg as u8);
    assert_eq!(pc.st.position(c.num), Some(crate::data::Pos { lat: 454_000_000, lon: -757_000_000 }));
    // cleared
    assert_eq!(pc.write_many(what::POSITION, &[(c.num, Vec::new())]), (0, std::vec![1]));
    assert_eq!(pc.st.position(c.num), None);
    assert_eq!(pc.st.get(c.num), Some(c));
    // no channel there: nothing
    let free = pc.st.free_num(500).unwrap();
    assert_eq!(pc.write_many(what::POSITION, &[(free, rec)]).0, 0);
    assert!(pc.st.get(free).is_none() && pc.st.position(free).is_none());
    assert_eq!(pc.ask(msg::USED, &[what::POSITION]).0, Status::BadArg as u8);
}

/// LINK-66 (decision 347): the radio record: Watch and Battery Save read and written as the menu changes them, a
/// value past the menu's refused, the fixed position with no fix (GPS-3), and the rest of a write ignored.
#[test]
fn radio_record_reads_and_writes() {
    let mut pc = Pc::new(true);
    let read = |pc: &mut Pc| {
        let (s, a) = pc.ask(msg::READ, &[what::RADIO, 0, 0, 0, 0, 200]);
        assert_eq!(s, 0);
        assert_eq!(u16::from_le_bytes([a[0], a[1]]) as usize, ferrite_core::link::RADIO_LEN);
        a[2..].to_vec()
    };
    let write = |pc: &mut Pc, rec: &[u8]| {
        let mut p = std::vec![what::RADIO, 0, 0, 0, 0];
        p.extend_from_slice(&(rec.len() as u16).to_le_bytes());
        p.extend_from_slice(rec);
        pc.ask(msg::WRITE, &p).0
    };
    let r = read(&mut pc);
    assert_eq!((r[0], r[1], r[2]), (pc.app.watch, pc.app.bsave, 3), "the sample's 3D fix");
    assert_ne!(&r[12..14], &[0xFF, 0xFF], "its age");
    assert_eq!(write(&mut pc, &[2, 3]), 0);
    assert_eq!((pc.app.watch, pc.app.bsave), (2, 3));
    assert_eq!(write(&mut pc, &[0, 0, 9, 4, 9]), 0, "the fix and position are ignored");
    assert_eq!((pc.app.watch, pc.app.bsave, pc.app.aprs_cfg().sound), (0, 0, 4), "the message sound written");
    assert_eq!(read(&mut pc)[3], 4);
    assert_eq!(write(&mut pc, &[0, 0, 0, 6]), Status::Invalid as u8, "no seventh chime");
    assert_eq!(write(&mut pc, &[3, 0]), Status::Invalid as u8);
    assert_eq!(write(&mut pc, &[0, 4]), Status::Invalid as u8);
    assert_eq!(write(&mut pc, &[1]), Status::BadArg as u8);
    assert_eq!((pc.app.watch, pc.app.bsave), (0, 0), "unchanged by a refusal");
    // No fix: the fixed position when it is on (GPS-3), no age.
    pc.app.set.fixed_on = true;
    pc.app.set.fixed = crate::data::Pos { lat: 491_234_567, lon: -1_231_234_567 };
    let mut buf = [0u8; ferrite_core::link::RADIO_LEN];
    let mut w = super::Window { b: &mut buf, from: 0, n: 0 };
    super::radio_record(&pc.app, None, &mut w);
    assert_eq!(buf[2], 0, "no fix");
    assert_eq!((i32::from_le_bytes(buf[4..8].try_into().unwrap()), i32::from_le_bytes(buf[8..12].try_into().unwrap())), (491_234_567, -1_231_234_567));
    assert_eq!(&buf[12..14], &[0xFF, 0xFF]);
}
