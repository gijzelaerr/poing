# Poing: agent notes

AI-native music workstation on Makepad 2.0. **Read [docs/PLAN.md](docs/PLAN.md) first:** concepts (Song → Sections → Stems → Takes), principles, workflow, architecture and milestones.

## Ground rules

- **Fewer controls, not more.** Selection drives the one inspector, there's one Describe field, nothing is destructive (everything is a take), and there are no modal dialogs. Check new UI against PLAN.md §2 before adding a button.
- **Audio thread:** no locks, no allocation, no syscalls. Bounded channels in, snapshots out. `Arc` payloads whose last drop happens off the audio thread. The device clock is the transport master.
- **Makepad:** a pinned git dependency on the `work` branch. Upstream rewrites history, so every pin is tagged `poing-pin-YYYYMMDD` in `gijzelaerr/makepad`. Fixes go upstream as small PRs to `makepad/makepad:work`. Follow Makepad's `AGENTS.md` for Splash/widget conventions.
- **Model weights and reference audio never go in git.** `xtask` downloads them, with licence notices.
- **Verify in the real app**, not just builds: run with `--remote` (Makepad remote control) and `MAKEPAD_HIDE_WINDOWS=1` for agent runs, and set `POING_MUTE=1` so test instances make no sound.
- **Every milestone has an acceptance test** from the two reference songs (PLAN.md §6). A milestone is done when its test passes.

## Build and run

```sh
cargo build --release -p poing          # the app (Makepad 2.0 comes in as a pinned git dependency)
cargo test --release --workspace        # headless tests (model, …)
target/release/poing path/to/project.json
target/release/poing --remote …         # Makepad remote control: snap / click / grab / gq
```

Layout: `app/` (Makepad app: arrangement timeline, inspector, live mix, render; `poing-plughost` runs one VST3 plugin in a process of its own and must sit next to `poing.exe`; `sounds.rs` finds NKS presets and sounds saved from Poing, so a MIDI part names a sound rather than a bare plugin), `model/` (`poing-model`: project.json types + tests), `engine/` (`poing-engine`: native models in a helper process, so poing never links CUDA).

**Native analysis.** `poing-engine` runs stems (BS-RoFormer, plus a Mel-Band RoFormer vocal for the lyrics), beats (Beat This!) and lyrics (Whisper large-v3, Q8_0) on Makepad's ggml runtime; the app starts it (`POING_ENGINE`, else the model manager's `engine-cuda` pack, else next to `poing.exe`) and then runs only SheetSage2 (`poing_analyze.py --only structure`) in Python. Without the engine, its models or a GPU it falls back to the full Python analysis (`POING_NATIVE=0` forces that). Setup on the CUDA PC:

```sh
MAKEPAD_CUDA_ROOT="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.4" cargo build --release -p poing-engine   # without it: CPU-only stub, analysis falls back to Python
target/release/poing-engine install --accept-licence   # ~3.2 GB into ~/.makepad/weights; BS-RoFormer weights are academic-use only, Mel-Band vocals not cleared for commercial use
target/release/poing-engine status
``` A `project.json` comes from the Python prep step in claude-space `projects/music-gen/build_project.py` until M2 brings import into the app.

**Installer and models** ([docs/INSTALL.md](docs/INSTALL.md)). `app/src/paths.rs` is the one resolver: dev overrides (`POING_SIDECAR`, `POING_ACESTEP`, `POING_UV`), then `sidecar/` and `uv.exe` beside an installed `poing.exe`, then the model manager's packs (`app/models.json`, `app/src/models.rs`, in `%LOCALAPPDATA%\Poing\models` or `POING_MODELS`). A build run from the repository still uses the repository's `sidecar/` and `../ACE-Step-1.5`. The installer's `poing-engine` is CPU-only (CI has no CUDA), so on Windows the GPU engine and the native analysis weights are packs too (`engine-cuda`, `native-analysis`); `packaging/build-engine-cuda.sh` rebuilds the engine pack (fat binary for sm_75 to sm_120 plus PTX) whenever `engine/` or the Makepad pin changes. To build the Windows installer (CI does this on a `v*` tag, `.github/workflows/release.yml`):

```sh
cargo build --release --workspace
# the pinned uv from release.yml (UV_VERSION, UV_SHA256), unzipped into target/packaging/uv/
cargo install cargo-packager --version 0.11.8 --locked
cargo packager -p poing --release      # target/release/poing_<version>_x64-setup.exe, ~21 MB
```

## Context

Earlier hands-on work that this plan comes from (pipelines, measurements, model notes) is in `gijzelaerr/claude-space` under `projects/music-gen/`. The prototype app is `apps/daw` on branch `daw` of `gijzelaerr/makepad`.
