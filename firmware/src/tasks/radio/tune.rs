//! Tuning the chips, the receive path select, the path plans and Battery Save; slot parameters and scans.

use super::*;

/// The fast scan's own wake (SCAN-21, decision 164) is off: see `scan_due`.
const SCAN_WAKE_OFF: bool = true;

/// PWR-21.6: while FM broadcast plays, a BK4829 put to sleep keeps its REG_37 supplies on (REG_30 off only), so the
/// watch hop's and Battery Save's sleeps and wakes don't click in the broadcast; false puts back PWR-5's full sleep
/// everywhere. The supplies go off once the broadcast stops ([`Radio::ldo_release`]).
const BCAST_LDO_ON: bool = true;

impl Radio {
    /// Chip `n`, every register write through its shadow (PWR-26.16).
    pub(super) fn chip(&mut self, n: u8) -> Bk4829<super::shadow::Shadowed<'_, bk4829::wire::Chip<'_, Out, Io, BusyDelay, Out>>> {
        Bk4829::new(super::shadow::Shadowed { bus: self.wire.chip(&mut self.cs[n as usize]), shadow: &mut self.shadow[n as usize] })
    }

    pub(super) fn has(&self, n: u8) -> bool {
        self.chips & 1 << n != 0
    }

    /// Carry out a path plan, waits included, before anything else reaches the pins (`ferrite_radio::run`,
    /// host-tested on a recording mock through [`ferrite_radio::Hw`] below).
    pub(super) async fn run(&mut self, steps: &Steps) {
        ferrite_radio::run(self, steps).await;
        if self.path.take_alarm() {
            // SAFE-22: the event log's "Relay switching too often".
            let mut e = Event::new(Kind::Fault, Source::Radio);
            e.payload = *b"relay switch";
            EVENTS.immediate_publisher().publish_immediate(e);
        }
    }

    /// The receive path for `f`. `ActiveSlot` (tuning or switching the active slot) may move the relay
    /// (SAFE-20); `Auto` (watch, scan, the hop) never does and is refused instead.
    pub(super) async fn select_rx(&mut self, f: Freq10, cause: Cause) {
        let want = Rx::Bk(Sel::rx(f));
        if self.path.rx_now() == want {
            return;
        }
        let mut s = Steps::default();
        let _ = self.path.rx(want, cause, now_ms() as u32, &mut s); // refused: stays as it is
        self.run(&s).await;
    }

    /// Tune chip `n` to a slot (or sleep it): only when it changes. Waking and sleeping take PWR-21's settle
    /// waits on a timer, so other tasks run meanwhile: the two halves are plain functions around the one wait
    /// (`ferrite_radio::chip::tune_settled` and `sleep_settled` are the same steps, host-tested).
    pub(super) async fn tune_chip(&mut self, n: u8, want: Option<(u8, Freq10)>) {
        #[cfg(feature = "bench")]
        let c0 = crate::hal::time::cycles();
        if let Some(ms) = self.tune_start(n, want) {
            if ms > 0 {
                Timer::after_millis(ms as u64).await;
            }
            self.tune_finish(n);
            // WATCH-17.4: how long a retune to a frequency took, its settle wait included
            #[cfg(feature = "bench")]
            if want.is_some() {
                self.turns.tune_took(crate::hal::time::cycles().wrapping_sub(c0) / crate::hal::clock::mhz());
            }
        }
    }

    /// SQL-17 (decision 166): chip `n`'s squelch open delay, 0 while it listens to a scan step or the strip's look
    /// (`fast`), stock's 7 for ordinary receive; written only when it changes.
    pub(super) fn open_delay(&mut self, n: u8, fast: bool) {
        if self.has(n) && self.open_fast[n as usize] != Some(fast) {
            self.open_fast[n as usize] = Some(fast);
            radio_chip::open_delay(&mut self.chip(n), fast);
        }
    }

