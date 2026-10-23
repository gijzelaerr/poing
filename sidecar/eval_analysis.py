"""Score a song's analysis.json against references.

    uv run --project sidecar python sidecar/eval_analysis.py <bundle> [--lyrics ref.txt] [--chords ref.lab] [--offset S|auto]

--lyrics   the song's words as plain text ([Section] tags and punctuation are ignored).
           Prints the word error rate of all lyric lines joined, against the whole reference,
           the share of reference lines found (line WER <= 0.5), and how well the lines sit on
           the voice: the share of lyric-line time over audible vocals and the longest line.
--chords   a reference chord .lab (start end Harte-label, e.g. Isophonics). Scored with
           mir_eval (duration-weighted root, majmin, mirex, sevenths), over the reference span.
--offset   seconds added to the reference times (the audio may start earlier or later than
           the annotated recording); "auto" grid-searches -3..3 s for the best majmin and prints it.
           Pick the offset once on the baseline and pass it to every later run.

References stay outside the repo (lyrics are copyrighted); this only reads them.
"""
import argparse
import json
import re
import sys
import unicodedata
from pathlib import Path

import numpy as np


def words(text: str):
    text = unicodedata.normalize("NFKC", text).lower()
    text = re.sub(r"\[[^\]]*\]", " ", text)
    text = text.replace("’", "'").replace("'", "")
    text = "".join(c if c.isalnum() or c.isspace() else " " for c in text)
    return text.split()


def edits(ref, hyp):
    """Levenshtein distance between two word lists."""
    prev = list(range(len(hyp) + 1))
    for i, r in enumerate(ref, 1):
        cur = [i] + [0] * len(hyp)
        for j, h in enumerate(hyp, 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (r != h))
        prev = cur
    return prev[-1]


def best_window(line, hyp):
    """Smallest WER of `line` against any stretch of `hyp` (semi-global alignment)."""
    if not line:
        return 0.0
    prev = [0] * (len(hyp) + 1)  # free start anywhere in hyp
    for i, r in enumerate(line, 1):
        cur = [i] + [0] * len(hyp)
        for j, h in enumerate(hyp, 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (r != h))
        prev = cur
    return min(prev) / len(line)  # free end anywhere


def score_lyrics(bundle: Path, analysis, ref_path: Path):
    ref_text = ref_path.read_text(encoding="utf-8")
    ref = words(ref_text)
    lines = analysis.get("lyrics") or []
    hyp = [w for l in lines for w in words(l["text"])]
    ref_lines = [words(l) for l in ref_text.splitlines()]
    ref_lines = [l for l in ref_lines if l]
    found = sum(best_window(l, hyp) <= 0.5 for l in ref_lines)
    out = {
        "wer": round(edits(ref, hyp) / max(len(ref), 1), 3),
        "ref_words": len(ref),
        "hyp_words": len(hyp),
        "lines_found": f"{found}/{len(ref_lines)}",
        "lyric_lines": len(lines),
        "longest_line_s": round(max((l["end"] - l["start"] for l in lines), default=0.0), 1),
    }
    vocals = bundle / "stems" / "vocals.wav"
    if vocals.exists() and lines:
        import librosa
        y, _ = librosa.load(vocals, sr=16000, mono=True)
        rms = librosa.feature.rms(y=y, hop_length=1600)[0]  # 10 frames per second
        loud = rms > max(float(np.percentile(rms, 95)) * 0.05, 1e-4)
        on = total = 0
        for l in lines:
            a, b = int(l["start"] * 10), max(int(l["end"] * 10), int(l["start"] * 10) + 1)
            on += int(loud[a:b].sum())
            total += b - a
        out["on_voice"] = round(on / max(total, 1), 3)
    return out


SUFFIX = {
    "": "maj", "maj": "maj", "m": "min", "min": "min", "7": "7", "maj7": "maj7", "m7": "min7",
    "min7": "min7", "dim": "dim", "aug": "aug", "sus2": "sus2", "sus4": "sus4", "6": "maj6",
    "maj6": "maj6", "m6": "min6", "min6": "min6", "dim7": "dim7", "hdim7": "hdim7", "m7b5": "hdim7",
    "9": "9", "maj9": "maj9", "m9": "min9", "min9": "min9", "minmaj7": "minmaj7", "m(maj7)": "minmaj7",
    "7sus4": "sus4(b7)", "sus4(b7)": "sus4(b7)", "+": "aug",
}


def harte(label: str):
    """Poing chord label ('F#', 'Em', 'B7', 'Gmaj6', 'D/F#') -> Harte ('F#:maj', ...)."""
    m = re.fullmatch(r"([A-G][#b]?)([^/]*)(?:/(.+))?", label)
    if not m:
        return "X"
    root, q, bass = m.groups()
    quality = SUFFIX.get(q)
    if quality is None:
        return "X"
    return f"{root}:{quality}"  # bass notes are not scored


def score_chords(analysis, ref_path: Path, offset):
    import mir_eval
    ref_iv, ref_lab = mir_eval.io.load_labeled_intervals(str(ref_path))
    chords = analysis.get("chords") or []
    if not chords:
        return {"error": "no chords"}
    est_iv = np.array([[c["start"], c["end"]] for c in chords])
    est_lab = [harte(c["label"]) for c in chords]

    def run(off):
        r_iv = ref_iv + off
        lo, hi = max(r_iv[0, 0], 0.0), r_iv[-1, 1]
        e_iv, e_lab = mir_eval.util.adjust_intervals(est_iv, est_lab, lo, hi, mir_eval.chord.NO_CHORD, mir_eval.chord.NO_CHORD)
        r2, rl = mir_eval.util.adjust_intervals(r_iv, list(ref_lab), lo, hi, mir_eval.chord.NO_CHORD, mir_eval.chord.NO_CHORD)
        iv, rl, el = mir_eval.util.merge_labeled_intervals(r2, rl, e_iv, e_lab)
        dur = mir_eval.util.intervals_to_durations(iv)
        res = {}
        for name in ("root", "majmin", "mirex", "sevenths"):
            comp = getattr(mir_eval.chord, name)(rl, el)
            res[name] = round(float(mir_eval.chord.weighted_accuracy(comp, dur)), 3)
        return res

    out = {}
    if offset == "auto":
        grid = np.arange(-3.0, 3.0001, 0.05)
        scores = [run(o)["majmin"] for o in grid]
        best = int(np.argmax(scores))
        offset = float(grid[best])
        out["offset_peak"] = f"{scores[best]:.3f} at {offset:+.2f}s (median {np.median(scores):.3f})"
    out["offset"] = round(float(offset), 2)
    out.update(run(float(offset)))
    out["chord_spans"] = len(chords)
    out["labels"] = len(set(c["label"] for c in chords))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bundle", type=Path)
    ap.add_argument("--lyrics", type=Path)
    ap.add_argument("--chords", type=Path)
    ap.add_argument("--offset", default="0")
    args = ap.parse_args()
    analysis = json.loads((args.bundle / "analysis.json").read_text(encoding="utf-8"))
    result = {"bundle": args.bundle.name}
    if args.lyrics:
        result["lyrics"] = score_lyrics(args.bundle, analysis, args.lyrics)
    if args.chords:
        result["chords"] = score_chords(analysis, args.chords, args.offset if args.offset == "auto" else float(args.offset))
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    sys.exit(main())
