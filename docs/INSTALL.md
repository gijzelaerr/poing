# Installing Poing: a small app, models on demand

Status: design, 2026-10-23. No code yet. Related: [PLAN.md](PLAN.md) (M8 signing, "model weights never in git, downloaded on first use", `xtask` model download).

**Goal.** One small installer per OS. Poing opens and plays, mixes and hosts plugins with nothing else installed. Each AI feature downloads what it needs the first time you use it, after you say yes, and can be removed again.

## 1. What Poing runs today

Read from `app/src/audio.rs`, `fx_writer.rs`, `midi_writer.rs` and `sidecar/` at `4f7fd79`. Sizes are measured on the dev PC (HF cache, `ACE-Step-1.5/checkpoints`). Licences come from the Hugging Face API (`cardData.license`) or the project's LICENSE, 2026-10-23.

### Models and tools

| Poing feature (job) | How it runs | Weights | On disk | Weights licence | GPU |
| --- | --- | --- | --- | --- | --- |
| **Understand: stems** (`Analyze`) | `uv run --project sidecar poing_analyze.py` | Demucs `htdemucs_ft` (torch hub) | 0.32 GB | MIT (repo licence) | CUDA or CPU (slow) |
| **Understand: sections, key, chords, beats, score** (`Analyze`) | same, SheetSage2 via `transformers` `trust_remote_code` | `m-a-p/SheetSage2` + its encoder `m-a-p/MERT-v2-FullSong` | 0.22 + 2.4 GB | **CC BY-NC 4.0** (both) | CUDA or CPU |
| **Understand: lyrics** (`Analyze`) | same, Whisper via `transformers` | `openai/whisper-large-v3` | 2.9 GB (fp32 files; fp16 needs ~1.6 GB) | Apache-2.0 | CUDA fp16 or CPU |
| **Repaint a section** (`Repaint`) | `uv run --project ACE-Step-1.5 acestep_repaint.py` | `acestep-v15-turbo` + VAE + Qwen3-Embedding-0.6B | 4.5 + 0.3 + 1.2 GB | MIT; Qwen3 Apache-2.0 | **CUDA only** (hardcoded) |
| **New song / remix from prompt** (`acestep_song.py`, in progress in the main checkout) | same | turbo as above, plus `acestep-5Hz-lm-1.7B` when "thinking" | +3.5 GB | MIT | CUDA only |
| **Add / replay a part** (`Layer`) | `acestep_lego.py`, then `poing_layer.py` (Demucs cleanup) | `acestep-v15-base` (+ VAE, Qwen3) and Demucs | 4.5 GB extra | MIT | CUDA only |
| **Less echo** (`Dereverb`) | `uv run --project sidecar/dereverb` (audio-separator) | `dereverb_mel_band_roformer_anvuew_sdr_19.1729.ckpt` | 0.91 GB | **GPL-3.0** (anvuew) | CUDA or CPU |
| **Whole-song cover from the score** (`Generate`) | native `yue2_render` (Makepad), `POING_YUE2_RENDER` | `m-a-p/YuE2-3B` + `YuE2-Vae` | 6.8 + 0.5 GB | **CC BY-NC 4.0** | CUDA, ~10.1 GB VRAM peak |
| **Effects, MIDI parts** (`WriteFx`, `WriteMidi`) | `claude -p` (Claude Code, headless), `POING_CLAUDE_MODEL` | none (cloud) | — | proprietary, user's own account | none |
| **MP3 export** (`Render`) | `ffmpeg` on PATH or `FFMPEG`; WAV without it | — | ~100 MB | LGPL or GPL, by build | none |

Not used today, though PLAN.md lists them: Beat This! (beats come from SheetSage2), Basic Pitch, BS-RoFormer, MiniMax-Music3. Lyrics writing by Claude is being built in the main session; it will use the same `claude -p` path.

### Python environments

| Env | Pins | Size on disk |
| --- | --- | --- |
| `sidecar/` | Python 3.11, torch 2.8.0 cu126, numpy 1.24 (SheetSage2), demucs 4.0.1 | up to 6.7 GB |
| `sidecar/dereverb/` | Python 3.12, torch 2.8.0 cu126, numpy 2 (audio-separator) | up to 6.9 GB |
| `ACE-Step-1.5/` (external git checkout) | its own lock | up to 6.2 GB |

