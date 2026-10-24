#!/usr/bin/env bash
# Build the "engine-cuda" model-manager pack (app/models.json): one zip with
# a CUDA build of poing-engine.exe for every NVIDIA GPU CUDA 13 runs on
# (Turing sm_75 to Blackwell sm_120, plus PTX that newer GPUs JIT-compile),
# the cuBLAS DLLs it links, and NVIDIA's EULA, which lets an application
# ship those DLLs (packaging/NOTICE.md).
#
# The installer's poing-engine.exe is built on CUDA-less CI runners, so it
# is CPU-only, and Makepad's LLM runs only on CUDA on Windows. This pack is
# what runs Poing's own model and the native analysis on an installed copy.
# The engine in it must speak the app's protocol: rebuild the pack when
# engine/ or the Makepad pin changes, and bump the pack in app/models.json.
#
#     packaging/build-engine-cuda.sh                 # build target/engine-cuda/poing-engine-cuda.zip
#     packaging/build-engine-cuda.sh --upload TAG    # attach that zip, as built, to GitHub release TAG
#
# Then paste the printed entry into the engine-cuda pack in app/models.json.
# Upload the zip you tested: a rebuild makes a new zip with a new sha256.
# TAG is `engine-cuda-YYYYMMDD`, never `v*` (that starts the installer build).
#
# Needs: Windows, the CUDA 13 toolkit, MSVC (Visual Studio C++ tools),
# Python (for the zip) and gh to upload.
set -euo pipefail

cd "$(dirname "$0")/.."
CUDA="${MAKEPAD_CUDA_ROOT:-C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.4}"
# SASS per generation (sm_80 also covers 8.6/8.7; 120a is RTX 50), PTX for anything newer.
ARCHS="${ENGINE_CUDA_ARCHS:-75,80,86,89,120a,compute_120}"
TARGET="${ENGINE_CUDA_TARGET:-target/engine-cuda}"
NAME=poing-engine-cuda
OUT="$TARGET/$NAME"
ZIP="$TARGET/$NAME.zip"
# cuBLAS and cuBLASLt. The CUDA 13 runtime (cudart.lib) links statically and
# talks to the driver's nvcuda.dll, so there is no cudart64_13.dll to ship.
DLLS=(cublas64_13.dll cublasLt64_13.dll)
TAG=""
if [ "${1:-}" = "--upload" ]; then
    TAG="${2:?--upload needs a release tag (engine-cuda-YYYYMMDD)}"
    case "$TAG" in v*) echo "a v* tag starts the installer build: use engine-cuda-YYYYMMDD" >&2; exit 1 ;; esac
fi

# The pack's file and unpacked size, for app/models.json.
entry() {
cat <<JSON
"files": [
  {
    "url": "https://github.com/gijzelaerr/poing/releases/download/${TAG:-TAG}/$NAME.zip",
    "path": "$NAME.zip",
    "size": $(stat -c %s "$ZIP"),
    "sha256": "$(sha256sum "$ZIP" | cut -d' ' -f1)"
  }
],
"unpack": { "zip": "$NAME.zip", "into": "bin", "bytes": $(( $(du -sb "$OUT" | cut -f1) - $(stat -c %s "$ZIP") )) }
JSON
}

if [ -n "$TAG" ]; then
    [ -f "$ZIP" ] || { echo "no $ZIP: build it first" >&2; exit 1; }
    gh release view "$TAG" >/dev/null 2>&1 || gh release create "$TAG" --prerelease --title "GPU engine for Windows ($TAG)" \
        --notes "The engine-cuda model-manager pack: poing-engine.exe built for NVIDIA GPUs (arch list $ARCHS), with the cuBLAS DLLs NVIDIA lets applications ship, and NVIDIA's EULA. Poing downloads and unpacks it itself (Settings → Models); it is not meant to be installed by hand."
    gh release upload "$TAG" "$ZIP" --clobber
    entry
    exit
fi

# A missing toolkit must fail here, not quietly build the CPU stub.
export MAKEPAD_CUDA_ROOT="$CUDA" MAKEPAD_GGML_CUDA_ARCH="$ARCHS" MAKEPAD_GGML_REQUIRE_CUDA=1
cargo build --release --locked -p poing-engine --target-dir "$TARGET"

rm -rf "$OUT" "$ZIP"
mkdir -p "$OUT"
cp "$TARGET/release/poing-engine.exe" "$OUT/"
for d in "${DLLS[@]}"; do cp "$CUDA/bin/x64/$d" "$OUT/"; done
cp packaging/licences/NVIDIA-CUDA-EULA.txt "$OUT/"

# Ship exactly what the engine links from the toolkit (cuDNN, say, would need adding).
DUMPBIN=$(ls "C:/Program Files/Microsoft Visual Studio/"*/*/VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe 2>/dev/null | tail -1)
if [ -n "$DUMPBIN" ]; then
    deps() { MSYS_NO_PATHCONV=1 "$DUMPBIN" /nologo /dependents "$1" | tr -d '\r' | sed -n 's/^ *\([A-Za-z0-9_.-]*\.[dD][lL][lL]\)$/\1/p'; }
    DEPS=$(for f in "$OUT"/*.exe "$OUT"/*.dll; do deps "$f"; done | sort -u)
    for dep in $DEPS; do
        if [ -f "$CUDA/bin/x64/$dep" ] && [ ! -f "$OUT/$dep" ]; then
            echo "the pack links $dep from the toolkit but lacks it" >&2
            exit 1
        fi
    done
    for d in "${DLLS[@]}"; do
        echo "$DEPS" | grep -qix "$d" || { echo "nothing in the pack links $d: not a CUDA build?" >&2; exit 1; }
    done
else
    echo "dumpbin not found: skipped the DLL check" >&2
fi

# It must answer with the GPU, using only its own DLLs.
PATH="$OUT:/c/Windows/System32:/c/Windows" "$OUT/poing-engine.exe" status > "$TARGET/status.json"
grep -q '"whisper_backend":"cuda"' "$TARGET/status.json" || { cat "$TARGET/status.json"; echo "the pack's engine has no CUDA backend" >&2; exit 1; }

# One zip with a top folder (the model manager drops it when unpacking).
python -I - "$TARGET" "$NAME" <<'PY'
import os, sys, zipfile
target, name = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(os.path.join(target, name + ".zip"), "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for f in sorted(os.listdir(os.path.join(target, name))):
        z.write(os.path.join(target, name, f), name + "/" + f)
PY

entry
