use poing_core::model::OnnxModel;
use std::path::Path;

fn main() {
    let model_dir = Path::new("models/musicgen-small");

    for name in [
        "text_encoder.onnx",
        "decoder_model_merged.onnx",
        "encodec_decode.onnx",
        "build_delay_pattern_mask.onnx",
    ] {
        let path = model_dir.join(name);
        println!("=== {} ===", name);
        match OnnxModel::load(&path) {
            Ok(m) => {
                println!("Inputs:");
                for input in m.session.inputs() {
                    println!("  {} : {:?}", input.name(), input.dtype());
                }
                println!("Outputs:");
                for output in m.session.outputs() {
                    println!("  {} : {:?}", output.name(), output.dtype());
                }
            }
            Err(e) => {
                eprintln!("  Failed to load: {}", e);
            }
        }
        println!();
    }
}
