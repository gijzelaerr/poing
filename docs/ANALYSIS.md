# Song analysis: what Poing uses and how good it is

Measured 2026-10-23 on an RTX 4070 Ti. Scores come from `sidecar/eval_analysis.py`. The reference lyrics and chord files stay outside the repo because they are copyrighted.

## Test songs and references

| Song | Lyrics reference | Chord reference |
|---|---|---|
| The Hamlets, "It's Autumn" (EN) | hand-checked lyrics from the cover work (started from Whisper output, so it favours Whisper) | none |
| Jean-Paul van der Mey, "Ik zoek een mooie rijke vrouw" (NL), original | hand-checked lyrics | none |
| "Zomer op de Farm" (NL), made by Poing | its own `lyrics.txt` (what was asked for; ACE-Step skipped one line) | none |
| The Beatles, "Penny Lane" | none | Isophonics `09_-_Penny_Lane.lab`. Our audio starts 2.75 s later, a clear peak in the offset search |

Penny Lane is very likely in SheetSage2's training data, and it is certainly in BTC's. Treat its chord scores as an upper bound, not a fair comparison.

## Lyrics

The method: Whisper large-v3 decodes the vocal stem in pieces of up to 20 s, split at pauses. The steps:

- **Lead-in:** each piece also hears up to 5 s of the audio before it.
- **Retry:** a piece that comes back as boilerplate or nearly empty is decoded again with the previous words as a prompt.
- **Language:** taken from `sheet.json`, otherwise detected once and forced for every piece.
- **Word timestamps and line splitting:** words carry their own times; lines are split at pauses and sentence ends.
- **Known lyrics:** when the bundle has `lyrics.txt`, its lines are timed by an edit-distance alignment against the transcribed words instead of transcribing.

| Song | Before (one long-form pass) | Now |
|---|---|---|
| Mooie rijke vrouw | 29.3% WER | 16.6–17.1% |
| Zomer op de Farm, transcribed | 33.8% | 14.6–15.2% |
| Zomer op de Farm, `lyrics.txt` aligned | 33.8% | 6.6% |
| It's Autumn | 4.6% | 7.4–9.3% |

The ranges come from two separate Demucs runs: the stems vary a little from run to run, and Whisper follows. The lead-in is what makes the pieces work. Without it, Whisper dropped or misheard pieces that start mid-line, the same failure the native engine saw with phrase windows.

## Chords

SheetSage2 gives sections, key, chords and the score that covers are built from. On Penny Lane its chords score 97% root, 97% majmin and 81% sevenths. What looked bad on the timeline was its raw labels (`Fmaj/3`, `Gmin7`). They are now shown as chord symbols (`F/A`, `Gm7`).

**Chords from notes** (a candidate that could run natively): Basic Pitch note posteriorgrams of the bass and other stems are summed per beat into a pitch-class profile. Each beat is scored against major/minor triad templates, plus a bonus when the lowest bass note is the root and an optional key prior. Viterbi smoothing follows, with a smaller change penalty on downbeats. The script is `sidecar/experiments/notes_chords.py`.

| On Penny Lane (Isophonics) | root | majmin | sevenths |
|---|---|---|---|
| SheetSage2 | 0.974 | 0.969 | 0.806 |
| notes → chords, no key prior | 0.858 | 0.800 | 0.721 |
| notes → chords, prior on the analysis key (weight 0.04) | 0.881 | 0.799 | 0.698 |
| notes → chords, best weighting found | 0.838 | 0.818 | 0.740 |

On the three songs without a reference, notes → chords agrees with SheetSage2 on 60–78% of the time (majmin). The disagreements found on It's Autumn:

- Short chords get smeared: a one-beat G inside Em becomes Em.
- The dominant in Bm–F#–Bm is lost.

In both cases SheetSage2's reading (Em Am D G, the Bm–F# turnaround) is the musical one.

**Verdict:** SheetSage2 stays. Notes → chords is about 15 points behind on majmin, and the gap on the unreferenced songs looks real, not like reference bias.

**What a native SheetSage2 would take:**

- **The MERT-v2 encoder:** about 2.4 GB, a conv feature extractor plus a transformer, the same kind of layers as Whisper's encoder.
- **SheetSage2's own heads:** transformers producing chord, melody, beat and structure tokens.
- **Its post-processing into labs and ABC:** pure logic.
- **The license:** CC BY-NC, the same as MERT.

## Which pieces have a native Makepad counterpart

Native ports live in Makepad `libs/ai/models`. The `poing-engine` crate is in PR #2.

