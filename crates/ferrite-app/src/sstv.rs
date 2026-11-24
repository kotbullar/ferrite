//! SSTV (chapter 27a): what the App keeps while the SSTV screen is open, and what it does with the DSP task's
//! news (passed in by the UI task: [`App::sstv_event`]). The pictures themselves live in the SPI flash
//! (`ferrite_core::sstv`, SSTV-4); the App holds a few bytes.

use crate::chan::ChannelStore;
use crate::data::DataWrite;
use crate::slotctl::{SlotChange, Tune};
use crate::App;
use ferrite_core::msg::RadioCmd;
use ferrite_core::sstv::{self, Event, Header, HEADER_LEN, ROW_LEN, SLOTS, WIDTH};

/// The ISS SSTV FM downlink, in 10 Hz units.
pub const ISS_DOWNLINK: u32 = 14_580_000;

/// The modes a picture is sent in (SSTV-5.1), their VIS codes in the Send page's order.
pub const SEND_MODES: [u8; 5] = [8, 44, 60, 99, 95];
/// The modes # starts by hand (SSTV-2.5), in order, then Auto (the VIS picks).
pub const FORCE_MODES: [u8; 5] = [8, 44, 60, 99, 95];
/// [`SstvState::forced`]'s flag: # has been pressed since the screen opened (one bit, not a field: the App's RAM).
pub const PICKED: u8 = 0x80;
/// What ◀▶ show after the slots: the test card.
pub const VIEW_CARD: u8 = SLOTS;

/// A picture being received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rx {
    pub vis: u8,
    pub slot: u8,
    /// Lines in so far.
    pub lines: u16,
    pub at_ms: u32,
}

/// A picture being sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tx {
    pub vis: u8,
    pub src: u8,
    pub at_ms: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SstvState {
    /// The screen is open: the DSP task's slot is SSTV's (SSTV-1).
    pub open: bool,
    /// The ISS preset keeps the SSTV screen in receive-only mode until closed.
    pub iss: bool,
    /// For the UI task to pass to the DSP task: a mode started by hand (its VIS), or 0: stop.
    pub force: Option<u8>,
    /// SSTV-2.5: the mode # picked (an index into [`FORCE_MODES`] plus one; 0 Auto), until its picture ends; with
    /// [`PICKED`] once # has been pressed since the screen opened (the screen's last line says the mode, "Mode: Auto"
    /// too). Read through [`SstvState::forced_vis`] and [`SstvState::picked`].
    pub forced: u8,
    pub rx: Option<Rx>,
    /// The picture shown: a slot, or [`VIEW_CARD`].
    pub view: u8,
    /// Moves on when a picture is saved or ends: the screen draws again.
    pub ver: u16,
    /// Just opened: the UI task reads the slots (the newest picture shown, the next slot erased).
    pub prep: bool,
    /// For the UI task: a picture to send (taken when it keys).
    pub send: Option<sstv::Send>,
    pub tx: Option<Tx>,
    /// The frame being sent (from the DSP task).
    pub frame: u16,
    /// BACK while sending: the UI task tells the DSP task.
    pub stop: bool,
    /// SSTV-2.6: the slot whose squelch a picture holds open (its monitor latched for it), plus one; 0 none.
    pub held: u8,
    /// SSTV-3.2: the picture that just ended (its slot and lines), for the screen until its header says so: the
    /// header's lines are written through the store's queue after its last rows. Slot 0 when none (slot 0 is a
    /// picture to send, never received into).
    pub ended: (u8, u16),
}

impl SstvState {
    /// SSTV-2.5: the mode # picked (its VIS), or None: Auto.
    pub fn forced_vis(&self) -> Option<u8> {
        match self.forced & !PICKED {
            0 => None,
            k => FORCE_MODES.get(k as usize - 1).copied(),
        }
    }

    /// SSTV-2.5: # has been pressed since the screen opened.
    pub fn picked(&self) -> bool {
        self.forced & PICKED != 0
    }
}

/// Every slot's header, read from the flash (an empty slot, or no flash: None).
pub fn headers(st: &impl ChannelStore) -> [Option<Header>; SLOTS as usize] {
    let mut out = [None; SLOTS as usize];
    for (s, h) in out.iter_mut().enumerate() {
        let mut b = [0xFFu8; HEADER_LEN];
        if st.data().data_read(sstv::slot_addr(s as u8), &mut b) {
            *h = Header::decode(&b);
        }
    }
    out
}