uv hardlinks from its cache, so the real total is lower than the sum; torch CUDA alone is ~2.5 GB to download and ~4–5 GB unpacked.

### What already exists natively in Makepad

Checked in `libs/ai/models` (fork `gijzelaerr/makepad`, `work` checkout at `287750a`; Poing pins `poing-pin-20261021`).

| Model | Native crate | State |
| --- | --- | --- |
| YuE2-3B | `music` (`yue2_render`) | Works on Windows CUDA: 44 s per song. Used by Poing today |
| ACE-Step 1.5 | `music` (`ace*.rs`) | **XL-turbo text2music only**: Qwen3 text encoder, 8-step DiT, Oobleck VAE, bf16. No repaint, cover or lego (no source-audio path), no CLI. Needs `acestep-v15-xl-turbo-diffusers`, 11.1 GB |
| MiniMax-Music3 | `music` | Native; licence not checked |
| BS-RoFormer 4-stem | `stems` | Native. Weights academic-use only (MUSDB18-HQ). A `melband` module exists too: check whether it loads the de-reverb checkpoint |
| Whisper | `speech` | Native (plus Silero VAD, Kokoro TTS) |
| Beat This!, Basic Pitch | `beats`, `notes` | Native. Basic Pitch Apache-2.0; Beat This! licence to check |
| SheetSage2/MERT, Demucs | — | None |

**How the native CUDA build ships.** `libs/ai/cuda/build.rs` builds kernels for one arch (whatever `nvidia-smi` reports, sm_89 here) and links `cudart` (0.5 MB), `cublas` (55 MB) and `cublasLt` (493 MB) dynamically. A release needs a fatbin for sm_75–sm_120 plus PTX, and those three DLLs next to the engine. On macOS the same crates run on Metal with no extra runtime.

### Licence summary

- **Clean for a product:** ACE-Step 1.5 (MIT), Qwen3-Embedding (Apache), Whisper (Apache), Demucs (MIT), Basic Pitch (Apache).
- **Non-commercial:** YuE2, SheetSage2, MERT. So today's "Understand" (sections, key, chords, score) is non-commercial, not just YuE2.
- **Academic only:** BS-RoFormer 4-stem weights.
- **GPL-3.0 weights:** the de-reverb roformer. Downloading it at runtime is fine; bundling it would put GPL terms on the bundle.

Personal use is fine for all of them. The default feature set must be the clean list; the rest is opt-in with a notice.

## 2. Design

### a. The installer

| OS | Format | Tool | Notes |
| --- | --- | --- | --- |
| Windows | NSIS `.exe`, per-user (`%LOCALAPPDATA%\Programs\Poing`, no admin) | `cargo-packager` (also does dmg and AppImage) | Unsigned until M8: SmartScreen "More info → Run anyway". Signing certificate at M8 |
| macOS | `.dmg` with `Poing.app` (arm64) | `cargo-packager` | Unsigned: right-click → Open. Developer ID + notarisation at M8 |
| Linux | AppImage (x86_64) | `cargo-packager` | No signing needed |

**Contents:** `poing` (33 MB release build today), the sidecar scripts with their `pyproject.toml`/`uv.lock` (KBs), a pinned `uv` binary (32 MB, MIT/Apache), licence notices. **Target: under 50 MB download, under 100 MB installed.** No Python, no CUDA, no weights, no ffmpeg. CI on the hosted runners builds all three on tag push.

The app finds its pieces through one resolver: dev overrides first (`POING_SIDECAR`, `POING_ACESTEP`, `POING_YUE2_*`, the HF cache), then the installed resources folder, then the model manager. Today's `env!("CARGO_MANIFEST_DIR")` fallbacks only work in a dev build and go behind the dev override.

### b. The model manager

**Registry.** A `models.toml` compiled into the app, versioned with it. Features name packs, packs name files:

```toml
[pack.acestep-turbo]
title = "ACE-Step 1.5 (turbo)"
licence = "MIT"                      # shown before download; NC packs carry a warning
commercial = true
needs = { gpu = "cuda", vram_gb = 8, runtime = "py-acestep" }   # or runtime = "native-cuda"
files = [
  { url = "https://huggingface.co/ACE-Step/Ace-Step1.5/resolve/<commit>/acestep-v15-turbo/model.safetensors",
    path = "acestep/acestep-v15-turbo/model.safetensors", size = 4_790_000_000, sha256 = "…" },
  # vae, Qwen3-Embedding …
]

[feature.repaint]   packs = ["acestep-turbo"]
[feature.new-song]  packs = ["acestep-turbo"]  optional = ["acestep-lm"]
[feature.add-part]  packs = ["acestep-base", "demucs"]
[feature.analyse]   packs = ["demucs", "whisper"]  optional = ["sheetsage2"]   # NC, opt-in
```

