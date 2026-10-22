# Poing

An AI-native music workstation. Drop in a song and Poing shows how it's built: stems, sections, chords, lyrics. Then you can replace any part of it, per stem, per section or entirely, from a prompt and/or a reference sample. You play and shape the result with instruments and effects (Makepad Splash audio shaders, CLAP, VST3, AU), yourself or through an AI agent.

Native and fast on macOS, Windows and Linux. Built on [Makepad](https://github.com/makepad/makepad) 2.0, with local open-weight models (YuE2, ACE-Step, MiniMax-Music3, BS-RoFormer and others).

**Status:** early development (October 2026), Windows/CUDA first. The plan is in [docs/PLAN.md](docs/PLAN.md). The earlier Poing, a MusicGen VST3/CLAP plugin, is archived at tag [`legacy-musicgen-plugin`](https://github.com/gijzelaerr/poing/tree/legacy-musicgen-plugin).

## Try it

```
cargo run --release -p poing -- path/to/song.mp3
```

- **Understand:** opening a song creates `song.poing/` next to it and analyses it in the background (uv, `sidecar/`): Demucs stems, SheetSage2 sections/key/chords/score, Whisper lyrics. About 2.5 minutes on an RTX 4070 Ti.
- **Describe:** type a style in the top bar and press Enter. Poing renders a whole-song cover take with native YuE2 from the song's own score and lyrics, so the chords, melody and words stay. About 45 s. A `lyrics.txt` in the bundle (hand-corrected, `[Verse]`-tagged) wins over the transcription.
- **Repaint:** select a section first and Describe repaints just that range with ACE-Step 1.5 (about 30 s), blended in over a second of original on each side. Needs an [ACE-Step-1.5](https://github.com/ace-step/ACE-Step-1.5) checkout next to this repository (or `POING_ACESTEP`).
- **Lanes:** select a lane (or drag a region on it) and Describe works on that part:
  - "less echo on the voice" splits the voice from its reverb (UVR de-reverb, about 40 s); *Blend* sets how much echo stays.
  - "add tight 80s disco drums ..." plays a new part with ACE-Step lego, keeps just that instrument and fits it into the lane's gaps (about 75 s).
  - anything else ("funky slap bass", "warm rhodes keys") plays the lane's part again as that instrument, level-matched, starting on a downbeat (about 2 minutes).
- **Takes:** chips above the lanes switch takes on and off (A/B); one per lane or range, layers stack. *Keep* opens a song take as a new song, bakes a lane take into its lane, or turns a layer into a lane.

YuE2 needs Makepad's `yue2_render` (built with CUDA; set `POING_YUE2_RENDER`) and the `m-a-p/YuE2-3B` and `m-a-p/YuE2-Vae` weights in the Hugging Face cache (or `POING_YUE2_MODEL` / `POING_YUE2_VAE`). `POING_MUTE=1` keeps test instances silent.

## Namesake

Named after the classic 1992 gabber track [**"Poing"** by Rotterdam Termination Source](https://open.spotify.com/track/71B21mv9KQXepeyf3ejhI9?si=392e03d6c0e243d2), a defining record of the Dutch hardcore scene built around a single iconic bouncing synthesizer sound.

## License

Code: MIT (see [LICENSE](LICENSE)). Model weights are downloaded separately and carry their own licences. Some are non-commercial, for example YuE2 (CC BY-NC 4.0).
