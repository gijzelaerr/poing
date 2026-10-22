"""Clean a generated part into a layer that fills the song's gaps.

Usage: uv run --project sidecar python poing_layer.py <job.json>

job.json: {"src": lego output, "out": wav, "stem": "drums"|"bass"|"vocals"|"other",
           "reference": lane wav?, "match_lane": lane wav?, "start": s?, "end": s?,
           "thin_db": -35, "gain_thin": 1.0, "gain_full": 0.35}

1. Demucs (htdemucs_ft) keeps just the wanted stem: lego output carries some
   of the song it was conditioned on.
2. With a reference lane, the layer plays at gain_thin where that lane is
   quieter than thin_db (per 2 s window) and at gain_full elsewhere, smoothed
   over ~1 s: it fills the gaps and sits under the parts that are already full.
3. With a lane to match (a replacement), it is scaled to that lane's loudness
   over the range: ACE-Step normalises its output.
4. Only [start, end] is written when a range is given (a region).
"""
import json
import sys
import tempfile
import time
from pathlib import Path


def emit(stage, progress, message, **extra):
    print(json.dumps({"stage": stage, "progress": round(progress, 3), "message": message, **extra}), flush=True)


def gap_fill_gain(ref, sr, thin_db, gain_thin, gain_full):
    import numpy as np
    n = len(ref)
    win = int(2 * sr)
    gain = np.empty(n)
    for a in range(0, n, win):
        seg = ref[a:a + win]
        db = 20 * np.log10(np.sqrt(np.mean(seg ** 2)) + 1e-9)
        gain[a:a + win] = gain_thin if db < thin_db else gain_full
    k = int(1.0 * sr)
    c = np.cumsum(np.pad(gain, (k // 2 + 1, k - k // 2 - 1), mode="edge"))
    return (c[k:] - c[:-k]) / k  # moving average via a running sum (np.convolve is O(n*k))


def read_at(path, sr):
    """A lane as (frames, channels) at rate sr (lanes Poing baked are 48 kHz, Demucs writes 44.1)."""
    import librosa
    import soundfile as sf
    x, x_sr = sf.read(path, always_2d=True)
    return x if x_sr == sr else librosa.resample(x.T, orig_sr=x_sr, target_sr=sr).T


def main():
    job = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    began = time.time()
    import numpy as np
    import soundfile as sf
    import torch
    import demucs.separate

    device = "cuda" if torch.cuda.is_available() else "cpu"
    stem = job.get("stem", "other")
    emit("clean", 0.6, f"Keeping just the {stem}")
    with tempfile.TemporaryDirectory() as tmp:
        demucs.separate.main(["-n", "htdemucs_ft", "-d", device, "-o", tmp, "--two-stems", stem, job["src"]])
        part, sr = sf.read(next(Path(tmp).rglob(f"{stem}.wav")), always_2d=True)
    reference = job.get("reference") or job.get("ref")
    if reference:
        emit("fill", 0.9, "Fitting it into the gaps")
        ref = read_at(reference, sr)
        n = min(len(part), len(ref))
        gain = gap_fill_gain(ref[:n].mean(axis=1), sr, job.get("thin_db", -35), job.get("gain_thin", 1.0),
                             job.get("gain_full", 0.35))
        part = part[:n] * gain[:, None]
    start, end = float(job.get("start") or 0.0), float(job.get("end") or 0.0)
    if job.get("match_lane"):
        lane = read_at(job["match_lane"], sr)
        a, b = (int(start * sr), int(end * sr)) if end > start else (0, min(len(lane), len(part)))
        rms = lambda x: float(np.sqrt(np.mean(x ** 2))) if len(x) else 0.0
        target, have = rms(lane[a:b]), rms(part[a:b])
        if have > 1e-6 and target > 1e-6:
            part = part * (target / have)
    if end > start:
        part = part[int(start * sr):int(end * sr)]
    sf.write(job["out"], part, sr, subtype="FLOAT")
    emit("done", 1.0, f"Layer ready in {time.time() - began:.0f}s")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # report to the app instead of a bare traceback
        emit("error", 0.0, f"{type(exc).__name__}: {exc}")
        raise
