//! HF mode on the SI4732.

use super::*;

/// HF-31: the SI4732's AM seek band in HF mode (kHz): HF mode's bottom (150 kHz) to chip 1's start (18 MHz).
const HF_SEEK_KHZ: (u16, u16) = (150, 18_000);
/// HF-15.1: the board's front end (an LNA before the SI4732's AM input) gain, estimated at 20 dB: the chip reads
/// 13-33 dBuV of indoor noise on the whip, which a receiver measured at the antenna shows as about S2-S5.
const SI_FRONT_DB: i16 = 20;
/// HF-19: the SI4732's I2C half period for the patch download, us (5 otherwise, about 50 kHz).
const PATCH_HALF_US: u32 = 1;
/// HF mode's modes as the UI sends them (`screens::hf`): USB, LSB, CW.
const MODE_USB: u8 = 3;
const MODE_LSB: u8 = 4;
const MODE_CW: u8 = 5;

/// A read of the SPI parts' range (`store::part_read`), tried again while the Storage task has the chip, for up
/// to half a second.
async fn part_read_wait(addr: u32, buf: &mut [u8]) -> Result<(), si4732::Error> {
    for _ in 0..500 {
        if crate::store::part_read(addr, buf) {
            return Ok(());
        }
        Timer::after_millis(1).await;
    }
    Err(si4732::Error::Timeout)
}

impl Radio {
    /// HF mode on, retuned or off (HF-3, HF-4, HF-10, HF-11): the SI4732 below 18 MHz, chip 1 through the
    /// energised relay from 18 MHz (decision 322, as stock and nicFW). Watch, scan and the other chip pause meanwhile (HF-8).
    pub(super) async fn hf(&mut self, freq: Freq10, mode: u8, on: bool, keep_rx: bool) {
        self.seek_cancel(); // HF-31: a retune or off ends a seek
        if !on {
            if self.hf.take().is_some_and(|h| h.si) {
                let _ = broadcast(&mut self.si, BroadcastCmd::Off, self.si_opts).await;
            }
            self.tuned = [None; 2];
            self.live.mode = ModeLive::Off;
            if !keep_rx {
                let f = self.slots[self.active as usize].freq;
                self.select_rx(f, Cause::HfReceiver).await;
            }
            log(b"hf off", 0);
            return;
        }
        let si = freq < mhz(18);
        let was = self.hf.replace(HfOn { freq, mode, si });
        for n in 0..2u8 {
            if !(n == 1 && !si) {
                self.tune_chip(n, None).await;
            }
        }
        speaker(false);
        let mut s = Steps::default();
        let rx = if si { Rx::Si } else { Rx::Bk(Sel::rx(freq)) };
        let _ = self.path.rx(rx, Cause::HfReceiver, now_ms() as u32, &mut s);
        self.run(&s).await;
        if si && matches!(mode, MODE_USB | MODE_LSB | MODE_CW) {
            // HF-19: SSB and CW with the patch (the UI offers them only with one stored, HF-20)
            let ok = self.si_ssb(freq, mode).await;
            log(if ok.is_ok() { b"hf si ssb ok, khz " } else { b"hf si ssb FAILED, khz " }, freq / 100);
            speaker(ok.is_ok() && !self.muted);
        } else if si {
            self.ssb_at = None;
            let khz = (freq / 100) as u16;
            let ok = broadcast(&mut self.si, BroadcastCmd::Am(khz), self.si_opts).await.is_ok();
            log(if ok { b"hf si am ok, khz " } else { b"hf si FAILED, khz " }, khz as u32);
            speaker(ok && !self.muted);
        } else {
            if was.is_some_and(|h| h.si) {
                let _ = broadcast(&mut self.si, BroadcastCmd::Off, self.si_opts).await;
            }
            self.tune_chip(1, Some((HF_SLOT, freq))).await;
            self.chip(1).af_out(AfOut::Normal);
            speaker(!self.muted);
        }
        log(b"hf khz ", freq / 100);
        self.hf_poll();
    }

