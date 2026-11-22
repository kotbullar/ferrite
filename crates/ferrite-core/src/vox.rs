//! VOX speech detection (manual chapter 18: VOX-2, VOX-3, VOX-5, VOX-14). The Radio task reads the mic level on
//! PA0 and feeds it here; this decides when speech starts and when it has stopped for the tail. Keying is the
//! UI's, with every PTT check (VOX-4): this only says whether there is speech.

/// Stock's thresholds (0x0802F73C, PA0 reading), numbered as stock's menu: level 1 = 21 (keys easiest) up to
/// level 9 = 101, in steps of 10: a higher level needs a louder voice, as a higher volume number (VOX-14,
/// decision 131).
pub fn threshold(level: u8) -> u16 {
    11 + 10 * level.clamp(1, 9) as u16
}

/// VOX-15: stock's default mic sensitivity, REG_7D[5:0] = 0x24 (0x0801DD90: Mic Gain setting 0, the default;
/// setting 1 gives 0x2D and setting 2 the calibration's 0xF210 byte 0). The chip starts at 0x12, 9 dB lower.
pub const MIC_STOCK: u8 = 0x24;

/// VOX-6: REG_7D[5:0] for a data key-up (APRS), stock V0.29's 0x34 for the burst (0x0801F01C); the next key-up
/// writes the mic gain again.
pub const MIC_DATA: u8 = 0x34;

/// Highest user setting for the BK4829 microphone gain (MENU 0 8 3).
pub const MIC_GAIN_MAX: u8 = 12;

/// VOX-15: REG_7D[5:0] is 0.5 dB per register step. Levels 1–9 keep their
/// original 2.5 dB spacing. Levels 10–12 add 1 dB each, up to 0x3F.
pub fn mic_reg(gain: u8) -> u8 {
    let gain = gain.clamp(1, MIC_GAIN_MAX);
    if gain <= 9 {
        0x39 - (9 - gain) * 5
    } else {
        0x39 + (gain - 9) * 2
    }
}

/// Stock's hysteresis: speech goes on above the threshold and stays on down to this many counts below it.
pub const HYSTERESIS: u16 = 8;

/// No VOX until this long after boot (VOX-3).
pub const BOOT_HOLD_MS: u32 = 5000;

/// After anything that blocks VOX (receive audio, the broadcast, a beep) ends, this long before it samples again:
/// stock's hold of 15 x 100 ms (0x08025994, traced Oct 13, 2026). The PA0 tap is an envelope that decays over
/// seconds and is dead while the audio stage (PB8) is on, so a reading just after the audio stops is not speech.
pub const HOLD_MS: u32 = 1500;

/// Stock's VOX reading from a 12-bit PA0 conversion (0x0801645C): bits 11-4, so the thresholds below compare
/// with 8-bit counts (VOX-14).
pub fn reading(raw12: u16) -> u16 {
    (raw12 >> 4) & 0xFF
}

/// The detector's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vox {
    /// Speech, the tail included.
    on: bool,
    /// When the level last stood above the off level (ms).
    last: u32,
    /// No sampling before this (ms): [`HOLD_MS`] after the last block.
    free_at: u32,
}

impl Vox {
    pub const fn new() -> Self {
        Vox { on: false, last: 0, free_at: 0 }
    }

    /// One PA0 conversion (12-bit, as read) at `now` (ms since boot). `level` 1-9, or 0 for VOX off; `tail_ms`
    /// the hang time; `blocked` while any audio plays or an accessory change is debounced (VOX-3), and
    /// [`HOLD_MS`] after. Returns whether speech is on, the tail included.
    pub fn step(&mut self, raw12: u16, level: u8, tail_ms: u32, blocked: bool, now: u32) -> bool {
        if blocked {
            *self = Vox { free_at: now.wrapping_add(HOLD_MS), ..Vox::new() };
            return false;
        }
        if level == 0 || now < BOOT_HOLD_MS || (now.wrapping_sub(self.free_at) as i32) < 0 {
            (self.on, self.last) = (false, 0);
            return false;
        }
        let mic = reading(raw12);
        let t = threshold(level);
        let above = if self.on { mic + HYSTERESIS >= t } else { mic >= t };
        if above {
            (self.on, self.last) = (true, now);
        } else if self.on && now.wrapping_sub(self.last) >= tail_ms {
            self.on = false; // VOX-5: below the level for the whole tail
        }
        self.on
    }

