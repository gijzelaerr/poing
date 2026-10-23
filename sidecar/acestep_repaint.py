"""Repaint one time range of a song with ACE-Step 1.5 and write just that range.

Usage (inside the ACE-Step-1.5 uv project):
    uv run --project <ACE-Step-1.5> python acestep_repaint.py <job.json>

job.json: {"src": audio, "out": wav, "start": s, "end": s, "margin": s,
           "caption": "...", "lyrics": "...", "seed": n, "bpm": n?, "key": "..."?}

ACE-Step returns the whole song with the original spliced back outside the
range. Only [start - margin, end + margin] is written: the margin is still the
original, so the app can cross-fade there without a seam. An end past the
song's length makes it longer (ACE-Step pads the source with silence and
fills it): then only the margin before the range is the original. Progress goes to
stdout as JSON lines like the analysis sidecar; the last one says where the
written range starts.
"""
import json
import os
import sys
import time
from pathlib import Path

import poing_ace


def emit(stage, progress, message, **extra):
    print(json.dumps({"stage": stage, "progress": round(progress, 3), "message": message, **extra}), flush=True)


def main():
    job = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    root = poing_ace.root()
    emit("load", 0.0, "Loading ACE-Step")
    import soundfile as sf
    from acestep.handler import AceStepHandler
    from acestep.inference import GenerationConfig, GenerationParams, generate_music

    began = time.time()
    dit = AceStepHandler()
    msg, ok = dit.initialize_service(project_root=root, config_path=os.environ.get("DIT", "acestep-v15-turbo"),
                                     device="cuda", offload_to_cpu=True)
    if not ok:
        emit("error", 0.0, f"ACE-Step did not load: {msg}")
        sys.exit(1)
    emit("repaint", 0.3, "Repainting")
    out = Path(job["out"])
    work = out.parent / (out.stem + "-full")
    params = GenerationParams(
        task_type="repaint", src_audio=job["src"], caption=job["caption"], lyrics=job.get("lyrics", ""),
        bpm=job.get("bpm") or None, keyscale=job.get("key") or "", vocal_language=job.get("language", "en"),
        repainting_start=float(job["start"]), repainting_end=float(job["end"]),
        inference_steps=8, guidance_scale=1.0, seed=int(job.get("seed", 1234)),
    )
    res = generate_music(dit, None, params=params, config=GenerationConfig(batch_size=1, audio_format="wav"),
                         save_dir=str(work))
    paths = [a.get("path") for a in (res.audios or []) if a.get("path")]
    if not res.success or not paths:
        emit("error", 0.0, f"repaint failed: {getattr(res, 'status_message', '')}")
        sys.exit(1)
    import numpy as np
    audio, sr = sf.read(paths[0], always_2d=True)
    margin = float(job.get("margin", 1.0))
    a = max(0.0, float(job["start"]) - margin)
    b = min(len(audio) / sr, float(job["end"]) + margin)
    take = audio[int(a * sr):int(b * sr)]
    # ACE-Step normalises its output (about -1 dB). The margins are the original
    # spliced back in, so the gain they show is undone to make the seams exact.
    src, src_sr = sf.read(job["src"], always_2d=True)
    if src_sr == sr and src.shape[1] == take.shape[1]:
        ref = src[int(a * sr):int(a * sr) + len(take)]
        m = int(min(margin, 0.5 * (b - a)) * sr)
        # a range past the song's end (making it longer) has only the margin before it
        idx = np.r_[0:m, len(take) - m:len(take)] if len(ref) == len(take) else np.r_[0:m]
        x, y = ref[idx].ravel(), take[idx].ravel()
        if len(ref) >= m and np.dot(x, x) > 0:
            gain = float(np.dot(x, y) / np.dot(x, x))
            if 0.5 < gain < 2.0:
                take = take / gain
    sf.write(out, take, sr)
    emit("done", 1.0, f"Repainted in {time.time() - began:.0f}s", start=round(a, 4), end=round(b, 4))


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # report to the app instead of a bare traceback
        emit("error", 0.0, f"{type(exc).__name__}: {exc}")
        raise
