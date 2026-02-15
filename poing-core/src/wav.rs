use std::path::Path;

/// Write mono f32 samples to a WAV file at the given path.
pub fn write_wav(
    samples: &[f32],
    sample_rate: u32,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Write mono f32 samples to a WAV file in a temp directory, returning the path.
pub fn write_wav_temp(
    samples: &[f32],
    sample_rate: u32,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join("poing_generated.wav");
    write_wav(samples, sample_rate, &path)?;
    Ok(path)
}
