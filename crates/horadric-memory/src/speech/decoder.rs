//! Whisper's text decoder again, with a cache of each layer's keys and
//! values. candle's own caches only the cross attention, so every step
//! reran every token written so far through every layer: a sentence of
//! sixty tokens did some two thousand token passes instead of sixty. The
//! weights and the arithmetic are candle's (`whisper::model::TextDecoder`),
//! only the self attention keeps what it has seen, nothing is copied twice
//! a step, and the weights read a token at a time are in eight bits.
//! Measured 2026-11-01 with the example on Windows TTS: 16.7 s of speech
//! from 5.5 s to 2.3 s, 3.5 s from 1.0 s to 0.7 s; the encoder is most of
//! what is left.

use candle_core::quantized::{GgmlDType, QMatMul, QTensor};
use candle_core::{IndexOp, Module, Result, Tensor};
use candle_nn::{embedding, layer_norm, Embedding, LayerNorm, VarBuilder};
use candle_transformers::models::whisper::Config;

/// A linear layer with its weights in eight bits, quantized as it loads.
/// Writing one token reads every weight of the decoder once for one row
/// of arithmetic, so fewer bytes is quicker: a quarter less time a token
/// than full floats, and no word changed on the samples tried.
struct Linear {
    weight: QMatMul,
    bias: Option<Tensor>,
}

