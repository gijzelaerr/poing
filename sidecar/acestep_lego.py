"""Generate one instrument track in the context of a song with ACE-Step 1.5 lego.

Usage (inside the ACE-Step-1.5 uv project):
    uv run --project <ACE-Step-1.5> python acestep_lego.py <job.json>

job.json: {"src": audio, "out": wav, "track": "drums", "caption": "...", "seed": n,
           "bpm": n?, "key": "..."?, "steps": 50?, "cfg": 7.0?}

Lego is base-model only. The output is the whole song's length; the app
cleans it into a layer or a lane replacement. Progress goes to stdout as JSON
lines like the analysis sidecar.
"""
import json
import os
import shutil
import sys
import time
from pathlib import Path

TRACKS = ["woodwinds", "brass", "fx", "synth", "strings", "percussion",
          "keyboard", "guitar", "bass", "drums", "backing_vocals", "vocals"]


def emit(stage, progress, message, **extra):
    print(json.dumps({"stage": stage, "progress": round(progress, 3), "message": message, **extra}), flush=True)


def main():
    job = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    if job["track"] not in TRACKS:
        raise ValueError(f"lego cannot make a {job['track']!r} track (one of {', '.join(TRACKS)})")
    root = os.environ.get("POING_ACESTEP") or os.getcwd()
    sys.path.insert(0, root)
    emit("load", 0.0, "Loading ACE-Step (base model)")
    from acestep.handler import AceStepHandler
    from acestep.inference import GenerationConfig, GenerationParams, generate_music

    began = time.time()
    dit = AceStepHandler()
    msg, ok = dit.initialize_service(project_root=root, config_path=os.environ.get("DIT", "acestep-v15-base"),
                                     device="cuda", offload_to_cpu=True)
    if not ok:
        emit("error", 0.0, f"ACE-Step did not load: {msg}")
        sys.exit(1)
    emit("lego", 0.25, f"Playing a new {job['track']} part")
    out = Path(job["out"])
    work = out.parent / (out.stem + "-lego")
    params = GenerationParams(
        task_type="lego", src_audio=job["src"],
        instruction=f"Generate the {job['track']} track based on the audio context:",
        caption=job["caption"], repainting_start=0.0, repainting_end=-1,
        bpm=int(job["bpm"]) if job.get("bpm") else None, keyscale=job.get("key") or "",
        thinking=False, inference_steps=int(job.get("steps", 50)), guidance_scale=float(job.get("cfg", 7.0)),
        seed=int(job.get("seed", 1)),
    )
    res = generate_music(dit, None, params=params, config=GenerationConfig(batch_size=1, audio_format="wav"),
                         save_dir=str(work))
    paths = [a.get("path") for a in (res.audios or []) if a.get("path")]
    if not res.success or not paths:
        emit("error", 0.0, f"lego failed: {getattr(res, 'status_message', '')}")
        sys.exit(1)
    shutil.move(paths[0], out)
    shutil.rmtree(work, ignore_errors=True)
    emit("done", 1.0, f"Played in {time.time() - began:.0f}s")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # report to the app instead of a bare traceback
        emit("error", 0.0, f"{type(exc).__name__}: {exc}")
        raise
