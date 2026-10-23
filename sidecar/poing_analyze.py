"""Poing analysis sidecar: understand a song.

    uv run --project sidecar python sidecar/poing_analyze.py <audio> <bundle_dir>

Writes into <bundle_dir>:
    stems/{vocals,drums,bass,other}.wav   Demucs htdemucs_ft
    score/                                SheetSage2 score and labs (sections, key, chords, beats)
    analysis.json                         tempo, key, beats, downbeats, chords, sections, lyrics
Lyrics: Whisper on the vocal stem, piece by piece between pauses. When the bundle has
lyrics.txt (songs Poing made, or corrected words) those lines are timed to the singing
instead, and sheet.json's "language" is passed to Whisper (otherwise it is detected).

    ... poing_analyze.py --only lyrics|structure <audio> <bundle>   redo one part (stems must exist)
The app runs `--only structure` after the native poing-engine has written stems, beats and
lyrics: SheetSage2 then adds key, chords and sections, and the engine's beats and tempo stay.
Streams one JSON object per line on stdout:
    {"stage": "...", "progress": 0..1, "message": "..."}   and finally {"stage": "done", ...}
Errors: {"stage": "error", "message": "..."} and a non-zero exit.
"""
import json
import os
import re
import sys
import tempfile
import time
from pathlib import Path

# (stage, share of the total time) for one overall progress number
STAGES = [("stems", 0.45), ("structure", 0.25), ("lyrics", 0.30)]
RUN = [name for name, _ in STAGES]  # the stages this run does (--only)


def emit(stage, progress, message="", **extra):
    print(json.dumps({"stage": stage, "progress": round(progress, 3), "message": message, **extra}), flush=True)


def overall(stage, within):
    stages = [(name, share) for name, share in STAGES if name in RUN]
    total = sum(share for _, share in stages)
    done = 0.0
    for name, share in stages:
        if name == stage:
            return (done + share * within) / total
        done += share
    return done / total


def separate(audio: Path, out: Path, device: str):
    import demucs.separate
    emit("stems", overall("stems", 0.0), "Separating vocals, drums, bass and other")
    with tempfile.TemporaryDirectory() as tmp:
        demucs.separate.main(["-n", "htdemucs_ft", "-d", device, "-o", tmp,
                              "--filename", "{stem}.{ext}", str(audio)])
        src = Path(tmp) / "htdemucs_ft"
        (out / "stems").mkdir(parents=True, exist_ok=True)
        for stem in ("vocals", "drums", "bass", "other"):
            os.replace(src / f"{stem}.wav", out / "stems" / f"{stem}.wav")
    emit("stems", overall("stems", 1.0), "Stems ready")


def read_lab(path: Path):
    rows = []
    if path.exists():
        for line in path.read_text().splitlines():
            parts = line.split()
            if len(parts) >= 2:
                rows.append(parts)
    return rows


NOTES_SHARP = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]
NOTES_FLAT = ["C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B"]
DEGREE = {"1": 0, "2": 2, "3": 4, "4": 5, "5": 7, "6": 9, "7": 11, "9": 2, "11": 5, "13": 9}
# Harte quality -> the chord symbol musicians read
QUALITY = {"": "", "maj": "", "min": "m", "7": "7", "maj7": "maj7", "min7": "m7", "maj6": "6", "min6": "m6",
           "dim": "dim", "dim7": "dim7", "hdim7": "m7b5", "aug": "+", "sus2": "sus2", "sus4": "sus4",
           "sus4(b7)": "7sus4", "9": "9", "maj9": "maj9", "min9": "m9", "minmaj7": "m(maj7)"}


def flat_key(key):
    """Spell notes with flats in flat keys ("F major", "G minor")."""
    if not key:
        return False
    root, _, mode = key.partition(" ")
    return "b" in root or root in (("D", "G", "C", "F") if mode == "minor" else ("F",))


def chord_name(label: str, flats=False):
    """SheetSage2's Harte labels -> chord symbols: 'F#:maj' -> 'F#', 'E:min' -> 'Em',
    'G:min7' -> 'Gm7', 'F:maj/3' -> 'F/A', 'F#:sus4(b7)' -> 'F#7sus4'; 'N' -> None."""
    if label in ("N", "X", "?"):
        return None
    root, _, rest = label.partition(":")
    quality, _, bass = rest.partition("/")
    q = QUALITY.get(quality)
    if q is None:
        q = "m" if quality.startswith("min") else ""
    name = root + q
    m = re.fullmatch(r"([b#]*)(\d+)", bass)
    notes = NOTES_SHARP if root in NOTES_SHARP and not flats else NOTES_FLAT
    if m and m.group(2) in DEGREE and root in NOTES_SHARP + NOTES_FLAT:
        pc = (NOTES_SHARP + NOTES_FLAT).index(root) % 12
        shift = DEGREE[m.group(2)] + m.group(1).count("#") - m.group(1).count("b")
        if shift % 12:
            name += "/" + notes[(pc + shift) % 12]
    return name