impl Linear {
    fn new(weight: &Tensor, bias: Option<Tensor>) -> Result<Linear> {
        Ok(Linear {
            weight: QMatMul::from_qtensor(QTensor::quantize(weight, GgmlDType::Q8_0)?)?,
            bias,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = self.weight.forward(&x.contiguous()?)?;
        match &self.bias {
            Some(b) => y.broadcast_add(b),
            None => Ok(y),
        }
    }
}

fn linear(inp: usize, out: usize, vb: VarBuilder) -> Result<Linear> {
    Linear::new(&vb.get((out, inp), "weight")?, Some(vb.get(out, "bias")?))
}

fn linear_no_bias(inp: usize, out: usize, vb: VarBuilder) -> Result<Linear> {
    Linear::new(&vb.get((out, inp), "weight")?, None)
}

/// The keys and values one attention reads, already split into heads,
/// scaled and laid out for the product, so a step copies none of them.
struct Seen {
    /// (1, heads, head size, tokens), scaled.
    keys: Tensor,
    /// (1, heads, tokens, head size).
    values: Tensor,
}

/// Self attention, its weights in eight bits and its keys and values
/// growing a token a step.
struct Attention {
    query: Linear,
    key: Linear,
    value: Linear,
    out: Linear,
    heads: usize,
    seen: Option<Seen>,
}

impl Attention {
    fn load(state: usize, heads: usize, vb: VarBuilder) -> Result<Attention> {
        Ok(Attention {
            query: linear(state, state, vb.pp("q_proj"))?,
            key: linear_no_bias(state, state, vb.pp("k_proj"))?,
            value: linear(state, state, vb.pp("v_proj"))?,
            out: linear(state, state, vb.pp("out_proj"))?,
            heads,
            seen: None,
        })
    }

    fn forward(&mut self, x: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
        let new = split(&self.key.forward(x)?, &self.value.forward(x)?, self.heads)?;
        let seen = match self.seen.take() {
            Some(old) => Seen {
                keys: Tensor::cat(&[&old.keys, &new.keys], 3)?,
                values: Tensor::cat(&[&old.values, &new.values], 2)?,
            },
            None => new,
        };
        let q = self.query.forward(x)?;
        let y = attend(&q, &seen, self.heads, mask)?;
        self.seen = Some(seen);
        self.out.forward(&y)
    }
}

/// Cross attention: the encoder's keys and values are made once a window,
/// from every frame at once, so their weights stay in full floats where
/// the matrix product is quick; the query and output are a token a step.
struct Cross {
    query: Linear,
    key: candle_nn::Linear,
    value: candle_nn::Linear,
    out: Linear,
    heads: usize,
    seen: Option<Seen>,
}

impl Cross {
    fn load(state: usize, heads: usize, vb: VarBuilder) -> Result<Cross> {
        Ok(Cross {
            query: linear(state, state, vb.pp("q_proj"))?,
            key: candle_nn::linear_no_bias(state, state, vb.pp("k_proj"))?,
            value: candle_nn::linear(state, state, vb.pp("v_proj"))?,
            out: linear(state, state, vb.pp("out_proj"))?,
            heads,
            seen: None,
        })
    }

    fn forward(&mut self, x: &Tensor, features: &Tensor) -> Result<Tensor> {
        if self.seen.is_none() {
            self.seen = Some(split(
                &self.key.forward(features)?,
                &self.value.forward(features)?,
                self.heads,
            )?);
        }
        let q = self.query.forward(x)?;
        let seen = self.seen.as_ref().expect("made above");
        self.out.forward(&attend(&q, seen, self.heads, None)?)
    }
}

fn heads(x: &Tensor, heads: usize) -> Result<Tensor> {
    let (batch, len, state) = x.dims3()?;
    x.reshape((batch, len, heads, state / heads))?
        .transpose(1, 2)
}

/// Whisper scales both the queries and the keys by the fourth root of the
/// head size, rather than their product by its square root.
fn scale(state: usize, heads: usize) -> f64 {
    ((state / heads) as f64).powf(-0.25)
}

fn split(k: &Tensor, v: &Tensor, n: usize) -> Result<Seen> {
    let state = k.dim(2)?;
    Ok(Seen {
        keys: (heads(k, n)?.transpose(2, 3)? * scale(state, n))?.contiguous()?,
        values: heads(v, n)?.contiguous()?,
    })
}

fn attend(q: &Tensor, seen: &Seen, n: usize, mask: Option<&Tensor>) -> Result<Tensor> {
    let state = q.dim(2)?;
    let q = (heads(q, n)? * scale(state, n))?.contiguous()?;
    let mut qk = q.matmul(&seen.keys)?;
    if let Some(mask) = mask {
        qk = qk.broadcast_add(mask)?;
    }
    let w = candle_nn::ops::softmax_last_dim(&qk)?;
    w.matmul(&seen.values)?.transpose(1, 2)?.flatten_from(2)
}

struct Block {
    attn: Attention,
    attn_ln: LayerNorm,
    cross: Cross,
    cross_ln: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    mlp_ln: LayerNorm,
}

impl Block {
    fn load(state: usize, heads: usize, ffn: usize, vb: VarBuilder) -> Result<Block> {
        Ok(Block {
            attn: Attention::load(state, heads, vb.pp("self_attn"))?,
            attn_ln: layer_norm(state, 1e-5, vb.pp("self_attn_layer_norm"))?,
            cross: Cross::load(state, heads, vb.pp("encoder_attn"))?,
            cross_ln: layer_norm(state, 1e-5, vb.pp("encoder_attn_layer_norm"))?,
            fc1: linear(state, ffn, vb.pp("fc1"))?,
            fc2: linear(ffn, state, vb.pp("fc2"))?,
            mlp_ln: layer_norm(state, 1e-5, vb.pp("final_layer_norm"))?,
        })
    }

    fn forward(&mut self, x: &Tensor, features: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
        let x = (x + self.attn.forward(&self.attn_ln.forward(x)?, mask)?)?;
        let x = (&x + self.cross.forward(&self.cross_ln.forward(&x)?, features)?)?;
        let mlp = self
            .fc2
            .forward(&self.fc1.forward(&self.mlp_ln.forward(&x)?)?.gelu()?)?;
        x + mlp
    }
}

pub struct Decoder {
    embed: Embedding,
    /// The token table again, quantized, for the logits: the biggest
    /// matrix of them all, read once a token.
    logits: Linear,
    positions: Tensor,
    blocks: Vec<Block>,
    ln: LayerNorm,
    /// How many tokens the caches hold, where the next one sits.
    seen: usize,
}

impl Decoder {
    pub fn load(vb: VarBuilder, config: &Config) -> Result<Decoder> {
        let state = config.d_model;
        let heads = config.decoder_attention_heads;
        let blocks = (0..config.decoder_layers)
            .map(|i| Block::load(state, heads, 4 * state, vb.pp(format!("layers.{i}"))))
            .collect::<Result<Vec<_>>>()?;
        let embed = embedding(config.vocab_size, state, vb.pp("embed_tokens"))?;
        Ok(Decoder {
            logits: Linear::new(embed.embeddings(), None)?,
            embed,
            positions: vb.get(
                (config.max_target_positions, state),
                "embed_positions.weight",
            )?,
            blocks,
            ln: layer_norm(state, 1e-5, vb.pp("layer_norm"))?,
            seen: 0,
        })
    }

    /// Forgets the tokens and the window, before a new window.
    pub fn reset(&mut self) {
        for b in &mut self.blocks {
            b.attn.seen = None;
            b.cross.seen = None;
        }
        self.seen = 0;
    }

    /// The logits of the token after `tokens`, which follow those given
    /// since the last [`Decoder::reset`].
    pub fn next(&mut self, tokens: &[u32], features: &Tensor) -> Result<Tensor> {
        let device = features.device();
        let len = tokens.len();
        let input = Tensor::new(tokens, device)?.unsqueeze(0)?;
        let mut x = self
            .embed
            .forward(&input)?
            .broadcast_add(&self.positions.narrow(0, self.seen, len)?)?;
        // Only the tokens given together need hiding from one another:
        // the ones cached are all before them.
        let seen = self.seen;
        let mask = (len > 1)
            .then(|| {
                let m: Vec<f32> = (0..len)
                    .flat_map(|i| {
                        (0..seen + len).map(
                            move |j| {
                                if j > seen + i {
                                    f32::NEG_INFINITY
                                } else {
                                    0.0
                                }
                            },
                        )
                    })
                    .collect();
                Tensor::from_vec(m, (len, seen + len), device)
            })
            .transpose()?;
        for b in &mut self.blocks {
            x = b.forward(&x, features, mask.as_ref())?;
        }
        self.seen += len;
        let last = self.ln.forward(&x.i((.., len - 1..))?)?;
        self.logits.forward(&last)?.i(0)?.i(0)
    }
}
