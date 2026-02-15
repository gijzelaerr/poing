use ndarray::{Array1, Array2, ArrayD, IxDyn, s};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

fn main() {
    let model_dir = Path::new("models/musicgen-small");

    // Load tokenizer and text encoder
    let tokenizer = tokenizers::Tokenizer::from_file(model_dir.join("tokenizer.json")).unwrap();
    let mut text_encoder =
        Session::builder().unwrap().commit_from_file(model_dir.join("text_encoder.onnx")).unwrap();

    // Tokenize
    let prompt = "upbeat electronic dance music";
    let encoding = tokenizer.encode(prompt, false).unwrap();
    let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    println!("Token IDs: {:?}", token_ids);
    println!("Token count: {}", token_ids.len());

    // Text encode
    let text_seq_len = token_ids.len();
    let input_ids = Array2::from_shape_vec((1, text_seq_len), token_ids).unwrap();
    let attention_mask = Array2::from_shape_vec(
        (1, text_seq_len),
        encoding.get_attention_mask().iter().map(|&m| m as i64).collect(),
    ).unwrap();

    let outputs = text_encoder.run(ort::inputs! {
        "input_ids" => Tensor::from_array(input_ids).unwrap(),
        "attention_mask" => Tensor::from_array(attention_mask).unwrap(),
    }).unwrap();
    let hidden = outputs["last_hidden_state"].try_extract_array::<f32>().unwrap();
    println!("\nEncoder hidden shape: {:?}", hidden.shape());
    println!("Encoder hidden[0,0,:5]: {:?}", hidden.slice(s![0, 0, ..5]));

    // Build delay pattern mask
    let mut mask_model =
        Session::builder().unwrap().commit_from_file(model_dir.join("build_delay_pattern_mask.onnx")).unwrap();

    let mask_input = Array2::from_elem((8, 16), 2048i64);
    let pad_tok = Array1::from_vec(vec![2048i64]);
    let max_len = Array1::from_vec(vec![1500i64]);

    let mask_out = mask_model.run(ort::inputs! {
        "input_ids" => Tensor::from_array(mask_input).unwrap(),
        "pad_token_id" => Tensor::from_array(pad_tok).unwrap(),
        "max_length" => Tensor::from_array(max_len).unwrap(),
    }).unwrap();

    let mask = mask_out["delay_pattern_mask"].try_extract_array::<i64>().unwrap();
    let edited = mask_out["input_ids_edited"].try_extract_array::<i64>().unwrap();
    println!("\nDelay pattern mask shape: {:?}", mask.shape());
    println!("Input IDs edited shape: {:?}", edited.shape());

    // Print first few columns of the mask for the first 4 rows (conditional batch)
    println!("\nDelay pattern mask (first 4 rows, first 20 cols):");
    for r in 0..4.min(mask.shape()[0]) {
        let row: Vec<i64> = (0..20.min(mask.shape()[1]))
            .map(|c| mask[[r, c]])
            .collect();
        println!("  CB {}: {:?}", r, row);
    }

    // Print first few columns of input_ids_edited
    println!("\nInput IDs edited (first 4 rows, first 20 cols):");
    for r in 0..4.min(edited.shape()[0]) {
        let row: Vec<i64> = (0..20.min(edited.shape()[1]))
            .map(|c| edited[[r, c]])
            .collect();
        println!("  CB {}: {:?}", r, row);
    }

    // Count mask values
    let mut neg_one = 0;
    let mut zeros = 0;
    let mut positive = 0;
    for &v in mask.iter() {
        if v == -1 { neg_one += 1; }
        else if v == 0 { zeros += 1; }
        else { positive += 1; }
    }
    println!("\nMask value counts: -1={}, 0={}, positive={}", neg_one, zeros, positive);

    // Show where -1 values start and end for each CB
    for r in 0..4.min(mask.shape()[0]) {
        let first_neg = (0..mask.shape()[1]).find(|&c| mask[[r, c]] == -1);
        let last_non_neg = (0..mask.shape()[1]).rev().find(|&c| mask[[r, c]] != -1);
        println!("  CB {}: first -1 at col {:?}, last non-(-1) at col {:?}", r, first_neg, last_non_neg);
    }

    // Quick 5-step decoder test to check logits
    let mut decoder =
        Session::builder().unwrap().commit_from_file(model_dir.join("decoder_model_merged.onnx")).unwrap();

    let batch = 2usize;
    let num_heads = 16;
    let head_dim = 64;
    let num_layers = 24;

    let next_tokens = Array2::from_elem((8, 1), 2048i64);
    let enc_hidden = ArrayD::<f32>::zeros(IxDyn(&[2, text_seq_len, 768]));
    let enc_attn = Array2::from_elem((2, text_seq_len), 1i64);

    let mut inputs: Vec<(std::borrow::Cow<'_, str>, ort::session::SessionInputValue<'_>)> = Vec::new();
    inputs.push(("encoder_attention_mask".into(), Tensor::from_array(enc_attn).unwrap().into()));
    inputs.push(("input_ids".into(), Tensor::from_array(next_tokens).unwrap().into()));
    inputs.push(("encoder_hidden_states".into(), Tensor::from_array(enc_hidden).unwrap().into()));

    for layer in 0..num_layers {
        inputs.push((format!("past_key_values.{}.decoder.key", layer).into(),
            Tensor::from_array(ArrayD::<f32>::zeros(IxDyn(&[batch, num_heads, 0, head_dim]))).unwrap().into()));
        inputs.push((format!("past_key_values.{}.decoder.value", layer).into(),
            Tensor::from_array(ArrayD::<f32>::zeros(IxDyn(&[batch, num_heads, 0, head_dim]))).unwrap().into()));
        inputs.push((format!("past_key_values.{}.encoder.key", layer).into(),
            Tensor::from_array(ArrayD::<f32>::zeros(IxDyn(&[batch, num_heads, 0, head_dim]))).unwrap().into()));
        inputs.push((format!("past_key_values.{}.encoder.value", layer).into(),
            Tensor::from_array(ArrayD::<f32>::zeros(IxDyn(&[batch, num_heads, 0, head_dim]))).unwrap().into()));
    }

    inputs.push(("use_cache_branch".into(), Tensor::from_array(Array1::from_vec(vec![false])).unwrap().into()));

    let dec_out = decoder.run(inputs).unwrap();
    let logits = dec_out["logits"].try_extract_array::<f32>().unwrap();
    println!("\nDecoder logits shape: {:?}", logits.shape());
    println!("Logits[0,0,:10]: {:?}", logits.slice(s![0, 0, ..10]));
    println!("Logits[4,0,:10]: {:?}", logits.slice(s![4, 0, ..10]));

    // Check KV cache shape
    let dk0 = dec_out["present.0.decoder.key"].try_extract_array::<f32>().unwrap();
    let ek0 = dec_out["present.0.encoder.key"].try_extract_array::<f32>().unwrap();
    println!("\nDecoder KV cache shape: {:?}", dk0.shape());
    println!("Encoder KV cache shape: {:?}", ek0.shape());

    // Check logits statistics
    let logits_slice = logits.slice(s![0, 0, ..]);
    let max_val = logits_slice.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min_val = logits_slice.iter().cloned().fold(f32::INFINITY, f32::min);
    let argmax = logits_slice.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
    println!("\nLogits[0] stats: min={:.4}, max={:.4}, argmax={}", min_val, max_val, argmax);
}
