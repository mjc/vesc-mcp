#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
cd "$repo_root"

if [[ ${VESC_RX5700XT_JINA_SHELL:-} != 1 ]]; then
  exec nix develop .#rocm -c env VESC_RX5700XT_JINA_SHELL=1 "$0" "$@"
fi

artifact=${1:?usage: $0 ARTIFACT [OUT_DIR] [SAMPLE_CHUNKS]}
out=${2:-target/provider-bench/rx5700xt-jina-sequence-matrix}
sample=${3:-32}
binary=target/release/gen-knowledge-index
model=target/models/jina-embeddings-v2-base-code-fp16
model_id=jinaai/jina-embeddings-v2-base-code
model_revision=516f4baf13dec4ddddda8631e019b5737c8bc250
time_bin=${VESC_TIME_BIN:-/run/current-system/sw/bin/time}

[[ -x $binary ]] || { echo "missing release binary: $binary" >&2; exit 1; }
[[ -x $time_bin ]] || { echo "missing GNU time: $time_bin" >&2; exit 1; }
scripts/rx5700xt-jina-split.sh verify >/dev/null

gpu_device=
for card in /sys/class/drm/card*/device; do
  [[ -f $card/vendor && -f $card/device ]] || continue
  if [[ $(<"$card/vendor") == 0x1002 && $(<"$card/device") == 0x731f ]]; then
    gpu_device=$card
    break
  fi
done
[[ -n $gpu_device ]] || { echo "RX 5700 XT sysfs device not found" >&2; exit 1; }
mkdir -p "$out"

monitor_gpu() {
  local pid=$1 output=$2
  printf 'epoch_ms\tgpu_busy_percent\tvram_bytes\n' >"$output"
  while kill -0 "$pid" 2>/dev/null; do
    printf '%s\t%s\t%s\n' \
      "$(date +%s%3N)" \
      "$(<"$gpu_device/gpu_busy_percent")" \
      "$(<"$gpu_device/mem_info_vram_used")" >>"$output"
    sleep 0.2
  done
}

lengths=(64 128 256 512)
batches=(64 32 16 8)
for index in "${!lengths[@]}"; do
  length=${lengths[$index]}
  batch=${batches[$index]}
  cache=$(mktemp -d "$out/cache-s${length}-b${batch}.XXXXXX")
  for aggregation in mean token-weighted-mean; do
    name=s${length}-b${batch}-${aggregation}
    json=$out/$name.json
    stderr=$out/$name.stderr
    telemetry=$out/$name.gpu.tsv
    ORT_MIGRAPHX_MODEL_CACHE_PATH=$cache \
      "$time_bin" -v "$binary" benchmark \
        --mode semantic \
        --artifact "$artifact" \
        --suite tests/evaluation/v2/queries.json \
        --format json \
        --semantic-model-dir "$model" \
        --semantic-model-id "$model_id" \
        --semantic-model-revision "$model_revision" \
        --semantic-provider migraphx \
        --semantic-device-id 0 \
        --semantic-max-length "$length" \
        --semantic-batch-size "$batch" \
        --semantic-length-bucketed true \
        --semantic-lossless-windows \
        --semantic-window-aggregation "$aggregation" \
        --semantic-graph-optimization-level 3 \
        --semantic-sample-chunks "$sample" \
        --warmup 1 \
        --repetitions 2 \
        >"$json.tmp" 2>"$stderr" &
    pid=$!
    monitor_gpu "$pid" "$telemetry" &
    monitor_pid=$!
    if wait "$pid"; then
      status=0
    else
      status=$?
      kill "$monitor_pid" 2>/dev/null || true
      wait "$monitor_pid" 2>/dev/null || true
      exit "$status"
    fi
    kill "$monitor_pid" 2>/dev/null || true
    wait "$monitor_pid" 2>/dev/null || true
    mv "$json.tmp" "$json"
  done
done

jq -s '{schema: 1, runs: .}' "$out"/s[0-9]*.json >"$out/matrix.json"
printf 'matrix: %s\n' "$out/matrix.json"