/// What ◀▶ step through: the received pictures newest first, then the two to send, then the test card.
pub fn views(hs: &[Option<Header>; SLOTS as usize], out: &mut [u8; SLOTS as usize + 1]) -> usize {
    let mut rx: [(u32, u8); 4] = [(0, 0); 4];
    let mut n = 0;
    for s in sstv::FIRST_RX..SLOTS {
        if let Some(h) = hs[s as usize] {
            rx[n] = (h.seq, s);
            n += 1;
        }
    }
    rx[..n].sort_unstable_by_key(|a| core::cmp::Reverse(a.0));
    let mut k = 0;
    for &(_, s) in &rx[..n] {
        out[k] = s;
        k += 1;
    }
    for s in 0..sstv::FIRST_RX {
        out[k] = s;
        k += 1;
    }
    out[k] = VIEW_CARD;
    k + 1
}

/// SSTV-2.4 (HOME-9: one line, nothing cut): a received picture's toast as it ends: whether it's whole, then its
/// mode ("Signal lost" / "Robot 36").
pub fn end_toast(vis: u8, whole: bool) -> (&'static [u8], &'static [u8]) {
    (if whole { b"Picture saved" } else { b"Signal lost" }, sstv::mode_name(vis).as_bytes())
}

/// SSTV-5 (HOME-9): a picture's toast as it's sent or stopped, then its mode ("SSTV sent" / "Martin M1").
pub fn sent_toast(vis: u8, whole: bool) -> (&'static [u8], &'static [u8]) {
    (if whole { b"SSTV sent" } else { b"SSTV stopped" }, sstv::mode_name(vis).as_bytes())
}

/// SSTV-5.1 (HOME-9): the body of "Too long" (to send): the picture's seconds and the transmit timeout's.
pub fn too_long_body(secs: u32, limit: u32) -> ferrite_ui::text::Buf<28> {
    let mut b = ferrite_ui::text::Buf::<28>::new();
    let _ = ufmt::uwrite!(b, "{} s, timeout {} s", secs, limit);
    b
}

/// The seconds a mode's transmission takes, its VIS included.
pub fn mode_secs(vis: u8) -> u32 {
    ferrite_core::usstv::Mode::by_vis(vis).map_or(0, |m| (m.image_us() + ferrite_core::usstv::mode::VIS_US).div_ceil(1_000_000))
}

impl App {
    /// SSTV-1: the screen opened (its open in `nav`): the slot to the SSTV decoder (the UI task's poll), the
    /// watch paused, the test card shown until [`App::sstv_prepare`] has read the slots.
    pub fn sstv_enter(&mut self) {
        if self.sstv.open {
            return;
        }
        self.sstv.open = true;
        self.sstv.iss = false;
        self.sstv.forced = 0;
        self.sstv.view = VIEW_CARD;
        self.sstv.prep = true;
        self.sstv.ver = self.sstv.ver.wrapping_add(1);
        self.out.push(RadioCmd::Watch(self.watch_mask()));
        // SSTV-1: a scan moving the slot would cut a picture: stopped where it is
        for s in 0..crate::SLOTS as u8 {
            if self.scan_mask & 1 << s != 0 {
                self.out.push(RadioCmd::Scan { slot: s, on: false });
                self.scan_mask &= !(1 << s);
            }
        }
    }

    /// Set up the active VFO for the ISS downlink and re-arm automatic mode detection.
    pub fn sstv_iss(&mut self, st: &impl ChannelStore) {
        if self.sstv.rx.is_some() {
            return self.show_toast(b"SSTV", b"Picture in progress", false);
        }
        let slot = self.active;
        let preset = SlotChange {
            vfo: true,
            freq: Some(ISS_DOWNLINK),
            mode: Some(0), // Wide voice FM.
            offset: Some((0, false)),
            rx_tone: Some(0),
            ..SlotChange::default()
        };
        if self.slot_apply(slot, &preset, st).is_err() {
            return self.show_toast(b"ISS SSTV", b"VFO unavailable", false);
        }
        let _ = self.slot_tune(slot, Tune::Quiet(0), st);
        self.sstv.iss = true;
        self.sstv.forced = 0;
        self.sstv.force = Some(0);
    }

    /// After [`App::sstv_enter`], with the store (the UI task's next turn): the newest picture received shown, and
    /// the next picture's slot erased if it holds an old one.
    pub fn sstv_prepare(&mut self, st: &impl ChannelStore) {
        if !core::mem::take(&mut self.sstv.prep) {
            return;
        }
        let hs = headers(st);
        let mut v = [0u8; SLOTS as usize + 1];
        views(&hs, &mut v);
        if v[0] >= sstv::FIRST_RX && v[0] < SLOTS {
            self.sstv.view = v[0];
            self.sstv.ver = self.sstv.ver.wrapping_add(1);
        }
        Self::sstv_ready(st, &hs);
    }

