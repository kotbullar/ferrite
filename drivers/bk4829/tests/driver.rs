//! The driver against a register-file model: stock's start-up word for word, the receive features' register
//! values, and that nothing a receive-only build can call sets a transmit bit.

use bk4829::{dcs_codeword, int, reg, AfOut, Bandwidth, Bk4829, Bus, Demod, Fsk, Squelch, POWER_BEEP};

#[derive(Default)]
struct Regs {
    regs: Vec<u16>,
    writes: Vec<(u8, u16)>,
}

impl Regs {
    fn new() -> Self {
        let mut regs = vec![0u16; 128];
        regs[0] = 0x4829;
        regs[0x40] = 0x34D0; // reset value
        Regs { regs, writes: Vec::new() }
    }
}

impl Bus for Regs {
    fn write(&mut self, reg: u8, value: u16) {
        self.writes.push((reg, value));
        self.regs[reg as usize] = value;
    }
    fn read(&mut self, reg: u8) -> u16 {
        self.regs[reg as usize]
    }
    /// Waits are logged as register 0xFF, so their place among the writes shows.
    fn delay_us(&mut self, us: u32) {
        self.writes.push((0xFF, us as u16));
    }
}

/// Stock's start-up (function 0x08009D44 in the stock image), REG_40's read-modify-write resolved against its reset
/// value 0x34D0: (0x34D0 & 0xF000) | 0x4D2.
const STOCK_INIT: &[(u8, u16)] = &[
    (0x00, 0x8000), (0x00, 0x0000), (0x37, 0x9D1F), (0x13, 0x03DF), (0x12, 0x03DB), (0x11, 0x033A), (0x10, 0x0318),
    (0x14, 0x0210), (0x49, 0x2AB2), (0x7B, 0x73DC), (0x1C, 0x07C0), (0x1D, 0xE555), (0x1E, 0x4C58), (0x1F, 0x865A),
    (0x3E, 0x94C6), (0x3F, 0x07FE), (0x25, 0xC1BA), (0x3A, 0x9A7C), (0x19, 0x1041), (0x28, 0x0B40), (0x29, 0xAA00),
    (0x2A, 0x6600), (0x2C, 0x0022), (0x2F, 0x9890), (0x53, 0x2028), (0x7E, 0x303E), (0x46, 0x6050), (0x4A, 0x5430),
    (0x48, 0xB3BF), (0x49, 0x2AB2), (0x4A, 0x5430), (0x4D, 0xA004), (0x4E, 0x3815), (0x4F, 0x3F3B), (0x77, 0xCCEF),
    (0x7E, 0x303E), (0x40, 0x34D2), (0x7D, 0xE912), (0x48, 0xB3FF), (0x74, 0xDF22), (0x44, 0x91C1), (0x45, 0x3040),
    (0x75, 0xE61C), (0x54, 0x91C1), (0x55, 0x3040),
];

#[test]
fn init_is_stocks() {
    let mut bk = Bk4829::new(Regs::new());
    assert_eq!(bk.id(), 0x4829);
    bk.init();
    assert_eq!(bk.release().writes, STOCK_INIT);
}

#[test]
fn rx_gain_preserves_other_register_bits() {
    let mut bk = Bk4829::new(Regs::new());
    bk.write(0x7E, 0x3056);
    for index in 0..4 {
        bk.rx_gain(Some(index));
        assert_eq!(bk.read(0x7E), 0x8056 | u16::from(index) << 12);
    }
    bk.rx_gain(None);
    assert_eq!(bk.read(0x7E), 0x3056);
}