    /// [`Radio::tune_chip`] up to the settle wait: None if nothing changes, else the wait. Asleep: muted and REG_30
    /// off (`chip::sleep_begin`), the wait is the sleep settle. Awake: REG_37's supplies on unless they are on
    /// already (woken a tick early by the hop, or awake), the wait is what is left of the wake settle.
    #[inline(never)]
    pub(super) fn tune_start(&mut self, n: u8, want: Option<(u8, Freq10)>) -> Option<u32> {
        let asleep = self.maybe_awake & 1 << n == 0;
        if !self.has(n) || self.tuned[n as usize] == want && (want.is_some() || asleep) {
            return None;
        }
        // A probe on the slot this chip had, or gets, ends: tuning sets the slot's own detector (TSRCH-12).
        for (s, _) in [self.tuned[n as usize], want].into_iter().flatten() {
            if let Some(h) = self.hints.get_mut(s as usize) {
                h.retuned();
            }
        }
        self.tuned[n as usize] = want;
        #[cfg(feature = "bench")]
        self.turns.tuned(now_ms() as u32, n, want); // WATCH-17.1, WATCH-17.4: what the chip actually did
        if want.is_none() {
            self.maybe_awake &= !(1 << n);
            self.sq_regs[n as usize] = None; // written again on waking
            self.sq_key[n as usize] = None; // SQL-18.6: and its soft squelch starts closed, not as it was before
            self.open_fast[n as usize] = None; // SQL-17: likewise
            self.data_rx[n as usize] = None; // APRS-55: likewise
            radio_chip::sleep_begin(&mut self.chip(n));
            // PWR-21.6: beside a broadcast the supplies stay on (no settle to wait: REG_37 isn't written).
            if BCAST_LDO_ON && self.woke[n as usize].is_some() && self.si.mode().is_some() && self.hf.is_none() {
                self.ldo_kept |= 1 << n;
                return Some(0);
            }
            self.woke[n as usize] = None;
            return Some(self.settle.sleep_ms as u32);
        }
        self.ldo_kept &= !(1 << n);
        self.maybe_awake |= 1 << n;
        let (settle, woke) = (self.settle, self.woke[n as usize]);
        let (at, left) = radio_chip::wake_start(&mut self.chip(n), settle, woke, now_ms() as u32);
        self.woke[n as usize] = Some(at);
        Some(left)
    }

    pub(super) fn apply_rf_gain(&mut self, n: u8) {
        if self.rf_gain_pending & (1 << n) != 0 {
            let fixed = self.rf_gain.checked_sub(1);
            self.chip(n).rx_gain(fixed);
            self.rf_gain_pending &= !(1 << n);
        }
    }

    /// [`Radio::tune_chip`] after the settle wait: REG_37's supplies off for a chip going to sleep; for one tuned,
    /// the receive set-up and receive on, then its squelch thresholds.
    #[inline(never)]
    fn tune_finish(&mut self, n: u8) {
        let Some((s, f)) = self.tuned[n as usize] else {
            if self.ldo_kept & 1 << n == 0 {
                self.chip(n).ldo_sleep();
            }
            return;
        };
        self.apply_rf_gain(n);
        let slot = match s {
            s if s < SLOTS as u8 => self.slots[s as usize],
            SCOPE_SLOT => {
                let m = self.scope.as_ref().map_or(0, |sc| sc.mode);
                SlotRx { freq: f, am: m == 2, narrow: m == 1, ..SlotRx::DEFAULT }
            }
            // Satellite mode's downlink (SAT-8): wide FM, the default squelch.
            sat::SAT_TUNED => SlotRx { freq: f, ..SlotRx::DEFAULT },
            // HF mode on chip 1 (18-64 MHz): AM is the mode's own (Param::Mode 2); FM otherwise.
            _ => SlotRx { freq: f, am: self.hf.is_some_and(|h| h.mode == 2), ..SlotRx::DEFAULT },
        };
        // Stock's modes 6 and 7, the sub-tone detector, muted until the audio decision (`ferrite_radio::chip`).
        // CAL-12: the chip tunes the word with the factory crystal trim; everything else keeps `f`.
        // APRS-55: the APRS slot's FM receive gets the data set-up; any other tune puts the voice one back.
        let data = (s as usize) < SLOTS && self.aprs_slots & 1 << s != 0 && !slot.am;
        let setup = RxSetup { freq: self.sqcal.rx_word(f), narrow: slot.narrow, am: slot.am, rx_tone: slot.rx_tone, rx_tail: slot.rx_tail, data };
        // SQL-1: stock's thresholds for this frequency and the slot's level (stock writes them at each receive
        // setup); only when they differ from what the chip has; before receive on (SQL-18.9).
        let regs = self.sqcal.regs(f, slot.sq);
        let sq = (self.sq_regs[n as usize] != Some(regs)).then_some(regs);
        self.sq_regs[n as usize] = Some(regs);
        radio_chip::tune_rest_sq(&mut self.chip(n), &setup, sq);
        if self.data_rx[n as usize] != Some(data) {
            radio_chip::rx_data(&mut self.chip(n), data);
            self.data_rx[n as usize] = Some(data);
        }
        // TERM-42: the bench's receive AF filter override over the FM tune's REG_2B (APRS-55's bits too: it wins).
        #[cfg(feature = "bench")]
        if let Some(bits) = self.rxaf.filter(|_| !slot.am) {
            radio_chip::rx_af_override(&mut self.chip(n), bits);
        }
        // TERM-42.2: the bench's receive IF override over the tune's REG_43 (`tx_setup` rewrites it; the unkey's
        // retune puts it back).
        #[cfg(feature = "bench")]
        if let Some(v) = self.rxif.filter(|_| !slot.am) {
            self.chip(n).write(bk4829::reg::RF_FILTER, v);
        }
        // TERM-42.3: the bench's held registers, last (over APRS-55's gate and AFC too).
        #[cfg(feature = "bench")]
        if !slot.am {
            let holds = self.holds;
            for (r, v) in holds.into_iter().filter(|h| h.0 != 0) {
                self.chip(n).write(r, v);
            }
        }
    }

