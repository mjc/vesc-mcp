#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
cd "$repo_root"

if [[ ${VESC_RX5700XT_JINA_SHELL:-} != 1 ]]; then
  exec nix develop .#rocm -c env VESC_RX5700XT_JINA_SHELL=1 "$0" "$@"
fi

model_id=jinaai/jina-embeddings-v2-base-code
model_revision=516f4baf13dec4ddddda8631e019b5737c8bc250
fp16_model=target/models/jina-embeddings-v2-base-code-fp16
int8_model=target/models/jina-embeddings-v2-base-code-quantized
artifact=target/knowledge-artifacts-jina-code-fp16-rx5700xt
export ORT_MIGRAPHX_MODEL_CACHE_PATH="$repo_root/target/provider-bench/rx5700xt-jina-fp16-512-b8"

verify_file() {
  local expected=$1
  local path=$2
  [[ -f $path ]] || { echo "missing $path" >&2; exit 1; }
  printf '%s  %s\n' "$expected" "$path" | sha256sum --check --status - || {
    echo "checksum mismatch: $path" >&2
    exit 1
  }
}

verify_combo() {
  local agents
  grep -q 'AMD Ryzen 5 8600G' /proc/cpuinfo || {
    echo "this profile requires an AMD Ryzen 5 8600G" >&2
    exit 1
  }
  agents=$(rocminfo 2>/dev/null)
  grep -q 'Marketing Name:.*AMD Radeon RX 5700 XT' <<<"$agents" || {
    echo "this preset requires an AMD Radeon RX 5700 XT" >&2
    exit 1
  }
  grep -q 'Name:.*gfx1010' <<<"$agents" || {
    echo "this preset requires the gfx1010 target" >&2
    exit 1
  }
  verify_file 1aafc4fcd63d2e6899e88402ff731e7c646c2e435048294a3cbc908a40d45d7c "$fp16_model/model.onnx"
  verify_file ed45870251c9f0cf656e78aab0d37a23489066df8a222bb1c8caf8a45f2cb16d "$int8_model/model.onnx"
  mkdir -p "$ORT_MIGRAPHX_MODEL_CACHE_PATH"
}

semantic_benchmark() {
  local model=$1
  local provider=$2
  local batch=$3
  shift 3
  cargo run --release -p vesc-knowledge-index \
    --features semantic-fastembed,semantic-migraphx \
    --bin gen-knowledge-index -- benchmark \
    --mode semantic \
    --suite tests/evaluation/v2/queries.json \
    --semantic-model-dir "$model" \
    --semantic-model-id "$model_id" \
    --semantic-model-revision "$model_revision" \
    --semantic-provider "$provider" \
    --semantic-device-id 0 \
    --semantic-max-length 512 \
    --semantic-batch-size "$batch" \
    --semantic-length-bucketed true \
    --semantic-lossless-windows \
    --semantic-graph-optimization-level 3 \
    --semantic-sample-chunks 16 \
    --warmup 0 \
    --repetitions 1 \
    "$@"
}

command=${1:-verify}
if (($#)); then shift; fi
verify_combo

case "$command" in
  verify)
    echo "RX 5700 XT Jina FP16-ingest/INT8-query combo verified"
    ;;
  smoke)
    semantic_benchmark "$fp16_model" migraphx 8 "$@"
    semantic_benchmark "$int8_model" cpu 1 "$@"
    ;;
  ingest)
    exec cargo run --release -p vesc-knowledge-index \
      --features semantic-fastembed,semantic-migraphx \
      --bin gen-knowledge-index -- build \
      --out "$artifact" \
      "$@"
    ;;
  serve)
    [[ -f $artifact/active.json ]] || {
      echo "missing $artifact/active.json; run '$0 ingest' first" >&2
      exit 1
    }
    exec cargo run --release -p vesc-mcp-server \
      --features semantic-fastembed -- "$@"
    ;;
  *)
    echo "usage: $0 {verify|smoke|ingest|serve [server args...]}" >&2
    exit 2
    ;;
esac
