"""Chords from notes: an experiment for a chord path Makepad could run natively.

    uv run --with basic-pitch --with onnxruntime --with "numpy<2" --with "setuptools<70"         python sidecar/experiments/notes_chords.py <bundle> [--vocab majmin|sevenths]

Basic Pitch note posteriorgrams of the bundle's bass and other stems are summed per beat
(beats from its analysis.json) into a pitch-class profile, scored against chord templates
plus a bonus for the bass on the root and an optional key prior, and smoothed with Viterbi
(a smaller change penalty on downbeats). Writes <bundle>/notechords_<vocab>.lab (Harte labels),
to score with mir_eval against a reference. Results are in docs/ANALYSIS.md: about 15 points
behind SheetSage2 on majmin, so it is not used.

Knobs (environment): PRIOR key-prior weight (0.08), AKEY=1 take the key from analysis.json
instead of a Krumhansl estimate, BW bass-profile weight (0.6), BR bass-on-root bonus (0.25),
TH weight of the third in the templates (1.0), PM / PD change penalty mid-bar / on downbeats (0.35 / 0.12).
"""
import json
import os
import sys
from pathlib import Path

import numpy as np

PRIOR = float(os.environ.get("PRIOR", "0.08"))
BW = float(os.environ.get("BW", "0.6"))
BR = float(os.environ.get("BR", "0.25"))
TH = float(os.environ.get("TH", "1.0"))
PM = float(os.environ.get("PM", "0.35"))
PD = float(os.environ.get("PD", "0.12"))

bundle = Path(sys.argv[1])
vocab = sys.argv[sys.argv.index("--vocab") + 1] if "--vocab" in sys.argv else "majmin"
cache = bundle / "bp_notes.npz"
if cache.exists():
    z = np.load(cache); post = {"bass": z["bass"], "other": z["other"]}; fps = float(z["fps"])
else:
    from basic_pitch.inference import predict, Model
    from basic_pitch import ICASSP_2022_MODEL_PATH
    model = Model(ICASSP_2022_MODEL_PATH)
    post = {}
    for s in ("bass", "other"):
        out, _, _ = predict(str(bundle / "stems" / f"{s}.wav"), model)
        post[s] = out["note"]  # frames x 88 (MIDI 21..108)
    fps = 22050 / 256
    np.savez(cache, bass=post["bass"], other=post["other"], fps=fps)

def chroma(P, lo=21):
    C = np.zeros((P.shape[0], 12))
    for k in range(P.shape[1]):
        C[:, (lo + k) % 12] += P[:, k]
    return C

def bass_chroma(P, lo=21):
    # the lowest strong note per frame
    C = np.zeros((P.shape[0], 12))
    for t in range(P.shape[0]):
        on = np.flatnonzero(P[t] > 0.3)
        if len(on):
            C[t, (lo + on[0]) % 12] += P[t, on[0]]
    return C

n = min(len(post["bass"]), len(post["other"]))
Co = chroma(post["other"][:n]); Cb = bass_chroma(post["bass"][:n]) + 0.3 * chroma(post["bass"][:n])
a = json.loads((bundle / "analysis.json").read_text(encoding="utf-8"))
beats = [b for b in a["beats"]]
end = n / fps
edges = [b for b in beats if b < end] + [end]

NOTES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]
QUAL = {"maj": [0, 4, 7], "min": [0, 3, 7]}
if vocab == "sevenths":
    QUAL.update({"7": [0, 4, 7, 10], "maj7": [0, 4, 7, 11], "min7": [0, 3, 7, 10]})
labels, T, roots = [], [], []
for r in range(12):
    for q, iv in QUAL.items():
        t = np.zeros(12); t[[(r + i) % 12 for i in iv]] = 1.0; t[(r + iv[1]) % 12] = TH
        if len(iv) == 4: t[(r + iv[3]) % 12] = 0.6
        labels.append(f"{NOTES[r]}:{q}"); T.append(t / np.linalg.norm(t)); roots.append(r)
T = np.array(T); roots = np.array(roots)

def seg_sum(C):
    out = []
    for s, e in zip(edges, edges[1:]):
        i, j = int(s * fps), max(int(e * fps), int(s * fps) + 1)
        out.append(C[i:j].sum(0))
    return np.array(out)

So, Sb = seg_sum(Co), seg_sum(Cb)
energy = So.sum(1) + Sb.sum(1)
X = So / (So.sum(1, keepdims=True) + 1e-9) + BW * Sb / (Sb.sum(1, keepdims=True) + 1e-9)
Xn = X / (np.linalg.norm(X, axis=1, keepdims=True) + 1e-9)
score = Xn @ T.T  # beats x chords
bassn = Sb / (Sb.sum(1, keepdims=True) + 1e-9)
score += BR * bassn[:, roots]  # bass on the root

# key (Krumhansl) from the whole song
maj = np.array([6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88])
mnr = np.array([6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17])
g = X.sum(0)
ks = [(np.corrcoef(np.roll(maj, k), g)[0, 1], k, "maj") for k in range(12)] + [(np.corrcoef(np.roll(mnr, k), g)[0, 1], k, "min") for k in range(12)]
_, kroot, kmode = max(ks)
if os.environ.get("AKEY") and a.get("key"):
    r_, m_ = a["key"].split(" ")
    kroot = (NOTES.index(r_) if r_ in NOTES else ["C","Db","D","Eb","E","F","Gb","G","Ab","A","Bb","B"].index(r_)); kmode = "maj" if m_ == "major" else "min"
tonic = kroot if kmode == "maj" else (kroot + 3) % 12  # relative major
diatonic = {(tonic + 0) % 12: "maj", (tonic + 2) % 12: "min", (tonic + 4) % 12: "min", (tonic + 5) % 12: "maj",
            (tonic + 7) % 12: "maj", (tonic + 9) % 12: "min"}
base = {"maj": "maj", "min": "min", "7": "maj", "maj7": "maj", "min7": "min"}
prior = np.array([PRIOR if diatonic.get(r) == base[l.split(":")[1]] else 0.0 for r, l in zip(roots, labels)])
if vocab == "sevenths":  # dominant 7 of the key
    prior[labels.index(f"{NOTES[(tonic + 7) % 12]}:7")] = 0.08 if vocab == "sevenths" else 0
score = score + prior

# Viterbi with a change penalty (smaller on downbeats)
down = set(round(d, 2) for d in a.get("downbeats", []))
pen_mid, pen_down = PM, PD
K, B = len(labels), len(score)
dp = score[0].copy(); back = np.zeros((B, K), int)
for b in range(1, B):
    pen = pen_down if round(edges[b], 2) in down else pen_mid
    best = dp.max(); arg = dp.argmax()
    stay = dp
    choose_stay = stay >= best - pen
    back[b] = np.where(choose_stay, np.arange(K), arg)
    dp = np.where(choose_stay, stay, best - pen) + score[b]
path = [int(dp.argmax())]
for b in range(B - 1, 0, -1):
    path.append(back[b][path[-1]])
path = path[::-1]
quiet = energy < 0.02 * np.median(energy)
rows = []
for b, k in enumerate(path):
    lab = "N" if quiet[b] else labels[k]
    s, e = edges[b], edges[b + 1]
    if rows and rows[-1][2] == lab:
        rows[-1][1] = e
    else:
        rows.append([s, e, lab])
out = bundle / f"notechords_{vocab}.lab"
out.write_text("".join(f"{s:.3f} {e:.3f} {l}\n" for s, e, l in rows))
print(bundle.name, vocab, "key", NOTES[kroot], kmode, len(rows), "spans ->", out.name)