    /// Both at once (the tests).
    pub fn sstv_open(&mut self, st: &impl ChannelStore) {
        self.sstv_enter();
        self.sstv_prepare(st);
    }

    /// The screen closed: the decoders back, the watch as it was. A picture still coming in ends there, marked
    /// incomplete (SSTV-2.4), so its slot doesn't read as receiving for ever.
    pub fn sstv_close(&mut self, st: &impl ChannelStore) {
        if !self.sstv.open {
            return;
        }
        if self.sstv.rx.is_some() {
            self.sstv_event(Event::End(false), &[0; WIDTH], st);
        }
        self.sstv.open = false;
        self.sstv.iss = false;
        self.sstv.prep = false;
        self.sstv.rx = None;
        self.sstv_release();
        self.out.push(RadioCmd::Watch(self.watch_mask()));
    }

    /// SSTV-4.1: the slot the next picture goes into erased now unless it is (about 2 s: done long before a picture
    /// comes, as the screen opens and as each picture ends). Erased means all of it: a slot with no picture in it can
    /// still hold old bytes (the range was free before decision 403, and a radio's flash had something there).
    fn sstv_ready(st: &impl ChannelStore, hs: &[Option<Header>; SLOTS as usize]) {
        let (slot, _) = sstv::next_rx(hs);
        if hs[slot as usize].is_some() || !Self::sstv_blank(st, slot) {
            st.data().picture_erase(slot);
        }
    }

    /// The slot holds nothing but 0xFF (its CRC; no flash here counts as blank).
    fn sstv_blank(st: &impl ChannelStore, slot: u8) -> bool {
        st.data().data_crc(sstv::slot_addr(slot), sstv::SLOT_LEN).is_none_or(|c| c == sstv::ERASED_CRC)
    }

    /// SSTV-2.6: the squelch a picture held open, closed again.
    fn sstv_release(&mut self) {
        if self.sstv.held != 0 {
            let slot = self.sstv.held - 1;
            self.sstv.held = 0;
            if self.monitor & 1 << slot == 0 {
                self.out.push(RadioCmd::Monitor { slot, on: false });
            }
        }
    }

    /// # (SSTV-2.5): the next mode by hand, then Auto; the decoder waits for that mode's next sync.
    pub fn sstv_force_next(&mut self) {
        let k = ((self.sstv.forced & !PICKED) + 1) % (FORCE_MODES.len() as u8 + 1);
        self.sstv.forced = PICKED | k;
        self.sstv.force = Some(self.sstv.forced_vis().unwrap_or(0));
    }

