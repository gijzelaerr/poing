# Live: Poing played from the OMNIS-DUO

A first live mode. Two decks each play a Poing song with its stems. The decks are synced on Poing's beat grid. You play them from Gijs's AlphaTheta OMNIS-DUO, used as a USB MIDI controller:
- the pads trigger hot cues on the sections, mute stems and set beat loops;
- the jogs nudge and scratch;
- the mixer has a crossfader, EQ and filter;
- there are two Splash effects.

This page has two parts. Part 1 is the research: what the DUO sends and receives. Part 2 is how Poing uses it, and what to check on the real hardware.

## 1. What the OMNIS-DUO is to a computer

### Sources

| Source | What it gives |
| --- | --- |
| rekordbox 7.2.19's own controller map, `C:\Program Files\rekordbox\rekordbox 7.2.19\MidiMappings\OMNIS-DUO.midi.csv` (the same file as in 7.2.18) | **The MIDI map.** Every control rekordbox's Performance mode reads, with its MIDI message, and the LEDs it writes. It is AlphaTheta's file, so it is not copied into Poing; Poing's own map restates the numbers. |
| Its driver, `C:\Program Files\AlphaTheta\OMNIS-DUO\OMNIS-DUOAudio.inf` (installed by `rekordbox 7.2.19\drivers\OMNIS-DUO_1.000.exe`), and `HKLM\SOFTWARE\ASIO` | The audio side: USB `VID_2B73&PID_0048`, a WDM device "AlphaTheta OMNIS-DUO" with speaker and line endpoints, and an **"OMNIS-DUO ASIO"** driver (installed on glacier). |
| The other rekordbox maps (DDJ-FLX4, DDJ-400…) in the same folder | How to read the file: the FLX4's pads come out on channels 8/10, which matches the FLX4's published MIDI list. |
| Reviews: [Digital DJ Tips](https://www.digitaldjtips.com/reviews/alphatheta-omnis-duo-all-in-one-dj-system/), [We Are Crossfader](https://wearecrossfader.co.uk/blog/alphatheta-omnis-duo-review-walkthrough/), [MusicTech](https://musictech.com/reviews/controllers/alphatheta-omnis-duo-review-pioneer-dj), [AlphaTheta product page](https://alphatheta.com/en/product/all-in-one-dj-system/omnis-duo/indigo/) | The physical controls. 4 hot cue pads per deck with no mode buttons ("only one mode, Hot Cues"). A loop encoder. Two beat-jump buttons. A cue and a play button. A tempo fader. Jogs. A 3-band EQ, trim and channel fader per channel. A Color FX knob per channel (filter by default). Beat FX with a depth knob, on/off and beat ◀ ▶. A crossfader. A touchscreen for sync, master, quantize, slip and vinyl. |

I found no published MIDI implementation chart from AlphaTheta. Serato DJ Lite support was announced, but I found no Serato map for the DUO. The rekordbox csv is the primary source.

### How rekordbox uses it

- **Performance mode.** You plug the DUO's second USB-C port (not the power one) into a PC or Mac running rekordbox. The DUO is then a controller and the computer plays the music. Performance mode is free with the DUO's hardware unlock.
- **Two USB functions.** The DUO shows up as a **MIDI device** for the controls, mapped by `OMNIS-DUO.midi.csv`. It is also a **USB audio interface** for the sound, through its driver and ASIO, out of the DUO's master and headphone outputs. One review says rekordbox talks to it "in HID mode". Pioneer units often add a HID channel next to MIDI for the screen and jog displays. The csv shows that the controls themselves are MIDI.
- **Serato.** Not shipped yet, as far as I found.
- **Not plugged in yet.** The DUO has never been plugged into glacier: Windows has seen only a DDJ-REV1 from AlphaTheta (VID 2B73). So the MIDI port's exact name is still unknown. Poing matches any port whose name contains `OMNIS`.

### Reading the csv

Each row has a name, a `type`, an `input` code, four deck columns, an `output` code (the LED) and options.

- **`input` set, deck columns small numbers.** The number is added to the channel nibble.
  - Example: `PlayPause,…,9000,0,1,2,3` means deck 1 is `90 00` and deck 2 is `91 00`.
  - Example: `PAD1_HotCue,…,9000,5,6,7,8` means deck 1's pads are on `95` and deck 2's on `96`.
- **`input` empty, deck columns full codes.** Each deck has its own absolute message. Example: `EQHigh,…,,B402,B407` means deck 1 is `B4 02` and deck 2 is `B4 07`. The mixer is on MIDI channel 5 (`B4`).
- **Types.**
  - `Button` and `Pad`: a note-on with velocity 7F when pressed, and 00 (or a note-off) when let go. The FX buttons are CCs (`B4 72`).
  - `KnobSlider`: a 7-bit CC, 0–127.
  - `KnobSliderHiRes`: 14-bit, the MSB on CC n and the LSB on CC n+0x20. On the DUO only the tempo fader is hi-res. Its mixer is 7-bit: `B4 34` (FX select) would collide with the LSB of `B4 14`.
  - `JogRotate`, `Rotary`, `Difference`: relative values around 0x40, with 0x41 one step clockwise and 0x3F one step back.
  - `Indicator`: output only.
- **LEDs.** An `output` code means rekordbox lights that control by sending the same message back: velocity (or value) 7F for on and 00 for off.

### The controls (deck 1 / deck 2)

| Control | Message | Kind | LED |
| --- | --- | --- | --- |
| Play/Pause | `90 00` / `91 00` | button | yes |
| Cue | `90 01` / `91 01` | button | yes |
| Shift+Cue (to the start) | `90 05` / `91 05` | button | yes |
| Shift | `90 3F` / `91 3F` | button | |
| Pads 1–8 (hot cue) | `95 00`–`95 07` / `96 00`–`96 07` | pad | yes |
| Shift + pads 1–8 (delete hot cue) | `95 08`–`95 0F` / `96 08`–`96 0F` | pad | yes |
| ◀ / ▶ next to the pads (memory cue call; "beat jump" on the panel) | `90 0C` / `90 0B` (and `91`) | button | |
| Loop encoder turn (loop size) | `B0 2E` / `B1 2E` | relative | |
| Loop encoder press (auto loop on/off) | `90 43` / `91 43` | button | |
| Shift + loop encoder press (reloop/exit) | `90 44` / `91 44` | button | |
| Jog top, touched (scratch) | `B0 22` / `B1 22` | relative | |
| Jog side, or top in CDJ mode (pitch bend) | `B0 21`, `B0 26` / `B1 21`, `B1 26` | relative | |
| Jog touch | `90 20` / `91 20` (`90 48` with shift) | button | |
| Jog search (shift + turn) | `B0 29` / `B1 29` | relative | |
| Tempo fader | `B0 00` + `B0 20` / `B1 00` + `B1 20` | 14-bit | |
| Needle search (touch strip) | `B0 1C` / `B1 1C` | 0–16383 | |
| Load | `90 51` / `91 51` | button | yes |
| Touch panel: Sync, Master, Quantize, Slip, Vinyl, Master Tempo, Auto Cue | `90 1F`, `1E`, `09`, `2B`, `12`, `11`, `0F` (and `91`) | button | |
| Trim | `B4 01` / `B4 06` | 7-bit | |
| EQ High, Mid, Low | `B4 02`, `03`, `04` / `B4 07`, `08`, `09` | 7-bit | |
| Color FX (filter) | `B4 05` / `B4 0A` | 7-bit | |
| Channel fader | `B4 11` / `B4 12` | 7-bit | |
| Crossfader | `B4 0B` | 7-bit | |
| Crossfader curve | `B4 5F` | 7-bit | |
| Headphone cue | `B4 46` / `B4 47` | button (CC) | yes |
| Channel level meter | out: `B0 02` / `B1 02` | indicator | out only |
| Beat FX on/off | `B4 72` | button (CC) | yes |
| Beat FX depth | `B4 5B` | 7-bit | |
| Beat FX beat ◀ / ▶ | `B4 4C` / `B4 4D` | button (CC) | |
| Shift + Beat FX ◀ / ▶ (auto / tap) | `B4 78` / `B4 79` | button (CC) | |
| Shift + Beat FX on (FX select) | `B4 34` | button (CC) | yes |
| Browse turn / press / shift+press | `B4 4F` / `90 33` / `90 61`; shift+turn (zoom) `B4 60` | | |

Decks 3 and 4 (`92`, `93`, pads `97`, `98`, mixer `B4 0C`…) are in the csv, but the DUO is a two-deck unit.

## 2. Poing's live mode

### Using it

- **Starting it.** Open a song and press **Live** in the top bar. Both decks load the song in front. The hardware **Load** button loads the song in front onto that deck, so a version or remix opened in another tab can go on deck 2. **Live** again goes back to the song.
- **What a deck plays.** The song as it plays in its tab when loaded: the lanes at their levels and mutes (the muted "Original" stays out), regions, and the takes that are on (a whole-song remix replaces the lanes). Each stem can be muted on its own (with a whole-song remix on, the stem mutes do nothing: the remix is one piece of audio). Two copies of one song share their audio, so nothing is loaded twice. A deck that gets a song stops on its hot cue A.
- **Tempo.** Speed and pitch move together, like vinyl. There is no key lock yet.
- **Beat grid and hot cues.** These are the DJ export's own: `dj_grid` and `dj_cues` from `djexport.rs`. Hot cues A–D sit on the intro, the first chorus, the bridge or drop (else the last chorus), and the outro (else the last section). A song may have fewer than four. "It's Autumn" has no outro and its last section is chorus 3, so it gets A, B and C, and pad 4 stays dark.

### The pads, on four pads with no mode buttons

| Control | Poing |
| --- | --- |
| Pads 1–4 | Hot cues A–D (lit when the cue exists) |
| Shift + pads 1–4 | Mute or unmute vocals, drums, bass, other (lit while the stem plays) |
| Pads 5–8, if the DUO sends them | Beat loops of 1, 2, 4 and 8 beats (lit while that loop runs) |
| Loop encoder press | Beat loop on/off at the loop size (4 beats to start with) |
| Loop encoder turn | Halve or double the loop size (1/4 to 32 beats). A running loop changes with it |
| Shift + loop press | Reloop/exit: the same toggle |
| ◀ / ▶ | Jump to the previous or next section, keeping the beat |

- **A hot cue or section jump while playing** lands on the cue plus the current offset from the last beat. The deck keeps its beat, so a synced deck stays in sync. While stopped, it jumps exactly to the cue. A hot cue also starts the deck, as on a CDJ.
- **Cue works like a CDJ.**
  - Playing: back to the cue point, and stop.
  - Stopped away from the cue: the cue point moves here, on the nearest beat.
  - Stopped on the cue: plays while held.

### Jog, tempo, sync

- **Jog side turn: nudge.** Each step moves the deck 1 ms. It is spread over a few blocks at up to 5% speed, so you hear a pitch bend.
- **Jog top, touched: scratch-lite.** The platter drives the playhead at one revolution per 1.8 s of audio (33⅓ rpm, 720 steps a revolution, assumed). The playhead follows the hand with a short glide, backwards too. Let go and the deck plays on.
- **Tempo fader.** ±8%. The low end of the travel is −8% (to check on the hardware: the map has `invert`).
- **Sync** (the touch panel's button) makes the deck follow the other deck:
  - its rate is set so their local tempos match on Poing's beat grid;
  - a small continuous phase correction keeps their beats together (the grid can be piecewise, so the tempo is read where each deck is);
  - turning sync on for one deck turns it off for the other.

  While synced, the deck's tempo fader does nothing and the other deck's fader moves both.

### Mixer and effects

- **Channel fader and crossfader.** The channel fader is squared, so it is smooth near the top. The crossfader is constant power: both decks are at −3 dB in the middle.
- **Trim.** ±12 dB.
- **EQ.** A 3-band isolator, low under 250 Hz and high over 2.5 kHz. Fully left kills the band, the middle is flat, and fully right is +6 dB. It is native Rust biquads, whose coefficients change only when a knob does.
- **Color FX: filter.** Left of the middle is a low-pass (20 kHz down to 80 Hz) and right is a high-pass (20 Hz up to 6 kHz). A small dead zone in the middle is off.
- **Beat FX.** Two Splash audio shaders on the master, built and handed to the audio thread as an `FxRack` the way song effects are:
  - **Echo** (`app/live/echo.splash`): a beat-synced echo. Its `bpm` follows the master deck and its length is in beats (◀ ▶: 1/4, 1/2, 3/4, 1, 2). Turning it off lets the tail ring out.
  - **Reverb**: `allpass_reverb.splash`.

  Shift + FX on switches between them, the depth knob is their mix, and FX on/off is bypass.
- **Where it plays.** The two decks go to the same output as the song (the device chosen in Settings). Cueing in headphones needs the DUO's own USB audio as a 4-channel device. That is a later step.

### The map is a data file

- **Where it is.** The mapping is [`app/live/omnis-duo.map`](../app/live/omnis-duo.map), built into Poing. A copy in Poing's config folder overrides it, so a wrong control can be fixed without a rebuild: `%APPDATA%\Poing\omnis-duo.map` on Windows, `~/Library/Application Support/Poing/omnis-duo.map` on a Mac.
- **The format.** One control per line: `status data1 action [deck] [argument] [options]`. Example: `95 08 mute 1 vocals`. The status byte carries the channel. The options are:
  - `hires`: a 14-bit fader whose LSB comes on data1+0x20;
  - `invert`: turns a fader around;
  - `noled`: no LED written back.

  The action `ignore` marks a control the DUO has that Poing does not use (Shift, the touch panel's other buttons, browse…), so it is not logged.
- **LEDs.** Buttons and pads with state get their LED written back. Poing sends the same message with 7F or 00 whenever the state changes, and writes the channel meters to `B0 02` / `B1 02`. Poing writes only to MIDI outputs whose name contains `OMNIS`, so the Launchpad and Launchkey on glacier get nothing.
- **Unmapped messages.** Every note or CC from the DUO that the map does not know is logged as `poing: live: unmapped 96 04 7F`. That line is what to put in the map.

### Tested without the hardware

Injected MIDI goes through makepad-remote (`midi a= b= c= p=OMNIS-DUO`) into a hidden, muted Poing. The `live` tool reads the decks back, and `midi k=out` reads the LEDs Poing sent. [`app/live/live_test.sh`](../app/live/live_test.sh) is the run below; its header says how to start the Poing it drives. If every deck stays at 0.0 s, the audio output did not open: the callback never ran. Once, "Remote Audio" refused with `Initialize: HRESULT 0x80070005`, and starting Poing again fixed it. The run, 2026-10-24, on glacier, used a copy of "It's Autumn" on both decks:

1. **The two decks out of step.**
   - Deck 2's tempo fader went to +3% (`B1 00 57`, `B1 20 7F`).
   - Deck 1 played from hot cue A (Intro, 0.34 s). Deck 2 started from hot cue B (`96 01`, Chorus 1 at 37.49 s).
   - After 3 s they were 0.036 beat apart, at 133.0 and 130.8 bpm.
2. **Sync on deck 2** (`91 1F`). Three seconds later the phase difference was **0.000 beat**. Deck 2 then played at 0.974× (123.08 bpm, deck 1's local tempo), and the beats were 13.462 and 92.462. It held at 0.000 two seconds later.
3. **Deck 1 hot cue C** (`95 02`) while playing: it jumped from about 8.7 s to 113.57 s. The phase difference stayed 0.000.
4. **Shift + pads.**
   - On deck 1 (`95 08`, `95 09`), vocals and drums went off.
   - On deck 2 (`96 0A`), bass went off.
   - Their LEDs went dark (`95 08 00`, `95 09 00`, `96 0A 00`).
5. **Crossfader.**
   - Hard left: deck 2 fell to −53 dB (its meter falling back) while deck 1 played at −13 dB.
   - Hard right: deck 1 fell to −61 dB.
6. **EQ, filter and loop.**
   - Deck 1's low was killed and its filter set to low-pass.
   - Deck 2's loop went on (`91 43`) and was halved (`B1 2E 3F`). It looped 2 beats and stayed in sync.
7. **Beat FX** on (`B4 72`) and set to 1 beat (`B4 4D`): Echo, 1 beat, on.
8. **Jog on deck 1.** It was touched (`90 20`) and turned back 72 steps (`B0 22`): the playhead went back 0.12 s and held. Synced deck 2 kept its tempo (0.995×) rather than chasing the scratched deck.
9. **An unknown message** (`96 0C 7F`) was logged as `poing: live: unmapped 96 0C 7F`.
10. **Leaving live mode.** Live off ("Back to the song") brought the song's timeline back.

The unit tests (`cargo test`, `deckmap` and `decks`) check every documented message against its control. They also cover sync pulling a 0.3-beat, 3% offset into phase within 2 s, and the CDJ cue. Further tests: nudge (20 steps, 20 ms), scratch (a tenth of a turn, 0.18 s back), hot cue phase, a loop, mute fades, the crossfader at −3 dB, an EQ kill and the filter, the echo landing exactly one beat later, and LEDs sent only on change.

### To check on the real DUO

1. **Does it show up as a MIDI port in PC mode, and under what name?** Poing logs the MIDI inputs it opens (`poing: MIDI inputs: …`) and the live map's port match.
2. **Do pads 5–8 or other pad notes exist?** Watch for `unmapped` lines.
3. **Jog resolution** (720 steps a revolution assumed) and its direction.
4. **Tempo fader direction** (`invert`) and whether the mixer knobs really send only 7 bits.
5. **Pad LEDs.** Does 7F light them? They may be RGB with a colour per velocity.
6. **Sync on the touch panel.** Does it send `90 1F` in PC mode?
7. **USB audio.** Does the DUO's ASIO device offer 4 outputs (master and headphones), for headphone cueing later?

### Not in this first version

- Lane effects and MIDI lanes. A deck plays the song's audio through the song's own mixer, with no effects rack, so a Splash or VST3 effect on a lane and the MIDI lanes' instruments are not heard live.
- Key lock (time-stretch).
- Headphone cueing.
- Saving changed cue points to the song.
- A waveform view (the live view shows the decks as text and bars).
- Decks 3 and 4.
- Recording the set.