    /// PWR-21: chip `n`'s REG_37 supplies on now if they are off, without receive (the hop's early wake, and a
    /// key-up's chip); the wake settle counts from here.
    pub(super) fn ldo_on(&mut self, n: u8) {
        self.ldo_kept &= !(1 << n);
        if self.woke[n as usize].is_none() {
            self.chip(n).wake();
            self.woke[n as usize] = Some(now_ms() as u32);
        }
        self.maybe_awake |= 1 << n;
    }

    /// SCAN-21 (decision 189): the slots running a range scan's fast pass (not a channel scan): the watch keeps
    /// their chip on them and the receive path with them but for the other chip's short turns.
    pub(super) fn range_scans(&self) -> u8 {
        (0..SLOTS).filter(|&i| self.scans[i].running() && self.scans[i].fast && !self.chans.is_on(i)).fold(0u8, |m, i| m | 1 << i)
    }

    /// PWR-21.6: the broadcast has stopped, so the chips asleep with their supplies kept on get PWR-5's full sleep
    /// (REG_37's supplies off; REG_30 went off when they slept).
    pub(super) fn ldo_release(&mut self) {
        for n in 0..2u8 {
            if self.ldo_kept & 1 << n != 0 && self.tuned[n as usize].is_none() {
                self.chip(n).ldo_sleep();
                self.woke[n as usize] = None;
            }
        }
        self.ldo_kept = 0;
    }

    /// Battery Save's phase change: both chips sleep together (WATCH-8), but for one a kept slot has (APRS mode's
    /// C, PWR-4: its decoder runs); waking, the next tick tunes them.
    pub(super) async fn apply(&mut self, a: Action) {
        match a {
            Action::None => return,
            // TX-29 (TX audit F12): never while keyed, whatever asks; the unkey's wait is inside its plan.
            Action::Sleep if !self.path.may_sleep() => {}
            Action::Sleep => {
                // BCR-19: only the BK4829s sleep; a broadcast playing keeps the SI4732 and the speaker.
                speaker(self.si_heard && !self.muted);
                let kept = self.kept();
                for n in 0..2u8 {
                    if !self.tuned[n as usize].is_some_and(|(s, _)| (s as usize) < SLOTS && kept & 1 << s != 0) {
                        self.tune_chip(n, None).await;
                    }
                }
                for (i, s) in self.live.slots.iter_mut().enumerate() {
                    s.squelch_open &= kept & 1 << i != 0;
                    s.carrier &= kept & 1 << i != 0;
                }
                self.carrier &= kept;
                if self.tuned.iter().all(Option::is_none) {
                    // BCR-19: during a broadcast the band's receive select stays on (switching it clicks in the
                    // shared amp); otherwise off, as stock.
                    let broadcast = self.si.mode().is_some() && self.hf.is_none();
                    let mut s = Steps::default();
                    let _ = self.path.rx_off(broadcast, &mut s); // refused only while keyed (TX-29)
                    self.run(&s).await;
                }
            }
            Action::Wake => {}
        }
        self.live.rx_asleep = self.save.asleep();
    }

