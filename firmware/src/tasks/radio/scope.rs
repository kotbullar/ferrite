//! The band scope and the home strip.

use super::*;

impl Radio {
    /// SCOPE-4, SCOPE-13: the scope on (the centre's chip sweeps, the other sleeps, the relay stays) or off (the
    /// engines retune on the next tick).
    pub(super) async fn scope(&mut self, centre: Freq10, step: u16, on: bool) {
        self.tuned = [None; 2];
        self.bcast_audio(now_ms() as u32, false); // the speaker off, or the broadcast's alone (MODE-5)
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
        let (n, tenths, strip, hf) = self.scope_cfg;
        if hf {
            if !(mhz(18)..mhz(64)).contains(&centre) || !self.has(chip) {
                self.scope = None;
                self.live.mode = ModeLive::Off;
                return;
            }
            self.select_rx(centre, Cause::HfReceiver).await;
            if self.path.rx_now() != Rx::Bk(Sel::rx(centre)) {
                self.scope = None;
                self.live.mode = ModeLive::Off;
                return;
            }
        }
        let home = strip.then_some(self.active);
        let n = n.clamp(1, ferrite_core::msg::SCOPE_BARS as u8);
        let keep = self.scope.as_ref().filter(|sc| sc.centre == centre && sc.step == step as Freq10 && sc.n == n);
        let last = keep.map_or([0; ferrite_core::msg::SCOPE_BARS], |sc| sc.last);
        self.scope = Some(ScopeRun {
            hf,
            chip,
            centre,
            step: step as Freq10,
            n,
            dwell_us: tenths.max(1) as u32 * 100,
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
        self.live.mode = ModeLive::Scope { centre, bars: last, n, sweeps: 0, listen: 0, open: false };
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
        if sc.hf && freq != 0 && !(mhz(18)..mhz(64)).contains(&freq) {
            return;
        }
        (sc.listen, sc.mode) = (freq, mode);
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
        if sc.hf && !(mhz(18)..mhz(64)).contains(&f) {
            return None;
        }
        let (here, centre) = (Sel::rx(f), Sel::rx(sc.centre));
        (here.chip() == centre.chip() && here.relay() == centre.relay() && freq::receivable(f)).then_some(f)
    }

    /// Tune the scope's chip to its current point: stock's per-point sequence, frequency then the REG_30 retune.
    pub(super) fn scope_point(&mut self) {
        let Some(sc) = self.scope.as_ref() else { return };
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
                heard = monitored || self.slots[i].toned(open, &st) && self.user_mute & 1 << s == 0;
                sc.closed = if open { 0 } else { sc.closed + 1 };
                if sc.closed >= HOLD_CLOSED {
                    sc.listen = 0;
                }
                let l = &mut self.live.slots[i];
                (l.squelch_open, l.rssi) = (open, if open { rssi } else { rssi.min(-121) });
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
}