URLs pin a commit, never `main`. That also pins the code SheetSage2 runs through `trust_remote_code`. Runtimes (a Python env, the CUDA pack) are packs too, so one "download" covers everything a feature needs.

**Where it goes.** `%LOCALAPPDATA%\Poing\models` · `~/Library/Application Support/Poing/models` · `$XDG_DATA_HOME/poing/models`. Movable in Settings (to a big D: drive, say) and with `POING_MODELS`. A pack already in the user's HF cache, with a matching sha256, is hardlinked or copied instead of downloaded.

**Download.** One background job per pack, in the jobs worker: HTTP range requests into `file.part` (resumes after a crash or a dropped line), sha256 check, then an atomic rename. Progress, speed and ETA in the job strip; cancel keeps the `.part`. A pack counts as installed only when a `pack.json` (registry version, hashes, accepted licence) is written last.

**Checks before download:**
- *Disk:* free space ≥ pack size + 10 %, else "Needs 6.0 GB, D: has 3.1 GB free. Change the models folder?"
- *GPU:* NVIDIA via `nvidia-smi` (name, VRAM, driver version); Metal on macOS. A pack whose `needs` can't be met shows why, and offers CPU only where that works (Demucs, Whisper).
- *Driver:* the CUDA 12.6 torch wheels and the CUDA 13 native engine each need a minimum NVIDIA driver. Check it and say "Update your NVIDIA driver" rather than failing mid-run.

**Licence.** First download of a pack shows its licence in one line plus a link. Non-commercial and GPL packs add "Personal, non-commercial use only". Acceptance goes in `pack.json`.

**Uninstall.** Settings → Models lists each pack with its size, the features it powers, and Remove. The app uninstaller asks "Also remove downloaded models (14.2 GB)?", default no.

**In the UI.** Every AI action is in one of four states:

| State | Looks like |
| --- | --- |
| Ready | normal |
| Needs download | normal, with a small download badge. On use: *"Making songs needs ACE-Step (6.0 GB, MIT licence). Download now?"* — Download · Not now · Details |
| Downloading | greyed, with progress; queued Describe requests run when it finishes |
| Can't run here | greyed, the reason on hover: "Needs an NVIDIA GPU with 8 GB" |

The first-run start screen shows no model prompt. Poing is useful without any. Import analyses automatically (PLAN.md principle 5), so without the analysis packs a song opens as one lane with a "Download analysis (3.2 GB + runtime)" badge instead of an error.

### c. Running the models

**End state: native in Makepad, no Python.** One engine binary (`poing-engine`, like `yue2_render` today) per GPU family, started by the jobs worker. Keeping it out of `poing.exe` means the app starts without CUDA DLLs, and a crashed model doesn't take the mix down. The CUDA runtime (`cudart`, `cublas`, `cublasLt`, ~550 MB; NVIDIA lists them as redistributable, verify the current EULA) is its own pack, shared by all native models. macOS needs nothing extra (Metal).

**Interim: the bundled `uv` builds the Python env on demand.** The installer ships `uv`, the scripts and their lockfiles. Installing a `py-*` runtime pack runs `uv sync --frozen` with `UV_CACHE_DIR`, `UV_PYTHON_INSTALL_DIR` and the env inside the models folder, so nothing touches the user's own Python. ACE-Step stops being a sibling checkout: a Poing-owned `sidecar/acestep/pyproject.toml` pins `ace-step @ git+…@<commit>`, and checkpoints come from the models folder. Check how ACE-Step's `initialize_service(project_root)` finds `checkpoints/`; a junction is the fallback.

