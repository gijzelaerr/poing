use std::collections::HashMap;
use std::path::Path;

use ndarray::{s, Array1, Array2, Array3, ArrayD, Axis, IxDyn};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use rand::distributions::WeightedIndex;
use rand::prelude::*;

const NUM_CODEBOOKS: usize = 4;
const NUM_HEADS: usize = 16;
const HEAD_DIM: usize = 64;
const NUM_LAYERS: usize = 24;
const BOS_TOKEN: i64 = 2048;
const PAD_TOKEN: i64 = 2048;
const GUIDANCE_SCALE: f32 = 3.0;
const MAX_LENGTH: usize = 1500;
const TOP_K: usize = 50;

struct MusicGenPipeline {
    text_encoder: Session,
    decoder: Session,
    encodec_decode: Session,
    tokenizer: tokenizers::Tokenizer,
}

impl MusicGenPipeline {
    fn load(model_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let session = || {
            Session::builder()?
                .with_optimization_level(GraphOptimizationLevel::Level1)
        };

        eprintln!("[poing] Loading text_encoder.onnx...");
        let text_encoder =
            session()?.commit_from_file(model_dir.join("text_encoder.onnx"))?;
        eprintln!("[poing] Loading decoder_model_merged.onnx...");
        let decoder =
            session()?.commit_from_file(model_dir.join("decoder_model_merged.onnx"))?;
        eprintln!("[poing] Loading encodec_decode.onnx...");
        let encodec_decode =
            session()?.commit_from_file(model_dir.join("encodec_decode.onnx"))?;
        eprintln!("[poing] Loading tokenizer...");
        let tokenizer = tokenizers::Tokenizer::from_file(model_dir.join("tokenizer.json"))
            .map_err(|e| e.to_string())?;

        eprintln!("[poing] All models loaded");
        Ok(Self {
            text_encoder,
            decoder,
            encodec_decode,
            tokenizer,
        })
    }

    fn generate(
        &mut self,
        prompt: &str,
        progress_callback: impl Fn(f32),
    ) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let mut rng = rand::thread_rng();

