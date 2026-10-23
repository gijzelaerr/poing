# Poing: licences

Poing is MIT-licensed (`LICENSE`). This installer also contains:

| What | Licence | Text |
| --- | --- | --- |
| Makepad (UI, audio, the model downloader), compiled into `poing.exe` and `poing-engine.exe` | MIT | `licences/makepad-LICENSE` |
| `uv` 0.13.1 by Astral, which builds the Python environments on demand | MIT or Apache-2.0 | `licences/uv-LICENSE-MIT`, `licences/uv-LICENSE-APACHE` |
| `vst3` and `vst3-host` crates (VST3 plugin hosting) | MIT / MIT or Apache-2.0 | crates.io |

## Downloaded later, only after you say yes

The installer has no Python, CUDA, model weights or ffmpeg. Each AI feature
asks before it downloads what it needs (Models, top right, lists them all and
removes them again). They go to `%LOCALAPPDATA%\Poing\models` (or
`POING_MODELS`) and carry their own licences, shown before the download:

| Pack | Licence |
| --- | --- |
| ACE-Step 1.5 turbo, song planner, runtime (ACE-Step code; PyTorch) | MIT; Qwen3-Embedding Apache-2.0; PyTorch BSD-3-Clause |
| Analysis runtime: Demucs, Whisper, SheetSage2 and MERT | MIT, Apache-2.0, **CC BY-NC 4.0** (SheetSage2 and MERT: personal, non-commercial use only) |

Claude Code (effects, MIDI parts, the Describe bar's agent) is not part of
Poing: it is Anthropic's CLI, installed and signed in to by you.
