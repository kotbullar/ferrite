//! Frequencies in 10 Hz units, the unit the BK4829 frequency registers use.

pub type Freq10 = u32;

/// Limits of HF Mode and the receiver changeover, in 10 Hz units.
pub const HF_BOTTOM: Freq10 = 15_000;
pub const HF_BK_FROM: Freq10 = mhz(18);
pub const HF_TOP: Freq10 = mhz(64);
/// The SI4732's AM tuning command supports frequencies through 30 MHz.
pub const HF_SI_TOP: Freq10 = mhz(30);

pub fn hf_scope_range(si: bool) -> core::ops::RangeInclusive<Freq10> {
    if si { HF_BOTTOM..=HF_SI_TOP } else { HF_BK_FROM..=HF_TOP - 1 }
}

pub const fn mhz(x: u32) -> Freq10 {
    x * 100_000
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Band {
    Hf,
    Vhf,
    B200,
    B300,
    Uhf,
}

/// Band and owning chip (0 = BK4829 #0, 64-300 MHz; 1 = BK4829 #1, 300 MHz up and HF), as stock splits them.
pub fn band(f: Freq10) -> (Band, u8) {
    match f {
        _ if f < mhz(64) => (Band::Hf, 1),
        _ if f < mhz(200) => (Band::Vhf, 0),
        _ if f < mhz(300) => (Band::B200, 0),
        _ if f < mhz(400) => (Band::B300, 1),
        _ => (Band::Uhf, 1),
    }
}

/// What the BK4829s receive: home VFOs and channels. The datasheet says 18-580 and 760-1160 MHz; as nicFW does,
/// Ferrite tunes 18-630 and 756-1300 MHz, both ends included, where the PLL is reported to lock (VFO-21; to
/// confirm on the bench, and pulled back if it won't lock at 1.2 GHz). Receive only above 1000 MHz: no transmit
/// plan reaches it.
/// Below 18 MHz is HF mode's, on the SI4732 (VFO-14).
pub const RX_RANGES: [(Freq10, Freq10); 2] = [(mhz(18), mhz(630)), (mhz(756), mhz(1300))];

/// Whether a chip can receive `f` ([`RX_RANGES`], inclusive: 630.000 and 1300.000 are in, VFO-21).
pub fn receivable(f: Freq10) -> bool {
    RX_RANGES.iter().any(|&(lo, hi)| f >= lo && f <= hi)
}

/// Whether `f` needs the band relay (PC4) switched to the HF path: chip 1 below 64 MHz ([`band`]'s HF).
pub fn needs_relay(f: Freq10) -> bool {
    band(f).0 == Band::Hf
}

/// Digits of v, at least `width` of them, into buf from pos; keeps one byte spare like the C version
/// (which NUL-terminates), so it truncates the same way.
fn put_uint(buf: &mut [u8], mut pos: usize, mut v: u32, width: usize) -> usize {
    let (mut tmp, mut n) = ([0u8; 10], 0);
    loop {
        tmp[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 || n == 10 {
            break;
        }
    }
    while n < width {
        tmp[n] = b'0';
        n += 1;
    }
    while n > 0 && pos + 1 < buf.len() {
        n -= 1;
        buf[pos] = tmp[n];
        pos += 1;
    }
    pos
}

/// "146.520 00": MHz, three decimals, space, the last two digits (100 Hz and 10 Hz). Returns the length.
pub fn format(f: Freq10, buf: &mut [u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }
    let (mhz, rest) = (f / 100_000, f % 100_000);
    let mut p = put_uint(buf, 0, mhz, 1);
    if p + 1 < buf.len() {
        buf[p] = b'.';
        p += 1;
    }
    p = put_uint(buf, p, rest / 100, 3);
    if p + 1 < buf.len() {
        buf[p] = b' ';
        p += 1;
    }
    put_uint(buf, p, rest % 100, 2)
}

/// What the user typed on the keypad ("147.06", "443.475", "7.1") in 10 Hz units; None for anything that
/// is not a plain decimal MHz value.
pub fn parse(s: &[u8]) -> Option<Freq10> {
    let (mut mhz, mut frac, mut digits, mut fdigits, mut dot) = (0u32, 0u32, 0, 0, false);
    for &c in s {
        match c {
            b'.' if dot => return None,
            b'.' => dot = true,
            b'0'..=b'9' if !dot => {
                digits += 1;
                if digits > 4 {
                    return None;
                }
                mhz = mhz * 10 + (c - b'0') as u32;
            }
            b'0'..=b'9' => {
                fdigits += 1;
                if fdigits > 5 {
                    return None;
                }
                frac = frac * 10 + (c - b'0') as u32;
            }
            _ => return None,
        }
    }
    if digits == 0 {
        return None;
    }
    for _ in fdigits..5 {
        frac *= 10;
    }
    Some(mhz * 100_000 + frac)
}

/// Tiny self-check for the on-target image.
pub fn selftest() -> bool {
    let mut b = [0u8; 16];
    format(mhz(146) + 52_000, &mut b);
    let f = parse(b"443.475");
    b[0] == b'1' && b[3] == b'.' && b[7] == b' ' && b[9] == b'0' && f == Some(44_347_500) && band(44_347_500).1 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(f: Freq10, len: usize) -> std::string::String {
        let mut b = [0u8; 16];
        let n = format(f, &mut b[..len]);
        std::string::String::from_utf8(b[..n].to_vec()).unwrap()
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt(14_652_000, 16), "146.520 00");
        assert_eq!(fmt(44_347_500, 16), "443.475 00");
        assert_eq!(fmt(710_000, 16), "7.100 00");
        assert_eq!(fmt(14_652_012, 16), "146.520 12");
        assert_eq!(fmt(14_652_000, 5).len(), 4); // truncates, never overflows
    }

    #[test]
    fn parsing() {
        assert_eq!(parse(b"147.06"), Some(14_706_000));
        assert_eq!(parse(b"443.475"), Some(44_347_500));
        assert_eq!(parse(b"7.1"), Some(710_000));
        assert_eq!(parse(b"146.52012"), Some(14_652_012));
        assert_eq!(parse(b"146.520123"), None); // finer than 10 Hz
        assert_eq!(parse(b"1.2.3"), None);
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b".5"), None);
        assert_eq!(parse(b"12a"), None);
    }

    #[test]
    fn bands() {
        assert_eq!(band(mhz(27)), (Band::Hf, 1));
        assert_eq!(band(mhz(64)), (Band::Vhf, 0));
        assert_eq!(band(mhz(146)).1, 0);
        assert_eq!(band(mhz(220)), (Band::B200, 0));
        assert_eq!(band(mhz(300)), (Band::B300, 1));
        assert_eq!(band(mhz(446)), (Band::Uhf, 1));
        assert!(selftest());
    }

    /// VFO-21: 18-630 and 756-1300 MHz, both ends included; the gap and HF mode's range below aren't.
    #[test]
    fn receivable_ranges() {
        for f in [mhz(18), mhz(146), mhz(630), mhz(756), mhz(1300)] {
            assert!(receivable(f), "{f}");
        }
        for f in [mhz(18) - 1, mhz(630) + 1, mhz(700), mhz(756) - 1, mhz(1300) + 1, 0] {
            assert!(!receivable(f), "{f}");
        }
        assert!(needs_relay(mhz(28)) && !needs_relay(mhz(146)));
    }

    /// Room for nothing writes nothing; more than four MHz digits is no frequency.
    #[test]
    fn format_and_parse_limits() {
        assert_eq!(fmt(14_652_000, 0), "");
        assert_eq!(fmt(14_652_000, 4), "146");
        assert_eq!(fmt(130_000_000, 16), "1300.000 00");
        assert_eq!(parse(b"12345.6"), None);
        assert_eq!(parse(b"1300"), Some(130_000_000));
    }
}
