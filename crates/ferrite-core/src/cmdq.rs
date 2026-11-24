//! MODE-6: the UI's queue of commands for the Radio task. The UI never waits for the Radio task: it hands each
//! command over only when the Radio task's channel has room, and keeps the rest here, in order, until then.
//!
//! - Order is kept: commands leave in the order they came, and nothing jumps ahead (a key-up's plan and its
//!   slot's settings go before it, as the UI sent them; an unkey goes after the key-up).
//! - A newer setting replaces an older one of the same kind still waiting ([`key`]: a slot's frequency, one of its
//!   settings, the active slot, the watch set ...), but only while no other command lies between them, so the
//!   replacement never moves a setting across a transmission, a mode change or a scan command.
//! - Full (the Radio task busy for long), the oldest command that only says "the UI is alive" goes first
//!   ([`Class::Drop`]: activity, lease renewals, the sweep hold, a key beep), then the oldest plain setting; a
//!   transmit, unkey, power-off or mode command ([`Class::Keep`]) is the last to go. With the Radio task taking
//!   commands at least every half second (MODE-7) the queue never gets there.

use crate::msg::RadioCmd;

/// How the queue treats a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Only a sign of life or a courtesy: dropped first when full.
    Drop,
    /// A setting whose newer value supersedes the older (its [`key`]).
    State,
    /// Everything else: transmit, unkey, power off, modes, scans, tones. Never replaced, never moved, and dropped
    /// only when the queue holds nothing else.
    Keep,
}

/// What the command is for the queue.
pub fn class(c: &RadioCmd) -> Class {
    match c {
        RadioCmd::Activity | RadioCmd::Lease | RadioCmd::SweepHold | RadioCmd::Beep { .. } | RadioCmd::Chime { .. } | RadioCmd::BootSound { .. } => Class::Drop,
        _ if key(c).is_some() => Class::State,
        _ => Class::Keep,
    }
}

/// The kind a newer command of which supersedes this one, if any. Transmit settings (power aside, which is a
/// slot setting the key-up's order already protects) are never merged: they stay as sent.
pub fn key(c: &RadioCmd) -> Option<u16> {
    Some(match *c {
        RadioCmd::Tune(_) => 1,
        RadioCmd::Squelch(_) => 2,
        RadioCmd::BatterySave(_) => 3,
        RadioCmd::Activity => 4,
        RadioCmd::Active(_) => 5,
        RadioCmd::Watch(_) => 6,
        RadioCmd::Awake(_) => 7,
        RadioCmd::ScanTx(_) => 8,
        RadioCmd::BeepVol(_) => 9,
        RadioCmd::Mute(_) => 10,
        RadioCmd::BcastSlot(_) => 11,
        RadioCmd::ScopeCfg { .. } => 12,
        RadioCmd::SweepHold => 13,
        RadioCmd::Lease => 14,
        RadioCmd::Same(_) => 15,
        RadioCmd::AprsSlot(_) => 16,
        RadioCmd::DtmfRx(_) => 17,
        RadioCmd::SlotOff(_) => 18,
        RadioCmd::RfGain(_) => 19,
        RadioCmd::TuneSlot { slot, .. } => 0x100 | slot as u16,
        RadioCmd::SlotParam { slot, param, .. } => 0x200 | (slot as u16) << 5 | param as u16,
        _ => return None,
    })
}

/// The queue: N commands, oldest first.
pub struct CmdRing<const N: usize> {
    q: [Option<RadioCmd>; N],
    len: u8,
}