def merge_sections(rows, min_len=2.0):
    """Merge consecutive identical labels and absorb slivers into their neighbour."""
    secs = []
    for start, end, label in rows:
        if secs and (secs[-1]["label"] == label or end - start < min_len):
            secs[-1]["end"] = end
        else:
            secs.append({"start": start, "end": end, "label": label})
    if len(secs) > 1 and secs[0]["end"] - secs[0]["start"] < min_len:
        secs[1]["start"] = secs[0]["start"]
        secs.pop(0)
    return secs


def rebuild_abc(model, score: Path) -> bool:
    """SheetSage2 refuses to write ABC when a bar repeats a beat ID (1,2,3,4,4).
    Drop the repeated beat rows and rebuild score.abc from its MIDI and labs."""
    import importlib
    try:
        pkg = type(model).__module__.rsplit(".", 1)[0]
        notation = importlib.import_module(pkg + ".notation_sheetsage2")
        fixed = []
        for row in (score / "beat.lab").read_text().splitlines():
            parts = row.split()
            if len(parts) < 2 or not parts[1].isdigit():
                continue
            beat, prev = int(parts[1]), int(fixed[-1].split()[1]) if fixed else None
            if prev is None or beat == 1 or beat == prev + 1:
                fixed.append(row)
        (score / "beat_fixed.lab").write_text("\n".join(fixed) + "\n")
        abc = notation.build_rebuilt_abc_score(
            score / "melody.mid", score / "beat_fixed.lab", score / "chord.lab", score / "key.lab",
            score / "structure.lab", meter_conflict="infer", melody_only=False)
        (score / "score.abc").write_text(notation.score_to_abc(abc), encoding="utf-8")
        return True
    except Exception as exc:  # the score is a bonus; the labs carry the analysis
        print(f"score rebuild failed: {exc!r}", file=sys.stderr)
        return False


