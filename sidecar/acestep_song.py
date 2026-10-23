"""Make several versions of a song with ACE-Step 1.5: a new song from a
caption and lyrics (text2music), or remixes of an existing recording (cover).

Usage (inside the ACE-Step-1.5 uv project):
    uv run --project <ACE-Step-1.5> python acestep_song.py <job.json>

job.json: {"task": "text2music" | "cover", "out_dir": dir, "count": n,
           "caption": "...", "lyrics": "...", "language": "nl"?, "bpm": n?,
           "key": "..."?, "duration": s?, "src": audio (cover),
           "strengths": [..] (cover, one per version), "seeds": [..]?,
           "thinking": bool (text2music: let the 5 Hz LM plan the song)}

The models load once; each version is written as <out_dir>/version-<i>.wav and
announced on stdout ({"stage": "version", "index": i, "path": ...}) as soon as
it is done, so the app can show it while the next one renders. Progress lines
are JSON like the other sidecars.
"""
import json
import os
import shutil
import sys
import time
from pathlib import Path


def emit(stage, progress, message, **extra):
    print(json.dumps({"stage": stage, "progress": round(progress, 3), "message": message, **extra}), flush=True)


def main():
    job = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    root = os.environ.get("POING_ACESTEP") or os.getcwd()
    sys.path.insert(0, root)
    task = job.get("task", "text2music")
    count = max(1, int(job.get("count", 3)))
    out_dir = Path(job["out_dir"])
    out_dir.mkdir(parents=True, exist_ok=True)
    emit("load", 0.0, "Loading ACE-Step")
    from acestep.handler import AceStepHandler
    from acestep.inference import GenerationConfig, GenerationParams, generate_music

    began = time.time()
    dit = AceStepHandler()
    msg, ok = dit.initialize_service(project_root=root, config_path=os.environ.get("DIT", "acestep-v15-turbo"),
                                     device="cuda", offload_to_cpu=True)
    if not ok:
        emit("error", 0.0, f"ACE-Step did not load: {msg}")
        sys.exit(1)
    llm = None
    thinking = task == "text2music" and bool(job.get("thinking", True))
    if thinking:
        emit("load", 0.1, "Loading the song planner")
        from acestep.llm_inference import LLMHandler
        llm = LLMHandler()
        msg, ok = llm.initialize(checkpoint_dir=os.path.join(root, "checkpoints"),
                                 lm_model_path=os.environ.get("LM", "acestep-5Hz-lm-1.7B"),
                                 backend="pt", device="cuda", offload_to_cpu=True)
        if not ok:
            # the DiT alone still makes a song, just less planned
            emit("load", 0.1, f"Song planner did not load ({msg}); going without")
            llm, thinking = None, False

    seeds = job.get("seeds") or [1000 + 7919 * i for i in range(count)]
    strengths = job.get("strengths") or [0.6] * count
    for i in range(count):
        emit("render", 0.15 + 0.85 * i / count, f"Making version {i + 1} of {count}")
        t0 = time.time()
        kw = dict(caption=job.get("caption", ""), lyrics=job.get("lyrics", ""),
                  vocal_language=job.get("language") or "unknown",
                  bpm=job.get("bpm") or None, keyscale=job.get("key") or "",
                  inference_steps=8, guidance_scale=1.0, seed=int(seeds[i % len(seeds)]))
        if task == "cover":
            kw.update(task_type="cover", src_audio=job["src"], audio_cover_strength=float(strengths[i % len(strengths)]))
        else:
            kw.update(task_type="text2music", duration=float(job.get("duration") or -1),
                      instrumental=not job.get("lyrics", "").strip(), thinking=thinking)
        work = out_dir / f"version-{i + 1}-work"
        res = generate_music(dit, llm, params=GenerationParams(**kw),
                             config=GenerationConfig(batch_size=1, audio_format="wav"), save_dir=str(work))
        paths = [a.get("path") for a in (res.audios or []) if a.get("path")]
        if not res.success or not paths:
            emit("error", 0.0, f"version {i + 1} failed: {getattr(res, 'status_message', '')}")
            sys.exit(1)
        out = out_dir / f"version-{i + 1}.wav"
        shutil.move(paths[0], out)
        shutil.rmtree(work, ignore_errors=True)
        emit("version", 0.15 + 0.85 * (i + 1) / count, f"Version {i + 1} is ready ({time.time() - t0:.0f}s)",
             index=i + 1, path=str(out))
    emit("done", 1.0, f"Made {count} versions in {time.time() - began:.0f}s")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # report to the app instead of a bare traceback
        emit("error", 0.0, f"{type(exc).__name__}: {exc}")
        raise
