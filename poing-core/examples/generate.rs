use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let prompt = if args.len() > 1 {
        args[1..].join(" ")
    } else {
        "upbeat electronic dance music".to_string()
    };

    let model_dir = Path::new("models/musicgen-small");
    let output_path = Path::new("output.wav");

    println!("Prompt: {}", prompt);
    println!("Model: {}", model_dir.display());
    println!("Output: {}", output_path.display());
    println!();

    let samples = poing_core::musicgen::generate_from_text(&prompt, model_dir, |progress| {
        let pct = (progress * 100.0) as u32;
        if pct % 5 == 0 {
            eprint!("\rGenerating... {}%", pct);
        }
    })
    .expect("generation failed");

    eprintln!("\rGenerating... done!    ");
    println!("Generated {} samples ({:.1}s at 32kHz)", samples.len(), samples.len() as f64 / 32000.0);

    poing_core::wav::write_wav(&samples, 32000, output_path).expect("failed to write WAV");
    println!("Wrote {}", output_path.display());
}
