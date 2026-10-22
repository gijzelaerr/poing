"""Split a vocal stem into its dry voice and its reverb/echo.

Usage: uv run --project sidecar/dereverb python poing_dereverb.py <vocals.wav> <out_dir> [start end]

Writes <out_dir>/dry.wav and <out_dir>/wet.wav at the input's rate. The wet
part is written as input - dry, so dry + wet is the input exactly and any
mix of the two is just "how much echo". With start and end (seconds, end >
start) only that range is written. Progress goes to stdout as JSON lines.
"""
import json
import os
import sys
import tempfile
import time
from pathlib import Path

MODEL = os.environ.get("POING_DEREVERB_MODEL", "dereverb_mel_band_roformer_anvuew_sdr_19.1729.ckpt")


def emit(stage, progress, message, **extra):
    print(json.dumps({"stage": stage, "progress": round(progress, 3), "message": message, **extra}), flush=True)


def main():
    src, out = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
    out.mkdir(parents=True, exist_ok=True)
    began = time.time()
    emit("load", 0.05, "Loading the de-reverb model")
    import numpy as np
    import soundfile as sf
    from audio_separator.separator import Separator

    with tempfile.TemporaryDirectory() as tmp:
        sep = Separator(output_dir=tmp, output_format="WAV", log_level=40)
        sep.load_model(model_filename=MODEL)
        emit("separate", 0.3, "Separating the voice from its echo")
        files = [Path(tmp) / f for f in sep.separate(str(src))]
        dry_file = next((f for f in files if "noreverb" in f.name.lower()), None)
        if dry_file is None:
            raise RuntimeError(f"no dry output among {[f.name for f in files]}")
        dry, sr_dry = sf.read(dry_file, always_2d=True)
    voice, sr = sf.read(src, always_2d=True)
    if sr_dry != sr:
        raise RuntimeError(f"model wrote {sr_dry} Hz for a {sr} Hz input")
    n = min(len(voice), len(dry))
    dry = np.zeros_like(voice) + np.pad(dry[:n], ((0, len(voice) - n), (0, 0)))[:, : voice.shape[1]]
    wet = voice - dry
    if len(sys.argv) > 4 and float(sys.argv[4]) > float(sys.argv[3]):
        a, b = int(float(sys.argv[3]) * sr), int(float(sys.argv[4]) * sr)
        dry, wet = dry[a:b], wet[a:b]
    sf.write(out / "dry.wav", dry, sr, subtype="FLOAT")
    sf.write(out / "wet.wav", wet, sr, subtype="FLOAT")
    emit("done", 1.0, f"De-reverbed in {time.time() - began:.0f}s")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # report to the app instead of a bare traceback
        emit("error", 0.0, f"{type(exc).__name__}: {exc}")
        raise