    pub fn on(&self) -> bool {
        self.on
    }
}

#[cfg(feature = "bench")]
/// TERM-32: how many PA0 samples `mic` takes, and how far apart (8 kHz: 32 ms in all).
pub const MIC_SAMPLES: usize = 256;
#[cfg(feature = "bench")]
pub const MIC_PERIOD_US: u32 = 125;

#[cfg(feature = "bench")]
/// TERM-32 (a bench test): a burst of PA0 readings summed up, to tell raw mic audio (it swings through its mean at
/// the tone's frequency) from a level or envelope (it sits). 12-bit counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicCapture {
    pub min: u16,
    pub max: u16,
    pub mean: u16,
    /// Crossings of the mean, with a hysteresis of an eighth of the swing either side so noise doesn't count.
    pub crossings: u16,
    /// The time the burst took, microseconds.
    pub us: u32,
    /// The first readings, 12 bits shifted down to 8.
    pub first: [u8; 16],
    /// TERM-32.1 (decision 294): readings at a rail ([`crate::rxlevel::at_rail`]): clipped.
    pub rails: u16,
}

#[cfg(feature = "bench")]
impl MicCapture {
    pub fn from_samples(s: &[u16], us: u32) -> Self {
        let (mut min, mut max, mut sum) = (u16::MAX, 0u16, 0u32);
        for &v in s {
            (min, max, sum) = (min.min(v), max.max(v), sum + v as u32);
        }
        let mean = if s.is_empty() { 0 } else { (sum / s.len() as u32) as u16 };
        let band = max.saturating_sub(min) / 8;
        let (mut crossings, mut side) = (0u16, None);
        for &v in s {
            let now = if v > mean + band { Some(true) } else if v + band < mean { Some(false) } else { side };
            if side.is_some() && now != side {
                crossings += 1;
            }
            side = now;
        }
        let mut first = [0u8; 16];
        for (f, &v) in first.iter_mut().zip(s) {
            *f = (v >> 4) as u8;
        }
        let rails = s.iter().filter(|&&v| crate::rxlevel::at_rail(v)).count() as u16;
        MicCapture { min: if s.is_empty() { 0 } else { min }, max, mean, crossings, us, first, rails }
    }

