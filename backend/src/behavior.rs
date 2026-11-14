//! Generator nilai auto mode (PLAN bagian 5). Parameter mengikuti acuan: interval, min..max, scale factor.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Nilai hanya berubah dari UI / API / perintah FEP
    Manual,
    /// 0/1 bergantian tiap interval (telesignal)
    Toggle,
    /// Acak min..max kelipatan scale factor
    Random,
    /// Naik-turun maks `step` per interval, tetap di min..max
    RandomWalk,
    /// Gelombang min..max dengan periode `period_s`
    Sine,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gen {
    pub mode: Mode,
    pub interval_ms: u64,
    pub min: f64,
    pub max: f64,
    /// Nilai dibulatkan ke kelipatan ini (0 = tanpa pembulatan)
    pub scale: f64,
    /// random_walk: langkah maksimum per interval
    pub step: f64,
    /// sine: periode gelombang (detik)
    pub period_s: f64,
}

impl Gen {
    pub fn manual() -> Self {
        Self { mode: Mode::Manual, interval_ms: 1000, min: 0.0, max: 1.0, scale: 0.0, step: 0.0, period_s: 60.0 }
    }

    pub fn validate(&self) -> Result<(), String> {
        let finite = [self.min, self.max, self.scale, self.step, self.period_s].iter().all(|v| v.is_finite());
        if !finite || self.min > self.max {
            return Err(format!("min ({}) harus <= max ({})", self.min, self.max));
        }
        if self.mode != Mode::Manual && self.interval_ms < 100 {
            return Err("interval minimal 100 ms".into());
        }
        if self.scale < 0.0 || self.step < 0.0 {
            return Err("scale & step tidak boleh negatif".into());
        }
        if self.mode == Mode::Sine && self.period_s <= 0.0 {
            return Err("periode sine harus > 0".into());
        }
        Ok(())
    }

    /// Nilai berikutnya. `cur` = nilai sekarang, `t_s` = detik sejak start (untuk sine).
    pub fn next(&self, cur: f64, t_s: f64, rng: &mut Rng) -> f64 {
        let v = match self.mode {
            Mode::Manual => return cur,
            Mode::Toggle => return if cur == 0.0 { 1.0 } else { 0.0 },
            Mode::Random => {
                if self.scale > 0.0 {
                    let n = ((self.max - self.min) / self.scale).floor() as u64;
                    self.min + rng.below(n + 1) as f64 * self.scale
                } else {
                    self.min + rng.f64() * (self.max - self.min)
                }
            }
            Mode::RandomWalk => cur + (rng.f64() * 2.0 - 1.0) * self.step,
            Mode::Sine => {
                let mid = (self.min + self.max) / 2.0;
                mid + (self.max - mid) * (std::f64::consts::TAU * t_s / self.period_s).sin()
            }
        };
        self.quantize(v)
    }

    fn quantize(&self, v: f64) -> f64 {
        let v = v.clamp(self.min, self.max);
        if self.scale > 0.0 {
            // bulatkan ke kelipatan scale yang masih di dalam min..max
            let k = ((v - self.min) / self.scale).round();
            let q = self.min + k * self.scale;
            if q > self.max { q - self.scale } else { q }
        } else {
            v
        }
    }
}

/// xorshift64*: cukup untuk data simulasi, tanpa dependency tambahan.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0.0 <= x < 1.0
    pub fn f64(&mut self) -> f64 {
        (self.u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 0..n
    fn below(&mut self, n: u64) -> u64 {
        self.u64() % n.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(mode: Mode) -> Gen {
        Gen { mode, interval_ms: 1000, min: 10.0, max: 20.0, scale: 0.5, step: 1.0, period_s: 10.0 }
    }

    #[test]
    fn nilai_dalam_batas_dan_kelipatan_scale() {
        let mut rng = Rng::new(42);
        for mode in [Mode::Random, Mode::RandomWalk, Mode::Sine] {
            let g = gen(mode);
            let mut v = 15.0;
            for i in 0..2000 {
                let n = g.next(v, i as f64 * 0.37, &mut rng);
                assert!((10.0..=20.0).contains(&n), "{mode:?}: {n}");
                assert!(((n - 10.0) / 0.5).fract().abs() < 1e-9, "{mode:?} bukan kelipatan 0.5: {n}");
                if mode == Mode::RandomWalk {
                    assert!((n - v).abs() <= 1.0 + 0.25 + 1e-9, "langkah terlalu besar: {v} -> {n}");
                }
                v = n;
            }
        }
    }

    #[test]
    fn random_mencapai_min_dan_max() {
        let (g, mut rng) = (gen(Mode::Random), Rng::new(7));
        let vals: Vec<f64> = (0..500).map(|_| g.next(0.0, 0.0, &mut rng)).collect();
        assert!(vals.contains(&10.0) && vals.contains(&20.0));
    }

    #[test]
    fn sine_dan_toggle_dan_manual() {
        let (g, mut rng) = (gen(Mode::Sine), Rng::new(1));
        assert_eq!(g.next(0.0, 0.0, &mut rng), 15.0); // tengah
        assert_eq!(g.next(0.0, 2.5, &mut rng), 20.0); // puncak di 1/4 periode
        let t = gen(Mode::Toggle);
        assert_eq!(t.next(0.0, 0.0, &mut rng), 1.0);
        assert_eq!(t.next(1.0, 0.0, &mut rng), 0.0);
        assert_eq!(gen(Mode::Manual).next(13.3, 0.0, &mut rng), 13.3);
    }

    #[test]
    fn validasi() {
        assert!(gen(Mode::Random).validate().is_ok());
        assert!(Gen { min: 5.0, max: 1.0, ..gen(Mode::Random) }.validate().is_err());
        assert!(Gen { interval_ms: 10, ..gen(Mode::Random) }.validate().is_err());
        assert!(Gen { period_s: 0.0, ..gen(Mode::Sine) }.validate().is_err());
        assert!(Gen { min: f64::NAN, ..gen(Mode::Random) }.validate().is_err());
    }
}
