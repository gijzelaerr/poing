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
| GPU engine (Windows, NVIDIA): `poing-engine.exe` built for CUDA (the CUDA runtime linked in), with NVIDIA's `cublas64_13.dll` and `cublasLt64_13.dll` | Poing and Makepad MIT; the CUDA runtime and the two DLLs under the NVIDIA CUDA Toolkit EULA (`licences/NVIDIA-CUDA-EULA.txt`, also in the pack) |
| Native analysis models: BS-RoFormer, Mel-Band RoFormer, Beat This!, Whisper large-v3 | BS-RoFormer weights **academic use only**; Mel-Band RoFormer **not cleared for commercial use**; Beat This! MIT; Whisper Apache-2.0 |
| Poing's language model: Qwen3.5 4B | Apache-2.0 |

**The CUDA runtime.** NVIDIA's EULA (CUDA Toolkit 13.4, §1.1.1 and
Attachment A) lets an application ship the CUDA runtime (`cudart.dll`,
`cudart_static.lib`) and `cuBLAS`/`cuBLASLt` (`cublas.dll`, `cublasLt.dll`,
and versioned names such as `cublas64_13.dll`) in object code. CUDA 13's
`cudart.lib` links the runtime into `poing-engine.exe`, so the pack ships
only the two cuBLAS DLLs. Its conditions: the application has material functionality
beyond those files, they are accessed only by the application (here only by
`poing-engine.exe`, which loads them from its own folder), the files are not
modified, and the terms Poing is distributed under stay consistent with the
EULA. The GPU engine pack is that: Poing's engine and the two unmodified
DLLs in one zip, downloaded and unpacked by Poing's model manager.
cuBLAS contains code under BSD-style licences from third parties; their
notices are in the EULA text, which ships in the pack.

Claude Code (effects, MIDI parts, the Describe bar's agent) is not part of
Poing: it is Anthropic's CLI, installed and signed in to by you.