#[test]
fn tune_and_receive() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.set_freq(145_500_000);
    assert_eq!(bk.freq(), 145_500_000);
    bk.rx_on();
    bk.bandwidth(Bandwidth::Narrow);
    bk.af_out(AfOut::Normal);
    bk.af_gain(0x3B, 15);
    bk.rx_filters(false, false, false); // flat audio for data
    let w = &r.writes;
    // 145.5 MHz / 10 Hz = 14,550,000 = 0x00DE_03F0.
    assert_eq!(w[..2], [(0x38, 0x03F0), (0x39, 0x00DE)]);
    assert_eq!(w[2..5], [(0x30, 0x0200), (0xFF, 50), (0x30, 0xBFF1)], "stock's receive sequence, its 50 us between");
    assert_eq!(w[5], (0x43, 0x4008));
    assert_eq!(r.regs[0x47] >> 8 & 15, 1);
    assert_eq!(r.regs[0x48] & 0x3FF, 0x3B << 4 | 15);
    assert_eq!(r.regs[0x2B], 0x0700);
}

#[test]
fn squelch_and_meters() {
    let mut r = Regs::new();
    r.regs[0x4D] = 0xA004;
    r.regs[0x4E] = 0x3815;
    r.regs[0x67] = 2 * (160 - 100); // -100 dBm
    r.regs[0x0C] = 0b0000_0100_0000_0011;
    let mut bk = Bk4829::new(&mut r);
    bk.squelch(&Squelch { rssi_open: 0x48, rssi_close: 0x46, noise_open: 0x2E, noise_close: 0x2F, glitch_open: 8, glitch_close: 0x20 });
    assert_eq!(bk.rssi_dbm(), -100);
    let s = bk.status();
    assert!(s.squelch_open && s.ctcss && s.interrupt && !s.cdcss_pos && !s.cdcss_neg && !s.vox);
    assert_eq!(r.regs[0x78], 0x4846);
    assert_eq!(r.regs[0x4F], 0x2F2E, "the table's reset value for the same thresholds");
    assert_eq!(r.regs[0x4D], 0xA020, "delays kept");
    assert_eq!(r.regs[0x4E], 0x3808);
    // SQL-17: the open delay alone changes (scan steps 0, ordinary receive stock's 7).
    Bk4829::new(&mut r).open_delay(0);
    assert_eq!(r.regs[0x4E], 0x0008);
    Bk4829::new(&mut r).open_delay(7);
    assert_eq!(r.regs[0x4E], 0x3808);
}

#[test]
fn subtones() {
    // D023N is 0x763813 (published tables); inverted is the 23-bit complement.
    assert_eq!(dcs_codeword(0o23, false), 0x76_3813);
    assert_eq!(dcs_codeword(0o23, true), 0x09_C7EC);
    // Every codeword is a Golay codeword: the remainder by the generator is 0.
    for code in 0..0o777 {
        let mut w = dcs_codeword(code, false);
        for i in (11..23).rev() {
            if w >> i & 1 != 0 {
                w ^= 0xC75 << (i - 11);
            }
        }
        assert_eq!(w, 0);
    }
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.ctcss_rx(885);
    bk.dcs_rx(0o23, false);
    bk.tail_detect();
    let w = &r.writes;
    assert_eq!(w[..3], [(0x51, 0x104A), (0x07, 1827), (0x52, 0x0292)], "88.5 Hz x 20.64888 = 1827");
    assert_eq!(w[3..7], [(0x51, 0xA000 | 51), (0x07, 0x0AD7), (0x08, 0x0813), (0x08, 0x8763)], "stock's CDCSS sequence");
    assert_eq!(w[7], (0x07, 0x21CD), "stock's 55 Hz tail word");
}

