//! Speech to text for dictation: Whisper, run here with candle like the
//! embedder, so what is said never leaves the machine.
//!
//! The model is picked in Settings ([`Model`]); distil-whisper's small
//! English one unless another was. It reads 30 seconds of sound at a time
//! and its decoder has four layers, so a sentence comes back in a second
//! or two on a laptop's CPU. The large ones hear better and take three to
//! four times as long, nearly all of it in the encoder. The weights are
//! fetched on first use into `models\`, the same way and with the same
//! checks as the memory's model.

use std::path::{Path, PathBuf};

use candle_core::{Device, Tensor, D};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::{
    self as w, audio,
    model::{AudioEncoder, Whisper},
};
use tokenizers::Tokenizer;

use crate::models::{self, File, Pinned};

mod decoder;
pub mod resample;
pub mod tidy;
pub mod words;
use decoder::Decoder;

/// A Whisper model dictation can listen with, picked in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// distil-whisper's small English one: four decoder layers, quick.
    SmallEn,
    /// distil-large-v3: English, hears better, a large encoder.
    LargeEn,
    /// OpenAI's small multilingual one, for Danish and the rest.
    Small,
    /// whisper-large-v3-turbo: multilingual, a large encoder and four
    /// decoder layers.
    Turbo,
}

const SMALL_EN: Pinned = Pinned {
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

const LARGE_EN: Pinned = Pinned {
    repo: "distil-whisper/distil-large-v3",
    commit: "8031d2e6ce6631b7fc45629dddfc00271116d981",
    files: &[
        File {
            name: "config.json",
            sha256: "ef5ee106790d92c963ffba0f80c2986fb2ce7bd24593cb93d851b72c037e4ad0",
            size: 1_372,
        },
        File {
            name: "tokenizer.json",
            sha256: "6d8cbd7cd0d8d5815e478dac67b85a26bbe77c1f5e0c6d76d1ce2abc0e5f21ca",
            size: 2_480_617,
        },
        File {
            name: "model.safetensors",
            sha256: "065e3775409aa2fb0e6893a91f7210d3f4c930536ab79acaa56dafbf7be3a475",
            size: 1_512_874_472,
        },
    ],
};

const SMALL: Pinned = Pinned {
    repo: "openai/whisper-small",
    commit: "973afd24965f72e36ca33b3055d56a652f456b4d",
    files: &[
        File {
            name: "config.json",
            sha256: "e6a2b489da1b5aed65a8eb8d1e7466fa867ad5643a8bc138ba708bd56b2875c4",
            size: 1_967,
        },
        File {
            name: "tokenizer.json",
            sha256: "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566",
            size: 2_480_466,
        },
        File {
            name: "model.safetensors",
            sha256: "1d7734884874f1a1513ed9aa760a4f8e97aaa02fd6d93a3a85d27b2ae9ca596b",
            size: 966_995_080,
        },
    ],
};

const TURBO: Pinned = Pinned {
    repo: "openai/whisper-large-v3-turbo",
    commit: "41f01f3fe87f28c78e2fbf8b568835947dd65ed9",
    files: &[
        File {
            name: "config.json",
            sha256: "c5b526b3e3cd64cd8940dabb45e8ba726629e22d8ed389c29b552f9140daf04a",
            size: 1_256,
        },
        File {
            name: "tokenizer.json",
            sha256: "297b13372ac43916285644fb9687add3cc62ee2a1adb60da3dc25cc94c1871fd",
            size: 2_710_337,
        },
        File {
            name: "model.safetensors",
            sha256: "542566a422ae4f3fd23f1ba11add198fca01bbf82e66e6a2857b3f608b1eb9d1",
            size: 1_617_824_864,
        },
    ],
};

impl Model {
    pub const ALL: [Model; 4] = [Model::SmallEn, Model::LargeEn, Model::Small, Model::Turbo];

    pub fn pinned(self) -> &'static Pinned {
        match self {
            Model::SmallEn => &SMALL_EN,
            Model::LargeEn => &LARGE_EN,
            Model::Small => &SMALL,
            Model::Turbo => &TURBO,
        }
    }

    /// Its folder under `models`, and its name in the saved settings.
    pub fn folder(self) -> &'static str {
        match self {
            Model::SmallEn => "distil-small.en",
            Model::LargeEn => "distil-large-v3",
            Model::Small => "whisper-small",
            Model::Turbo => "whisper-large-v3-turbo",
        }
    }

    pub fn from_folder(name: &str) -> Option<Model> {
        Model::ALL.into_iter().find(|m| m.folder() == name)
    }

    /// Whether it hears languages other than English, so Settings offers
    /// the language.
    pub fn is_multilingual(self) -> bool {
        matches!(self, Model::Small | Model::Turbo)
    }

    /// How big the first fetch is, for the human waiting on it.
    pub fn download_size(self) -> usize {
        self.pinned().files.iter().map(|f| f.size).sum()
    }

    /// Whether the model is on disk already, so the first dictation can
    /// say it has to fetch it first.
    pub fn is_fetched(self, models: &Path) -> bool {
        self.fetched_bytes(models) == self.download_size()
    }

    /// How much of [`Model::download_size`] is on disk already, so a
    /// fetch that broke off counts on from where it was.
    pub fn fetched_bytes(self, models: &Path) -> usize {
        let dir = models.join(self.folder());
        self.pinned()
            .files
            .iter()
            .filter(|f| {
                std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.size as u64)
            })
            .map(|f| f.size)
            .sum()
    }

    /// The folder the model loads from, fetching whatever is missing.
    pub fn ensure(
        self,
        models: &Path,
        fetch: impl Fn(&str, usize) -> Result<Vec<u8>, String>,
    ) -> Result<PathBuf, String> {
        let dir = models.join(self.folder());
        models::fill(self.pinned(), &dir, fetch)?;
        Ok(dir)
    }
}

