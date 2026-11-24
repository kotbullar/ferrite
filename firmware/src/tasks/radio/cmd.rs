//! The UI's commands.

use super::*;

impl Radio {
    /// MODE-3: the UI stopped renewing the exclusive mode (an app bug, or a UI too busy to run): stop it here and
    /// go back to normal receive, and tell the UI through the snapshot's drop count.
    pub(super) async fn lease_check(&mut self) {
        // Commands still queued (this task was busy, say loading the SSB patch) may hold the renewals.
        if !crate::chan::RADIO.is_empty() {
            return;
        }
        let Some(h) = self.lease.expired(now_ms() as u32) else { return };
        log(b"mode lease ran out, stopped ", h as u32);
        let restore_rx = self.scope.as_ref().is_some_and(|scope| scope.hf);
        self.cmd(h.stop_cmd()).await;
        if restore_rx {
            let f = self.slots[self.active as usize].freq;
            self.select_rx(f, Cause::HfReceiver).await;
        }
        self.live.lease_drops = self.live.lease_drops.wrapping_add(1);
    }

    /// One command from the UI.
    pub(super) async fn cmd(&mut self, c: RadioCmd) {
        let now = now_ms() as u32;
        // The single-receiver command is the active slot's tune.
        let c = if let RadioCmd::Tune(freq) = c { RadioCmd::TuneSlot { slot: self.active, freq } } else { c };
        self.lease.note(&c, now); // MODE-3
        // Channel scans: the set, priorities, resume rule, start, and the frequencies the UI answers with; one
        // a stopped scan waited for is tuned now.
        if let Some((i, f)) = self.chans.cmd(&c, &mut self.scans, now) {
            self.slots[i].freq = f;
            self.live.slots[i].freq = f;
        }
        match c {
            RadioCmd::ScanChans { slot, .. } => {
                let sl = &mut self.live.slots[slot as usize % SLOTS];
                (sl.scanning, sl.hits, sl.skips) = (true, 0, 0);
                log(b"scan start (channels) slot ", slot as u32);
            }
            RadioCmd::ScanSet { .. }
            | RadioCmd::ScanPrio { .. }
            | RadioCmd::ScanResume { .. }
            | RadioCmd::ScanEntry { .. }
            | RadioCmd::ScanEntryTx { .. } => {}
            RadioCmd::TuneSlot { slot, freq } => {
                let i = slot as usize % SLOTS;
                (self.slots[i].freq, self.slots[i].clar) = (freq, 0); // VFO-22: a retune clears the clarifier
                self.scans[i].stop();
                self.chans.off(i);
                let sl = &mut self.live.slots[i];
                (sl.freq, sl.scanning) = (freq, false);
                if slot == self.active && self.hf.is_none() {
                    self.select_rx(freq, Cause::ActiveSlot).await; // the active slot may move the relay
                }
                self.retune(slot);
                self.save.activity(now_ms());
            }
            RadioCmd::SlotParam { slot, param, value } => self.param(slot, param, value),
            RadioCmd::Active(a) => {
                self.active = a % SLOTS as u8;
                if self.hf.is_none() {
                    let f = self.slots[self.active as usize].freq;
                    self.select_rx(f, Cause::ActiveSlot).await;
                }
            }
            RadioCmd::Watch(m) => self.watch_set = m,
            RadioCmd::Awake(m) => self.keep = m,
            RadioCmd::RfGain(gain) => {
                self.rf_gain = gain.min(4);
                self.rf_gain_pending = self.chips;
                if self.path.keyed().is_none() {
                    for n in 0..2 {
                        if self.tuned[n as usize].is_some() {
                            self.apply_rf_gain(n);
                        }
                    }
                }
            }
            RadioCmd::AprsSlot(m) => {
                // APRS-55: a slot that gains or loses APRS retunes, for the receive set-up (the unkey retunes anyway).
                let changed = self.aprs_slots ^ m;
                self.aprs_slots = m;
                if self.path.keyed().is_none() {
                    (0..SLOTS as u8).filter(|s| changed & 1 << s != 0).for_each(|s| self.retune(s));
                }
            }
            RadioCmd::PauseWatch(secs) => self.watcher.pause(now, secs),
            RadioCmd::Scan { slot, on } => self.scan(slot, on),
            RadioCmd::ScanRangeSet { slot, what, f } => self.scan_range_set(slot, what, f),
            RadioCmd::ScanSkip { slot } => {
                let i = slot as usize % SLOTS;
                let o = if self.chans.is_on(i) {
                    self.chans.skip_once(i, &mut self.scans[i], now)
                } else {
                    let r = Range { skipped: if self.preset_slot as usize == i { &self.preset_skips[..self.preset_n as usize] } else { &[] }, ..self.ranges[i] };
                    self.scans[i].skip_once(&r, now)
                };
                self.scan_out(i, o);
            }
            RadioCmd::ScanDir { slot, up, .. } => {
                let i = slot as usize % SLOTS;
                let o = if self.chans.is_on(i) {
                    self.chans.dir(i, &mut self.scans[i], up, now)
                } else {
                    let r = Range { skipped: if self.preset_slot as usize == i { &self.preset_skips[..self.preset_n as usize] } else { &[] }, ..self.ranges[i] };
                    self.scans[i].dir(up, &r, now)
                };
                self.scan_out(i, o);
            }
            RadioCmd::Monitor { slot, on } => self.monitor = self.monitor & !(1 << slot) | (on as u8) << slot,
            RadioCmd::Mute(m) => self.user_mute = m,
            RadioCmd::SlotOff(m) => self.user_off = m,
            RadioCmd::ScanTx(on) => self.scan_tx = on,
            RadioCmd::ToneLevel(l) => self.tone_level = l.min(2),
            RadioCmd::BeepVol(v) => {
                // SND-3: Low, Mid, High. The DAC beep at levels 5, 7, 9 (a quiet Low; 11 would be full scale); the DTMF sidetone (on the
                // BK4829, it goes on air) at the matching points of the generator's old 1-5 scale.
                let i = v.clamp(1, 3) as usize - 1;
                self.beep_gain = tonegen::beep_gain([2, 3, 5][i]);
                self.beep_level = [5, 7, 9][i];
            }
            #[cfg(feature = "tx")]
            RadioCmd::MicGain(g) => self.tx.mic = g.clamp(1, ferrite_core::vox::MIC_GAIN_MAX),
            #[cfg(feature = "tx")]
            RadioCmd::TxTimeout(s) => self.tx.timeout_s = s.clamp(30, 600),
            #[cfg(not(feature = "tx"))]
            RadioCmd::MicGain(_) | RadioCmd::TxTimeout(_) => {}
            #[cfg(feature = "tx")]
            RadioCmd::Vox { level, tail } => {
                self.tx.vox = (level.min(9), tail.max(1));
                if level == 0 {
                    self.tx.vox_det = ferrite_core::vox::Vox::new();
                    self.live.tx.voice = false;
                }
            }
            #[cfg(feature = "tx")]
            RadioCmd::VoxScreen(ok) => self.tx.vox_screen = ok,
            // Nothing keys in this build, so VOX has nothing to do.
            #[cfg(not(feature = "tx"))]
            RadioCmd::Vox { .. } | RadioCmd::VoxScreen(_) => {}
            // SND-12, from the input task: as stock, PE1 follows the speaker mic and PB8 goes on with it. Removed,
            // PB8 stays as it is (it may carry receive or repeat audio now) until the next speaker change clears it.
            RadioCmd::Accessory(on) => {
                ACC_ROUTE.write(on);
                if on {
                    AMP_EN[0].write(true);
                }
            }
            RadioCmd::BatterySave(level) => {
                let a = self.save.set_level(level, now_ms());
                self.apply(a).await;
            }
            RadioCmd::LowPower(on) => {
                let a = self.save.set_cycle(on.then_some(ferrite_core::rxsleep::LOW_POWER), now_ms());
                self.apply(a).await;
            }
            RadioCmd::Activity => {
                let a = self.save.activity(now_ms());
                self.apply(a).await;
            }
            RadioCmd::Squelch(l) => self.param(self.active, Param::Squelch, l as i32),
            RadioCmd::Broadcast(BroadcastCmd::Seek { up }) => self.bcast_seek(up).await,
            RadioCmd::Broadcast(b) => {
                self.seek_cancel(); // a station, AM or off ends a seek
                // Errors: the UI shows no station. The cable log (TERM-28) says what the SI4732 did.
                let r = broadcast(&mut self.si, b, self.si_opts).await;
                // BCR-2, BCR-5: FM's live report (frequency, signal, stereo, RDS) from here on; the tick polls it.
                self.fm = match (b, &r) {
                    (BroadcastCmd::Off, _) | (BroadcastCmd::Am(_), _) | (_, Err(_)) => None,
                    (_, Ok(f)) => Some(FmRx { freq: *f, rds: si4732::rds::Rds::init(), poll_at: 0, rds_turn: false }),
                };
                // A new station (or none): nothing of the last one's RDS carries over.
                if matches!(self.live.mode, ModeLive::Fm { .. }) {
                    self.live.mode = ModeLive::Off;
                }
                match (b, r.is_ok()) {
                    (BroadcastCmd::Off, _) => log(b"si off", 0),
                    (BroadcastCmd::Fm(f), true) => log(b"si fm ok, 10 khz ", f as u32),
                    (BroadcastCmd::Am(k), true) => log(b"si am ok, khz ", k as u32),
                    (BroadcastCmd::Seek { .. }, true) => {} // its own arm above
                    (_, false) => log(b"si FAILED, cmd ", match b { BroadcastCmd::Fm(_) => 1, BroadcastCmd::Am(_) => 2, _ => 3 }),
                }
                if r.is_ok() && b != BroadcastCmd::Off {
                    if let Ok(q) = self.si.rsq(&mut BusyDelay) {
                        log(b"si rssi dbuv ", q.rssi as u32);
                        log(b"si snr db ", q.snr as u32);
                    }
                }
            }
            RadioCmd::BcastSlot(s) => self.bcast_slot = s,
            RadioCmd::Hf { freq, mode, on, keep_rx } => self.hf(freq, mode, on, keep_rx).await,
            RadioCmd::HfSeek { up } => self.hf_seek(up),
            RadioCmd::SiOpts(o) => {
                // BCR-32, HF-35: kept for the next power-up, and applied now if the SI4732 runs
                if o != self.si_opts {
                    self.si_opts = o;
                    if si_apply(&mut self.si, o).is_err() {
                        log(b"si opts FAILED", 0);
                    }
                }
            }
            RadioCmd::Shutdown => {
                // TX-33: power off while keyed (a pass of the repeater too) unkeys first: PA drive 0, the chip out
                // of transmit, the unkey wait, every select off, before any chip sleeps or the SI4732 is touched
                // (TX audit M6). Stock unkeys nothing at power off (PE0, the save, then PA11 cleared).
                // Each step in the cable log at its time (ms since boot, as the UI's "power off" lines).
                log(b"shutdown: start at ms ", now_ms() as u32);
                #[cfg(feature = "tx")]
                if self.path.keyed().is_some() || self.tx.seq.keyed() {
                    let mut s = Steps::default();
                    self.tx.seq.safe(&mut self.path, now_ms() as u32, &mut s);
                    self.run(&s).await;
                    self.unkeyed();
                    log(b"shutdown: unkeyed at ms ", now_ms() as u32);
                }
                for n in 0..2u8 {
                    self.tune_chip(n, None).await;
                }
                self.seek_cancel();
                let _ = broadcast(&mut self.si, BroadcastCmd::Off, self.si_opts).await;
                let mut s = Steps::default();
                self.path.safe(now_ms() as u32, &mut s);
                self.run(&s).await;
                speaker(false);
                log(b"shutdown: chips off at ms ", now_ms() as u32);
            }
            RadioCmd::Counter(on) => self.counter(on).await,
            RadioCmd::Lease => {} // MODE-3: renewed above (`Lease::note`)
            RadioCmd::Same(on) => self.same_on = on,
            RadioCmd::DtmfRx(on) => self.dtmf_on = on,
            RadioCmd::Beep { hz, ms } => self.beep(hz, ms).await,
            RadioCmd::Chime { sound, level } => self.chime(sound, level).await,
            RadioCmd::BootSound { at } => self.boot_sound(at).await,
            #[cfg(feature = "tx")]
            RadioCmd::ToneBurst { on } => self.keyed_tone(if on { Some(1750) } else { None }, None),
            #[cfg(feature = "tx")]
            RadioCmd::Dtmf { key, on } => self.keyed_tone(None, if on { tonegen::dtmf_symbol(key) } else { None }),
            // Without the `tx` build nothing is keyed, so there is nothing to send them on.
            #[cfg(not(feature = "tx"))]
            RadioCmd::ToneBurst { .. } | RadioCmd::Dtmf { .. } => {}
            #[cfg(feature = "tx")]
            RadioCmd::TxTones { phase, cw, sidetone } => (self.tx.phase, self.tx.cw, self.tx.sidetone) = (phase.min(3), cw.min(2), sidetone),
            #[cfg(feature = "tx")]
            RadioCmd::CwCall { chunk, text } => {
                let at = (chunk as usize).min(1) * 5;
                self.tx.cw_call[at..at + 5].copy_from_slice(&text);
                self.tx.cw_len = self.tx.cw_call.iter().position(|&c| c == 0).unwrap_or(10) as u8;
            }
            #[cfg(feature = "tx")]
            RadioCmd::DtmfTiming { ms, delay_10ms } => self.tx.dtmf = (ms.clamp(50, 200), delay_10ms.min(100)),
            #[cfg(feature = "tx")]
            RadioCmd::DtmfSeq { chunk, keys } => {
                let at = (chunk as usize).min(3) * 5;
                let n = (16 - at).min(5);
                self.tx.digits[at..at + n].copy_from_slice(&keys[..n]);
            }
            #[cfg(feature = "tx")]
            RadioCmd::DtmfSend { len } => self.dial(len),
            // Nothing transmits in this build.
            #[cfg(not(feature = "tx"))]
            RadioCmd::TxTones { .. }
            | RadioCmd::CwCall { .. }
            | RadioCmd::DtmfTiming { .. }
            | RadioCmd::DtmfSeq { .. }
            | RadioCmd::DtmfSend { .. } => {}
            // Not on the radio yet, each waiting on something named in the manual's Status lines: band scope,
            // tone search, crossband (chapters 21, 22, 19): their own exclusive modes, after 2d.
            RadioCmd::ScopeCfg { bars, dwell, strip, hf, si } => self.scope_cfg = (bars, dwell, strip, hf, si),
            RadioCmd::Sweep { centre, step, on } => {
                self.sweep_hold.clear();
                self.scope(centre, step, on).await
            }
            // SCOPE-14: the pass starts again from its first point once the hold is over.
            RadioCmd::SweepHold => {
                let now = now_ms() as u32;
                if let Some(sc) = self.scope.as_mut().filter(|sc| sc.listen == 0) {
                    if self.sweep_hold.input(now) {
                        (sc.i, sc.started, sc.look, sc.si_tune_at) = (0, now, 0, None);
                        self.scope_point();
                    }
                }
            }
            RadioCmd::ScopeListen { freq, mode } => self.scope_listen(freq, mode).await,
            // Tone search: its own exclusive mode (TSRCH-3); until Oct 15, 2026 this dropped it.
            RadioCmd::ToneSearch { slot, on } => self.tone_search(slot, on),
            #[cfg(feature = "tx")]
            RadioCmd::Crossband(c) => self.crossband(c).await,
            // XB-2: no transmit in this build; the UI refuses first, and the radio says so too.
            #[cfg(not(feature = "tx"))]
            RadioCmd::Crossband(c) => {
                let stopped = ferrite_core::xb::Stop::NoTx as u8;
                self.live.mode = if c.is_some() { ModeLive::Xb(ferrite_core::msg::XbLive { stopped, ..Default::default() }) } else { ModeLive::Off };
            }
            #[cfg(feature = "tx")]
            RadioCmd::TxPlan { tx, region, extend, confirmed } => {
                let r = Region::ALL.get(region as usize).copied().unwrap_or(Region::Other);
                self.tx.plan = Some((tx, r, extend, confirmed));
                self.tx.region = Some((r, extend));
            }
            RadioCmd::SatRx(f) => self.sat_rx(f).await,
            RadioCmd::SatUp(f) => self.sat_up(f).await,
            RadioCmd::SatTone { tone, hd, power } => self.sat_tone(tone, hd, power),
            #[cfg(feature = "tx")]
            RadioCmd::Transmit { slot: sat::SAT_SLOT, on } if self.sat.is_some() => {
                if on {
                    self.sat_key().await;
                } else {
                    self.sat_unkey().await;
                }
            }
            #[cfg(feature = "tx")]
            RadioCmd::Transmit { slot, on } => {
                // XB-14: local PTT interrupts a pass (unkeyed first) and holds the repeater until it's released.
                if on {
                    self.xb_hold(true).await;
                }
                self.transmit(slot, on, None).await;
                if !on {
                    self.xb_hold(false).await;
                }
            }
            #[cfg(feature = "tx")]
            RadioCmd::TxCut { .. } => {
                // LINK-20, TX-47: a remote key-up lost: unkeyed as a cut, the carrier dropped at once.
                self.transmit_cut().await;
                self.xb_hold(false).await;
            }
            #[cfg(feature = "tx")]
            RadioCmd::TransmitData { slot, power, on } => self.transmit(slot, on, Some(power)).await,
            // Transmit code is out of this build: the UI refuses first (RX-only build).
            #[cfg(not(feature = "tx"))]
            RadioCmd::Transmit { .. } | RadioCmd::TxCut { .. } | RadioCmd::TransmitData { .. } | RadioCmd::TxPlan { .. } => {}
            RadioCmd::Tune(_) => {}
        }
    }
}