/// TONE-20.3, TONE-20.4 (decision 142): DCS receive as stock V0.27's 0x0800D3CC: REG_51 0xA000 (normal) or 0x8000
/// (inverted) with the normal codeword for both; REG_0C bit 14 reads as the positive code, bit 15 as the negative.
#[test]
fn dcs_rx_matches_stock() {
    for (inverted, r51) in [(false, 0xA000 | 51), (true, 0x8000 | 51)] {
        let mut r = Regs::new();
        let mut bk = Bk4829::new(&mut r);
        bk.dcs_rx(0o23, inverted);
        assert_eq!(r.writes[..4], [(0x51, r51), (0x07, 0x0AD7), (0x08, 0x0813), (0x08, 0x8763)], "inverted {inverted}");
    }
    let mut r = Regs::new();
    for (v, pos, neg) in [(1u16 << 14, true, false), (1 << 15, false, true), (0, false, false)] {
        r.regs[0x0C] = v;
        let s = Bk4829::new(&mut r).status();
        assert_eq!((s.cdcss_pos, s.cdcss_neg), (pos, neg), "REG_0C {v:#06x}");
    }
}

#[test]
fn interrupts_and_dtmf() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    assert_eq!(bk.interrupts(), 0, "nothing waiting: REG_02 untouched");
    bk.dtmf_rx(true);
    bk.release();
    assert_eq!(r.writes.len(), 19);
    assert_eq!(r.writes[0], (0x09, 0x006F));
    assert_eq!(r.writes[15], (0x09, 0xF09F));
    assert_eq!(r.writes[16..], [(0x24, 0x87FF), (0x70, 0xC5C5), (0x3F, int::DTMF)]);
    r.regs[0x0B] = 14 << 8;
    r.regs[0x0C] = 1;
    let mut bk = Bk4829::new(&mut r);
    assert_eq!(bk.dtmf_code(), '*');
    // The model returns REG_02 as written (0): a real chip returns the flags; the order is what matters.
    bk.interrupts();
    bk.dtmf_rx(false);
    assert_eq!(r.writes[19], (0x02, 0));
    assert_eq!(r.regs[0x24] & 0x20, 0);
    assert_eq!(r.regs[0x70], 0);
}

fn ffsk() -> Fsk {
    // Stock's FFSK 1200 receive set-up (function 0x0801E0D0).
    Fsk {
        mode: Fsk::FFSK1200,
        tone_word: 0x3065,
        tone_gain: 0x6C,
        preamble: 3,
        sync: [0xFB, 0x72, 0x40, 0x99],
        sync4: true,
        crc: 0xA730,
        scramble: true,
        len: 28,
    }
}

#[test]
fn fsk_receive_matches_stock() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.fsk_setup(&ffsk());
    bk.fsk_rx(&ffsk());
    let get = |reg: u8| r.writes.iter().rev().find(|w| w.0 == reg).unwrap().1;
    assert_eq!(get(0x58), 0x3FC3);
    assert_eq!(get(0x70), 0x00EC);
    assert_eq!(get(0x72), 0x3065);
    assert_eq!(get(0x5D), 0x1B00);
    assert_eq!(get(0x5A), 0xFB72);
    assert_eq!(get(0x5B), 0x4099);
    assert_eq!(get(0x5C), 0xA730);
    let n = r.writes.len();
    assert_eq!(r.writes[n - 3..n - 1], [(0x59, 0x6028), (0x59, 0x3028)], "stock's clear-then-receive");
    assert_eq!(bk4829::tone_word(1200), 0x3065, "1200 Hz x 10.32444: stock's tone 2 word");
}

#[test]
fn sleep_and_wake_are_stocks() {
    // Stock's Battery Save (0x0801CC9E / 0x0801CCD0): REG_30 0 then REG_37 0x1D00; back with REG_37 0x9D1F, then
    // the receive sequence.
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.sleep();
    bk.wake();
    bk.rx_on();
    // Stock waits about 50 us between the two REG_30 writes (the TX audit, Oct 13, 2026).
    assert_eq!(r.writes, [(0x30, 0), (0x37, 0x1D00), (0x37, 0x9D1F), (0x30, 0x0200), (0xFF, 50), (0x30, 0xBFF1)]);
}