/// The rate [`Listener::transcribe`] takes its samples at.
pub const SAMPLE_RATE: usize = w::SAMPLE_RATE;

/// The most tokens one 30 second window is written in. Whisper's own
/// limit is half its context, and dictation is far below it.
const MOST_TOKENS: usize = 224;

/// The frames of silence kept after the speech, two seconds.
const TAIL: usize = 200;

/// Below this loudness (RMS of samples from -1 to 1) nothing was said.
/// Whisper given silence makes something up, "Thank you." most often.
const SILENCE: f32 = 0.004;

/// The most tokens of prompt given ahead of the speech, its end kept.
/// Whisper takes up to half its context, but distil-whisper given 64 or
/// more loops on a phrase or drops a sentence of the speech.
const MOST_PROMPT: usize = 40;

/// Tokens a second of speech can take at most, and a few over: a fast
/// talker says about four. More than this and Whisper is looping.
const TOKENS_A_SECOND: usize = 10;

pub struct Listener {
    encoder: AudioEncoder,
    decoder: Decoder,
    config: w::Config,
    tokenizer: Tokenizer,
    filters: Vec<f32>,
    sot: u32,
    /// Starts the prompt: text Whisper reads as what came before.
    prev: Option<u32>,
    no_timestamps: u32,
    eot: u32,
    /// Whether the vocabulary has the language tokens, as Whisper's own
    /// code tells a multilingual model.
    multilingual: bool,
    /// Between the start and no timestamps: the language and the task,
    /// so a multilingual model neither guesses the language nor
    /// translates. Empty for an English only model.
    task: Vec<u32>,
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
        let prev = id("<|startofprev|>").ok();
        let multilingual = config.vocab_size >= 51_865;
        let task = if multilingual {
            vec![id("<|en|>")?, id("<|transcribe|>")?]
        } else {
            Vec::new()
        };
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
        let decoder = Decoder::load(vb.pp("model.decoder"), &config).map_err(err)?;
        // candle's decoder is dropped for ours, which caches what it has
        // read; its encoder has no loader of its own. The encoder stays
        // in full floats: it multiplies many rows at once, where candle's
        // eight bit product is nine times slower than f32 and f16 a fifth
        // slower (measured 2026-11-08).
        let Whisper {
            encoder, config, ..
        } = Whisper::load(&vb, config).map_err(err)?;
        Ok(Listener {
            encoder,
            decoder,
            config,
            tokenizer,
            filters,
            sot,
            prev,
            no_timestamps,
            eot,
            multilingual,
            task,
            suppress,
            begin_suppress,
        })
    }

    /// Hear `code` ("da", "en", Whisper's two letters) from now on. An
    /// English only model hears English whatever it is told, and a code
    /// the model does not know is refused.
    pub fn set_language(&mut self, code: &str) -> Result<(), String> {
        if !self.multilingual {
            return Ok(());
        }
        let token = format!("<|{code}|>");
        let id = self
            .tokenizer
            .token_to_id(&token)
            .ok_or_else(|| format!("the model does not know the language {code}"))?;
        self.task[0] = id;
        Ok(())
    }

    /// What was said in `pcm`: mono samples at 16 kHz, from -1 to 1.
    /// `prompt` is read as what came before it: the words said earlier
    /// and names the speech may use, so they are spelled the same.
    pub fn transcribe(&mut self, pcm: &[f32], prompt: &str) -> Result<String, String> {
        self.transcribe_from(pcm, prompt, "")
    }

    /// [`Listener::transcribe`], told the speech starts with the words
    /// `start`: words a live draft has settled on are kept as they are,
    /// and only what follows them is written again.
    pub fn transcribe_from(
        &mut self,
        pcm: &[f32],
        prompt: &str,
        start: &str,
    ) -> Result<String, String> {
        if is_silent(pcm) {
            return Ok(String::new());
        }
        let err = |e: candle_core::Error| e.to_string();
        let bins = self.config.num_mel_bins;
        let mel = audio::pcm_to_mel(&self.config, pcm, &self.filters);
        let frames = mel.len() / bins;
        let mel = Tensor::from_vec(mel, (1, bins, frames), &Device::Cpu).map_err(err)?;
        let spoken = pcm.len() / w::HOP_LENGTH;
        let mut bare = vec![self.sot];
        bare.extend(&self.task);
        bare.push(self.no_timestamps);
        let mut prefix = self.prompt(prompt);
        prefix.extend(&bare);
        let mut forced = self.forced(start);
        let mut text = String::new();
        let mut seek = 0;
        while seek < spoken {
            // The speech and a little of the silence after it, not the
            // whole 30 seconds Whisper was trained on: the encoder's time
            // grows with the window, and on dictation the words come out
            // the same. Even, since the encoder halves it.
            let frames = (spoken - seek + TAIL).min(w::N_FRAMES) & !1;
            let window = mel.narrow(2, seek, frames).map_err(err)?;
            let most = (frames / 100 * TOKENS_A_SECOND + 16).min(MOST_TOKENS);
            // A prompt sometimes sends Whisper round in a loop; without
            // one it seldom does, and a loop left anyway is cut off.
            let mut said = self.window(&window, &prefix, &forced, most).map_err(err)?;
            if said.1 && prefix.len() > bare.len() {
                said = self.window(&window, &bare, &forced, most).map_err(err)?;
            }
            // The settled words begin the first window, not the next.
            forced.clear();
            let (mut tokens, looped) = said;
            if looped {
                tokens.truncate(loop_start(&tokens));
            }
            let part = self
                .tokenizer
                .decode(&tokens, true)
                .map_err(|e| e.to_string())?;
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

    /// The prompt's tokens, its last [`MOST_PROMPT`], behind the token
    /// that marks them as earlier text. None when the tokenizer lacks it.
    fn prompt(&self, prompt: &str) -> Vec<u32> {
        let (Some(prev), false) = (self.prev, prompt.trim().is_empty()) else {
            return Vec::new();
        };
        let Ok(enc) = self.tokenizer.encode(format!(" {}", prompt.trim()), false) else {
            return Vec::new();
        };
        let ids = enc.get_ids();
        let mut out = vec![prev];
        out.extend_from_slice(&ids[ids.len().saturating_sub(MOST_PROMPT)..]);
        out
    }

    /// The tokens of `start`, as Whisper would go on after them.
    fn forced(&self, start: &str) -> Vec<u32> {
        if start.trim().is_empty() {
            return Vec::new();
        }
        self.tokenizer
            .encode(format!(" {}", start.trim()), false)
            .map(|e| e.get_ids().to_vec())
            .unwrap_or_default()
    }

    /// The tokens Whisper writes for `mel` after `prompt`, beginning with
    /// `forced`, at most `most` more, and whether it went round in a loop
    /// or ran past `most` doing so.
    fn window(
        &mut self,
        mel: &Tensor,
        prompt: &[u32],
        forced: &[u32],
        most: usize,
    ) -> candle_core::Result<(Vec<u32>, bool)> {
        let features = self.encoder.forward(mel, true)?;
        let vocab = self.config.vocab_size;
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
        let mut tokens = prompt.to_vec();
        tokens.extend_from_slice(forced);
        self.decoder.reset();
        for i in 0..most {
            // The prompt goes in whole, then one token a step: the
            // decoder keeps the rest.
            let fresh = if i == 0 {
                &tokens[..]
            } else {
                &tokens[tokens.len() - 1..]
            };
            let mut logits = self.decoder.next(fresh, &features)?.broadcast_add(&mask)?;
            if i == 0 && forced.is_empty() {
                logits = logits.broadcast_add(&begin)?;
            }
            let next = logits.argmax(D::Minus1)?.to_scalar::<u32>()?;
            if next == self.eot {
                return Ok((tokens.split_off(prompt.len()), false));
            }
            tokens.push(next);
            if loops(&tokens[prompt.len()..]) {
                return Ok((tokens.split_off(prompt.len()), true));
            }
        }
        Ok((tokens.split_off(prompt.len()), true))
    }
}

/// The loop `tokens` end in, the way Whisper loops: one token said five
/// times over, a run of two to twelve said three times, or a run of
/// eight or more said twice, a whole sentence again as the large models
/// do on a short window. Its length and how many rounds of it there are.
fn the_loop(tokens: &[u32]) -> Option<(usize, usize)> {
    let len = tokens.len();
    let twice = (LONG_RUN..=len / 2).find(|&n| tokens[len - 2 * n..len - n] == tokens[len - n..]);
    if let Some(n) = twice {
        return Some((n, 2));
    }
    (1..=12).find_map(|n| {
        let mut rounds = 1;
        while len >= n * (rounds + 1)
            && tokens[len - n * (rounds + 1)..len - n * rounds] == tokens[len - n..]
        {
            rounds += 1;
        }
        (rounds >= if n == 1 { 5 } else { 3 }).then_some((n, rounds))
    })
}

/// The shortest run of tokens that said twice over is a loop: shorter
/// ones are said twice on purpose ("no, no"), a whole clause seldom.
const LONG_RUN: usize = 8;

fn loops(tokens: &[u32]) -> bool {
    the_loop(tokens).is_some()
}

/// How many of `tokens` to keep: up to the end of the loop's first round,
/// or all of them when they do not loop.
fn loop_start(tokens: &[u32]) -> usize {
    match the_loop(tokens) {
        Some((n, rounds)) => tokens.len() - n * (rounds - 1),
        None => tokens.len(),
    }
}

/// Whether nothing was said in `pcm`, too quiet to be speech: a muted
/// microphone, or the wrong one.
pub fn is_silent(pcm: &[f32]) -> bool {
    loudness(pcm) < SILENCE
}

/// The RMS of `pcm`, 0 for none.
pub fn loudness(pcm: &[f32]) -> f32 {
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
    fn a_loop_is_seen_and_cut_after_its_first_round() {
        assert!(!loops(&[1, 2, 3, 4, 5]));
        assert!(!loops(&[7, 7, 7, 7]));
        assert!(loops(&[7, 7, 7, 7, 7]));
        assert!(loops(&[1, 8, 9, 8, 9, 8, 9]));
        let t = [1, 2, 8, 9, 8, 9, 8, 9];
        assert_eq!(&t[..loop_start(&t)], &[1, 2, 8, 9]);
        let sentence: Vec<u32> = (10..20).collect();
        let mut again = vec![1, 2];
        again.extend(&sentence);
        assert!(!loops(&again));
        again.extend(&sentence);
        assert!(loops(&again));
        assert_eq!(&again[..loop_start(&again)], &again[..12]);
        assert!(!loops(&[1, 2, 3, 1, 2, 3]));
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
        assert!((300_000_000..400_000_000).contains(&Model::SmallEn.download_size()));
    }
}