    /// The kept slots that are listened to (decision 111): their chip stays awake beside the hop's, and through
    /// Battery Save's sleep.
    pub(super) fn kept(&self) -> u8 {
        self.keep & (self.watch_set | 1 << self.active) & !self.bcast_off()
    }

    /// SCAN-21: the soonest end of a fast scan step on a slot with a chip (`now_ms` time), for the main loop to
    /// tick on it rather than on the next 10 ms tick (`Scan::due`). The step's 15 ms counts from the tick that
    /// tuned it, so a step takes 15 ms plus that tick's retune and reads: about 17 ms, 55-60 steps a second.
    /// None while the tick doesn't step scans (HF mode, Battery Save asleep with no kept slot, the scope, the
    /// counter, keyed, the crossband repeater): a due time it never moves would go past and wake the task every
    /// 1 ms (decision 163). The main loop also ignores a due time the last tick didn't move whatever the reason
    /// (`ferrite_core::scan::tick_wake`, decision 165): these returns are the first guard, not the only one.
    pub(super) fn scan_due(&self, now: u64) -> Option<u64> {
        // Switched off (Oct 15, 2026): with this wake the radio hung in a range scan on two builds (no
        // key, no link, a power cycle needed); a build without it ran the same test (27.2 steps/s). Back to the 10 ms
        // tick until the hang is understood (SCAN-21 stays open).
        if SCAN_WAKE_OFF {
            return None;
        }
        #[cfg(feature = "tx")]
        if self.xb.is_some() {
            return None;
        }
        if self.hf.is_some() || self.save.asleep() && self.kept() == 0 {
            return None;
        }
        if self.scope.is_some() || self.counter.is_some() || self.path.keyed().is_some() {
            return None;
        }
        let tuned = |i: usize| self.tuned.iter().any(|t| t.is_some_and(|(s, _)| s as usize == i));
        (0..SLOTS)
            .filter(|&i| tuned(i))
            .filter_map(|i| self.scans[i].due())
            .map(|d| {
                let left = d.wrapping_sub(now as u32);
                now + if left > u32::MAX / 2 { 0 } else { left as u64 }
            })
            .min()
    }

    /// Something running that keeps the engines ticking fast: a slot sharing the chips, or a scan.
    pub(super) fn busy(&self) -> bool {
        let set = self.watch_set | 1 << self.active;
        let xb = {
            #[cfg(feature = "tx")]
            {
                self.xb.is_some()
            }
            #[cfg(not(feature = "tx"))]
            {
                false
            }
        };
        // TERM-43: the FSK modem test reads its FIFO every tick (8 words fill in 53 ms at 2400 baud).
        #[cfg(feature = "bench")]
        let xb = xb || self.fsk.is_some();
        // APRS-56.5: the APRS slot's squelch read every tick (10 ms), not every poll (50 ms): a packet's flags may last
        // only 100 ms
        set.count_ones() > 1 || self.aprs_slots != 0 || self.scans.iter().any(|s| s.running()) || self.path.keyed().is_some() || self.counter.is_some() || self.scope.is_some() || self.seek.is_some() || xb
    }