def structure(audio: Path, bundle: Path, device: str):
    import torch
    from transformers import AutoModel
    emit("structure", overall("structure", 0.0), "Finding sections, key, chords and beats")
    model = AutoModel.from_pretrained(os.environ.get("POING_SHEETSAGE", "m-a-p/SheetSage2"),
                                      trust_remote_code=True).eval().to(device)
    # The score (ABC, MIDI, labs) stays in the bundle: covers are rendered from it.
    score = bundle / "score"
    score.mkdir(exist_ok=True)
    try:
        model.transcribe(str(audio), output_dir=str(score))
    except (ValueError, RuntimeError) as exc:
        print(f"transcribe: {exc!r}", file=sys.stderr)
    if not (score / "score.abc").exists() and not rebuild_abc(model, score):
        emit("structure", overall("structure", 0.8), "No score; using annotations")
    beats = [(float(r[0]), int(r[1])) for r in read_lab(score / "beat.lab") if r[1].isdigit()]
    key_rows = read_lab(score / "key.lab")
    chord_rows = read_lab(score / "chord.lab")
    struct_rows = read_lab(score / "structure.lab")
    del model
    if device == "cuda":
        torch.cuda.empty_cache()
    times = [t for t, _ in beats]
    gaps = sorted(b - a for a, b in zip(times, times[1:]) if b > a)
    tempo = round(60.0 / gaps[len(gaps) // 2], 1) if gaps else None
    key = None
    if key_rows:
        root, _, mode = key_rows[0][2].partition(":")
        key = f"{root} {mode}".strip()
    chords = []
    for r in chord_rows:
        name = chord_name(r[2], flat_key(key)) if len(r) > 2 else None
        if name is None:
            continue
        start, end = float(r[0]), float(r[1])
        if chords and chords[-1]["label"] == name:
            chords[-1]["end"] = end
        else:
            chords.append({"start": round(start, 3), "end": round(end, 3), "label": name})
    sections = merge_sections([(float(r[0]), float(r[1]), r[2]) for r in struct_rows if len(r) > 2])
    emit("structure", overall("structure", 1.0), f"{len(sections)} sections, key {key}, {tempo} bpm")
    return {
        "tempo": tempo,
        "key": key,
        "beats": [round(t, 3) for t in times],
        "downbeats": [round(t, 3) for t, b in beats if b == 1],
        "chords": chords,
        "sections": [{"start": round(s["start"], 3), "end": round(s["end"], 3), "label": s["label"]} for s in sections],
    }


# Whisper's subtitle-trained boilerplate that appears over instrumentals, or no words at all
# ("***"). Whole lines only: "muziek" is also a real Dutch word.
HALLUCINATIONS = re.compile(
    r"^\W*(zang en muziek|muziek|tv gelderland|ondertiteling.*|ondertiteld.*|thanks for watching.*"
    r"|subtitles by.*|.*amara\.org.*|\[music\])?\W*$",
    re.IGNORECASE,
)
WHISPER = os.environ.get("POING_WHISPER", "openai/whisper-large-v3")
SR = 16000
FPS = 10  # loudness frames per second


def loud_frames(audio):
    import librosa
    import numpy as np
    rms = librosa.feature.rms(y=audio, frame_length=2 * SR // FPS, hop_length=SR // FPS)[0]
    return rms, rms > max(float(np.percentile(rms, 95)) * 0.05, 1e-4)


def voiced_chunks(rms, loud, max_len=20.0):
    """Cut the vocal stem into pieces Whisper decodes one at a time (long-form decoding
    skips whole verses on sung vocals): voiced stretches split at pauses, at most max_len s."""
    import numpy as np
    regions = []
    for i in np.flatnonzero(loud):
        if regions and i - regions[-1][1] <= FPS // 2:
            regions[-1][1] = i + 1
        else:
            regions.append([i, i + 1])
    regions = [r for r in regions if r[1] - r[0] >= 3]
    longest = int(max_len * FPS)
    split = []
    while regions:  # break stretches that are too long at their quietest frame
        a, b = regions.pop(0)
        if b - a <= longest:
            split.append([a, b])
        else:
            cut = a + 2 * FPS + int(np.argmin(rms[a + 2 * FPS:b - 2 * FPS]))
            regions[:0] = [[a, cut], [cut, b]]
    chunks = []
    for a, b in split:  # join neighbours while they fit and the gap is short
        if chunks and b - chunks[-1][0] <= longest and a - chunks[-1][1] <= 2 * FPS:
            chunks[-1][1] = b
        else:
            chunks.append([a, b])
    end = len(rms) / FPS
    return [(max(a / FPS - 0.2, 0.0), min(b / FPS + 0.2, end)) for a, b in chunks]


def detect_language(asr, pieces):
    """Most common language Whisper hears in the three longest pieces."""
    import collections
    import torch
    longest = sorted(pieces, key=len, reverse=True)[:3]
    feats = asr.feature_extractor(longest, sampling_rate=SR, return_tensors="pt").input_features
    feats = feats.to(asr.model.device, asr.model.dtype)
    with torch.no_grad():
        ids = asr.model.detect_language(feats)
    langs = [asr.tokenizer.convert_ids_to_tokens(int(i)).strip("<|>") for i in ids.flatten()]
    return collections.Counter(langs).most_common(1)[0][0]


def transcribe(audio, language, device):
    """Words with times: [{"start", "end", "text", "chunk"}], and the language used."""
    import torch
    from transformers import pipeline
    rms, loud = loud_frames(audio)
    chunks = voiced_chunks(rms, loud)
    if not chunks:
        return [], language
    dtype = torch.float16 if device == "cuda" else torch.float32
    asr = pipeline("automatic-speech-recognition", model=WHISPER, torch_dtype=dtype, device=device)
    if not language:
        language = detect_language(asr, [audio[int(a * SR):int(b * SR)] for a, b in chunks])
    words, prev = [], ""
    for n, (a, b) in enumerate(chunks):
        # Each piece is heard with up to 5 s of the audio before it: Whisper often mishears
        # or drops a piece that starts mid-line. Words in that lead-in belong to the piece before.
        at = max(a - 5.0, b - 30.0, chunks[n - 1][0]) if n else a
        out = decode(asr, audio[int(at * SR):int(b * SR)], language)
        out["chunks"] = [w for w in out["chunks"] if at + (w["timestamp"][0] or 0.0) >= a - 0.3]
        out["text"] = " ".join(w["text"].strip() for w in out["chunks"])
        if not usable(out, b - a) and prev:
            # "MUZIEK" or a word or two over singing: the words before it as a prompt bring
            # them back (a real instrumental stays empty).
            retry = decode(asr, audio[int(a * SR):int(b * SR)], language, prompt=prev)
            if usable(retry, b - a) or len(retry["chunks"]) > len(out["chunks"]):
                out, at = retry, a
        text = out["text"].strip()
        if not text or HALLUCINATIONS.search(text) or looped(text):
            continue
        prev = text
        for w in out["chunks"]:
            s, e = w["timestamp"]
            s = at + (s or 0.0)
            e = at + e if e is not None else s + 0.3
            f, g = int(s * FPS), max(int(e * FPS), int(s * FPS) + 1)
            if w["text"].strip() and loud[f:g].any():  # no words over silence
                words.append({"start": s, "end": max(e, s + 0.05), "text": w["text"].strip(), "chunk": n})
    del asr
    if device == "cuda":
        torch.cuda.empty_cache()
    return words, language


def decode(asr, piece, language, prompt=""):
    """One piece with word times. Unpadded and one at a time: batched word timestamps fail
    on pieces of different lengths (transformers 4.45) and padding breaks the word timing.
    With a prompt the word timing collapses to 0, so then the words share out the time of
    the segments they are in, by length."""
    import torch
    kwargs = {"language": language, "task": "transcribe"}
    if not prompt:
        return asr({"raw": piece, "sampling_rate": SR}, return_timestamps="word", generate_kwargs=kwargs)
    ids = asr.tokenizer.get_prompt_ids(prompt[-400:], return_tensors="np")
    kwargs["prompt_ids"] = torch.tensor(ids, device=asr.model.device)
    out = asr({"raw": piece, "sampling_rate": SR}, return_timestamps=True, generate_kwargs=kwargs)
    words = []
    for seg in out["chunks"]:
        s, e = seg["timestamp"]
        s = s or 0.0
        e = e if e is not None else len(piece) / SR
        parts = seg["text"].split()
        total = sum(len(w) + 1 for w in parts)
        t = s
        for w in parts:
            d = (e - s) * (len(w) + 1) / total
            words.append({"text": w, "timestamp": (t, t + d)})
            t += d
    return {"text": out["text"], "chunks": words}


def usable(out, seconds):
    """Words that look like singing: not boilerplate, not a loop, and about one per 1.5 s."""
    text = out["text"].strip()
    return (bool(text) and not HALLUCINATIONS.search(text) and not looped(text)
            and len(out["chunks"]) >= seconds / 1.5)


def looped(text):
    """Whisper stuck repeating itself ("zang en muziek zang en muziek ...")."""
    import zlib
    data = text.encode("utf-8")
    return len(data) > 80 and len(data) / len(zlib.compress(data)) > 3.0


def words_to_lines(words):
    """Group words into sung lines at pauses, sentence ends and chunk edges; a line still
    longer than 10 s is split at its longest pause."""
    lines, cur = [], []
    for w in words:
        if cur and (w["chunk"] != cur[-1]["chunk"] or w["start"] - cur[-1]["end"] >= 1.0
                    or cur[-1]["text"][-1] in ".?!"):
            lines.append(cur)
            cur = []
        cur.append(w)
    if cur:
        lines.append(cur)
    done = []
    while lines:
        line = lines.pop(0)
        if line[-1]["end"] - line[0]["start"] <= 10.0 or len(line) < 6:
            done.append(line)
            continue
        # the longest pause (a comma first) that leaves each half at least a third of the line
        t0, t1 = line[0]["start"], line[-1]["end"]
        fair = [i for i in range(1, len(line)) if min(line[i]["start"] - t0, t1 - line[i]["start"]) >= (t1 - t0) / 3]
        if fair:
            cut = max(fair, key=lambda i: (line[i - 1]["text"][-1] in ",;:", line[i]["start"] - line[i - 1]["end"]))
        else:
            cut = min(range(1, len(line)), key=lambda i: abs(line[i]["start"] - (t0 + t1) / 2))
        lines[:0] = [line[:cut], line[cut:]]
    return [{"start": l[0]["start"], "end": l[-1]["end"], "text": " ".join(w["text"] for w in l)} for l in done]


def norm(word):
    import unicodedata
    word = unicodedata.normalize("NFKC", word).lower().replace("’", "").replace("'", "")
    return "".join(c for c in word if c.isalnum())


def known_lines(path: Path):
    """The song's words from lyrics.txt, one entry per sung line ([Section] tags dropped)."""
    lines = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not re.fullmatch(r"\[[^\]]*\]", line) and any(norm(w) for w in line.split()):
            lines.append(line)
    return lines


def align_known(lines, words):
    """Time the known lines with the transcribed words: an edit-distance alignment of the
    two word lists, so skipped, repeated or misheard lines don't throw the rest off.
    A known line takes the span of the words aligned to it; lines nobody sang are dropped,
    and sung stretches the text doesn't have keep their transcription."""
    import difflib
    known = [(norm(w), i) for i, line in enumerate(lines) for w in line.split() if norm(w)]
    heard = [norm(w["text"]) for w in words]
    n, m = len(known), len(heard)

    def sub(a, b):
        return 0.0 if a == b else 0.5 if difflib.SequenceMatcher(None, a, b).ratio() >= 0.6 else 1.0

    cost = [[0.0] * (m + 1) for _ in range(n + 1)]
    for i in range(1, n + 1):
        cost[i][0] = i
    for j in range(1, m + 1):
        cost[0][j] = j
    for i in range(1, n + 1):
        ki, row, prev = known[i - 1][0], cost[i], cost[i - 1]
        for j in range(1, m + 1):
            row[j] = min(prev[j] + 1, row[j - 1] + 1, prev[j - 1] + sub(ki, heard[j - 1]))
    spans, used = {}, set()
    i, j = n, m
    while i > 0 and j > 0:
        c = sub(known[i - 1][0], heard[j - 1])
        if cost[i][j] == cost[i - 1][j - 1] + c:
            if c < 1.0:
                spans.setdefault(known[i - 1][1], []).append(j - 1)
                used.add(j - 1)
            i, j = i - 1, j - 1
        elif cost[i][j] == cost[i - 1][j] + 1:
            i -= 1
        else:
            j -= 1
    out = []
    for li, js in spans.items():
        size = sum(1 for _, l in known if l == li)
        if len(js) * 3 >= size:  # at least a third of the line was heard
            out.append({"start": min(words[j]["start"] for j in js), "end": max(words[j]["end"] for j in js),
                        "text": lines[li]})
    # Stretches of three or more words the text doesn't have (extra repeats, ad-libs).
    run = []
    for j, w in enumerate(words + [None]):
        if w is not None and j not in used:
            run.append(w)
            continue
        if len(run) >= 3:
            out.extend(l for l in words_to_lines(run) if len(l["text"].split()) >= 3)
        run = []
    out.sort(key=lambda l: l["start"])
    for a, b in zip(out, out[1:]):
        a["end"] = min(a["end"], b["start"])
    return [l for l in out if l["end"] > l["start"]]


def lyrics(vocals: Path, bundle: Path, device: str):
    import librosa
    sheet = bundle / "sheet.json"
    language = None
    if sheet.exists():
        try:
            language = json.loads(sheet.read_text(encoding="utf-8")).get("language") or None
        except ValueError:
            pass
    known = known_lines(bundle / "lyrics.txt") if (bundle / "lyrics.txt").exists() else []
    emit("lyrics", overall("lyrics", 0.0), "Aligning the song's lyrics" if known else "Transcribing lyrics")
    audio, _ = librosa.load(vocals, sr=SR, mono=True)
    words, language = transcribe(audio, language, device)
    lines = align_known(known, words) if known else words_to_lines(words)
    lines = [{"start": round(l["start"], 2), "end": round(l["end"], 2), "text": l["text"]} for l in lines]
    emit("lyrics", overall("lyrics", 1.0), f"{len(lines)} lyric lines ({language})")
    return lines


def main():
    global RUN
    args = sys.argv[1:]
    # --only lyrics|structure: redo one part on a bundle that already has stems and analysis.json
    only = args[args.index("--only") + 1] if "--only" in args else None
    if only:
        i = args.index("--only")
        args = args[:i] + args[i + 2:]
        RUN = [only]
    audio, bundle = Path(args[0]).resolve(), Path(args[1]).resolve()
    began = time.time()
    try:
        import torch
        device = "cuda" if torch.cuda.is_available() else "cpu"
        emit("start", 0.0, f"Analysing on {device}")
        if only:
            result = json.loads((bundle / "analysis.json").read_text(encoding="utf-8"))
        else:
            separate(audio, bundle, device)
        if only in (None, "structure"):
            found = structure(audio, bundle, device)
            if only:
                # Beats and tempo the native engine found stay; SheetSage2's fill in.
                for k in ("tempo", "beats", "downbeats"):
                    if result.get(k):
                        found.pop(k)
                result.update(found)
            else:
                result = found
        if only in (None, "lyrics"):
            result["lyrics"] = lyrics(bundle / "stems" / "vocals.wav", bundle, device)
        result["stems"] = {s: f"stems/{s}.wav" for s in ("vocals", "drums", "bass", "other")}
        (bundle / "analysis.json").write_text(json.dumps(result, indent=1, ensure_ascii=False), encoding="utf-8")
        emit("done", 1.0, f"Analysis done in {time.time() - began:.0f}s")
    except Exception as exc:  # report to the app instead of a bare traceback
        emit("error", 0.0, f"{type(exc).__name__}: {exc}")
        raise


if __name__ == "__main__":
    main()