    /// HF-19: the SI4732 in SSB (USB, LSB; CW as USB [`ferrite_core::ssb::CW_PITCH_HZ`] low): powered up with the
    /// patch and the patch streamed in when it isn't in SSB yet (HF-3's progress), then tuned to the whole kHz
    /// with the rest on the BFO; a move within the same kHz and sideband only sets the BFO.
    async fn si_ssb(&mut self, freq: Freq10, mode: u8) -> Result<(), si4732::Error> {
        let d = &mut BusyDelay;
        let (khz, usb, bfo) = ferrite_core::ssb::tune(freq, mode == MODE_USB, mode == MODE_CW);
        if self.si.mode() != Some(SiMode::Ssb) {
            self.ssb_at = None;
            if self.si.mode().is_some() {
                self.si.power_down(d)?;
            }
            si_input(SiMode::Ssb);
            SI_RST.write(false);
            Timer::after_millis(10).await;
            SI_RST.write(true);
            Timer::after_millis(10).await;
            self.si.power_up_start(SiMode::Ssb)?;
            let t0 = now_ms();
            while !self.si.ready()? {
                if now_ms() - t0 > SI_TUNE_MAX_MS {
                    return Err(si4732::Error::Timeout);
                }
                Timer::after_millis(5).await;
            }
            // The patch as soon as the chip answers (AN332: right after POWER_UP), then whatever is left of the
            // crystal's 550 ms before the first tune (stock waits it out first)
            let up = now_ms();
            self.ssb_patch(freq).await?;
            let settled = up + 550;
            if now_ms() < settled {
                Timer::after_millis(settled - now_ms()).await;
            }
            self.si.volume(SI_VOLUME, d)?;
            si_apply(&mut self.si, self.si_opts)?;
        }
        if self.ssb_at != Some((khz, usb)) {
            self.si.tune_start(khz, if usb { Sideband::Usb } else { Sideband::Lsb }, d)?;
            let t0 = now_ms();
            while self.si.tune_poll(d)?.is_none() {
                if now_ms() - t0 > SI_TUNE_MAX_MS {
                    return Err(si4732::Error::Timeout);
                }
                Timer::after_millis(10).await;
            }
            self.ssb_at = Some((khz, usb));
        }
        self.si.bfo(bfo, d)
    }

    /// HF-3, HF-19: the stored patch (SPI, `layout::SSB_PATCH`, checked at boot) to the chip, 8 bytes a record,
    /// read in 64-byte pieces; the progress goes to the screen as it goes ([`ModeLive::Hf`]'s `loading`).
    async fn ssb_patch(&mut self, freq: Freq10) -> Result<(), si4732::Error> {
        use ferrite_core::{layout::SSB_PATCH, ssb};
        let mut h = [0u8; ssb::HEADER];
        part_read_wait(SSB_PATCH.start, &mut h).await?;
        let p = ssb::stored(&h).ok_or(si4732::Error::Rejected)?;
        let at = SSB_PATCH.start + ssb::HEADER as u32;
        let mut buf = [0u8; 64];
        let live = LIVE.sender();
        let t0 = now_ms();
        // The download only on a faster clock (1 us half periods against the usual 5), within the chip's
        // 400 kHz and gentle on SDA's internal pull-up; back to the usual after, whatever happened.
        let slow = core::mem::replace(&mut self.si.bus_mut().0.half_us, PATCH_HALF_US);
        let r = async {
            for off in (0..p.len).step_by(buf.len()) {
                let n = (p.len - off).min(buf.len() as u32) as usize;
                part_read_wait(at + off, &mut buf[..n]).await?;
                if off == 0 && buf[0] != 0x15 {
                    return Err(si4732::Error::Rejected); // not a patch's first record (ssb::records_ok)
                }
                for r in buf[..n].as_chunks::<8>().0 {
                    self.si.patch_record(r, &mut BusyDelay)?;
                }
                // This task runs above the UI and the link: a turn for them every 32 records (a few ms), or the
                // screen freezes for the whole download (black, Nov 3, 2026)
                if off % 256 == 0 {
                    if off % 1024 == 0 {
                        let loading = (off as u64 * 100 / p.len as u64) as u8;
                        self.live.mode = ModeLive::Hf { freq, rssi: -127, snr: 0, si4732: true, loading };
                        live.send(self.live);
                    }
                    Timer::after_millis(1).await;
                }
            }
            Ok(())
        }
        .await;
        self.si.bus_mut().0.half_us = slow;
        r?;
        log(b"hf ssb patch ms ", (now_ms() - t0) as u32);
        Ok(())
    }

    pub(super) fn hf_poll(&mut self) {
        let Some(h) = self.hf else { return };
        let (rssi, snr) = if h.si {
            match self.si.rsq(&mut BusyDelay) {
                Ok(q) => (q.rssi as i16 - 107 - SI_FRONT_DB, q.snr), // HF-15.1: dBuV at the chip to dBm at the antenna
                Err(_) => (-127, 0),
            }
        } else {
            (self.chip(1).rssi_dbm(), 0)
        };
        self.live.mode = ModeLive::Hf { freq: h.freq, rssi, snr, si4732: h.si, loading: 255 };
        // The cable log (TERM-28): the HF receiver's level once a second.
        static LOGGED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        let now = now_ms() as u32;
        if now.wrapping_sub(LOGGED.load(core::sync::atomic::Ordering::Relaxed)) >= 1000 {
            LOGGED.store(now, core::sync::atomic::Ordering::Relaxed);
            log(b"hf rssi -dbm ", (-(rssi as i32)).max(0) as u32);
            log(b"hf snr db ", snr as u32);
        }
    }

