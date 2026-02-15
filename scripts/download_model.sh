#!/usr/bin/env bash
# Download MusicGen-small ONNX model (int8 quantized) from HuggingFace.
#
# Usage: ./scripts/download_model.sh
#
# Downloads to: models/musicgen-small/

set -eo pipefail

REPO="harisnaeem/musicgen-small-ONNX"
BASE_URL="https://huggingface.co/${REPO}/resolve/main"
MODEL_DIR="models/musicgen-small"

cd "$(dirname "$0")/.."
mkdir -p "$MODEL_DIR"

download() {
    local src="$1"
    local dest="${MODEL_DIR}/$2"
    if [ -f "$dest" ]; then
        echo "  [skip] ${dest} (already exists)"
    else
        echo "  [download] ${dest}..."
        curl -L --progress-bar "${BASE_URL}/${src}" -o "$dest"
    fi
}

echo "Downloading MusicGen-small (int8 quantized) to ${MODEL_DIR}/"
echo ""

download "onnx/text_encoder_int8.onnx"              "text_encoder.onnx"
download "onnx/decoder_model_merged_int8.onnx"       "decoder_model_merged.onnx"
download "onnx/encodec_decode.onnx"                   "encodec_decode.onnx"
download "onnx/build_delay_pattern_mask_int8.onnx"   "build_delay_pattern_mask.onnx"
download "config.json"                                "config.json"
download "generation_config.json"                     "generation_config.json"
download "tokenizer.json"                             "tokenizer.json"
download "preprocessor_config.json"                   "preprocessor_config.json"
download "special_tokens_map.json"                    "special_tokens_map.json"

echo ""
echo "Done. Model files:"
ls -lh "$MODEL_DIR"