    /// The DSP task's news (the UI task passes it on, a row with each [`Event::Line`]).
    pub fn sstv_event(&mut self, ev: Event, row: &[u16; WIDTH], st: &impl ChannelStore) {
        match ev {
            Event::Start(vis) => {
                let hs = headers(st);
                let (slot, seq) = sstv::next_rx(&hs);
                if !Self::sstv_blank(st, slot) {
                    // not ready (it should have been, SSTV-4.1): erased now, the first rows may be lost meanwhile
                    st.data().picture_erase(slot);
                }
                let freq = self.tuned_freq(self.active);
                let h = Header::receiving(vis, self.sat_now().unwrap_or(0), freq, seq);
                let mut bytes = [0xFFu8; ferrite_core::link::data::PROGRAM_MAX];
                bytes.copy_from_slice(&h.encode()[..ferrite_core::link::data::PROGRAM_MAX]);
                st.data().picture_write(DataWrite::Program { addr: sstv::slot_addr(slot), len: HEADER_LEN as u8, bytes });
                self.sstv.rx = Some(Rx { vis, slot, lines: 0, at_ms: self.now });
                self.sstv.ended = (0, 0);
                // SSTV-2.6: a squelch closing for a moment would cut the picture: held open until it ends (not a
                // monitor the user latched, which stays theirs)
                let a = self.active;
                if self.sstv.held == 0 && self.monitor & 1 << a == 0 {
                    self.out.push(RadioCmd::Monitor { slot: a, on: true });
                    self.sstv.held = a + 1;
                }
                // SSTV-2.5: the mode # picked stays shown while its picture waits and comes in; a header heard
                // instead (another mode) is the decoder's own
                if self.sstv.forced_vis().is_some_and(|f| f != vis) {
                    self.sstv.forced &= PICKED;
                }
                self.sstv.view = slot;
                self.sstv.ver = self.sstv.ver.wrapping_add(1);
            }
            Event::Line(y, _) => {
                let Some(rx) = self.sstv.rx.as_mut() else { return };
                if y as usize >= sstv::LINES {
                    return;
                }
                let mut b = [0u8; ROW_LEN];
                for (x, c) in row.iter().enumerate() {
                    b[2 * x..2 * x + 2].copy_from_slice(&c.to_le_bytes());
                }
                st.data().picture_row(sstv::row_addr(rx.slot, y), &b);
                rx.lines = rx.lines.max(y + 1);
            }
            Event::End(whole) => {
                self.sstv_release();
                let Some(rx) = self.sstv.rx.take() else { return };
                // SSTV-2.5: the decoder is back on Auto (the VIS) once the mode # picked ends; a picture # itself
                // ended (the next mode picked) leaves the new pick
                if self.sstv.forced_vis() == Some(rx.vis) {
                    self.sstv.forced &= PICKED;
                }
                if rx.lines == 0 {
                    // SSTV-2.5: a mode # picked that never found its sync (# again, or the screen closed): no
                    // picture, no toast; its slot erased again so the older pictures stay
                    st.data().picture_erase(rx.slot);
                    // the newest picture received shown again (the erase is queued: that slot's header may still read)
                    let mut v = [0u8; SLOTS as usize + 1];
                    let n = views(&headers(st), &mut v);
                    self.sstv.view = v[..n].iter().copied().find(|&s| (sstv::FIRST_RX..SLOTS).contains(&s) && s != rx.slot).unwrap_or(VIEW_CARD);
                    self.sstv.ver = self.sstv.ver.wrapping_add(1);
                    return;
                }
                self.sstv.ended = (rx.slot, rx.lines);
                // SSTV-4.4: the flags and lines programmed over the header written at the start
                let l = rx.lines.to_le_bytes();
                let mut bytes = [0xFFu8; ferrite_core::link::data::PROGRAM_MAX];
                bytes[..5].copy_from_slice(&[if whole { 0xFE } else { 0xFF }, (WIDTH as u16).to_le_bytes()[0], (WIDTH as u16).to_le_bytes()[1], l[0], l[1]]);
                st.data().picture_write(DataWrite::Program { addr: sstv::slot_addr(rx.slot) + 7, len: 5, bytes });
                self.sstv.ver = self.sstv.ver.wrapping_add(1);
                let (head, body) = end_toast(rx.vis, whole);
                self.show_toast(head, body, false);
                // the picture just written counts now: the slot after it gets ready
                let mut hs = headers(st);
                if hs[rx.slot as usize].is_none() {
                    hs[rx.slot as usize] = Some(Header::receiving(rx.vis, 0, 0, u32::MAX));
                }
                Self::sstv_ready(st, &hs);
            }
        }
    }

    /// SSTV-5: send the picture shown in mode `vis` on the active slot: refused (a toast) when it's longer than the
    /// transmit timeout (SSTV-5.1). The UI task keys and plays it.
    pub fn sstv_send(&mut self, vis: u8, src: u8) {
        if self.sstv.iss {
            return self.show_toast(b"ISS SSTV", b"Receive only", false);
        }
        let secs = mode_secs(vis);
        let limit = self.set.tx_timeout_s();
        if secs > limit {
            return self.show_toast(b"Too long", too_long_body(secs, limit).as_bytes(), false);
        }
        let mut call = [0u8; 10];
        let c = self.set.callsign.as_bytes();
        call[..c.len().min(10)].copy_from_slice(&c[..c.len().min(10)]);
        self.sstv.send = Some(sstv::Send { vis, src, call, amp: ferrite_core::audio::AFSK_AMP });
        self.sstv.stop = false;
        self.sstv.frame = 0;
    }

    /// The UI task keyed and handed the picture over.
    pub fn sstv_sending(&mut self, s: &sstv::Send) {
        self.sstv.tx = Some(Tx { vis: s.vis, src: s.src, at_ms: self.now });
    }

    /// The picture's end (whole, or stopped, or refused).
    pub fn sstv_sent(&mut self, whole: bool) {
        let Some(t) = self.sstv.tx.take() else { return };
        let (head, body) = sent_toast(t.vis, whole);
        self.show_toast(head, body, false);
    }
}
