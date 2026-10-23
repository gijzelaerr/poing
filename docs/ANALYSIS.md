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