/// The sleep in two halves, for a caller that waits between them: the same writes as [`Bk4829::sleep`].
#[test]
fn sleep_in_two_halves() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.off();
    bk.ldo_sleep();
    assert_eq!(r.writes, [(0x30, 0), (0x37, 0x1D00)]);
}

/// Everything a receive-only build has, called in turn: REG_30 never gets a transmit bit (PA gain, MIC ADC,
/// transmit DSP), REG_51 never its transmit enable except in DCS receive's stock values (TONE-20.3, decision 142:
/// stock writes its transmit REG_51 on receive too; bit 15 only picks the sub-tone source and cannot key the chip,
/// which REG_30 does), REG_36 (PA) never written.
#[test]
fn receive_never_keys() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.init();
    bk.set_freq(446_006_250);
    bk.rx_on();
    bk.bandwidth(Bandwidth::Wide);
    bk.af_out(AfOut::Tone);
    bk.af_gain(63, 15);
    bk.rx_filters(true, true, true);
    bk.squelch(&Squelch { rssi_open: 1, rssi_close: 1, noise_open: 1, noise_close: 1, glitch_open: 1, glitch_close: 1 });
    bk.ctcss_rx(1000);
    bk.dcs_rx(0o754, true);
    bk.tail_detect();
    bk.subtone_scan();
    bk.subtone_off();
    bk.dtmf_rx(true);
    bk.dtmf_rx(false);
    bk.fsk_setup(&ffsk());
    bk.fsk_rx(&ffsk());
    bk.fsk_read();
    bk.fsk_off();
    bk.interrupts_enable(0xFFFF);
    bk.demod(Demod::Am);
    bk.demod(Demod::Fm);
    let g = bk.beep_on(1000);
    bk.beep_off(g);
    bk.freq_scan(true);
    bk.freq_scan_result();
    bk.freq_scan(false);
    bk.sleep();
    bk.wake();
    bk.off();
    for &(reg, v) in &r.writes {
        match reg {
            // The local beep's REG_30 has the transmit DSP (stock's mode 3) but no PLL, PA gain or microphone: no
            // carrier, nothing radiates (checked below).
            reg::POWER if v == POWER_BEEP => {}
            reg::POWER => assert_eq!(v & 0b1110, 0, "REG_30 {v:#06x}"),
            reg::SUBTONE if v == bk4829::DCS_RX_NORMAL || v == bk4829::DCS_RX_INVERTED => {}
            reg::SUBTONE => assert_eq!(v & 0x8000, 0),
            reg::PA => panic!("PA written"),
            reg::FSK_CTRL => assert_eq!(v & 1 << 11, 0, "FSK transmit"),
            _ => {}
        }
    }
}

#[test]
fn beep_cannot_radiate() {
    assert_eq!(POWER_BEEP & 0x00F0, 0, "PLL/VCO off");
    assert_eq!(POWER_BEEP & 0b1100, 0, "PA gain and microphone off");
}

/// Stock's local beep (0x0801EB8C with 0, then 0x0801EB4C), V0.29's tone gain.
#[test]
fn beep_matches_stock() {
    let mut r = Regs::new();
    r.regs[0x48] = 0xB2BF;
    let mut bk = Bk4829::new(&mut r);
    let g = bk.beep_on(1000);
    assert_eq!(g, 0xB2BF);
    assert_eq!(r.regs[0x71], 10324, "1000 Hz x 10.32444");
    assert_eq!(r.regs[0x70], 0x9700, "tone 1 on, gain 0x17");
    assert_eq!((r.regs[0x30], r.regs[0x47], r.regs[0x48]), (0x0302, 0x6342, 0xB0A3));
    let mut bk = Bk4829::new(&mut r);
    bk.beep_off(g);
    assert_eq!((r.regs[0x70], r.regs[0x48], r.regs[0x47] & 0x0F00), (0, 0xB2BF, 0));
}

