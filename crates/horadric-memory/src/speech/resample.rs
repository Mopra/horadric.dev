//! Brings a microphone's own rate down to Whisper's 16 kHz, for a system
//! that does not do it for us: WASAPI converts in shared mode, the Mac's
//! AVAudioEngine hands out 44.1 or 48 kHz as the hardware runs.
//!
//! Each sample out is the mean of the samples in over its stretch, which
//! is a crude low pass: enough for speech, which Whisper only reads up to
//! 8 kHz anyway, and nothing to tune.

pub struct Resampler {
    /// Samples in per sample out.
    step: f64,
    /// Samples in so far, and where the sample being made ends.
    at: f64,
    until: f64,
    sum: f32,
    n: usize,
}

impl Resampler {
    pub fn new(from: f64, to: f64) -> Resampler {
        let step = if from > 0.0 && to > 0.0 {
            from / to
        } else {
            1.0
        };
        Resampler {
            step,
            at: 0.0,
            until: step,
            sum: 0.0,
            n: 0,
        }
    }

    /// `input` taken in, what it makes added to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &s in input {
            self.sum += s;
            self.n += 1;
            self.at += 1.0;
            while self.at >= self.until {
                // Raising the rate, a sample in makes several out.
                out.push(if self.n == 0 {
                    s
                } else {
                    self.sum / self.n as f32
                });
                self.sum = 0.0;
                self.n = 0;
                self.until += self.step;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forty_eight_kilohertz_comes_out_a_third_as_long() {
        let mut r = Resampler::new(48_000.0, 16_000.0);
        let mut out = Vec::new();
        // In uneven pieces, as an audio callback hands them over.
        for piece in [vec![0.3f32; 1000], vec![0.3; 2001], vec![0.3; 2999]] {
            r.push(&piece, &mut out);
        }
        assert_eq!(out.len(), 2000);
        assert!(out.iter().all(|s| (s - 0.3).abs() < 1e-6));
    }

    #[test]
    fn an_odd_rate_keeps_its_length_and_its_tone() {
        let mut r = Resampler::new(44_100.0, 16_000.0);
        let tone: Vec<f32> = (0..44_100)
            .map(|i| (i as f32 * std::f32::consts::TAU * 440.0 / 44_100.0).sin())
            .collect();
        let mut out = Vec::new();
        r.push(&tone, &mut out);
        assert!((out.len() as i64 - 16_000).abs() <= 1);
        // A 440 Hz tone still crosses zero about 880 times a second.
        let crossings = out
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!((870..=890).contains(&crossings), "{crossings}");
    }

    #[test]
    fn the_same_rate_passes_through() {
        let mut r = Resampler::new(16_000.0, 16_000.0);
        let mut out = Vec::new();
        r.push(&[0.1, 0.2, 0.3], &mut out);
        assert_eq!(out, [0.1, 0.2, 0.3]);
    }
}