        // Step 1: Tokenize prompt (add_special_tokens=true to append T5 EOS token)
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| e.to_string())?;
        let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let text_seq_len = token_ids.len();

        let input_ids = Array2::from_shape_vec((1, text_seq_len), token_ids)?;
        let attention_mask = Array2::from_shape_vec((1, text_seq_len), attention)?;

        // Step 2: Text encode (conditional)
        let cond_hidden = {
            let input_ids_tensor = Tensor::from_array(input_ids.clone())?;
            let attn_tensor = Tensor::from_array(attention_mask.clone())?;
            let outputs = self.text_encoder.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attn_tensor,
            })?;
            outputs["last_hidden_state"]
                .try_extract_array::<f32>()?
                .to_owned()
        };

        // Step 3: CFG setup -- stack conditional + unconditional (zeros)
        // Both Python and JS reference implementations use zeros for unconditional
        // hidden states and attention mask (not a T5 encoding of empty string).
        let cond_hidden_3d = cond_hidden
            .view()
            .into_dimensionality::<ndarray::Ix3>()
            .unwrap()
            .to_owned();
        let uncond_hidden = Array3::<f32>::zeros(cond_hidden_3d.raw_dim());

        let encoder_hidden_states = ndarray::concatenate(
            Axis(0),
            &[cond_hidden_3d.view(), uncond_hidden.view()],
        )?;

        let uncond_attn = Array2::<i64>::zeros(attention_mask.raw_dim());
        let encoder_attention_mask = ndarray::concatenate(
            Axis(0),
            &[attention_mask.view(), uncond_attn.view()],
        )?;

        // Step 4: Build delay pattern manually
        // Codebook k has delay k. With 1 BOS token at position 0:
        //   CB k is active (generates) at positions (1+k), (2+k), (3+k), ...
        //   Positions 0..k are PAD for CB k, position k is where BOS sits
        // Total sequence length for the delayed representation:
        let total_seq_len = MAX_LENGTH; // 1500 positions (0..1499)
        let total_codebook_rows = 2 * NUM_CODEBOOKS; // 8 (CFG batch)

        // Collected tokens: [total_codebook_rows, total_seq_len]
        // Initialize all to PAD
        let mut all_tokens = Array2::from_elem((total_codebook_rows, total_seq_len), PAD_TOKEN);
        // Set BOS at the start position for each codebook (position 0)
        for r in 0..total_codebook_rows {
            all_tokens[[r, 0]] = BOS_TOKEN;
        }

        // Step 5: Autoregressive decoder loop
        let batch_size = 2usize;
        let mut decoder_cache: HashMap<String, ArrayD<f32>> = HashMap::new();
        let mut encoder_cache: HashMap<String, ArrayD<f32>> = HashMap::new();

        for layer in 0..NUM_LAYERS {
            decoder_cache.insert(
                format!("past_key_values.{}.decoder.key", layer),
                ArrayD::zeros(IxDyn(&[batch_size, NUM_HEADS, 0, HEAD_DIM])),
            );
            decoder_cache.insert(
                format!("past_key_values.{}.decoder.value", layer),
                ArrayD::zeros(IxDyn(&[batch_size, NUM_HEADS, 0, HEAD_DIM])),
            );
            encoder_cache.insert(
                format!("past_key_values.{}.encoder.key", layer),
                ArrayD::zeros(IxDyn(&[batch_size, NUM_HEADS, 0, HEAD_DIM])),
            );
            encoder_cache.insert(
                format!("past_key_values.{}.encoder.value", layer),
                ArrayD::zeros(IxDyn(&[batch_size, NUM_HEADS, 0, HEAD_DIM])),
            );
        }

        let mut next_tokens = Array2::from_elem((total_codebook_rows, 1), BOS_TOKEN);

        // Generate for (MAX_LENGTH - 1) steps (positions 1..MAX_LENGTH-1)
        let num_gen_steps = total_seq_len - 1;

        for step in 0..num_gen_steps {
            let use_cache = step > 0;

            let mut inputs: Vec<(
                std::borrow::Cow<'_, str>,
                ort::session::SessionInputValue<'_>,
            )> = Vec::new();

            inputs.push((
                "encoder_attention_mask".into(),
                Tensor::from_array(encoder_attention_mask.clone())?.into(),
            ));
            inputs.push((
                "input_ids".into(),
                Tensor::from_array(next_tokens.clone())?.into(),
            ));
            inputs.push((
                "encoder_hidden_states".into(),
                Tensor::from_array(encoder_hidden_states.clone())?.into(),
            ));

            for layer in 0..NUM_LAYERS {
                let dk = format!("past_key_values.{}.decoder.key", layer);
                let dv = format!("past_key_values.{}.decoder.value", layer);
                let ek = format!("past_key_values.{}.encoder.key", layer);
                let ev = format!("past_key_values.{}.encoder.value", layer);

                inputs.push((
                    dk.clone().into(),
                    Tensor::from_array(decoder_cache[&dk].clone())?.into(),
                ));
                inputs.push((
                    dv.clone().into(),
                    Tensor::from_array(decoder_cache[&dv].clone())?.into(),
                ));
                inputs.push((
                    ek.clone().into(),
                    Tensor::from_array(encoder_cache[&ek].clone())?.into(),
                ));
                inputs.push((
                    ev.clone().into(),
                    Tensor::from_array(encoder_cache[&ev].clone())?.into(),
                ));
            }

            let cache_flag = Array1::from_vec(vec![use_cache]);
            inputs.push((
                "use_cache_branch".into(),
                Tensor::from_array(cache_flag)?.into(),
            ));

            let outputs = self.decoder.run(inputs)?;

            let logits = outputs["logits"].try_extract_array::<f32>()?.to_owned();

            // Update decoder KV caches
            for layer in 0..NUM_LAYERS {
                let dk = format!("past_key_values.{}.decoder.key", layer);
                let dv = format!("past_key_values.{}.decoder.value", layer);
                let pdk = format!("present.{}.decoder.key", layer);
                let pdv = format!("present.{}.decoder.value", layer);
                decoder_cache.insert(
                    dk,
                    outputs[pdk.as_str()].try_extract_array::<f32>()?.to_owned(),
                );
                decoder_cache.insert(
                    dv,
                    outputs[pdv.as_str()].try_extract_array::<f32>()?.to_owned(),
                );
            }

            // Update encoder KV caches (only on first step)
            if !use_cache {
                for layer in 0..NUM_LAYERS {
                    let ek = format!("past_key_values.{}.encoder.key", layer);
                    let ev = format!("past_key_values.{}.encoder.value", layer);
                    let pek = format!("present.{}.encoder.key", layer);
                    let pev = format!("present.{}.encoder.value", layer);
                    encoder_cache.insert(
                        ek,
                        outputs[pek.as_str()].try_extract_array::<f32>()?.to_owned(),
                    );
                    encoder_cache.insert(
                        ev,
                        outputs[pev.as_str()].try_extract_array::<f32>()?.to_owned(),
                    );
                }
            }

            // Sample next tokens from logits
            let logits_3d = logits.into_dimensionality::<ndarray::Ix3>()?;
            let cond_logits = logits_3d.slice(s![..NUM_CODEBOOKS, .., ..]).to_owned();
            let uncond_logits = logits_3d
                .slice(s![NUM_CODEBOOKS.., .., ..])
                .to_owned();

            // CFG: guided = uncond + scale * (cond - uncond)
            let cfg_logits =
                &uncond_logits + GUIDANCE_SCALE * (&cond_logits - &uncond_logits);

            let mut sampled = vec![PAD_TOKEN; total_codebook_rows];
            for cb in 0..NUM_CODEBOOKS {
                let logit_slice = cfg_logits.slice(s![cb, 0, ..]);
                let token =
                    top_k_sample(logit_slice.as_slice().unwrap(), TOP_K, &mut rng);
                sampled[cb] = token;
                sampled[cb + NUM_CODEBOOKS] = token;
            }

            // Write sampled tokens into the delayed representation
            // Position in all_tokens is (step + 1) since step 0 produces position 1
            let pos = step + 1;
            if pos < total_seq_len {
                for r in 0..total_codebook_rows {
                    let cb = r % NUM_CODEBOOKS;
                    let delay = cb; // codebook k has delay k
                    if pos > delay {
                        // This codebook is active at this position
                        all_tokens[[r, pos]] = sampled[r];
                    }
                    // else: position is before this codebook's active region, stays PAD
                }

                // Prepare next input: the tokens we just wrote
                next_tokens = Array2::zeros((total_codebook_rows, 1));
                for r in 0..total_codebook_rows {
                    next_tokens[[r, 0]] = all_tokens[[r, pos]];
                }
            }

            progress_callback(step as f32 / num_gen_steps as f32);
        }

        progress_callback(1.0);

        // Step 6: Undelay -- align codebooks by removing delay offsets
        // CB k's first generated token is at position (1 + k) in all_tokens.
        // Aligned timestep t maps to all_tokens[cb, 1 + cb + t].
        // Number of aligned timesteps: total_seq_len - 1 - (NUM_CODEBOOKS - 1)
        let aligned_len = total_seq_len - 1 - (NUM_CODEBOOKS - 1);
        let mut audio_codes_flat = vec![0i64; NUM_CODEBOOKS * aligned_len];
        for cb in 0..NUM_CODEBOOKS {
            for t in 0..aligned_len {
                let src_col = 1 + cb + t;
                if src_col < total_seq_len {
                    let val = all_tokens[[cb, src_col]]; // Use conditional batch (rows 0..4)
                    audio_codes_flat[cb * aligned_len + t] =
                        if val == PAD_TOKEN { 0 } else { val };
                }
            }
        }

        // Step 7: EnCodec decode
        // Input shape: [1, batch_size, 4, chunk_length]
        let codes_shape = [1usize, 1, NUM_CODEBOOKS, aligned_len];
        let codes_tensor = Tensor::from_array((codes_shape, audio_codes_flat))?;
        let decode_outputs = self.encodec_decode.run(ort::inputs! {
            "audio_codes" => codes_tensor,
        })?;

        let audio_values = decode_outputs["audio_values"]
            .try_extract_array::<f32>()?
            .to_owned();

        let samples: Vec<f32> = audio_values.iter().copied().collect();
        Ok(samples)
    }
}

