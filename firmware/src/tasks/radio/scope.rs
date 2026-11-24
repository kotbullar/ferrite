//! The band scope and the home strip.

use super::*;

impl ScopeRun {
    pub(super) fn uses_si(&self) -> bool {
        self.si
    }
}

impl Radio {
    /// SCOPE-4, SCOPE-13: the scope on (the centre's chip sweeps, the other sleeps, the relay stays) or off (the
    /// engines retune on the next tick).
    pub(super) async fn scope(&mut self, centre: Freq10, step: u16, on: bool) {
        self.tuned = [None; 2];
        self.bcast_audio(now_ms() as u32, false); // the speaker off, or the broadcast's alone (MODE-5)
        let (n, tenths, strip, hf, si) = self.scope_cfg;
        let si = hf && si && on;
        if self.scope.as_ref().is_some_and(ScopeRun::uses_si) && !si {
            speaker(false);
            let _ = broadcast(&mut self.si, BroadcastCmd::Off, self.si_opts).await;
        }
        if !on {
            for n in 0..2u8 {
                if self.has(n) {
                    self.chip(n).af_out(AfOut::Mute);
                }
            }
            self.scope = None;
            self.live.mode = ModeLive::Off;
            log(b"scope ", 0);
            return;
        }
        let chip = Sel::rx(centre).chip();
        if hf {
            if !freq::hf_scope_range(si).contains(&centre) || (!si && !self.has(chip)) {
                self.scope = None;
                self.live.mode = ModeLive::Off;
                return;
            }
            let receiver = if si { Rx::Si } else { Rx::Bk(Sel::rx(centre)) };
            let mut steps = Steps::default();
            let _ = self.path.rx(receiver, Cause::HfReceiver, now_ms() as u32, &mut steps);
            self.run(&steps).await;
            if self.path.rx_now() != receiver {
                self.scope = None;
                self.live.mode = ModeLive::Off;
                return;
            }
        }
        let home = strip.then_some(self.active);
        let n = n.clamp(1, ferrite_core::msg::SCOPE_BARS as u8);
        let keep = self.scope.as_ref().filter(|sc| sc.si == si && sc.centre == centre && sc.step == step as Freq10 && sc.n == n);
        let last = keep.map_or([0; ferrite_core::msg::SCOPE_BARS], |sc| sc.last);
        self.scope = Some(ScopeRun {
            hf,
            si,
            chip,
            centre,
            step: step as Freq10,
            n,
            dwell_us: tenths.max(1) as u32 * 100,
            si_tune_at: None,
            i: 0,
            last,
            sweeps: 0,
            started: now_ms() as u32,
            listen: 0,
            mode: 0,
            home,
            look: 0,
            since: 0,
            closed: 0,
        });
        self.live.mode = ModeLive::Scope { centre, bars: last, n, sweeps: 0, listen: 0, open: false, si };
        if si {
            speaker(false);
            for n in 0..2 {
                self.tune_chip(n, None).await;
            }
            // Start the AM receiver only once. Subsequent points use asynchronous tune polling.
            if self.si.mode() != Some(SiMode::Am)
                && broadcast(&mut self.si, BroadcastCmd::Am((centre / 100) as u16), self.si_opts).await.is_err()
            {
                let _ = broadcast(&mut self.si, BroadcastCmd::Off, self.si_opts).await;
                self.scope = None;
                self.live.mode = ModeLive::Off;
            }
            return;
        }
        if self.has(1 - chip) {
            self.tune_chip(1 - chip, None).await;
        }
        // The chip's full receive setup once on the centre (filter, demod, squelch), muted; the path asked for with
        // Cause::Auto, so the relay never moves (SCOPE-5). The strip's is its slot's own (tone, squelch level).
        self.tune_chip(chip, Some((home.unwrap_or(SCOPE_SLOT), centre))).await;
        self.chip(chip).af_out(AfOut::Mute);
        self.select_rx(centre, Cause::Auto).await;
        self.scope_point();
        log(b"scope points ", n as u32);
    }

