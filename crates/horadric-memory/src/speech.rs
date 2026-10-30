//! Speech to text for dictation: Whisper, run here with candle like the
//! embedder, so what is said never leaves the machine.
//!
//! The model is distil-whisper's small English one. It reads 30 seconds of
//! sound at a time and its decoder has four layers, so a sentence comes
//! back in a second or two on a laptop's CPU. The weights are fetched on
//! first use into `models\`, the same way and with the same checks as the
//! memory's model.

use std::path::{Path, PathBuf};

use candle_core::{Device, IndexOp, Tensor, D};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::{self as w, audio, model::Whisper};
use tokenizers::Tokenizer;

use crate::models::{self, File, Pinned};

pub const MODEL: Pinned = Pinned {
    repo: "distil-whisper/distil-small.en",
    commit: "9e4a67ca4569c30be43a3fe7fba1621e504f0093",
    files: &[
        File {
            name: "config.json",
            sha256: "077f053cb2c0f7163dd04048546a3588d55c2abce3055d5d119f5b2e2ef7bd73",
            size: 2_262,
        },
        File {
            name: "tokenizer.json",
            sha256: "c9c77688528c0509abbd41873edab16384b8f041531b1d3092ab4d1d83a02a50",
            size: 2_405_466,
        },
        File {
            name: "model.safetensors",
            sha256: "b1312c5e27c750ea877235ae51b47b9b42d1298d17d387bec4b58a00fe644f50",
            size: 332_297_344,
        },
    ],
};

/// The rate [`Listener::transcribe`] takes its samples at.
pub const SAMPLE_RATE: usize = w::SAMPLE_RATE;

/// The model's folder under `models`.
pub const FOLDER: &str = "distil-small.en";

/// How big the first fetch is, for the human waiting on it.
pub fn download_size() -> usize {
    MODEL.files.iter().map(|f| f.size).sum()
}

/// Whether the model is on disk already, so the first dictation can say
/// it has to fetch it first.
pub fn is_fetched(models: &Path) -> bool {
    let dir = models.join(FOLDER);
    MODEL
        .files
        .iter()
        .all(|f| std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.size as u64))
}

/// How much of [`download_size`] is on disk already, so a fetch that
/// broke off counts on from where it was.
pub fn fetched_bytes(models: &Path) -> usize {
    let dir = models.join(FOLDER);
    MODEL
        .files
        .iter()
        .filter(|f| std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.size as u64))
        .map(|f| f.size)
        .sum()
}

/// The folder the model loads from, fetching whatever is missing.
pub fn ensure(
    models: &Path,
    fetch: impl Fn(&str, usize) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    let dir = models.join(FOLDER);
    models::fill(&MODEL, &dir, fetch)?;
    Ok(dir)
}

/// The most tokens one 30 second window is written in. Whisper's own
/// limit is half its context, and dictation is far below it.
const MOST_TOKENS: usize = 224;

/// The frames of silence kept after the speech, two seconds.
const TAIL: usize = 200;

/// Below this loudness (RMS of samples from -1 to 1) nothing was said.
/// Whisper given silence makes something up, "Thank you." most often.
const SILENCE: f32 = 0.004;

pub struct Listener {
    model: Whisper,
    tokenizer: Tokenizer,
    filters: Vec<f32>,
    sot: u32,
    no_timestamps: u32,
    eot: u32,
    /// Tokens never written: the config's list and every special token
    /// past the end of text.
    suppress: Vec<u32>,
    /// Never the first token: a bare space and the end of text.
    begin_suppress: Vec<u32>,
}