fn top_k_sample(logits: &[f32], k: usize, rng: &mut impl Rng) -> i64 {
    let k = k.min(logits.len());

    let mut indexed: Vec<(usize, f32)> =
        logits.iter().enumerate().map(|(i, &v)| (i, v)).collect();
    indexed
        .sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed.truncate(k);

    let max_logit = indexed[0].1;
    let exps: Vec<f32> = indexed.iter().map(|(_, v)| (v - max_logit).exp()).collect();
    let sum: f32 = exps.iter().sum();
    let probs: Vec<f32> = exps.iter().map(|e| e / sum).collect();

    let dist = WeightedIndex::new(&probs).unwrap();
    let chosen = dist.sample(rng);
    indexed[chosen].0 as i64
}

/// Generate audio from a text prompt using a MusicGen ONNX model.
///
/// Returns mono f32 samples at 32 kHz.
pub fn generate_from_text(
    prompt: &str,
    model_dir: &Path,
    progress_callback: impl Fn(f32),
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let mut pipeline = MusicGenPipeline::load(model_dir)?;
    pipeline.generate(prompt, progress_callback)
}

/// Generate audio from a text prompt combined with input audio using a MusicGen ONNX model.
///
/// Returns mono f32 samples at the model's native sample rate (typically 32kHz).
pub fn generate_from_audio(
    _prompt: &str,
    _input_audio: &[f32],
    _model_dir: &Path,
    _progress_callback: impl Fn(f32),
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    todo!("MusicGen audio-to-audio inference not yet implemented")
}