    /// SCOPE-11: listen on `freq` with the scope's mode (the path set first); 0 goes back to sweeping.
    pub(super) async fn scope_listen(&mut self, freq: Freq10, mode: u8) {
        let Some(sc) = self.scope.as_mut() else { return };
        if sc.hf && freq != 0 && !freq::hf_scope_range(sc.uses_si()).contains(&freq) {
            return;
        }
        let si = sc.uses_si();
        (sc.listen, sc.mode) = (freq, mode);
        if si {
            speaker(false);
            if sc.si_tune_at.take().is_some() {
                let _ = self.si.seek_cancel(&mut BusyDelay);
            }
            let open = freq != 0
                && broadcast(&mut self.si, BroadcastCmd::Am((freq / 100) as u16), self.si_opts).await.is_ok();
            speaker(open && !self.muted);
            if let ModeLive::Scope { listen, open: heard, .. } = &mut self.live.mode {
                (*listen, *heard) = (freq, open);
            }
            return;
        }
        let chip = sc.chip;
        let back = sc.centre;
        self.tuned[chip as usize] = None;
        self.tune_chip(chip, Some((SCOPE_SLOT, if freq != 0 { freq } else { back }))).await;
        self.chip(chip).af_out(AfOut::Mute);
        self.open_delay(chip, false); // SQL-17: listening is ordinary receive
        if freq != 0 {
            self.select_rx(freq, Cause::Auto).await;
        } else {
            speaker(false);
            self.scope_point();
        }
    }

    /// The frequency of point `i`, or None when it needs the other chip or relay state (SCOPE-5) or no chip
    /// receives it.
    fn scope_freq(sc: &ScopeRun, i: u8) -> Option<Freq10> {
        let low = sc.centre as i64 - sc.step as i64 * sc.n as i64 / 2;
        let f = low + sc.step as i64 * i as i64;
        let f = Freq10::try_from(f).ok()?;
        if sc.hf && !freq::hf_scope_range(sc.uses_si()).contains(&f) {
            return None;
        }
        if sc.uses_si() {
            return Some(f);
        }
        let (here, centre) = (Sel::rx(f), Sel::rx(sc.centre));
        (here.chip() == centre.chip() && here.relay() == centre.relay() && freq::receivable(f)).then_some(f)
    }

    /// Tune the scope's chip to its current point: stock's per-point sequence, frequency then the REG_30 retune.
    pub(super) fn scope_point(&mut self) {
        let Some(sc) = self.scope.as_ref() else { return };
        if sc.uses_si() {
            return;
        }
        if let Some(f) = Self::scope_freq(sc, sc.i) {
            let chip = sc.chip;
            let w = self.sqcal.rx_word(f); // CAL-12
            let mut bk = self.chip(chip);
            bk.set_freq(w * 10);
            bk.rx_on();
        }
    }

