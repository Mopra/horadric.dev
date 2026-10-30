//! Memory: text found by what it means, not only by the words it shares
//! with the question.
//!
//! This part turns text into vectors. The model runs here, in Rust, with
//! candle: nothing to install beside Horadric and no service that has to
//! be running for recall to work. [`store`] keeps the text and the
//! vectors on disk, [`recall`] ranks what answers a question, and
//! [`keeper::Keeper`] is all of it as the app runs it.

pub mod brethren;
pub mod distill;
pub mod horadrim;
pub mod keeper;
pub mod models;
pub mod parts;
pub mod recall;
pub mod service;
pub mod share;
pub mod sharing;
pub mod speech;
pub mod store;
pub mod transcripts;

use std::path::Path;
use std::sync::OnceLock;

use candle_core::{DType, Device, Tensor, D};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use tokenizers::Tokenizer;

/// The model Horadric embeds with. One today; the kind still names the
/// folder and the tag, so a new model means new vectors, not mixed ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// sentence-transformers/all-MiniLM-L6-v2: 384 numbers, English,
    /// mean pooled, six layers.
    MiniLm,
}

impl Kind {
    /// The model's folder name, under the state folder's `models\`.
    pub fn folder(self) -> &'static str {
        match self {
            Kind::MiniLm => "all-MiniLM-L6-v2",
        }
    }

    /// The name a vector made by this model is kept under: the model and
    /// the weights' commit. A new one means the kept text is embedded
    /// again.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::MiniLm => "all-MiniLM-L6-v2@1110a243",
        }
    }

    /// The longest input read, in tokens: what the model was trained to
    /// read, as sentence-transformers cuts it.
    pub fn most_tokens(self) -> usize {
        match self {
            Kind::MiniLm => 256,
        }
    }
}

pub struct Embedder {
    kind: Kind,
    model: BertModel,
    tokenizer: Tokenizer,
}

impl Embedder {
    /// Loads `kind` from `dir`, the folder its files were fetched into.
    pub fn load(kind: Kind, dir: &Path) -> Result<Embedder, String> {
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| e.to_string())?;
        // Some tokenizers come set to pad every input to one length; one
        // input at a time needs none, and pads would be pooled as words.
        tokenizer.with_padding(None);
        tokenizer.with_truncation(None).map_err(|e| e.to_string())?;
        let err = |e: candle_core::Error| e.to_string();
        let cfg: BertConfig = serde_json::from_slice(
            &std::fs::read(dir.join("config.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        // SAFETY: the file is only read, and nothing writes it while a
        // model is loaded.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[dir.join("model.safetensors")],
                DType::F32,
                &Device::Cpu,
            )
            .map_err(err)?
        };
        Ok(Embedder {
            kind,
            model: BertModel::load(vb, &cfg).map_err(err)?,
            tokenizer,
        })
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The vector for a question. MiniLM was trained without prompts, so
    /// a question and a passage are embedded alike.
    pub fn query(&self, text: &str) -> Result<Vec<f32>, String> {
        self.embed(text)
    }

    /// The vector for a passage that may answer one.
    pub fn document(&self, text: &str) -> Result<Vec<f32>, String> {
        self.embed(text)
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let enc = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| e.to_string())?;
        let ids = keep_ends(enc.get_ids(), self.kind.most_tokens());
        mean_pooled(&self.model, &ids).map_err(|e| e.to_string())
    }
}

/// The threads the embedding runs on. Left alone, candle's matrix work
/// runs on rayon's global pool, a thread per logical core, and memory
/// embeds whenever sessions write, so it kept a core or more busy beside
/// them. Two threads below normal priority: the agents and the window
/// come first. Whisper keeps the global pool, since a human waits on it.
fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .thread_name(|i| format!("horadric-embed-{i}"))
            .start_handler(|_| below_normal())
            .build()
            .expect("the embedding pool starts")
    })
}

#[cfg(windows)]
fn below_normal() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: the pseudo handle of the calling thread needs no closing.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

#[cfg(not(windows))]
fn below_normal() {}

fn mean_pooled(model: &BertModel, ids: &[u32]) -> candle_core::Result<Vec<f32>> {
    pool().install(|| pooled_here(model, ids))
}

fn pooled_here(model: &BertModel, ids: &[u32]) -> candle_core::Result<Vec<f32>> {
    let dev = Device::Cpu;
    let input = Tensor::new(ids, &dev)?.unsqueeze(0)?;
    let types = input.zeros_like()?;
    let out = model.forward(&input, &types, None)?;
    let pooled = out.mean(1)?;
    let unit = pooled.broadcast_div(&pooled.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?)?;
    unit.squeeze(0)?.to_vec1()
}

/// The first `most` ids, keeping the last one (the end token the models
/// were trained to see) when the input is cut.
fn keep_ends(ids: &[u32], most: usize) -> Vec<u32> {
    if ids.len() <= most {
        return ids.to_vec();
    }
    let mut out = ids[..most - 1].to_vec();
    out.extend(ids.last());
    out
}

/// The cosine of two unit vectors.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_input_keeps_its_end_token() {
        assert_eq!(keep_ends(&[2, 5, 6, 7, 1], 3), vec![2, 5, 1]);
        assert_eq!(keep_ends(&[2, 5, 1], 3), vec![2, 5, 1]);
    }
}