impl Listener {
    pub fn load(dir: &Path) -> Result<Listener, String> {
        let err = |e: candle_core::Error| e.to_string();
        let config: w::Config = serde_json::from_slice(
            &std::fs::read(dir.join("config.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| e.to_string())?;
        let id = |t: &str| {
            tokenizer
                .token_to_id(t)
                .ok_or_else(|| format!("the tokenizer has no {t}"))
        };
        let (sot, no_timestamps, eot) = (
            id(w::SOT_TOKEN)?,
            id(w::NO_TIMESTAMPS_TOKEN)?,
            id(w::EOT_TOKEN)?,
        );
        let mut suppress = config.suppress_tokens.clone();
        suppress.extend(eot + 1..config.vocab_size as u32);
        let begin_suppress = vec![id("Ġ").unwrap_or(220), eot];
        let filters = mel_filters(config.num_mel_bins, w::N_FFT, w::SAMPLE_RATE);
        // SAFETY: the file is only read, and nothing writes it while the
        // model is loaded.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[dir.join("model.safetensors")],
                w::DTYPE,
                &Device::Cpu,
            )
            .map_err(err)?
        };
        let model = Whisper::load(&vb, config).map_err(err)?;
        Ok(Listener {
            model,
            tokenizer,
            filters,
            sot,
            no_timestamps,
            eot,
            suppress,
            begin_suppress,
        })
    }

    /// What was said in `pcm`: mono samples at 16 kHz, from -1 to 1.
    pub fn transcribe(&mut self, pcm: &[f32]) -> Result<String, String> {
        if is_silent(pcm) {
            return Ok(String::new());
        }
        let err = |e: candle_core::Error| e.to_string();
        let bins = self.model.config.num_mel_bins;
        let mel = audio::pcm_to_mel(&self.model.config, pcm, &self.filters);
        let frames = mel.len() / bins;
        let mel = Tensor::from_vec(mel, (1, bins, frames), &Device::Cpu).map_err(err)?;
        let spoken = pcm.len() / w::HOP_LENGTH;
        let mut text = String::new();
        let mut seek = 0;
        while seek < spoken {
            // The speech and a little of the silence after it, not the
            // whole 30 seconds Whisper was trained on: the encoder's time
            // grows with the window, and on dictation the words come out
            // the same. Even, since the encoder halves it.
            let frames = (spoken - seek + TAIL).min(w::N_FRAMES) & !1;
            let window = mel.narrow(2, seek, frames).map_err(err)?;
            let part = self.window(&window).map_err(err)?;
            let part = part.trim();
            if !part.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(part);
            }
            seek += w::N_FRAMES;
        }
        Ok(text)
    }

    fn window(&mut self, mel: &Tensor) -> candle_core::Result<String> {
        let features = self.model.encoder.forward(mel, true)?;
        let vocab = self.model.config.vocab_size;
        let mut mask = vec![0f32; vocab];
        for &t in &self.suppress {
            if let Some(m) = mask.get_mut(t as usize) {
                *m = f32::NEG_INFINITY;
            }
        }
        let mask = Tensor::new(mask.as_slice(), &Device::Cpu)?;
        let mut begin = vec![0f32; vocab];
        for &t in &self.begin_suppress {
            begin[t as usize] = f32::NEG_INFINITY;
        }
        let begin = Tensor::new(begin.as_slice(), &Device::Cpu)?;
        let prompt = [self.sot, self.no_timestamps];
        let mut tokens = prompt.to_vec();
        for i in 0..MOST_TOKENS {
            let input = Tensor::new(tokens.as_slice(), &Device::Cpu)?.unsqueeze(0)?;
            let ys = self.model.decoder.forward(&input, &features, i == 0)?;
            let (_, len, _) = ys.dims3()?;
            let logits = self
                .model
                .decoder
                .final_linear(&ys.i((..1, len - 1..))?)?
                .i(0)?
                .i(0)?;
            let mut logits = logits.broadcast_add(&mask)?;
            if i == 0 {
                logits = logits.broadcast_add(&begin)?;
            }
            let next = logits.argmax(D::Minus1)?.to_scalar::<u32>()?;
            if next == self.eot {
                break;
            }
            tokens.push(next);
        }
        self.tokenizer
            .decode(&tokens[prompt.len()..], true)
            .map_err(|e| candle_core::Error::Msg(e.to_string()))
    }
}