    /// HF-31: the SI4732's own seek from where it is (AM, below 30 MHz), started here and polled by the tick
    /// ([`Radio::seek_poll`]): the task keeps taking commands meanwhile (MODE-7), and the frequency it stops on is
    /// kept. A seek already running is cancelled first. The seek band is HF mode's SI4732 range ([`HF_SEEK_KHZ`];
    /// the chip's own default, 520-1710 kHz, sent a seek up from 9.4 MHz to 1.71 MHz on the radio, Oct 15, 2026),
    /// and it stops at the band edge rather than wrapping round.
    pub(super) fn hf_seek(&mut self, up: bool) {
        self.seek_cancel();
        if self.hf.is_none_or(|h| !h.si) {
            return;
        }
        let d = &mut BusyDelay;
        let (lo, hi) = HF_SEEK_KHZ;
        // HF-34, HF-35: the seek steps by the band's channel spacing (MW 9 or 10 kHz, LW 9, shortwave 5)
        let space = self.si_opts.am_space(self.hf.map_or(0, |h| (h.freq / 100) as u16));
        if self.si.am_seek_band(lo, hi, d)
            .and_then(|_| self.si.set_property(si4732::prop::AM_SEEK_FREQ_SPACING, space, d))
            .and_then(|_| self.si.seek_start(up, false, d))
            .is_err()
        {
            return;
        }
        let now = now_ms() as u32;
        self.seek = Some(SiSeek { hf: true, t0: now, poll_at: now.wrapping_add(SEEK_POLL_MS) });
        log(b"hf seek start, up ", up as u32);
    }

    /// FM broadcast's seek (BCR-18), the same way: started here, polled by the tick.
    pub(super) async fn bcast_seek(&mut self, up: bool) {
        self.seek_cancel();
        if self.hf.is_some() || broadcast(&mut self.si, BroadcastCmd::Seek { up }, self.si_opts).await.is_err() {
            return log(b"si FAILED, cmd ", 3);
        }
        let now = now_ms() as u32;
        self.seek = Some(SiSeek { hf: false, t0: now, poll_at: now.wrapping_add(SEEK_POLL_MS) });
    }

    /// A seek running is stopped where it is (FM_TUNE_STATUS with CANCEL): any HF or broadcast command, a new seek,
    /// power off.
    pub(super) fn seek_cancel(&mut self) {
        if self.seek.take().is_some() && self.si.mode().is_some() {
            let _ = self.si.seek_cancel(&mut BusyDelay);
            log(b"si seek cancelled", 0);
        }
    }

    /// The tick's turn for a seek (every [`SEEK_POLL_MS`]): done, it is kept as HF mode's frequency or the
    /// broadcast's station; with nothing found in [`SEEK_MAX_MS`] it is cancelled where it got to.
    pub(super) fn seek_poll(&mut self) {
        let Some(sk) = self.seek else { return };
        let now = now_ms() as u32;
        if now.wrapping_sub(sk.poll_at) > u32::MAX / 2 {
            return; // not due
        }
        let d = &mut BusyDelay;
        let found = match self.si.tune_poll(d) {
            Ok(None) if now.wrapping_sub(sk.t0) < SEEK_MAX_MS => {
                self.seek = Some(SiSeek { poll_at: now.wrapping_add(SEEK_POLL_MS), ..sk });
                return;
            }
            Ok(None) => {
                log(b"si seek timed out", 0);
                self.si.seek_cancel(d).ok().map(|t| t.freq)
            }
            Ok(Some(t)) => Some(t.freq),
            Err(_) => None,
        };
        self.seek = None;
        if sk.hf {
            if let (Some(h), Some(khz)) = (self.hf.filter(|h| h.si), found) {
                self.hf = Some(HfOn { freq: khz as Freq10 * 100, ..h });
                log(b"hf seek khz ", khz as u32);
            }
            self.hf_poll();
        } else if let Some(f) = found.filter(|_| self.hf.is_none()) {
            self.fm = Some(FmRx { freq: f, rds: si4732::rds::Rds::init(), poll_at: 0, rds_turn: false });
            if matches!(self.live.mode, ModeLive::Fm { .. }) {
                self.live.mode = ModeLive::Off; // nothing of the last station's RDS carries over
            }
            log(b"si seek ok, 10 khz ", f as u32);
        }
    }
}