/// A beep at another tone gain only changes REG_70's gain field; the rest is stock's beep.
#[test]
fn beep_gain_is_the_tone_field() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.beep_on_gain(1000, 0x2D);
    assert_eq!(r.regs[0x70], 0x8000 | 0x2D << 8);
    assert_eq!(r.regs[0x30], 0x0302, "still the beep's REG_30: nothing radiates");
    let mut bk = Bk4829::new(&mut r);
    bk.beep_on_gain(1000, 0xFF);
    assert_eq!(r.regs[0x70] >> 8, 0x80 | 0x7F, "the gain is 7 bits");
}

/// Stock's receive-path modes 7 (FM) and 6 (AM), V0.27 0x0801DED8.
#[test]
fn demod_matches_stock() {
    let mut r = Regs::new();
    r.regs[0x31] = 0x0002;
    let mut bk = Bk4829::new(&mut r);
    bk.demod(Demod::Am);
    assert_eq!((r.regs[0x31], r.regs[0x42], r.regs[0x2A], r.regs[0x2B]), (0x0003, 0x6F5C, 0x7434, 0x0500));
    let mut bk = Bk4829::new(&mut r);
    bk.demod(Demod::Fm);
    assert_eq!((r.regs[0x31], r.regs[0x42], r.regs[0x2A], r.regs[0x2B]), (0x0002, 0x6B5A, 0x7400, 0));
}

/// Stock's counter: REG_32 0x8245 on, 0x8244 off; the reading from REG_0D/0E in 10 Hz, none while REG_0D bit 15.
#[test]
fn freq_scan_matches_stock() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.freq_scan(true);
    assert_eq!(r.regs[0x32], 0x8245);
    r.regs[0x0D] = 0x8000;
    assert_eq!(Bk4829::new(&mut r).freq_scan_result(), None);
    let f = 446_006_250u32 / 10;
    (r.regs[0x0D], r.regs[0x0E]) = ((f >> 16) as u16, f as u16);
    assert_eq!(Bk4829::new(&mut r).freq_scan_result(), Some(446_006_250));
    Bk4829::new(&mut r).freq_scan(false);
    assert_eq!(r.regs[0x32], 0x8244);
}

#[cfg(feature = "tx")]
mod tx {
    use super::*;
    use bk4829::tx::{dtmf_pair, Source};

    /// Keying as stock (0x0801E5CC, TX audit F3): REG_30 0x0200 (wide) or 0 (narrow), the 50 us gap, then the
    /// transmit value; unkeying as stock's 0x0801D224 (F4): the PA output off, then REG_30 0.
    #[test]
    fn key_and_unkey() {
        let mut r = Regs::new();
        let mut bk = Bk4829::new(&mut r);
        bk.pa(0x80, 7, 7, true);
        bk.tx_on(Source::Mic, true);
        bk.tx_off();
        bk.tx_on(Source::Tones, false);
        assert_eq!(r.writes[0], (0x36, 0x80BF));
        assert_eq!(r.writes[1..4], [(0x30, 0x0200), (0xFF, 50), (0x30, 0xC1FE)], "stock's voice transmit, wide");
        assert_eq!(r.writes[4], (0x36, 0x803F), "PA output off first");
        assert_eq!(r.writes[5..6], [(0x30, 0)], "out of transmit, not into receive");
        assert_eq!(r.writes[6..9], [(0x30, 0), (0xFF, 50), (0x30, 0xC3FA)], "stock's tone transmit, narrow");
    }

    /// TX audit F1, F2, F7: REG_7D's 6-bit mic field, REG_40 as stock's key-up writes it, REG_51's 7-bit gain.
    #[test]
    fn mic_deviation_and_subtone_gain() {
        let mut r = Regs::new();
        r.regs[0x7D] = 0xE912;
        r.regs[0x40] = 0x34D2;
        let mut bk = Bk4829::new(&mut r);
        bk.mic_gain(0x24);
        bk.tx_deviation();
        bk.ctcss_tx(885);
        bk.subtone_gain(0x46);
        assert_eq!((r.regs[0x7D], r.regs[0x40], r.regs[0x51]), (0xE924, 0x350A, 0x9046), "stock's 0xE924, 0x350A");
        let mut bk = Bk4829::new(&mut r);
        bk.mic_gain(0x1C);
        assert_eq!(r.regs[0x7D], 0xE91C, "bit 5 is the field's top bit");
    }