/// Whether nothing was said in `pcm`, too quiet to be speech: a muted
/// microphone, or the wrong one.
pub fn is_silent(pcm: &[f32]) -> bool {
    loudness(pcm) < SILENCE
}

fn loudness(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt()
}

/// Hertz on the Slaney mel scale, as librosa and Whisper use it: linear
/// below 1 kHz, logarithmic above.
fn hz_to_mel(hz: f64) -> f64 {
    let step = 200.0 / 3.0;
    if hz < 1000.0 {
        hz / step
    } else {
        1000.0 / step + (hz / 1000.0).ln() / (6.4f64.ln() / 27.0)
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    let step = 200.0 / 3.0;
    let knee = 1000.0 / step;
    if mel < knee {
        mel * step
    } else {
        1000.0 * ((6.4f64.ln() / 27.0) * (mel - knee)).exp()
    }
}

/// The mel filter bank Whisper was trained with, `librosa.filters.mel`
/// with Slaney's area normalisation: `mels` rows of `n_fft / 2 + 1`
/// weights. Made here rather than shipped as a file, since it is a few
/// lines of arithmetic.
pub fn mel_filters(mels: usize, n_fft: usize, rate: usize) -> Vec<f32> {
    let bins = n_fft / 2 + 1;
    let top = hz_to_mel(rate as f64 / 2.0);
    let edges: Vec<f64> = (0..mels + 2)
        .map(|i| mel_to_hz(top * i as f64 / (mels + 1) as f64))
        .collect();
    let mut out = vec![0f32; mels * bins];
    for m in 0..mels {
        let (lo, mid, hi) = (edges[m], edges[m + 1], edges[m + 2]);
        let norm = 2.0 / (hi - lo);
        for k in 0..bins {
            let f = k as f64 * rate as f64 / n_fft as f64;
            let up = (f - lo) / (mid - lo);
            let down = (hi - f) / (hi - mid);
            out[m * bins + k] = (up.min(down).max(0.0) * norm) as f32;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mel_scale_turns_back() {
        for hz in [0.0, 440.0, 999.0, 1000.0, 4000.0, 8000.0] {
            assert!((mel_to_hz(hz_to_mel(hz)) - hz).abs() < 1e-6, "{hz}");
        }
        assert!((hz_to_mel(1000.0) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn mel_filters_match_whispers() {
        let f = mel_filters(80, 400, 16000);
        assert_eq!(f.len(), 80 * 201);
        // Values from Whisper's own mel_filters.npz.
        assert!((f[1] - 0.024_862_595).abs() < 1e-7, "{}", f[1]);
        assert!((f[201 + 2] - 0.022_871_772).abs() < 1e-7, "{}", f[203]);
        let last: f32 = f[79 * 201..].iter().sum();
        assert!((last - 0.024_925_34).abs() < 1e-6, "{last}");
        // Each filter rises then falls once: no bin is lit twice by one row.
        for row in f.chunks(201) {
            let lit: Vec<usize> = (0..201).filter(|&k| row[k] > 0.0).collect();
            assert!(!lit.is_empty());
            assert_eq!(lit.last().unwrap() - lit[0] + 1, lit.len());
        }
    }

    #[test]
    fn silence_is_quiet() {
        assert!(loudness(&[0.0; 1600]) < SILENCE);
        assert!(loudness(&[]) < SILENCE);
        let tone: Vec<f32> = (0..1600).map(|i| (i as f32 * 0.1).sin() * 0.1).collect();
        assert!(loudness(&tone) > SILENCE);
    }

    #[test]
    fn the_download_is_about_a_third_of_a_gigabyte() {
        assert!((300_000_000..400_000_000).contains(&download_size()));
    }
}