    /// A slot's setting from the UI; the chip it is on retunes on the next tick.
    pub(super) fn param(&mut self, slot: u8, param: Param, value: i32) {
        let Some(s) = self.slots.get_mut(slot as usize) else { return };
        match param {
            Param::Mode => (s.narrow, s.am) = (value == 1, value == 2),
            Param::Bandwidth => s.narrow = value == 1,
            Param::Squelch => {
                // SQL-7.2: the new thresholds straight to the chip holding the slot, without a retune: the audio
                // carries on, and the soft squelch keeps its state with the new limits from the next tick (a review
                // of Nov 20, 2026: each knob step retuned the chip and dropped the audio). A chip asleep gets them as
                // it wakes, from tune_finish.
                s.sq = value.clamp(0, 9) as u8;
                let sq = s.sq;
                for n in 0..2u8 {
                    if let Some((ts, f)) = self.tuned[n as usize] {
                        if ts == slot && f != 0 {
                            let regs = self.sqcal.regs(f, sq);
                            if self.sq_regs[n as usize] != Some(regs) {
                                radio_chip::squelch(&mut self.chip(n), regs);
                                self.sq_regs[n as usize] = Some(regs);
                            }
                        }
                    }
                }
                return;
            }
            Param::RxTone => s.rx_tone = value as u16,
            Param::Clarifier => s.clar = value.clamp(-2_500, 2_500) as i16,
            #[cfg(feature = "tx")]
            Param::TxTone => return self.tx.tone[slot as usize % 4] = value as u16,
            #[cfg(feature = "tx")]
            Param::Power => return self.tx.power[slot as usize % 4] = value.clamp(0, 2) as u8,
            #[cfg(feature = "tx")]
            Param::EndTone => return self.tx.end[slot as usize % 4] = (value as u8).min(tonegen::END_TONES.len() as u8 - 1),
            Param::Tail => {
                s.rx_tail = value & 1 != 0;
                #[cfg(feature = "tx")]
                {
                    self.tx.tail[slot as usize % 4] = value & 2 != 0;
                }
            }
            #[cfg(not(feature = "tx"))]
            Param::TxTone | Param::Power | Param::EndTone => return,
            // TX audit F11: kept for the key-up's own refusals (TX-21, CHAN-4); nothing to retune.
            Param::BusyLock => return s.busy = value != 0,
            Param::RxOnly => return s.rx_only = value != 0,
            // The transmit frequency comes with each key-up (RadioCmd::TxPlan).
            Param::Offset | Param::Reverse | Param::Split => return,
        }
        self.retune(slot);
    }

    /// Whichever chip has `slot` tunes it again on the next tick.
    pub(super) fn retune(&mut self, slot: u8) {
        for t in &mut self.tuned {
            if t.is_some_and(|(s, _)| s == slot) {
                *t = Some((slot, 0)); // a frequency no slot has: the next tick's tune_chip sees a change
            }
        }
    }

    /// SCAN-27: one part of a range preset for `slot`'s next scan ([`RadioCmd::ScanRangeSet`]).
    pub(super) fn scan_range_set(&mut self, slot: u8, what: u8, f: Freq10) {
        use ferrite_core::msg::{RANGE_HI, RANGE_LO, RANGE_SKIP, RANGE_SKIPS, RANGE_STEP};
        let i = slot as usize % SLOTS;
        match what {
            RANGE_LO => {
                self.ranges[i] = Range { lo: f, hi: f, step: 1, skipped: &[] };
                self.preset_arm |= 1 << i;
                (self.preset_slot, self.preset_n) = (i as u8, 0);
            }
            RANGE_HI => self.ranges[i].hi = f.max(self.ranges[i].lo),
            RANGE_STEP => self.ranges[i].step = f.max(1),
            RANGE_SKIP if self.preset_slot as usize == i && (self.preset_n as usize) < RANGE_SKIPS => {
                self.preset_skips[self.preset_n as usize] = f;
                self.preset_n += 1;
            }
            _ => {}
        }
    }

    /// A scan on `slot`: its band from where it is, or the range preset set for it, the fast pass (SCAN-21).
    pub(super) fn scan(&mut self, slot: u8, on: bool) {
        let i = slot as usize % SLOTS;
        let sl = &mut self.live.slots[i];
        sl.scanning = on;
        if on {
            (sl.hits, sl.skips) = (0, 0);
            // SCAN-27: a range preset set for this scan, or the band it's in (a band scan drops a preset's skips)
            let r = if self.preset_arm & 1 << i != 0 {
                self.preset_arm &= !(1 << i);
                self.ranges[i]
            } else {
                if self.preset_slot as usize == i {
                    self.preset_n = 0;
                }
                scan::band_range(self.slots[i].freq)
            };
            self.ranges[i] = r;
            let sc = &mut self.scans[i];
            sc.fast = true; // the resume rule is the last ScanResume's (SCAN-5, SCAN-24)
            sc.start(scan::range_pos(&r, self.slots[i].freq), now_ms() as u32);
            log(b"scan start slot ", i as u32);
        } else {
            self.scans[i].stop();
        }
        self.chans.off(i); // a band scan, or none
    }

    /// A scan's answer to ▲▼ or \*: the step it tuned, the end.
    pub(super) fn scan_out(&mut self, i: usize, o: scan::Out) {
        let sl = &mut self.live.slots[i];
        if let Some(f) = o.tune {
            self.slots[i].freq = f;
            sl.freq = f;
        }
        sl.skips = self.scans[i].skips.max(sl.skips);
        if o.ended {
            sl.scanning = false;
            self.chans.off(i);
        }
    }
}
