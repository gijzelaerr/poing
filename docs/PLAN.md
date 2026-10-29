# Poing: plan

An AI-native music workstation. Poing shows how a song is built, lets you replace any part of it per stem, per section or entirely, from a prompt and/or reference audio, and hosts instruments and effects (Splash audio shaders, CLAP, VST3, AU) that you or an AI agent can play. Native, fast, realtime where physics allows, on macOS, Windows and Linux. Built on [Makepad](https://github.com/makepad/makepad) 2.0 (`work` branch).

Status: M0-M2 done; M3 song covers done (2026-10-22): "It's Autumn" Beatles cover from Describe to playable take in 46 s, Keep re-analyses it; section repaint with ACE-Step in 30 s with seamless 1 s cross-fades. M4 done (2026-10-22): per-lane takes from Describe (less echo, add a part, play a part again), several takes at once, blend and Keep; the disco v4 echo fix and drum layer rebuilt in Poing without scripts. M5 done (2026-10-22): Describe an effect on a lane and Claude writes it as a Splash audio shader, checked before it plays (tape saturation on the bass: 38 s, 2 rounds, 1586 ops/sample); knobs, bypass, live recompile; interface redesigned. M6 phase 1 (2026-10-22, Windows): VST3 effects on lanes and the master from Describe, knobs, bypass, undo, render instances; MIDI lanes (2026-10-22): Claude writes a part from the song's key, chords and sections (a disco octave bass: 590 notes in 2.5 minutes), played by built-in Splash instruments or a VST3 instrument (Serum 2, FM8, Vital, Massive X; Vital and Massive X were silent until their output bus was activated and, for Massive X, its engine given time to start), with a piano roll for editing. Three ways to start (2026-10-23): split a song, remix it from a prompt (three ACE-Step covers in ~45 s, combined per section), or make a new song from a prompt (Claude writes the sheet and lyrics, ACE-Step text2music with its LM planner sings three versions in ~3 minutes; pick one and it is analysed). M7 started (2026-10-23): Poing is a Makepad AI service (8 tools); Claude Code over `makepad-remote mcp` combined remixes per section, raised the vocals and exported a mix in 41 s with no clicks. M6 phase 2 (2026-10-23, Windows): latency compensation (FabFilter Pro-Q 3 in linear phase is 107 ms late; with it on the bass a render matches the song without it to −139 dB) and plugins in their own process, with their editor windows ("Run in its own process": a killed helper is reloaded with the plugin's state in ~50 ms while Poing keeps playing). Still to do: macOS/Linux plugins. The previous Poing (a MusicGen VST3/CLAP plugin) is archived at tag [`legacy-musicgen-plugin`](https://github.com/gijzelaerr/poing/tree/legacy-musicgen-plugin).

---

## 1. Why this shape

**The old idea was a generator inside someone else's DAW.** A plugin can't see the song (stems, sections, lyrics, chords), so it can only add audio blindly. The problem is the reverse: the tool that knows the song should host the plugins, not the other way round.

**What exists today, and what's missing**

| Kind | Examples | What it does well | Gap Poing fills |
| --- | --- | --- | --- |
| Generative "studios" | [Suno Studio](https://www.veena.studio/blog/suno-studio-daw-review) (browser, up to 12 stems, MIDI export), Udio (inpainting; [claims unverified](https://ucstrategies.com/news/udio-v4-ai-music-editing-with-inpainting-stem-separation-2026/)) | Generate whole songs, split stems | Reviewers call Suno Studio "a generator wearing a DAW costume". Editing is non-deterministic and cloud-only, so most pros [pair it with Reaper or Logic](https://undetectr.com/blog/suno-studio-review). No local models, no plugin hosting |
| AI inside classic DAWs | [Logic Pro 12](https://blog.dubspot.com/apple-logic-pro-12-mac-ipad): Session Players, Stem Splitter, Chord ID (Jan 2026) | AI as a *musician in the band* that follows the song's chords. A deterministic DAW around it | Closed and macOS-only. No prompt-to-section or per-stem regeneration |
| Agents driving DAWs | Claude + Ableton/Splice [MCP connectors](https://sonicfield.org/ai-agents-enter-the-studio-the-mcp-turn-in-audio), community [REAPER MCP](https://enterprisedna.co/directories/mcp/twelvetake-studios-reaper-mcp) servers | Natural-language control of an existing DAW | Bolted on from outside: the agent sees only what the DAW exposes |
| Open DAW engines in Rust | [Firewheel](https://docs.rs/crate/firewheel/latest) (game audio, "not a DAW engine"), Meadowlark/Dropseed (dormant since late 2025) | Realtime-safe graphs | No maintained DAW engine to adopt. We build the graph on Makepad's audio stack |

**Research Poing builds on**

- **Per-stem generation and editing:** [MusicGen-Stem](https://arxiv.org/abs/2501.01757) (bass/drums/other, editing one stem conditioned on the rest), [Stemphonic](https://www.researchgate.net/publication/400662017_Stemphonic_All-at-once_Flexible_Multi-stem_Music_Generation) (all stems at once, Feb 2026), [Instruct-MusicGen](https://arxiv.org/pdf/2405.18386) (text-instructed edits). The intra- vs inter-stem editing split is from [arXiv 2411.12641](https://arxiv.org/pdf/2411.12641). How to measure that an edit preserved the rest: [arXiv 2512.14629](https://arxiv.org/pdf/2512.14629).
- **Models we already run locally:** YuE2-3B (score + lyrics → song, native in Makepad, 44 s per song on an RTX 4070 Ti), ACE-Step 1.5 (`cover`, `repaint`, `lego` = add one instrument track in context), MiniMax-Music3, BS-RoFormer stems, Beat This!, Basic Pitch, SheetSage2 (audio → ABC score), Whisper (lyrics). Proven in the [claude-space music-gen notes](https://github.com/gijzelaerr/claude-space) on two real songs.
- **Song understanding:** [All-In-One](https://github.com/mir-aidj/all-in-one) (beats, downbeats, labelled sections), plus Makepad `beats`/`notes`, SheetSage2 (key, chords, melody) and Whisper.
- **AI using effects:** [LLM2Fx](https://arxiv.org/pdf/2505.20770) (text → EQ/reverb parameters zero-shot, WASPAA 2025) and [LLM2Fx-Tools](https://arxiv.org/abs/2512.01559) (tool-calling Fx chains, ICLR 2026). [Text2FX](https://arxiv.org/pdf/2409.18847) (CLAP-guided). This is the pattern for "AI plays with plugins": parameters as tools.
- **Realtime generation:** [Magenta RealTime 2](https://pasqualepillitteri.it/en/news/4258/magenta-realtime-2-google-open-ai-music-model) (open weights, June 2026, ~200 ms control latency steered by text/audio/MIDI; reported for Apple Silicon, unverified elsewhere). A later "jam" mode, not the core.
- **Plugin formats:** the VST3 SDK has been [MIT since Oct 2025](https://sonicstate.com/news/2025/10/30/vst-3-now-available-under-mit-license). Rust hosting is fragmented: [clack](https://github.com/prokopyl/clack) (CLAP, low level), [rust-vst3-host](https://github.com/HelgeSverre/rust-vst3-host) (VST3, process isolation), [plughost](https://github.com/emulette/plughost) (VST3/CLAP/AU, v0.0.2). Makepad has dormant macOS AudioUnit discovery/instantiation code (`platform/hal/src/backend/apple/media/audio.rs`).
- **Interchange:** [DAWproject](https://www.bitwig.com/stories/bitwig-and-presonus-are-making-it-easy-to-share-projects-between-programs-271/) (Bitwig, Studio One, Cubase/Nuendo) carries tracks, audio, notes, automation and plugin state. It's how Poing hands a song to a classic DAW.

## 2. Principles

Apple's pro apps (Logic for iPad, Final Cut) are the reference: few concepts, direct manipulation, depth on demand.

1. **Four nouns:** Song → Sections → Stems → Takes. Everything else is a property of one of these.
2. **Selection drives everything.** Select a section, a stem, a region or the whole song, and the one inspector shows what you can do with *that*. No modes, no modal dialogs, no menus of AI features.
3. **One way to ask.** A single *Describe* field (text) that also accepts a dropped audio reference. "Make this chorus darker", "replace the drums with a breakbeat like this [drop file]".
4. **Nothing is destructive.** Every AI action and every edit produces a **take**. Takes stack on the lane, A/B with one key, and keep or discard. Unlimited undo over the whole project.
5. **Understand first.** On import Poing analyses automatically (stems, tempo/beats, sections, key/chords, lyrics, melody) and shows it as the song's structure. You never press "analyse".
6. **Honest time.** Mixing, playback, shaders and plugins are realtime. Generation is a job with a progress ring on the take it's making. You keep working while it renders, and the take appears when it's ready.
7. **Fast is a feature.** 120 fps UI, sample-accurate audio thread with no locks and no allocation, instant waveforms, work offloaded to workers. Mistakes are cheap to undo.
8. **Depth on demand.** Default views show the music. Knobs, parameters, the score and plugin UIs open from the inspector when you go looking.
9. **Agents are users too.** Everything a person can do is a tool on the app's MCP server, with the same undo and the same takes.

## 3. The workflow

```
 Import a song ─┐                                   ┌─ Export: stems, mix, mastered MP3, DAWproject
 or Generate ───┼─► Understand ─► Select ─► Describe ─► Takes ─► Mix / Play ─┘
 (prompt+ref)   │   (automatic)   (section,  (text +    (A/B,     (realtime: plugins,
                │                  stem,     reference   keep)     shaders, AI tweaks)
                └──────────────────── region, song)  audio)
```

| Step | What the user sees | Under the hood |
| --- | --- | --- |
| **Import** | Drop an audio file. Within seconds the waveform appears; stems, sections and chords fill in as they're found | BS-RoFormer stems, `allin1`-style structure, Beat This! grid, SheetSage2 score/key/chords, Whisper lyrics. Background jobs, results streamed into the project |
| **Generate** | Empty song + Describe ("90s gabber with a bouncy lead, 170 bpm") + optional reference audio | YuE2 or ACE-Step text2music. The result is imported and understood exactly like a dropped file |
| **Understand** | The arrangement: a section ribbon (Intro, Verse, Chorus…), stem lanes, a chord/lyric strip, the key. Click a section to hear it loop | The `project.json` model: sections, stems, regions and analysis |
| **Select** | Click a section, stem lane, region or Cmd-A. The inspector shows it with its Describe field | Selection = scope of the next action |
| **Describe** | Type and/or drop audio, then Return | Routed by scope (see below) |
| **Takes** | New takes appear stacked on the lane as they finish. Space plays, ↑/↓ switches take, Return keeps | Each take carries its prompt, model, seed, and the quality checks we built (lyric WER, key agreement, loudness) as a small badge |
| **Mix / Play** | Volume, mute/solo, a few smart effects per stem; plugin windows on demand; play an instrument from a MIDI keyboard | Realtime engine, Splash shaders, hosted CLAP/VST3/AU |
| **Export** | Share → mix (mastered), stems, DAWproject | Two-pass loudnorm, DAWproject writer |

**What "replace" means per scope** (honest mapping to today's models):

| Scope | Action | Model path |
| --- | --- | --- |
| Whole song | Restyle/cover, keep melody and lyrics | Score (SheetSage2) → YuE2 with new style; or ACE-Step `cover` |
| Section (all stems) | Regenerate or change this chorus | ACE-Step `repaint` on the section; YuE2 from the edited score region |
| One stem, whole song | New drums / bass / vocal performance | ACE-Step `lego` (add or replace one track in context; proven for our drum layer); MusicGen-Stem for bass/drums/other |
| One stem, one section | Fix a bad vocal line | Lego/repaint masked to the region; singing model (Makepad `ai/models/sing`) for a re-sung line from notes + lyrics |
| From a sample | "Like this" | Reference-audio conditioning (ACE-Step `reference_audio`/cover). YuE2 real-audio encoder (community, unverified) |
| Effects | "Warmer", "less echo" | LLM2Fx-style parameter tools over the stem's chain; a Splash shader written on the spot when no plugin fits |

## 4. Interface

One window, three zones, nothing else by default:

- **Arrangement (center):** section ribbon on top (coloured, labelled, draggable edges); stem lanes with waveforms; takes stack inside a lane (a folded count badge, unfolds on click). Playhead and loop follow the selected section.
- **Inspector (right, ~320 px):** title of the selection; the **Describe** field with drop target; context actions (≤ 4 visible: e.g. *New take*, *Keep*, *Solo*, *Effects*); the take badges. Depth (parameters, score, lyrics, plugin list) is behind disclosure triangles.
- **Transport (top bar):** play, position, tempo/key, the song's name, a jobs indicator (ring) that expands into the job list.

Keyboard first: Space play, L loop selection, ↑/↓ takes, Return keep, Delete remove take, Cmd-Z/Shift-Cmd-Z undo/redo, `/` focuses Describe, R records a MIDI keyboard into the selected MIDI lane (A to K play it when no MIDI keyboard is plugged in). Dark and light themes, following the system. Makepad Splash for the UI, styled like Amp's hardware-grade widgets but quieter.

## 5. Architecture

```
poing/
  app/            Makepad 2.0 app: UI (Splash), arrangement, inspector, transport
  engine/         realtime audio graph: lanes → takes → chains → buses → output.
                  Lock-free; command queue in, snapshots out; sample-accurate transport
  model/          project model: Song/Section/Stem/Take/Region/Chain, undo log,
                  project bundle (.poing = folder: project.json + audio), DAWproject export
  ai/             job system + model adapters (native Makepad crates first:
                  stems, beats, notes, yue2, ace, music3, sing; Python sidecar fallback
                  for models without a native port: SheetSage2, allin1, Whisper)
  plugins/        hosts: Splash audio shaders (in-process), CLAP (clack), VST3,
                  AU (macOS, Makepad AU code). Third-party plugins out-of-process
                  with shared-memory audio, so a crash never takes the session down
  mcp/            MCP server: project, selection, takes, every parameter as tools
  xtask/          bundling, model download, release packaging
```

- **Makepad as a git dependency**, pinned. Upstream rewrites `work`, so every pin is tagged in `gijzelaerr/makepad` (`poing-pin-YYYYMMDD`) to stay fetchable. Fixes we need upstream go as PRs to `makepad/makepad:work` (Rik merges; #1320–#1322 opened 2026-10-21).
- **Audio thread rules** (Makepad's own): no locks, no allocation, `Arc` payloads over bounded channels, the device clock is the transport master (`libs/score_play`).
- **Jobs:** one long-lived worker per GPU model family, FIFO, cancellable. Results land as takes with full provenance (model, prompt, seed, inputs).
- **Model weights:** never in git. Downloaded on first use into the user's model folder with licence notices (YuE2 CC BY-NC, BS-RoFormer academic-only, ACE-Step MIT).

## 6. Milestones

Each one ends with an acceptance test drawn from the two songs already made by hand: The Hamlets "It's Autumn" (1966 → Beatles-style) and Jean Paul van der Mey "Ik zoek een mooie rijke vrouw" (→ 80s disco remix).

| # | Milestone | Done when |
| --- | --- | --- |
| M0 | **Foundation.** Repo restart, Makepad 2.0 git dep (pinned, tagged), empty app window, CI green on macOS/Windows/Linux, headless test job | CI builds and runs headless tests on all three OSes |
| M1 | **Player.** Port `apps/daw` (stem lanes, sections, regions, live mix, render). `.poing` bundle | Open the mooie-rijke-vrouw stems; play, mute/solo, mark, save, render the v4 mix bit-identical to today |
| M2 | **Understand.** Import any audio → stems, beats, sections, key/chords, lyrics, automatically, streamed in | Drop the original "It's Autumn" MP3; sections and chords match the SheetSage2 transcription we did |
| M3 | **Takes + Describe.** Take model, A/B, keep; Describe field; whole-song cover (score → YuE2) and section repaint | Reproduce the Beatles-style "It's Autumn" cover from inside Poing in under 2 minutes |
| M4 | **Per-stem replace.** Lego/MusicGen-Stem per lane, region-masked edits, reference audio | Reproduce the disco v4 drum layer and the vocal-echo fix as takes, no scripts |
| M5 | **Shaders.** Splash audio shader slots per lane and bus, live recompile, param knobs; Claude writes a shader from a description | "Tape saturation on the bass" produces a working shader in one Describe |
| M6 | **Plugins.** CLAP host (clack), then VST3; out-of-process; floating editor windows; AU on macOS via Makepad's AU code | Vital (CLAP/VST3) plays from MIDI on all three OSes; a crash in a test plugin leaves Poing running |
| M7 | **Agents.** MCP server: project/selection/takes/params as tools; LLM2Fx-style "make it warmer" over hosted plugins | Claude, through MCP only, finishes a mix pass on the disco remix that the user keeps |
| M8 | **Share.** DAWproject export, mastered export, signed builds and installers | Open a Poing export in Bitwig/Studio One with stems, sections as markers, plugin state |
| Later | Live jam (Magenta RealTime 2), Rust-source plugins under Makepad's JIT, MilkDrop visuals from Amp | |

## 7. CI and quality

- **GitHub Actions** matrix (hosted runners only): `macos-latest` (arm64), `windows-latest`, `ubuntu-latest`. Build, `cargo test`, and a headless smoke test of the app (Makepad `--remote` + `MAKEPAD_HIDE_WINDOWS=1`; Linux under a virtual display). CPU-only: GPU kernels build without CUDA in CI.
- **GPU and model tests run by hand**, not in CI. `cargo xtask gpu-check` runs on the RTX 4070 Ti dev PC (Windows, CUDA): `yue2_render --check`, stem split, SNR gates as in Makepad PR #1322. Results are recorded per milestone.
- **Metal/AU tests by hand on the Mac** at each milestone (no Mac runner).
- **Golden tests:** the two reference songs' stems, scores and expected renders (hashes, SNR thresholds), with audio stored outside git and fetched by `xtask`.
- **Releases:** unsigned CI builds until M8. Then signed and notarised macOS `.dmg`, signed Windows installer, Linux AppImage (needs an Apple Developer ID and a Windows signing certificate).

## 8. Decisions (2026-10-21)

| Topic | Decision |
| --- | --- |
| First use | **Remixing existing songs**: M0–M4 come first (import → understand → replace sections/stems as takes) |
| CI | Hosted GitHub runners only. GPU tests by hand on the Windows/CUDA dev PC; Metal/AU by hand on the Mac |
| Signing | Later (M8); unsigned dev builds until then |
| Agent autonomy | May push to `main` once a milestone's checks pass, open Makepad PRs, install dev tooling, use CI freely |
| Makepad | `work` branch via a pinned git dependency; no commercial (VJ/Stage) code needed (Rik, 2026-10-21) |
| Licence | Code MIT; model weights downloaded separately under their own licences |

## 9. Risks

| Risk | Mitigation |
| --- | --- |
| Makepad `work` moves fast and rewrites history | Pin + tag; small upstream PRs; Rik is reachable and merges |
| Plugin GUI embedding in a Makepad window | Start with floating editor windows; embed later per platform |
| Plugin hosting libraries are immature (AU especially) | Own thin hosts over clack / VST3 SDK (MIT) / Makepad AU code; out-of-process from day one |
| Model quality varies per take (lyric WER 5–40 % across seeds) | Takes + automatic quality badges + best-of-N in the background |
| Licences of model weights (YuE2 CC BY-NC, BS-RoFormer academic) | Code MIT; weights downloaded separately with notices; MIT models (ACE-Step) as defaults where a product is ever sold |
| macOS/Metal paths can't be verified on the Windows dev box | A Mac test machine or self-hosted runner |

## Road to 1.0 (2026-10-23)

**0.1 (2026-10-24):** everything below as a first pre-release, so friends can try it. 1.0 waits until the open checks under "1.0 QA" are done.

1.0 means *a solid tool for us* (Gijs, Megan, Rik and friends): unsigned builds and non-commercial models are fine, and **every feature works without a subscription** (a local model by default, Claude Code optional).

| | Workstream | State |
|---|---|---|
| D | Job safety + Settings: closing warns while a job runs, interrupted requests offered again ("Make it again"), Settings panel (sound output, AI, compute, plugins, models) | done |
| B | Installer I1: Windows installer, model manager, bundled uv | done (#8) |
| C | Native lyrics on the MelBand vocal stem | done (#9) |
| L | Local AI: song sheets, MIDI parts, effects and the Describe assistant on a local Qwen model; Claude optional | done (#10) |
| A | Remote compute: UI on the Mac, AI and compute on a Windows node (`poing-node`, Makepad AI hub discovery and credentials) | done (#7) |
| G+K | Export: stems, MIDI files, DAWproject; DJ export (rekordbox XML with beatgrid, key, section cues) for rekordbox and the OMNIS-DUO | done (#6) |
| E | Arrangement (move/duplicate/delete sections, make it longer) and editing lyrics/chords by hand | done (#11) |
| F | MIDI keyboard: play and record into a lane, quantise, count-in, metronome | done (#12) |
| H | Plugins phase 2: latency compensation, plugins in a helper process | done (#13) |
| O | Orchestral parts: Kontakt 8 and BBC SO load (vst3-host patch); named sounds (NKS presets, sounds saved from a plugin's window); a strings role (held voiced notes, a dynamics curve sent as CC1, kept to the sound's range); keyswitch articulations; lane level automation; `listen`/`compare` hear VST3 plugins (second instances in offline mode); humanise; winds; Serum 2 and Vital presets as sounds (4,430 sounds here); Kontakt libraries not yet (an .nkm given as Kontakt's state takes minutes and comes up empty) | done |
| QA | End-to-end, then tag v1.0 | on the Windows PC: done (below); Gijs's checks left |

### 1.0 QA on the Windows PC (2026-10-24)

A hidden, muted Poing (its own settings folder) on a copy of It's Autumn, driven through makepad-remote, plus a poing-node on 127.0.0.1. All 186 tests pass.

| Flow | Result |
|---|---|
| Settings: AI | Poing's own model / Claude Code, kept in `settings.json` across a restart (`POING_LLM` still wins) |
| Settings: Compute | This PC or a node (paired or heard on the LAN); shows where jobs run |
| Describe bar, Claude | "vocals a bit louder, bass a bit quieter": +2 / −2 dB in ~30 s, one undo; it noticed a remix was playing over the stems |
| Describe bar, Poing's own model | the same in 13 s (its summary wrongly said "in the chorus") |
| Remix (3 versions) | ~25 s on this PC; 41 s through the node including sending 31 files |
| New song, Dutch prompt, own model | sheet in ~5 s, Dutch lyrics, 3 versions in 2.5 min |
| MIDI part, own model | keys part, 504 notes, checked in 1 round, 115 s (short stabs rather than held chords) |
| MIDI keyboard | one bar count-in, 4 injected notes recorded, quantised |
| Arrange | repeat a chorus 0.2 s (2:14.6 → 2:32.3), cut, undo back to 2:14.6 |
| Plugins | Pro-Q 3 in its own process: 1516 blocks, 0 missed, slowest 1.9 ms; its helper killed while playing: back in ~2 s, 1 block missed |
| Tabs | 6 switches while playing, no missed blocks, no helper left behind |
| Export | stems, DAWproject, MIDI, For DJ (2–3 tracks, grid, key, cues) |
| Node killed mid-remix | "The Windows PC is not reachable" at once, Poing carries on, no ACE-Step left running |

Fixed during QA: the Settings switches for AI and compute (they were text only); the DJ grid on a drifting band (stray downbeats made 105/150/166 bpm marks; now bars a few beats short where the analysis moves the bar keep the tempo, test on It's Autumn's real downbeats); stage directions under [Intro] in a new song's lyrics (ACE-Step would sing them); a plain message when poing-engine has no GPU support; a stale "lanes are not delay-compensated" line.

**Open for 1.0:**
- **The installer's poing-engine has no CUDA**, and on Windows Makepad's LLM runs only on CUDA, so an installed copy can't run Poing's own model. Fixed by the `engine-cuda` pack (2026-10-24): a fat `poing-engine.exe` (sm_75 to sm_120 plus PTX, Makepad pin `poing-pin-20261024`) and cuBLAS in one 463 MB zip on the release `engine-cuda-20261024`, a pack of `llm` and a GPU pack of `analysis`, with the native weights as a second pack (`native-analysis`). Tested on a fake clean PC (INSTALL.md). Hugging Face's per-connection throttle makes the native weights a ~3 h first download.
- **Gijs's checks:** the Mac UI against this PC (docs/REMOTE.md), the rekordbox import onto the OMNIS-DUO (PR #6 steps), the installer on a clean account, clicking inside a plugin's own window.
- Build note: `poing-engine` must be built with `MAKEPAD_CUDA_ROOT` (CLAUDE.md) or the local model and native analysis fall back / fail; a plain `cargo build` gives the CPU stub.

**After 1.0:** isolated plugins restart on every tab switch (in-process ones stay loaded); tool calls with unknown arguments are ignored instead of refused; the small model's chords are short stabs; long take names in DJ file names are cut with "…".

Later, not 1.0: signed installers, licence-clean default models, native ACE-Step cover/repaint/lego, AU/CLAP on macOS, live jam.