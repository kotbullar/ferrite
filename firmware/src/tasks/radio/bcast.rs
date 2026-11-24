//! The broadcast receiver's audio, FM report and watch mask.

use super::*;

impl Radio {
    /// BCR-6: in FM or AM broadcast the SI4732 is the source while no slot is heard (radio monitor); a heard slot
    /// mutes it and plays the BK4829 instead, and the broadcast returns 2 s after it goes quiet (BCR-24). A
    /// broadcast slot muted with \* stays muted (HOME-15). Then the speaker for whichever plays. Without this the
    /// amp stayed off in FM broadcast on the radio (Oct 12, 2026): only the BK4829s were ever counted as a source.
    /// Also run while Battery Save has the BK4829s asleep (BCR-19), so the broadcast comes back on time.
    pub(super) fn bcast_audio(&mut self, now: u32, slot_heard: bool) {
        let broadcast = self.si.mode().is_some() && self.hf.is_none() && !self.scope.as_ref().is_some_and(ScopeRun::uses_si);
        if !broadcast && self.ldo_kept != 0 {
            self.ldo_release(); // PWR-21.6: the supplies kept on for the broadcast go off with it
        }
        let turn = self.bturn.tick(now, slot_heard);
        let muted = self.bcast_slot.is_some_and(|b| (self.user_mute | self.user_off) & 1 << b != 0); // HOME-15.1: off too
        let si_heard = broadcast && turn && !muted;
        if self.si_heard != si_heard {
            if broadcast {
                let _ = self.si.mute(!si_heard, &mut BusyDelay);
            }
            self.si_heard = si_heard;
            self.live.bcast_heard = si_heard;
        }
        speaker((slot_heard || si_heard) && !self.muted);
    }

    /// HF mode's report: frequency, signal and which receiver (ModeLive::Hf).
    /// BCR-2, BCR-5: FM broadcast's report ten times a second (the signal, stereo pilot and SNR: about 2 ms of I2C
    /// each) and, every other time, up to 8 RDS groups into the decoder (station name, radio text, programme type). Nothing in HF
    /// mode or with FM off.
    #[inline(never)] // in slow flash (memory.x): ten times a second at most
    pub(super) fn fm_poll(&mut self) {
        let now = now_ms() as u32;
        let Some(fm) = self.fm.as_mut() else { return };
        // MODE-5: home's strip sweeping beside the broadcast has the mode report (nothing shows the RDS meanwhile).
        if self.hf.is_some() || self.scope.is_some() || self.seek.is_some() || now.wrapping_sub(fm.poll_at) > u32::MAX / 2 {
            return;
        }
        fm.poll_at = now.wrapping_add(100);
        fm.rds_turn = !fm.rds_turn;
        let d = &mut BusyDelay;
        let q = self.si.rsq(d).ok();
        for _ in 0..if fm.rds_turn { 8 } else { 0 } {
            match self.si.rds_read(d) {
                Ok(Some(g)) => fm.rds.feed(g.blocks[0], g.blocks[1], g.blocks[2], g.blocks[3], g.errors),
                _ => break,
            }
        }
        // BCR-30: the radio text whole, once complete; until then (and through an A/B change) the last one shown.
        let rt = match self.live.mode {
            _ if fm.rds.rt_complete() => fm.rds.rt,
            ModeLive::Fm { rt, .. } => rt,
            _ => [0; 64],
        };
        self.live.mode = ModeLive::Fm {
            freq: fm.freq,
            ps: fm.rds.ps,
            rt,
            rssi: q.map_or(0, |q| q.rssi),
            snr: q.map_or(0, |q| q.snr),
            stereo: q.is_some_and(|q| q.stereo),
            pty: fm.rds.pty, // BCR-26.8
            heard: self.si_heard,
        };
    }

    /// BCR-23: while FM broadcast plays no chip listens to the broadcast slot, nor to a slot needing the band relay,
    /// which FM leaves released (BCR-15). In slow flash (memory.x): once a receive tick.
    #[inline(never)]
    pub(super) fn bcast_off(&self) -> u8 {
        if self.fm.is_none() {
            return 0;
        }
        let relay = (0..SLOTS).filter(|&i| Sel::rx(self.slots[i].freq).relay()).fold(0u8, |m, i| m | 1 << i);
        relay | self.bcast_slot.map_or(0, |s| 1 << s)
    }
}
