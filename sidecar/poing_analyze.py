"""Poing analysis sidecar: understand a song.

    uv run --project sidecar python sidecar/poing_analyze.py <audio> <bundle_dir>

Writes into <bundle_dir>:
    stems/{vocals,drums,bass,other}.wav   Demucs htdemucs_ft
    analysis.json                         tempo, key, beats, downbeats, chords, sections, lyrics
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


def emit(stage, progress, message="", **extra):
    print(json.dumps({"stage": stage, "progress": round(progress, 3), "message": message, **extra}), flush=True)


def overall(stage, within):
    done = 0.0
    for name, share in STAGES:
        if name == stage:
            return done + share * within
        done += share
    return done


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


def chord_name(label: str):
    """SheetSage2 'F#:maj' / 'E:min' / 'B:7' -> 'F#' / 'Em' / 'B7'; 'N' -> None."""
    if label in ("N", "X", "?"):
        return None
    root, _, quality = label.partition(":")
    q = {"maj": "", "min": "m", "": ""}.get(quality, quality)
    return root + q


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


def structure(audio: Path, device: str):
    import torch
    from transformers import AutoModel
    emit("structure", overall("structure", 0.0), "Finding sections, key, chords and beats")
    model = AutoModel.from_pretrained(os.environ.get("POING_SHEETSAGE", "m-a-p/SheetSage2"),
                                      trust_remote_code=True).eval().to(device)
    with tempfile.TemporaryDirectory() as tmp:
        try:
            model.transcribe(str(audio), output_dir=tmp)
        except (ValueError, RuntimeError) as exc:
            # The ABC score can fail on a beat-grid glitch; the labs are still written.
            emit("structure", overall("structure", 0.8), f"score unavailable ({exc}); using annotations")
        tmp = Path(tmp)
        beats = [(float(r[0]), int(r[1])) for r in read_lab(tmp / "beat.lab") if r[1].isdigit()]
        key_rows = read_lab(tmp / "key.lab")
        chord_rows = read_lab(tmp / "chord.lab")
        struct_rows = read_lab(tmp / "structure.lab")
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
        name = chord_name(r[2]) if len(r) > 2 else None
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


# Whisper's subtitle-trained boilerplate that appears over instrumentals.
HALLUCINATIONS = re.compile(
    r"(zang en muziek|muziek|tv gelderland|ondertiteling|ondertiteld|thanks for watching|subtitles by|amara\.org|\[music\]|♪)",
    re.IGNORECASE,
)


def lyrics(vocals: Path, device: str):
    import librosa
    import numpy as np
    import torch
    from transformers import AutoProcessor, WhisperForConditionalGeneration
    emit("lyrics", overall("lyrics", 0.0), "Transcribing lyrics")
    audio, _ = librosa.load(vocals, sr=16000, mono=True)
    dtype = torch.float16 if device == "cuda" else torch.float32
    proc = AutoProcessor.from_pretrained("openai/whisper-large-v3")
    model = WhisperForConditionalGeneration.from_pretrained("openai/whisper-large-v3", torch_dtype=dtype).to(device)
    inputs = proc(audio, sampling_rate=16000, return_tensors="pt", truncation=False, padding="longest",
                  return_attention_mask=True)
    inputs = {k: v.to(device, dtype if v.dtype == torch.float32 else v.dtype) for k, v in inputs.items()}
    # Long-form decoding: the chunked pipeline mode loops on sung vocals.
    out = model.generate(**inputs, task="transcribe", return_timestamps=True, return_segments=True,
                         condition_on_prev_tokens=False, temperature=(0.0, 0.2, 0.4, 0.6),
                         compression_ratio_threshold=1.8, logprob_threshold=-1.0, no_speech_threshold=0.6)
    rms = librosa.feature.rms(y=audio, hop_length=1600)[0]  # 10 frames per second
    loud = rms > max(float(np.percentile(rms, 95)) * 0.05, 1e-4)
    lines = []
    for seg in out["segments"][0]:
        text = proc.decode(seg["tokens"], skip_special_tokens=True).strip()
        start, end = float(seg["start"]), float(seg["end"])
        if not text or HALLUCINATIONS.search(text):
            continue
        # Drop text Whisper placed over silence in the vocal stem.
        a, b = int(start * 10), max(int(end * 10), int(start * 10) + 1)
        if not loud[a:b].any():
            continue
        lines.append({"start": round(start, 2), "end": round(end, 2), "text": text})
    del model
    if device == "cuda":
        torch.cuda.empty_cache()
    emit("lyrics", overall("lyrics", 1.0), f"{len(lines)} lyric lines")
    return lines


def main():
    audio, bundle = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
    began = time.time()
    try:
        import torch
        device = "cuda" if torch.cuda.is_available() else "cpu"
        emit("start", 0.0, f"Analysing on {device}")
        separate(audio, bundle, device)
        result = structure(audio, device)
        result["lyrics"] = lyrics(bundle / "stems" / "vocals.wav", device)
        result["stems"] = {s: f"stems/{s}.wav" for s in ("vocals", "drums", "bass", "other")}
        (bundle / "analysis.json").write_text(json.dumps(result, indent=1, ensure_ascii=False), encoding="utf-8")
        emit("done", 1.0, f"Analysis done in {time.time() - began:.0f}s")
    except Exception as exc:  # report to the app instead of a bare traceback
        emit("error", 0.0, f"{type(exc).__name__}: {exc}")
        raise


if __name__ == "__main__":
    main()