    #[test]
    fn subtones_and_tones() {
        let mut r = Regs::new();
        let mut bk = Bk4829::new(&mut r);
        bk.ctcss_tx(885);
        assert_eq!(r.regs[0x51], 0x904A);
        assert_eq!(dtmf_pair('5'), Some((770, 1336)));
        assert_eq!(dtmf_pair('#'), Some((941, 1477)));
        assert_eq!(dtmf_pair('x'), None);
        let mut bk = Bk4829::new(&mut r);
        bk.tones(1750, 0x40, 0, 0);
        assert_eq!(r.regs[0x70], 0xC000);
        assert_eq!(r.regs[0x71], 18068, "1750 Hz x 10.32444");
    }

    /// Stock V0.27's DCS transmit (0x0800D3CC with the codeword from 0x08014C44): REG_51 0xA000 (normal, bit 13
    /// set) or 0x8000 (inverted) with the gain, REG_07 0x0AD7, then the normal codeword's halves in both; the
    /// calibration's gain (byte 2, the black radio's 0x34 here) keeps the mode bits (decision 140).
    #[test]
    fn dcs_tx_matches_stock() {
        for (inverted, r51) in [(false, 0xA034), (true, 0x8034)] {
            let mut r = Regs::new();
            let mut bk = Bk4829::new(&mut r);
            bk.dcs_tx(0o23, inverted);
            bk.subtone_gain(0x34);
            assert_eq!(r.writes[..4], [(0x51, if inverted { 0x8000 } else { 0xA000 } | 51), (0x07, 0x0AD7), (0x08, 0x0813), (0x08, 0x8763)]);
            assert_eq!(r.regs[0x51], r51, "inverted {inverted}");
        }
    }

    /// The tones' REG_40 (TONE-19.3) and back to stock's, the top four bits kept.
    #[test]
    fn tone_deviation_and_back() {
        let mut r = Regs::new();
        r.regs[0x40] = 0x350A;
        let mut bk = Bk4829::new(&mut r);
        bk.tone_deviation(true);
        assert_eq!(bk.read(0x40), 0x343C);
        bk.tone_deviation(false);
        assert_eq!(r.regs[0x40], 0x350A);
    }

    /// Tail elimination as stock sends it (0x0801D078): the 55 Hz tone with no sub-tone, REG_52's shift with one;
    /// ending it leaves REG_52's other bits as they were.
    #[test]
    fn tail_matches_stock() {
        use bk4829::tx::Tail;
        let mut r = Regs::new();
        r.regs[0x52] = 0x0292;
        let mut bk = Bk4829::new(&mut r);
        bk.tail_tx(Some(Tail::Tone55 { dev: 0x46 }));
        assert_eq!((r.regs[0x51], r.regs[0x07], r.regs[0x52]), (0x9046, 0x0471, 0x0292), "stock's 55 Hz");
        let mut bk = Bk4829::new(&mut r);
        bk.tail_tx(Some(Tail::Shift { phase: 2 }));
        assert_eq!(r.regs[0x52], 0x8000 | 2 << 13 | 0x0292, "180 degrees");
        let mut bk = Bk4829::new(&mut r);
        bk.tail_tx(Some(Tail::Shift { phase: 0 }));
        assert_eq!(r.regs[0x52], 0x8292, "DCS: the 134.4 Hz turn-off code");
        let mut bk = Bk4829::new(&mut r);
        bk.tail_tx(None);
        assert_eq!((r.regs[0x51], r.regs[0x52]), (0, 0x0292));
    }

