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
- **What a deck plays.** The song as it plays in its tab when loaded: the lanes at their levels and mutes (the muted "Original" stays out), regions, and the takes that are on (a whole-song remix replaces the lanes). The effects on the lanes and the master are heard, and so are the MIDI lanes with their instruments:
  - Each deck has its own copy of the song's effects rack. A Splash effect or instrument's knobs are the song's own: turn one in the song's tab and the deck follows.
  - A VST3 effect or instrument gets an instance of its own for the deck, with the state it has when the song is loaded. Changes made to the plugin after that reach the deck with the next Load. A plugin that does not load for the deck is named in the status line, and the deck plays without it.
  - The deck renders the song through that rack and then plays it at its speed, so effects and instruments bend with the tempo as the stems do. With a plugin that adds latency, the deck renders that much ahead, so it stays on its beat grid.
  - A MIDI lane mutes with the stem its name says (a lane called "Bass synth" with the bass), else with "other". Each stem can be muted on its own (with a whole-song remix on, the stem mutes do nothing: the remix is one piece of audio). Two copies of one song share their audio, so nothing is loaded twice. A deck that gets a song stops on its hot cue A.
- **Tempo.** Speed and pitch move together, like vinyl, unless the deck's key lock is on.
- **Key lock.** The touch panel's **Master Tempo** button (`90 11` / `91 11`) turns it on or off for a deck. With it on, the tempo fader, sync and a nudge change the tempo and leave the pitch where it is.
  - How: the deck lays pieces of the song 40 ms long, a new one every 20 ms, each faded over the one before. Each piece is taken from within 10 ms of where the playhead is, at the spot that best carries on from the last one (WSOLA). At the song's own tempo that gives back the song unchanged.
  - The deck's position and beat are the playhead's, so sync and the beat grid work as without it. What you hear can be up to 10 ms either side of it.
  - The platter in the hand (touched) always plays like vinyl. Going into and out of the lock crossfades over 20 ms.
  - A jump (a hot cue, a loop's end) is heard at once: the piece being played is cut and fades out over 20 ms under the new place.
  - With the lock on the deck renders about 50 ms ahead of what you hear, so a stem mute or a turned knob of a lane effect is heard that much later.
  - It holds from half to double speed. The effects and instruments are in what it stretches, so they keep their pitch too.
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

- **A loop and the notes around it.** A deck in a loop renders nothing past the loop's end, so a MIDI note that starts on the beat after the loop is not started, and a lane effect does not get the audio after it. A note that starts on the loop's first beat plays every time round: after any jump the deck renders from 5 ms before where it lands.
- **A hot cue or section jump while playing** lands on the cue plus the current offset from the last beat. The deck keeps its beat, so a synced deck stays in sync. While stopped, it jumps exactly to the cue. A hot cue also starts the deck, as on a CDJ.
- **Cue works like a CDJ.**
  - Playing: back to the cue point, and stop.
  - Stopped away from the cue: the cue point moves here, on the nearest beat.
  - Stopped on the cue: plays while held.

### Jog, tempo, sync

- **Jog side turn: nudge.** Each step moves the deck 1 ms. It is spread over a few blocks at up to 5% speed, so you hear a pitch bend.
- **Jog top, touched: scratch-lite.** The platter drives the playhead at one revolution per 1.8 s of audio (33⅓ rpm, 720 steps a revolution, assumed). The playhead follows the hand with a short glide, backwards too. Let go and the deck plays on. Going back, the platter plays what the deck played last (it keeps 30 s, with the effects in it, like a record). Turned back further than that, or back from a cue the deck has not played from yet, you hear the lanes without their effects and without the MIDI lanes.
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
- **Where it plays.** The two decks go to the same output as the song (the device chosen in Settings).
- **Headphone cue.** The mixer's cue buttons (`B4 46` / `B4 47`, lit while on) put a deck in the headphones: after its EQ, filter and trim, before its channel fader and the crossfader, and without the beat FX. So a deck can be heard there with its fader down. With both buttons on you hear both decks; with none, nothing.
  - The headphones are a second sound output: **Settings, Sound, "Headphones in live mode"**. Makepad opens a Windows output with two channels, so a four-channel device cannot carry master and headphones together. Whether the DUO shows its master and headphones as two outputs is not known: its driver file names a "Speaker" and a "Line" and is otherwise a generic USB audio driver, and "Line" may be its input. If it is one four-channel output, the DUO's own headphone jack needs four-channel output in Makepad first. Any other pair of outputs works too, such as an audio interface for the room and the PC's headphone jack.
  - The headphones only hear the cue buttons, not the master: there is no cue/master mix in Poing (the DUO has that knob itself).
  - The headphones play behind the decks by twice the largest block either output works in (about 21 ms with 10 ms blocks, 11 ms at least), plus their own device's delay. The two outputs each run on their own clock. When they drift apart far enough the headphones skip ahead or repeat that much, which you hear as a tick. Two endpoints of one device share a clock and do not drift.
  - An output with four channels gets the headphones on channels 3 and 4 as well, with no second output and no delay. That needs Makepad to open more than two channels, which on Windows it does not do yet.
  - The live view's status line names the headphones' output, and the `live` tool gives `headphones_level_db`.

### The waveforms

Each deck shows its song under a fixed playhead in the middle, four seconds either side, scrolling as the deck moves (also under the hand on the platter).

- **The stems** each have a colour: vocals orange, drums blue, bass purple, other green. In every column the widest is drawn first, so the quieter ones stay visible on top. A muted stem stays as a shadow. A whole-song remix is one piece of audio and shows in one colour. MIDI lanes have no waveform.
- **The beat grid** is a line per beat, every fourth one brighter (counted from the grid's first beat). **Sections** have a line and their name along the bottom. **Hot cues** are a red line with their letter, the **cue point** a yellow tick at the bottom, a running **loop** a blue band.
- **Under it** is the whole song in one strip: what has been played is dimmed, with a line where the deck is and ticks for the hot cues.
- It only shows; it takes no clicks.

### Recording the set

**Record the set** (top right of the live view) writes what live mode plays, both decks through the mixer and the beat FX, to a WAV file (24-bit stereo, at the song's sample rate). The button then reads **Stop recording** with the time so far.

- **Where.** `Music\Poing\Sets\set-2026-11-11-153000.wav` (the songs folder, `POING_SONGS` moves it). The date and time in the name are UTC. The status line says where it went when it stops.
- **When it stops.** With the button, on leaving live mode, or by itself after three hours or when the disk is full.
- **A muted Poing records too** (`POING_MUTE`): the file is what would have played.
- **The disk does not hold up the sound.** The audio callback puts the frames in a ring and a thread of its own writes them. If the disk stands still for over two thirds of a second, what could not be kept is left out, and the status line says how much. The file's header is brought up to date every second, so a recording that is cut off is still a WAV file.
- The `live` tool takes `record: true` or `false` and gives `recording` (on, file, seconds).

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

**Effects and MIDI lanes on a deck, 2026-11-11, on glacier.**

- A test song with two lanes: the drums through a Splash effect that turns them down 20 dB, and a MIDI lane on the built-in bass. With the drums muted by shift + pad 2 the deck played the MIDI lane alone at −9 dB. With the bass muted and deck 1 looping 4 beats, the drums read −27 and −29 dB with the effect and −13 and −9 dB with it bypassed from the `effect` tool.
- A copy of "ded24d v2" (six BBC Symphony Orchestra lanes and three Serum 2 lanes), with every audio stem muted. A deck loaded its nine plugin instances in 3.7 s. Deck 1 at +5% played the MIDI lanes at −16 to −20 dB, and no plugin block was missed.
- The same song with one Serum 2 lane set to run in its own process and the other MIDI lanes muted: the deck played that lane at −18 to −22 dB, with a `poing-plughost` for the song's tab and one for each deck. Deck 2 synced to deck 1: ten readings from 4 s after sync went on were all within 0.0011 beat.
- Two tabs: with deck 1 playing "ded24d v2", the test song was opened in a second tab and loaded onto deck 2 with the hardware Load message (`91 51`). Deck 1 played on (−21 to −25 dB) and deck 2 played the test song (−10 dB). Closing the "ded24d v2" tab left deck 1 playing it, and deck 2's old plugin instances were gone (one `poing-plughost` left, deck 1's).
- `live_test.sh` on a copy of "It's Autumn", three times on a fresh Poing: all 13 checks passed twice. The first run failed one, the timing of deck 2's hot cue B 3 s after it was pressed; its numbers were not kept and it did not happen again.
- Not checked in the app: a VST3 effect on a lane (only instruments were on the test songs; the unit test covers the latency of an effect slot). Every run was muted and judged by level: nobody has listened to it yet.

**Key lock and the headphone cue, 2026-11-11, on glacier** (a copy of "It's Autumn" on both decks; the PC has one sound output, so nothing played through a second one).

- Key lock on both decks, deck 2 at +3%: both played at their levels. With sync on, the phase difference read 0.000 beat, and stayed there through a hot cue on deck 1 while playing and a 2-beat loop on deck 2. Touching deck 1's platter held it; letting go, it played on.
- The crossfader on deck 1 and deck 2's cue button on: `headphones_level_db` read +3 dB while deck 2's own level read silent, and −inf again with the button off. Its LED was sent on and off (`B4 47 7F`, `B4 47 00`).
- Settings shows the second choice under Sound ("No headphones in live mode").
- `live_test.sh` again on a fresh Poing: 13 of 13.
- Not checked: sound through a second output (the callback that plays the headphones never ran here), and how key lock sounds. Four clips for the ear are made by the `key_lock_by_ear` test (12 s of a song at +8% and −8%, like vinyl and locked).
- After these runs the loop handling changed (nothing rendered past a loop's end, the note on a loop's first beat, a jump under key lock heard at once) and the headphones' distance behind the decks became two blocks. Run again later that day with those changes in: `live_test.sh` 13 of 13, and with key lock on both decks the same moves with a 2-beat loop held for 6 s, the phase difference under 0.02 beat throughout. (A hidden Poing's decks only run while Windows has a sound output: with the remote desktop session disconnected there is none.)

**The waveforms and recording the set, 2026-11-11, on glacier.**

- A screenshot of the live view with deck 1 playing and deck 2 in a 2-beat loop from hot cue B with its vocals muted: both waveforms with their four stems, the beat lines, the section names, the B marker, the loop's band over two beats and the vocals as a shadow on deck 2; the strips under them with the position.
- `live {"record": true}`, six seconds, `{"record": false}`: a 24-bit stereo 48 kHz WAV of 6.26 s in `Sets`, peaking at 0.9, no frames lost. The button read "Stop recording · 0:06.1" meanwhile.
- Nobody has listened to a recorded set yet.

The unit tests (`cargo test`, `deckmap` and `decks`) check every documented message against its control. They also cover sync pulling a 0.3-beat, 3% offset into phase within 2 s, and the CDJ cue. Further tests: nudge (20 steps, 20 ms), scratch (a tenth of a turn, 0.18 s back), hot cue phase, a loop, mute fades, the crossfader at −3 dB, an EQ kill and the filter, the echo landing exactly one beat later, and LEDs sent only on change. For the rack: a deck at the song's speed gives the same samples as the song's mixer through a feedback delay (in blocks of 512 and 333), a MIDI note at +5% starts on time and holds, a loop's end lets go of the note it cuts, a deck with a 300-frame latent plugin slot still plays a click where its position says, and the rack goes back to its song when the deck gets another. For key lock: a 440 Hz tone at +8% reads 475 Hz like vinyl and 440 Hz locked (at −8% too) at a steady level, the song's own tempo gives the mixer's samples back, and turning the lock on and off leaves no step between two frames larger than a tone's own. For loops, with and without key lock: a note that starts exactly at a one-beat loop's end is not heard in it, and a note on its first beat sounds each of six times round at +3.7%. For the set: 2.5 s pushed through the ring comes out of the file sample for sample with a header that matches, and a writer kept away for too long gets the newest half of the ring and counts the rest as lost. For the waveform: the lanes that sound, at their levels, per stem. For the headphones: a cued deck is in the cue ring and not in the master with its fader down, and the reader starts two blocks behind, goes 200 blocks of 1500 frames without a gap whichever output goes first, plays silence when it catches up and skips when far behind.

### To check on the real DUO

1. **Does it show up as a MIDI port in PC mode, and under what name?** Poing logs the MIDI inputs it opens (`poing: MIDI inputs: …`) and the live map's port match.
2. **Do pads 5–8 or other pad notes exist?** Watch for `unmapped` lines.
3. **Jog resolution** (720 steps a revolution assumed) and its direction.
4. **Tempo fader direction** (`invert`) and whether the mixer knobs really send only 7 bits.
5. **Pad LEDs.** Does 7F light them? They may be RGB with a colour per velocity.
6. **Sync and Master Tempo on the touch panel.** Do they send `90 1F` and `90 11` in PC mode?
7. **USB audio.** Do the DUO's master and headphone outputs show up as two sound outputs in Settings? If Windows shows one four-channel output only, the headphone cue needs four-channel output in Makepad first.
8. **The headphone cue buttons.** Do `B4 46` / `B4 47` arrive as a press and a release (they are CCs), and do they light?
9. **Key lock by ear.** It has only been measured (the pitch of a tone, its level). Listen to it at ±8% on drums and on a voice.
10. **A fast forward spin on a song with plugins.** The platter going forward past what the deck has played renders the song at up to 4 times its speed, through every effect and instrument, in the audio callback. Listen for dropouts on a song with many plugins.

### Not in this first version

- Changes to the song after it was loaded: press Load again.
- Bringing back a deck's plugin after its process crashed. In the song's tab a plugin that runs in its own process is reloaded when its helper dies; a deck's instance is not, so that lane stays silent until the next Load.
- A cue/master mix and a level for the headphones in Poing, and master and headphones on one four-channel output.
- Saving changed cue points to the song.
- Decks 3 and 4.
- A click on the waveform to jump there (needle search).