impl<const N: usize> Default for CmdRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> CmdRing<N> {
    pub const fn new() -> Self {
        CmdRing { q: [None; N], len: 0 }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    /// Queues `c` at the end. A waiting command of the same [`key`] with only commands of other keys after it
    /// (none of them [`Class::Keep`]) is removed first. Returns the command dropped to make room, if one was.
    pub fn push(&mut self, c: RadioCmd) -> Option<RadioCmd> {
        if let Some(k) = key(&c) {
            for i in (0..self.len()).rev() {
                let Some(q) = self.q[i] else { break };
                if key(&q) == Some(k) {
                    self.remove(i);
                    break;
                }
                if class(&q) == Class::Keep {
                    break; // never past a transmission, a mode change, a scan command
                }
            }
        }
        let dropped = if self.len() == N {
            let at = [Class::Drop, Class::State, Class::Keep]
                .into_iter()
                .find_map(|cl| (0..N).find(|&i| self.q[i].is_some_and(|q| class(&q) == cl)))
                .unwrap_or(0);
            self.remove(at)
        } else {
            None
        };
        let n = self.len();
        self.q[n] = Some(c);
        self.len += 1;
        dropped
    }

    /// The oldest command, taken.
    pub fn pop(&mut self) -> Option<RadioCmd> {
        if self.len == 0 {
            return None;
        }
        self.remove(0)
    }

    fn remove(&mut self, i: usize) -> Option<RadioCmd> {
        let c = self.q[i].take();
        let n = self.len();
        self.q[i..n].rotate_left(1);
        self.len -= 1;
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::Param;
    use std::vec::Vec;

    fn drain<const N: usize>(r: &mut CmdRing<N>) -> Vec<RadioCmd> {
        core::iter::from_fn(|| r.pop()).collect()
    }

    /// MODE-6.1, MODE-6.2: order kept; a retune replaces the waiting one.
    #[test]
    fn order_kept_and_retunes_merge() {
        let mut r = CmdRing::<8>::new();
        r.push(RadioCmd::TuneSlot { slot: 0, freq: 100 });
        r.push(RadioCmd::Active(1));
        r.push(RadioCmd::TuneSlot { slot: 1, freq: 300 });
        r.push(RadioCmd::TuneSlot { slot: 0, freq: 200 });
        assert_eq!(drain(&mut r), [RadioCmd::Active(1), RadioCmd::TuneSlot { slot: 1, freq: 300 }, RadioCmd::TuneSlot { slot: 0, freq: 200 }]);
    }

    /// MODE-6.1, MODE-6.2: a setting never merges across a transmit command: the key-up keeps the power it was planned with.
    #[test]
    fn no_merge_across_transmit() {
        let mut r = CmdRing::<8>::new();
        let p = |v| RadioCmd::SlotParam { slot: 0, param: Param::Power, value: v };
        r.push(p(2));
        r.push(RadioCmd::Transmit { slot: 0, on: true });
        r.push(p(0));
        r.push(RadioCmd::Transmit { slot: 0, on: false });
        assert_eq!(drain(&mut r), [p(2), RadioCmd::Transmit { slot: 0, on: true }, p(0), RadioCmd::Transmit { slot: 0, on: false }]);
    }

    /// MODE-6.3: full, the signs of life go first, then settings; transmit, unkey and power off stay, in order.
    #[test]
    fn full_drops_least_important_first() {
        let mut r = CmdRing::<4>::new();
        r.push(RadioCmd::Transmit { slot: 0, on: true });
        r.push(RadioCmd::Activity);
        r.push(RadioCmd::Watch(3));
        r.push(RadioCmd::Transmit { slot: 0, on: false });
        assert_eq!(r.push(RadioCmd::Shutdown), Some(RadioCmd::Activity));
        assert_eq!(r.push(RadioCmd::TxCut { slot: 0 }), Some(RadioCmd::Watch(3)));
        assert_eq!(
            drain(&mut r),
            [RadioCmd::Transmit { slot: 0, on: true }, RadioCmd::Transmit { slot: 0, on: false }, RadioCmd::Shutdown, RadioCmd::TxCut { slot: 0 }]
        );
    }

    /// MODE-6.2: repeated signs of life take one place; seeks and HF commands are never merged.
    #[test]
    fn classes() {
        let mut r = CmdRing::<8>::new();
        for _ in 0..5 {
            r.push(RadioCmd::Activity);
            r.push(RadioCmd::Lease);
        }
        assert_eq!(r.len(), 2);
        assert_eq!(class(&RadioCmd::HfSeek { up: true }), Class::Keep);
        assert_eq!(class(&RadioCmd::Shutdown), Class::Keep);
        assert_eq!(class(&RadioCmd::TxPlan { tx: 0, region: 0, extend: false, confirmed: false }), Class::Keep);
        assert_eq!(class(&RadioCmd::Beep { hz: 1, ms: 1 }), Class::Drop);
        assert_ne!(key(&RadioCmd::SlotParam { slot: 2, param: Param::Mode, value: 0 }), key(&RadioCmd::SlotParam { slot: 1, param: Param::Mode, value: 0 }));
    }

    /// MODE-6.2: every kind of setting has its own key, so only a newer one of the same kind replaces it.
    #[test]
    fn each_setting_its_own_key() {
        let settings = [
            RadioCmd::Tune(1),
            RadioCmd::Squelch(1),
            RadioCmd::BatterySave(crate::rxsleep::Level::L1),
            RadioCmd::Active(1),
            RadioCmd::Watch(1),
            RadioCmd::Awake(1),
            RadioCmd::ScanTx(true),
            RadioCmd::BeepVol(1),
            RadioCmd::Mute(1),
            RadioCmd::BcastSlot(Some(1)),
            RadioCmd::ScopeCfg { bars: 1, dwell: 1, strip: true, hf: false, si: false },
            RadioCmd::Same(true),
            RadioCmd::AprsSlot(1),
            RadioCmd::DtmfRx(true),
            RadioCmd::TuneSlot { slot: 1, freq: 1 },
            RadioCmd::SlotParam { slot: 1, param: Param::Power, value: 1 },
        ];
        let keys: Vec<u16> = settings.iter().map(|c| key(c).unwrap()).collect();
        for (i, k) in keys.iter().enumerate() {
            assert!(!keys[i + 1..].contains(k), "{:?} shares its key", settings[i]);
            assert_eq!(class(&settings[i]), Class::State);
        }
        assert_eq!((key(&RadioCmd::Activity), key(&RadioCmd::SweepHold), key(&RadioCmd::Lease)), (Some(4), Some(13), Some(14)));
        assert_eq!(class(&RadioCmd::SweepHold), Class::Drop);
        assert_eq!(class(&RadioCmd::Chime { sound: 0, level: 0 }), Class::Drop);
        assert_eq!(class(&RadioCmd::BootSound { at: 0 }), Class::Drop);
        // All of them queue side by side; a second round replaces each in place of none.
        let mut r = CmdRing::<16>::default();
        for c in settings {
            assert_eq!(r.push(c), None);
        }
        assert_eq!(r.len(), 16);
        assert_eq!(r.push(RadioCmd::Squelch(5)), None, "replaced its own kind, so room was made");
        assert_eq!(r.len(), 16);
        assert_eq!(drain(&mut r).last(), Some(&RadioCmd::Squelch(5)));
    }

    /// MODE-6.3: a ring full of Keep commands drops the oldest; an empty one pops nothing.
    #[test]
    fn full_of_keep_drops_the_oldest() {
        let mut r = CmdRing::<2>::new();
        assert!(r.is_empty() && r.capacity() == 2);
        assert_eq!(r.pop(), None);
        r.push(RadioCmd::Shutdown);
        r.push(RadioCmd::Transmit { slot: 0, on: true });
        assert!(!r.is_empty());
        assert_eq!(r.push(RadioCmd::Transmit { slot: 0, on: false }), Some(RadioCmd::Shutdown));
        assert_eq!(drain(&mut r), [RadioCmd::Transmit { slot: 0, on: true }, RadioCmd::Transmit { slot: 0, on: false }]);
        assert!(r.is_empty());
    }
}