| Cost or risk | Size or effect | Mitigation |
| --- | --- | --- |
| torch CUDA download | ~2.5 GB per env, 4–5 GB unpacked; three envs today | Merge `sidecar` and `dereverb` once the numpy pins allow it. uv's hardlinks share torch between envs |
| Driver too old for cu126 | `torch.cuda.is_available()` false, silent CPU fallback (hours) | Driver check up front; refuse CPU for ACE-Step |
| No NVIDIA GPU / macOS | ACE-Step sidecars hardcode `device="cuda"` | Mark those features "can't run here". MPS for ACE-Step is a later option |
| Antivirus | Scans ~30k files on first sync (minutes); may quarantine DLLs | Run the sync as a visible job with a log; tell the user to exclude the models folder if it stalls |
| Long paths (Windows, 260 chars) | Some wheels fail to unpack | Keep the models root short; detect and suggest `D:\PoingModels` |
| Proxy, offline, PyPI/PyTorch index down | Sync fails | Retry with resume. The error names the host that failed |
| A half-built env | Next run crashes on import | Build into `env.tmp`, rename when `uv sync` and a smoke import pass |

### d. Claude Code is optional

Claude Code is Anthropic's proprietary CLI and the user's own subscription. Poing never bundles it.

- **Detect:** `claude --version` on PATH (plus the usual install dirs) at startup and when the user opens a Claude feature. Missing → "Effects and MIDI parts are written by Claude Code. Install it (link), then sign in." Everything else works.
- **Signed in?** Learn it from the first real call: an auth error from `claude -p` becomes "Sign in to Claude Code" with a button that opens a terminal running `claude`. Don't spend a request on a ping.
- `POING_CLAUDE_MODEL` stays as the override. A later option is the Claude API with the user's own key, for people without Claude Code.

### e. Migration to native, in order

| # | Move | Why this order | Effort |
| --- | --- | --- | --- |
| 1 | **Model manager + bundled uv** (interim) | Needed by every feature, native or not | 1–2 weeks |
| 2 | **YuE2 as a pack**: ship `yue2_render` with a multi-arch fatbin and the CUDA pack | Already native and already used; proves the CUDA pack. Opt-in, NC | days |
| 3 | **Whisper native** (`speech` crate) | Native port exists, Apache weights, takes lyrics out of the Python analysis env | ~1 week |
| 4 | **ACE-Step native repaint/cover/lego** in `music`: the source-latent and mask path, a CLI, quantised weights for 8–12 GB cards | The default generation model; removes the largest env. Do it with Makepad upstream | 4–8 weeks |
| 5 | **Stems**: port HTDemucs natively, or find permissively licensed roformer weights | Native BS-RoFormer exists, but its weights are academic-only | 2–4 weeks |
| 6 | **Structure without SheetSage2**: Beat This! (native) for beats and downbeats, plus key, chords and sections from a clean model | SheetSage2 and MERT are NC with no native port | open; research first |
| 7 | **De-reverb**: try the native `melband` module on the anvuew checkpoint | GPL weights, downloaded separately either way | ~1 week if it loads |

## 3. Recommendation

Ship the interim path first and move features native one at a time:

1. A small unsigned installer (under 50 MB) from CI on all three OSes.
2. A model manager with a pinned registry and resumable, verified downloads. It asks per feature, with size and licence.
3. The bundled `uv` builds pinned Python envs in Poing's own folder, so there are no git checkouts and no env vars.
4. Default features use only clean weights (ACE-Step, Whisper, Demucs). YuE2 and SheetSage2 analysis are opt-in, marked "personal, non-commercial".

Go native in the order above: YuE2 packaging and Whisper are cheap, ACE-Step's source-audio tasks are the big one. Being honest about it: the biggest licence gap is **Understand**, not generation. Sections, key and chords rest on NC weights, so step 6 needs research before a product exists.

### First milestone: I1 "Install on a clean PC" (Windows)

- CI builds an unsigned per-user NSIS installer under 50 MB on tag push.
- `models.toml` with the `acestep-turbo` pack and a `py-acestep` runtime pack, pinned and with sha256.
- Model manager: disk, GPU and driver checks; resumable download; licence prompt; Settings → Models with Remove.
- The repaint and new-song actions show "Needs download (6.0 GB)" and prompt on first use.
- **Done when:** on a second Windows PC (or a clean user account) with an NVIDIA GPU and no Python, uv, git checkouts or env vars, you install Poing and open a song. Analysis offers its own download; "Not now" leaves the song playable. You mark a range, repaint it, and the take plays. Then you remove the pack and the disk space comes back.