    /// The frequency the crossings would mean for raw audio (two per cycle), Hz.
    pub fn hz(&self) -> u32 {
        (self.crossings as u32 * 500_000).checked_div(self.us).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {

    #[cfg(feature = "bench")]
    /// TERM-32: a 1 kHz sine at 8 kHz gives two crossings a cycle (about 1000 Hz); a steady level, none.
    #[test]
    fn mic_capture_tells_audio_from_a_level() {
        let sine: [u16; 256] = core::array::from_fn(|i| (2048.0 + 600.0 * (i as f64 * core::f64::consts::TAU / 8.0 + 0.3).sin()) as u16);
        let c = MicCapture::from_samples(&sine, 32_000);
        assert!((970..=1010).contains(&c.hz()), "{}", c.hz()); // the first crossing has no side before it
        assert!(c.min < 1500 && c.max > 2600 && c.mean.abs_diff(2048) < 20);
        let level: [u16; 256] = core::array::from_fn(|i| 300 + (i % 3) as u16);
        let c = MicCapture::from_samples(&level, 32_000);
        assert!(c.crossings <= 2 * 3 && c.hz() < 100, "noise of a few counts mustn't read as a tone: {:?}", c);
        assert_eq!(MicCapture::from_samples(&[], 0).hz(), 0);
    }

    use super::*;

    /// VOX-14.1: stock's thresholds, 101 at level 1 down to 21 at level 9.
    #[test]
    fn thresholds_are_stocks() {
        assert_eq!(threshold(1), 21);
        assert_eq!(threshold(5), 61);
        assert_eq!(threshold(9), 101);
    }

    /// VOX-2, VOX-5, VOX-14.1: on at the threshold, held down to 8 counts below it, off after the tail.
    #[test]
    fn speech_and_tail() {
        let mut v = Vox::new();
        let t = threshold(5) << 4; // 12-bit, as PA0 reads
        assert!(!v.step(t - 1, 5, 2000, false, 6000), "below the threshold");
        assert!(v.step(t, 5, 2000, false, 6100), "at the threshold");
        assert!(v.on(), "as the last step said");
        assert!(v.step(t - (HYSTERESIS << 4), 5, 2000, false, 6200), "hysteresis holds it");
        assert!(v.step(0, 5, 2000, false, 8100), "the tail");
        assert!(!v.step(0, 5, 2000, false, 8200), "2 s below: off");
        assert!(!v.on());
    }

    /// VOX-1, VOX-3.2, VOX-3.3: off at level 0, during the first 5 s and while receiving.
    #[test]
    fn never_while_off_booting_or_receiving() {
        let mut v = Vox::new();
        assert!(!v.step(500, 0, 2000, false, 9000), "VOX off");
        assert!(!v.step(500, 1, 2000, false, 4999), "within 5 s of boot");
        assert!(v.step(500, 1, 2000, false, 9000));
        assert!(!v.step(500, 1, 2000, true, 9100), "the slot receives: speech drops at once");
        assert!(!v.step(500, 1, 2000, false, 9100 + HOLD_MS - 1), "stock's 1.5 s hold after the audio");
        assert!(v.step(500, 1, 2000, false, 9100 + HOLD_MS));
    }

    /// VOX-14.1: stock compares bits 11-4 of the 12-bit reading. The noise floor and an envelope's decay (under
    /// 300 raw on the black radio, Oct 13, 2026) never reach level 1's 21; the loud whistle seen (~800 raw = 49)
    /// passes levels 1-3 only.
    #[test]
    fn stock_scaling() {
        assert_eq!(reading(799), 49);
        assert_eq!(reading(0xFFF), 255);
        let mut v = Vox::new();
        assert!(!v.step(48, 1, 2000, false, 9000), "a decaying envelope (48 raw = 3) is not speech");
        assert!(!v.step(335, 1, 2000, false, 9020), "335 raw = 20: below level 1's 21");
        assert!(v.step(336, 1, 2000, false, 9040), "336 raw = 21: level 1");
        let mut v = Vox::new();
        assert!(!v.step(799, 5, 2000, false, 9000), "the whistle (49) is below level 5's 61");
    }

    /// VOX-15: 9 writes stock's loudest and a step (0x39), 2.5 dB a step down; 5 (the default) half a dB over stock's
    /// default.
    #[test]
    fn mic_gain_to_reg_7d() {
        assert_eq!((mic_reg(1), mic_reg(5), mic_reg(9)), (0x11, 0x25, 0x39));
        assert_eq!((mic_reg(10), mic_reg(11), mic_reg(12)), (0x3B, 0x3D, 0x3F));
        assert_eq!((mic_reg(0), mic_reg(u8::MAX)), (0x11, 0x3F), "clamped to 1–12");
        assert!(mic_reg(5).abs_diff(MIC_STOCK) <= 1, "5 within half a dB of stock's default");
        assert!(mic_reg(8) < 0x37 && mic_reg(9) >= 0x37, "9 reaches stock's loudest, its setting 2 (0xF210 byte 0 on the bench radios)");
    }
}