    /// The scope's tick: while listening, the squelch decides the audio; while sweeping, the point tuned at the
    /// last wait is read (after the dwell) and the next is tuned. Each sweep's time goes to the bring-up log.
    /// In slow flash (memory.x): once a point, at least 2 ms apart; the chip's writes it calls stay fast.
    #[inline(never)]
    pub(super) fn scope_tick(&mut self, now: u32) {
        let Some(mut sc) = self.scope.take() else { return };
        if sc.uses_si() {
            self.scope_si_tick(&mut sc, now);
            self.scope = Some(sc);
            return;
        }
        let chip = sc.chip;
        // SQL-9.4: a latched or held monitor on the strip's slot holds it there, open, as on home without the strip
        // (it was ignored: monitor worked only once a menu stopped the strip). Not while the slot is muted or off.
        let monitored = sc.home.is_some_and(|s| self.monitor & !(self.user_mute | self.user_off) & 1 << s != 0);
        if sc.listen != 0 {
            let mut bk = self.chip(chip);
            let (st, rssi) = (bk.status(), bk.rssi_dbm());
            let open = st.squelch_open || monitored;
            let mut heard = open;
            if let Some(s) = sc.home {
                // MODE-5: the strip held on its slot's frequency: that slot's tone and quick mute (HOME-15) decide
                // the audio, its card shows it; HOLD_CLOSED closed polls and the sweep goes on where it was.
                let i = s as usize % SLOTS;
                // SQL-13 on the strip: level 0 is heard while the strip is on its slot's frequency; holding the strip
                // there still goes by a carrier (or monitor), or level 0 would stop the sweep for good
                let level0 = self.slots[i].sq == 0;
                heard = monitored || self.slots[i].toned(open || level0, &st) && self.user_mute & 1 << s == 0;
                sc.closed = if open { 0 } else { sc.closed + 1 };
                if sc.closed >= HOLD_CLOSED {
                    sc.listen = 0;
                }
                let l = &mut self.live.slots[i];
                let shown = open || level0;
                (l.squelch_open, l.rssi, l.rssi_raw) = (shown, if shown { rssi } else { rssi.min(-121) }, rssi);
                l.carrier = st.squelch_open; // SQL-13.1: the chip's own decision
            }
            self.chip(chip).af_out(if heard { AfOut::Normal } else { AfOut::Mute });
            self.bcast_audio(now, heard); // BCR-24: a call heard mutes the broadcast, which returns 2 s after
            if let ModeLive::Scope { listen, open: o, .. } = &mut self.live.mode {
                (*listen, *o) = (sc.listen, open);
            }
            let back = sc.listen == 0;
            self.scope = Some(sc);
            if back {
                self.scope_point();
            }
            return;
        }
        self.bcast_audio(now, false); // the broadcast's 2 s return after a hold (BCR-24)
        if self.sweep_hold.held(now) {
            self.scope = Some(sc);
            return; // SCOPE-14: the UI first
        }
        if sc.look > 0 {
            // MODE-5: the strip on its slot's own frequency: a carrier there holds the sweep.
            sc.look -= 1;
            let back = if monitored || self.chip(chip).status().squelch_open {
                (sc.listen, sc.closed, sc.look) = (self.slots[sc.home.unwrap_or(0) as usize % SLOTS].rx_freq(), 0, 0);
                self.open_delay(chip, false); // SQL-17: held on the call, ordinary receive
                false
            } else {
                sc.look == 0
            };
            self.scope = Some(sc);
            if back {
                self.scope_point();
            }
            return;
        }
        let i = sc.i as usize;
        let level = if Self::scope_freq(&sc, sc.i).is_some() {
            let dbm = self.chip(chip).rssi_dbm();
            (dbm + 160).clamp(1, 255) as u8
        } else {
            0
        };
        let bar = if level != 0 && sc.last[i] != 0 { ((level as u16 + sc.last[i] as u16) / 2) as u8 } else { level };
        sc.last[i] = level;
        if let ModeLive::Scope { bars, listen, open, .. } = &mut self.live.mode {
            bars[i] = bar;
            (*listen, *open) = (0, false);
        }
        sc.i += 1;
        if sc.i >= sc.n {
            sc.i = 0;
            sc.sweeps = sc.sweeps.wrapping_add(1);
            if let ModeLive::Scope { sweeps, .. } = &mut self.live.mode {
                *sweeps = sc.sweeps;
            }
            log(b"scope sweep ms ", now.wrapping_sub(sc.started));
            sc.started = now;
        }
        if let Some(s) = sc.home {
            sc.since += 1;
            if sc.since >= LOOK_EVERY || monitored {
                // MODE-5: back to the slot's own frequency for a look, then on from the next point.
                (sc.since, sc.look) = (0, LOOK_WAITS);
                let f = self.slots[s as usize % SLOTS].rx_freq();
                self.scope = Some(sc);
                self.open_delay(chip, true); // SQL-17: the look hears a carrier at once
                let w = self.sqcal.rx_word(f); // CAL-12
                let mut bk = self.chip(chip);
                bk.set_freq(w * 10);
                bk.rx_on();
                return;
            }
        }
        self.scope = Some(sc);
        self.scope_point();
    }
    /// Each SI4732 point starts a tune and waits for STC before taking a sample.
    fn scope_si_tick(&mut self, sc: &mut ScopeRun, now: u32) {
        if sc.listen != 0 || self.sweep_hold.held(now) {
            return;
        }
        let level = if let Some(f) = Self::scope_freq(sc, sc.i) {
            let khz = (f / 100) as u16;
            if let Some(start) = sc.si_tune_at {
                match self.si.tune_poll(&mut BusyDelay) {
                    Ok(Some(tune)) if tune.freq == khz => (super::hf::si_rssi_dbm(tune.rssi) + 160).clamp(1, 255) as u8,
                    Ok(_) if now.wrapping_sub(start) < SI_TUNE_MAX_MS as u32 => return,
                    _ => 0,
                }
            } else if self.si.tune_start(khz, Sideband::Usb, &mut BusyDelay).is_ok() {
                sc.si_tune_at = Some(now);
                return;
            } else {
                0
            }
        } else {
            0
        };
        sc.si_tune_at = None;
        if let ModeLive::Scope { bars, sweeps, .. } = &mut self.live.mode {
            bars[sc.i as usize] = level;
            sc.i += 1;
            if sc.i >= sc.n {
                sc.i = 0;
                sc.sweeps = sc.sweeps.wrapping_add(1);
                *sweeps = sc.sweeps;
            }
        }
    }

}