| Analysis step | Today | Native counterpart |
|---|---|---|
| Stems | Demucs htdemucs_ft (Python) | BS-RoFormer `stems/` (in poing-engine) |
| Beats, downbeats, tempo | SheetSage2 | Beat This! `beats/` (in poing-engine; F 0.98–0.99 against SheetSage2) |
| Lyrics transcription | Whisper large-v3 (transformers) | Whisper `speech/src/whisper` in poing-engine (large-v3 Q8_0): `transcribe` on the GPU, then `force_align` on the decoded tokens for word times (segment times when the alignment collapses); `transcribe` with `prompt` for the retries; greedy only, as used here |
| Pause chunking, silence gate, hallucination filter, line splitting | Python logic | none needed: plain logic, ported to `engine/src/lyrics.rs` (energy gate; Silero VAD is available in `speech/src/vad.rs`) |
| Known-lyrics alignment | edit distance over transcribed words | logic ports directly; `text_align.rs` (teacher-forced DTW) can refine word times within a line |
| Language detection | Whisper | native Whisper detects the language |
| Chords, key, sections, score | SheetSage2 | none yet. Notes → chords over Basic Pitch `notes/` runs natively but scores about 15 points lower (see above) |
| Chord labels → symbols | Python logic | logic |

## Native lyrics: which vocal Whisper hears

The native engine's Whisper gave It's Autumn 17.6% WER, against 7.4–9.3% for Python. On the same vocal stem the two Whispers are about equal, so the gap was BS-RoFormer's vocal stem. Since this change the engine also runs Makepad's vocals-only Mel-Band RoFormer (KimberleyJSN's checkpoint), and Whisper hears **the mean of the two separators' vocals**. The mean is kept as `lyrics/vocals.wav` in the bundle, and `poing-engine lyrics <bundle>` uses it. The mix keeps BS-RoFormer's four lanes, which sum to the song. There is no separation reference to show that Mel-Band's vocal is a better lane, so the lanes don't change.

Lyrics WER, all on copies of the bundles, measured 2026-10-23:

| Song | Native before (BS-RoFormer vocal) | Mel-Band vocal alone | **Native now (mean of both)** | Python (Demucs) |
|---|---|---|---|---|
| It's Autumn | 17.6% | 16.7% | **11.1%** | 8.3% |
| Mooie rijke vrouw | 13.2% | 18.5% | **13.2%** | 18.5% |
| Zomer op de Farm, transcribed | 12.6% | 11.3% | **12.6%** | 12.6% |
| Zomer op de Farm, `lyrics.txt` aligned | 2.0% | 4.0% | **4.0%** | 4.0% |

Other Whisper inputs tried on It's Autumn:

| Input | Native Whisper | Python Whisper |
|---|---|---|
| Demucs vocal | 10.2% | 8.3% |
| 0.3 Mel-Band + 0.7 BS-RoFormer | 13.0% | |
| 0.5 + 0.5 (the mean) | 11.1% | 23.1% |
| 0.7 Mel-Band + 0.3 BS-RoFormer | 13.0% | |
| Mel-Band + 10% of the other three BS-RoFormer stems | 16.7% | |

On Mooie and Zomer, native Whisper on the Demucs vocal scores 19.0% and 16.9%, so Demucs is not the better stem everywhere.

What this means:

- Native lyrics are now no worse than Python on Mooie (better) and on Zomer (equal). **On It's Autumn native is still behind: 11.1% vs 8.3%, three words of 108.**
- The rest of that gap is not one cause. Native Whisper on the Demucs vocal gets 10.2%, and Python Whisper on the mean gets 23.1%, so each decoder does best on a different stem. Python's Whisper also has temperature fallback and native's is greedy.
- Treat single-word differences as noise: one word is 0.9% on It's Autumn and 0.5% on Mooie. Python's WER also moves between Demucs runs (7.4–9.3% on It's Autumn). Remember too that the It's Autumn reference started from Whisper output.
- The weights 0.3 and 0.7 are both worse than the mean on It's Autumn, so the mean stays. It was not tuned further on these three songs.

Time and memory on the RTX 4070 Ti (12 GB) are below. VRAM is the whole card, including about 0.3–0.6 GB for the desktop. Other sessions shared the GPU during some runs, so the numbers come from runs on an idle card, or from stages where nothing else ran.

| Stage | Time (2:15–3:17 songs) | VRAM peak |
|---|---|---|
| BS-RoFormer stems | 35–45 s | 3.4 GB |
| Mel-Band vocal (new) | 19–27 s | 2.9 GB |
| Beat This! | < 1 s | 0.8 GB |
| Whisper large-v3 Q8_0 | 37–58 s (78 s aligned) | 3.5 GB |
| Whole native analysis | 93–137 s (was 73–131 s) | **3.5 GB** (was 6.9 GB) |
| Python, Demucs + lyrics | 12–15 s + 23–39 s | 1.7 GB, then 7.7 GB |

Each model is now dropped before the next one loads. Before this change the BS-RoFormer separator stayed loaded while Whisper ran, which made the peak 6.9 GB. So the native analysis now peaks at half the memory, even with the extra model.

**Licence.** The Mel-Band weights repository (`KimberleyJSN/melbandroformer`, pinned at revision `ac9b061`, SHA-256 `87201f4d…559e`) has an MIT tag. Its training data is not disclosed, so Makepad records `VOCALS_MODEL_COMMERCIAL_USE = false`. `poing-engine install` asks for this licence the way it asks for BS-RoFormer's, and records the acceptance in Makepad's licence store. Without these weights, the engine feeds Whisper the BS-RoFormer vocal as before.