    /// Stock's transmitted tones: a single tone at V0.29's gain 0x17 (REG_70 0x9700; V0.27's 0xBC00 was about
    /// 8 dB hotter), a DTMF pair at 0x45 each (0xC5C5), the sidetone on AF route 3.
    #[test]
    fn tones_match_stock() {
        let mut r = Regs::new();
        let mut bk = Bk4829::new(&mut r);
        bk.tone_tx(1750);
        assert_eq!(r.regs[0x70], 0x9700);
        let mut bk = Bk4829::new(&mut r);
        assert!(bk.dtmf_tx('D'));
        assert_eq!(r.regs[0x70], 0xC5C5);
        assert_eq!((r.regs[0x71], r.regs[0x72]), (bk4829::tone_word(941), bk4829::tone_word(1633)));
        let mut bk = Bk4829::new(&mut r);
        assert!(!bk.dtmf_tx('x'));
        bk.sidetone(true);
        assert_eq!(r.regs[0x47], 0x6342);
        let mut bk = Bk4829::new(&mut r);
        bk.tone_tx(0);
        assert_eq!(r.regs[0x70], 0);
    }

    #[test]
    fn fsk_send_matches_stock() {
        // Stock (function 0x0801E3C0): REG_3F 0x8000, REG_59 0xA028 then 0x2028, the words, REG_59 0x2828.
        let mut r = Regs::new();
        let mut bk = Bk4829::new(&mut r);
        bk.fsk_send(&ffsk(), &[0x1234, 0x5678]);
        bk.fsk_tx_end(&ffsk());
        assert_eq!(
            r.writes,
            [(0x3F, 0x8000), (0x59, 0xA028), (0x59, 0x2028), (0x5F, 0x1234), (0x5F, 0x5678), (0x59, 0x2828), (0x02, 0), (0x3F, 0), (0x59, 0x2028)]
        );
    }
}

/// TSRCH-3: REG_68's CTCSS reading at the BK4829's scale (88.5 Hz read 87.3 at the BK4819's 20.64888): rounded to
/// 0.1 Hz; bit 15 and the bits above 12 are not the count.
#[test]
fn scan_reading_scale() {
    assert_eq!(bk4829::scan_dhz(1803), 885);
    assert_eq!(bk4829::scan_dhz(0x6000 | 1803), 885);
    assert_eq!(bk4829::scan_dhz(1365), 670);
    assert_eq!(bk4829::scan_dhz(5101), 2503);
}

/// The detectors share REG_3F: DTMF, then the FSK modem receiving, then an FSK packet sent and ended, and the
/// DTMF bit and the init table's bits are still there; DTMF off clears its bit alone.
#[cfg(feature = "tx")] // sends an FSK packet
#[test]
fn interrupt_bits_are_shared() {
    let mut r = Regs::new();
    r.regs[0x3F] = 0x07FE; // the init table's
    let mut bk = Bk4829::new(&mut r);
    bk.dtmf_rx(true);
    bk.fsk_setup(&ffsk());
    bk.fsk_rx(&ffsk());
    bk.fsk_send(&ffsk(), &[0x1234]);
    bk.fsk_tx_end(&ffsk());
    bk.release();
    assert_eq!(r.regs[0x3F], 0x07FE & !int::FSK_ALL | int::DTMF);
    let mut bk = Bk4829::new(&mut r);
    bk.fsk_rx(&ffsk());
    bk.release();
    assert_eq!(r.regs[0x3F] & int::DTMF, int::DTMF, "the modem keeps DTMF's bit");
    let mut bk = Bk4829::new(&mut r);
    bk.dtmf_rx(false);
    bk.release();
    assert_eq!(r.regs[0x3F], 0x07FE & !int::FSK_ALL | int::FSK_RX_DONE | int::FSK_FIFO_FULL | int::FSK_SYNC);
}

