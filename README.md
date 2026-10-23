# Poing

An AI-native music workstation. Drop in a song and Poing shows how it's built: stems, sections, chords, lyrics. Then you can replace any part of it, per stem, per section or entirely, from a prompt and/or a reference sample. You play and shape the result with instruments and effects (Makepad Splash audio shaders, CLAP, VST3, AU), yourself or through an AI agent.

Native and fast on macOS, Windows and Linux. Built on [Makepad](https://github.com/makepad/makepad) 2.0, with local open-weight models (YuE2, ACE-Step, MiniMax-Music3, BS-RoFormer and others).

**Status:** early development (October 2026), Windows/CUDA first. The plan is in [docs/PLAN.md](docs/PLAN.md). The earlier Poing, a MusicGen VST3/CLAP plugin, is archived at tag [`legacy-musicgen-plugin`](https://github.com/gijzelaerr/poing/tree/legacy-musicgen-plugin).

## Try it

```
cargo run --release -p poing -- path/to/song.mp3
```

- **Start:** the start screen has three ways in. *Split a song*: drop a song on it. *Remix a song*: say how ("80s synthpop, drum machine") and drop the song on that card; once Poing understands it, ACE-Step makes three remixes (close, looser, free; about a minute) as takes to compare, and with a section selected *Use the playing remix here* takes that section from the remix, so remixes combine per section. *Make a new song*: say what it is ("a summer song about the farm, in Dutch"); Claude Code writes the title, style, tempo, key and lyrics, ACE-Step 1.5 (with its 5 Hz planner) sings three versions (about 3 minutes for 2:30 songs), you compare them and *Use this version*, which then gets split and analysed. New songs go to `Music/Poing` (or `POING_SONGS`); *Songs* in the top bar returns to the start screen.
- **Understand:** opening a song creates `song.poing/` next to it and analyses it in the background (uv, `sidecar/`): Demucs stems, SheetSage2 sections/key/chords/score, Whisper lyrics. About 2.5 minutes on an RTX 4070 Ti.
- **Describe:** type a style in the top bar and press Enter. Poing renders a whole-song cover take with native YuE2 from the song's own score and lyrics, so the chords, melody and words stay. About 45 s. A `lyrics.txt` in the bundle (hand-corrected, `[Verse]`-tagged) wins over the transcription.
- **Repaint:** select a section first and Describe repaints just that range with ACE-Step 1.5 (about 30 s), blended in over a second of original on each side. Needs an [ACE-Step-1.5](https://github.com/ace-step/ACE-Step-1.5) checkout next to this repository (or `POING_ACESTEP`).
- **Lanes:** select a lane (or drag a region on it) and Describe works on that part:
  - "less echo on the voice" splits the voice from its reverb (UVR de-reverb, about 40 s); *Blend* sets how much echo stays.
  - "add tight 80s disco drums ..." plays a new part with ACE-Step lego, keeps just that instrument and fits it into the lane's gaps (about 75 s).
  - anything else ("funky slap bass", "warm rhodes keys") plays the lane's part again as that instrument, level-matched, starting on a downbeat (about 2 minutes).
- **Effects:** select a lane and describe an effect ("tape saturation", "a slapback delay"; with nothing selected, "... on the whole mix" for the master). Claude Code (`claude` on PATH, your own login) writes it as a Makepad Splash audio shader; Poing compiles and auditions it and sends problems back, up to three rounds. Its knobs appear when you click its chip; edit `fx/*.splash` and it recompiles live.
- **Plugins (Windows):** name an installed VST3 plugin in Describe ("Raum on the vocals", "pro-q"; with nothing selected it goes on the master). It plays in the lane's chain next to shader effects, with its main knobs in the inspector, bypass, undo and a second instance for Export. *Open its window* (on the plugin's chip, or a MIDI lane played by a plugin) shows the plugin's own editor; whatever you change there (presets, wavetables, every parameter) is saved with the song in `plugins/<id>.state` and used by Export. Plugins run in Poing's process for now (a crashing plugin takes Poing down) and lanes are not yet delay-compensated.
- **MIDI parts:** with nothing selected (or a section), "add a funky 80s disco bassline" has Claude Code write a part from the song's key, chords and sections, on its beat grid. It becomes a MIDI lane played by Poing's built-in bass, keys, lead or drums (Splash instruments), or by an installed VST3 instrument named in the same Describe ("add funky disco chords played by Serum 2"). Select the lane to open its piano roll: click a note to move it, drag its end to lengthen, click an empty spot to draw one, Delete removes it, Ctrl+Z undoes. With the lane selected, its built-in instrument's knobs (cutoff, decay, ...) show under it and are saved with the song; "play it with FM8" swaps its instrument.
- **Ask in words:** with nothing selected, a request Poing doesn't map to one action itself ("vocals a bit louder, bass a bit quieter", "the song needs a jazzy upright bass under it") goes to Claude Code, which works on the song with the tools below and reports what it changed; Ctrl+Z undoes the whole pass. It needs `claude` and Makepad's `makepad-remote` (on PATH or `POING_MAKEPAD_REMOTE`); without them, such requests make whole-song remixes as before.
- **Agents:** Poing exposes its tools (`song`, `describe`, `add_part`, `set_lane`, `effect`, `take`, `combine`, `transport`, `export`; tools that start a job answer when it is done) as a Makepad AI service, so an agent can work on the open song the way a person does. For Claude Code, add Makepad's MCP bridge once: `claude mcp add poing -- makepad-remote mcp poing.exe --no-builtins` (`makepad-remote` is built from the Makepad checkout). Then ask, e.g., "take every chorus from Remix 3, the second verse from Remix 1, vocals up 2 dB, export": that took Claude 41 s.
- **Tabs:** every song you open gets a tab under the top bar (up to six). Click one to switch: while playing, the other song carries on at the same moment, so songs and versions compare by ear. Opening a song that is already open switches to its tab; + goes to the start screen. Jobs keep running for songs in background tabs (their tab shows …).
- **Takes:** chips above the lanes switch takes on and off (A/B); one per lane or range, layers stack. *Keep* opens a song take as a new song, bakes a lane take into its lane, or turns a layer into a lane.

YuE2 needs Makepad's `yue2_render` (built with CUDA; set `POING_YUE2_RENDER`) and the `m-a-p/YuE2-3B` and `m-a-p/YuE2-Vae` weights in the Hugging Face cache (or `POING_YUE2_MODEL` / `POING_YUE2_VAE`). `POING_MUTE=1` keeps test instances silent.

## Namesake

Named after the classic 1992 gabber track [**"Poing"** by Rotterdam Termination Source](https://open.spotify.com/track/71B21mv9KQXepeyf3ejhI9?si=392e03d6c0e243d2), a defining record of the Dutch hardcore scene built around a single iconic bouncing synthesizer sound.

## License

Code: MIT (see [LICENSE](LICENSE)). Model weights are downloaded separately and carry their own licences. Some are non-commercial, for example YuE2 (CC BY-NC 4.0).
