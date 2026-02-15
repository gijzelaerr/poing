# Poing

A neural network audio plugin that loads ONNX models (starting with Facebook's MusicGen) and generates audio directly inside your DAW. It supports two modes: text-to-audio (generate from a text prompt) and audio-to-audio (record incoming audio, then generate based on prompt + audio). Generated audio can be dragged from the plugin into the DAW timeline.

Built with Rust using [nih-plug](https://github.com/robbert-vdh/nih-plug) (VST3 + CLAP), [ort](https://github.com/pykeio/ort) (ONNX Runtime), and [Makepad](https://github.com/makepad/makepad) (GPU-accelerated GUI).

## Status

Early development. The plugin scaffold compiles and loads as a VST3/CLAP with stereo pass-through audio.

## Building

```
cargo xtask bundle poing-plugin --release
```

## Project Structure

```
poing-core/             Model loading, inference, audio buffers, WAV export
poing-plugin/           nih-plug Plugin (audio processing, VST3/CLAP export)
poing-makepad-bridge/   Embeds Makepad GUI into DAW parent window
poing-gui/              Makepad UI (prompt input, waveform, controls)
```

## Namesake

This project is named after and inspired by the classic 1992 gabber track [**"Poing"** by Rotterdam Termination Source](https://open.spotify.com/track/71B21mv9KQXepeyf3ejhI9?si=392e03d6c0e243d2) -- a defining record of the Dutch hardcore/gabber scene built around a single iconic bouncing synthesizer sound.

## License

MIT