/// Stock's receive audio level and the AF routes: REG_48 0xB2BF; REG_47 0x6042 with the route in bits 11:8 (a raw
/// route keeps its low four bits).
#[test]
fn af_level_and_raw_route() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.af_gain_rx();
    bk.af_out(AfOut::Raw(0x13));
    bk.af_out(AfOut::Tone);
    bk.af_out(AfOut::Mute);
    assert_eq!(r.writes, [(0x48, 0xB2BF), (0x47, 0x6342), (0x47, 0x6242), (0x47, 0x6042)]);
}

/// The meters read only their fields: noise REG_65 bits 6:0, glitches REG_63's low byte, RSSI REG_67 bits 8:0; the
/// FSK FIFO word whole.
#[test]
fn meters_mask_their_fields() {
    let mut r = Regs::new();
    r.regs[0x65] = 0xFF85;
    r.regs[0x63] = 0x1234;
    r.regs[0x67] = 0xFE00 | (2 * 160); // 0 dBm, junk above bit 8
    r.regs[0x5F] = 0xBEEF;
    let mut bk = Bk4829::new(&mut r);
    assert_eq!((bk.noise(), bk.glitch(), bk.rssi(), bk.rssi_dbm()), (5, 0x34, 320, 0));
    assert_eq!(bk.fsk_read(), 0xBEEF);
    // The packet's CRC result is REG_0B bit 4 (the DTMF code in bits 11:8 shares the register).
    assert!(!bk.fsk_crc_ok());
    bk.release();
    r.regs[0x0B] = 0x0F10;
    let mut bk = Bk4829::new(&mut r);
    assert!(bk.fsk_crc_ok());
    r.regs[0x0B] = 0x0FEF;
    assert!(!Bk4829::new(&mut r).fsk_crc_ok());
    assert!(r.writes.is_empty(), "reads only");
}

/// Sub-tone scan: REG_51 = 51 and CTC1 0x0AD7 (the 134.4 Hz bit rate); bit 15 of REG_68 or REG_69 set means that
/// detector has nothing yet.
#[test]
fn subtone_scan_on_and_readings() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.subtone_scan_on();
    assert_eq!(bk.subtone_scan(), (Some(0), Some(0)));
    assert_eq!(bk.release().writes, [(0x51, 51), (0x07, 0x0AD7)]);
    r.regs[0x68] = 0x8000 | 1803;
    r.regs[0x69] = 0x8813;
    r.regs[0x6A] = 0x0763;
    assert_eq!(Bk4829::new(&mut r).subtone_scan(), (None, None));
    r.regs[0x68] = 1803;
    r.regs[0x69] = 0x0813;
    assert_eq!(Bk4829::new(&mut r).subtone_scan(), (Some(885), Some(0x813_763)));
}

/// A bus with no wait of its own: receive on still writes stock's two REG_30 values, back to back.
#[test]
fn bus_without_a_delay() {
    struct Plain(Vec<(u8, u16)>);
    impl Bus for Plain {
        fn write(&mut self, reg: u8, value: u16) {
            self.0.push((reg, value));
        }
        fn read(&mut self, _: u8) -> u16 {
            0
        }
    }
    let mut bk = Bk4829::new(Plain(Vec::new()));
    bk.rx_on();
    assert_eq!(bk.release().0[..2], [(0x30, 0x0200), (0x30, 0xBFF1)]);
}

/// The start of stock's TX set-up: REG_30 0, then the 50 us gap; sidetone off is the AF route muted.
#[cfg(feature = "tx")]
#[test]
fn tx_start_and_sidetone_off() {
    let mut r = Regs::new();
    let mut bk = Bk4829::new(&mut r);
    bk.tx_start();
    bk.sidetone(true);
    bk.sidetone(false);
    assert_eq!(r.writes, [(0x30, 0), (0xFF, 50), (0x47, 0x6342), (0x47, 0x6042)]);
}
