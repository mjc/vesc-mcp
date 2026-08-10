#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
cd "$repo_root"

if [[ ${VESC_MCP_PROFILE_SHELL:-} != 1 ]]; then
  exec nix develop --max-jobs 1 --cores 1 --option builders '' -c env VESC_MCP_PROFILE_SHELL=1 "$0" "$@"
fi

command=${1:-}
[[ -n $command ]] || {
  echo "usage: $0 {build|provider-build|provider-baseline|provider-heaptrack|provider-flamegraph|query-build|query-baseline|query-heaptrack|query-flamegraph|prepare-time|heaptrack|heaptrack-report|coz|coz-report|flamegraph|perf-report|flamegraph-report} [args...]" >&2
  exit 2
}
shift

if [[ -v VESC_MCP_PROFILE_PACKAGE ]]; then
  profile_package=$VESC_MCP_PROFILE_PACKAGE
  profile_package_is_default=false
else
  profile_package="$repo_root/result-profile"
  profile_package_is_default=true
fi
profile_root=${VESC_MCP_PROFILE_ROOT:-"${XDG_CONFIG_HOME:-"$HOME/.config"}/vesc-mcp/profiles"}
profile_migraphx_cache=${VESC_MCP_PROFILE_MIGRAPHX_CACHE_PATH:-}
timeout_secs=${VESC_MCP_PROFILE_TIMEOUT_SECS:-600}
coz_progress_points=${VESC_MCP_PROFILE_MAX_PROGRESS_POINTS:-10000}
memory_high=${VESC_MCP_PROFILE_MEMORY_HIGH:-3G}
memory_max=${VESC_MCP_PROFILE_MEMORY_MAX:-4G}
build_memory_high=${VESC_MCP_PROFILE_BUILD_MEMORY_HIGH:-4G}
build_memory_max=${VESC_MCP_PROFILE_BUILD_MEMORY_MAX:-6G}
scope=(
  systemd-run --user --scope --quiet --expand-environment=no
  -p MemoryHigh="$memory_high"
  -p MemoryMax="$memory_max"
  -p MemorySwapMax=0
  -p CPUWeight=10
  -p IOWeight=10
)
build_scope=(
  systemd-run --user --scope --quiet --expand-environment=no
  -p MemoryHigh="$build_memory_high"
  -p MemoryMax="$build_memory_max"
  -p MemorySwapMax=0
  -p CPUWeight=10
  -p IOWeight=10
)
provider_target="$repo_root/target/provider-profile"
provider_binary="$provider_target/release/gen-knowledge-index"
query_binary="$provider_target/release/vesc-mcp-server"
provider_model_id=jinaai/jina-embeddings-v2-base-code
provider_model_revision=516f4baf13dec4ddddda8631e019b5737c8bc250
profile_source_paths=(
  Cargo.toml
  Cargo.lock
  flake.nix
  flake.lock
  rust-toolchain.toml
  .cargo
  crates
  vendor
  scripts
)

profile_source_digest() {
  git ls-files -z --cached --others --exclude-standard -- "${profile_source_paths[@]}" |
    sort -z |
    while IFS= read -r -d '' path; do
      if [[ -f $path ]]; then
        sha256sum "$path"
      else
        printf 'missing  %s\n' "$path"
      fi
    done |
    sha256sum |
    cut -d' ' -f1
}

require_profile_source_clean() {
  git diff --quiet HEAD -- "${profile_source_paths[@]}" || {
    echo "profiling source differs from HEAD; commit it before durable profiling" >&2
    exit 1
  }
  [[ -z $(git ls-files --others --exclude-standard -- "${profile_source_paths[@]}") ]] || {
    echo "profiling source has untracked build inputs; commit them before durable profiling" >&2
    exit 1
  }
}

require_fresh_binary() {
  local identity_file=$1.source-digest
  local expected
  local actual
  [[ -f $identity_file ]] || {
    echo "profiling binary has no source identity; rebuild it" >&2
    exit 1
  }
  expected=$(profile_source_digest)
  actual=$(<"$identity_file")
  [[ $actual == "$expected" ]] || {
    echo "stale profiling binary; rebuild it" >&2
    exit 1
  }
}

validate_provider_report() {
  jq -e \
    --arg model_id "$provider_model_id" \
    --arg model_revision "$provider_model_revision" '
      .schema == 2
      and .requested_provider == "Cpu"
      and .model_id == $model_id
      and .model_revision == $model_revision
      and (.model_sha256 | length == 64)
      and .construction_us > 0
      and .first_query_us > 0
      and (.vector_digest | startswith("sha256:"))
      and .rss_after_construction_bytes > 0
      and .rss_after_query_bytes > 0
      and .rss_after_provider_drop_bytes > 0
      and (
        .vector_artifact_path == null
        or (
          (.vector_artifact_sha256 | startswith("sha256:"))
          and .vector_artifact_model_id == $model_id
          and .vector_artifact_model_revision == $model_revision
          and (.vector_artifact_corpus_digest | startswith("sha256:"))
          and .vector_artifact_rows > 0
          and .vector_search_hits > 0
          and (.vector_search_digest | startswith("sha256:"))
        )
      )
    ' "$1" >/dev/null
}

validate_query_report() {
  jq -e '
    .schema == 2
    and .mode == "hybrid"
    and .query_count > 0
    and (.snapshot_ids | length == 1)
    and (.result_counts | all(. > 0))
    and (.response_digests | length > 0)
    and (.response_digests | all(startswith("sha256:")))
  ' "$1" >/dev/null
}

stop_service() {
  if systemctl --user cat vesc-mcp.service &>/dev/null; then
    systemctl --user stop vesc-mcp.service
  fi
}

load_service_args() {
  if (($#)); then
    service_args=("$@")
  else
    service_args=(--http)
  fi
}

configure_profile_data() {
  local config=$1
  local data_root=$2
  local configured_data_root
  local service_migraphx_cache
  [[ -f $config ]] || {
    echo "profile config does not exist: $config" >&2
    exit 2
  }
  configured_data_root=$(python3 - "$config" <<'PY'
import pathlib
import sys
import tomllib

with pathlib.Path(sys.argv[1]).open("rb") as config:
    value = tomllib.load(config).get("knowledge", {}).get("data_root")
if not isinstance(value, str) or not value:
    raise SystemExit("profile config must set [knowledge] data_root")
print(value)
PY
  )
  config=$(realpath -m -- "$config")
  data_root=$(realpath -m -- "$data_root")
  configured_data_root=$(realpath -m -- "$configured_data_root")
  [[ $configured_data_root == "$data_root" ]] || {
    echo "profile config data_root does not match the monitored data root" >&2
    exit 2
  }
  export VESC_MCP_CONFIG=$config
  export VESC_MCP_PROFILE_DATA_ROOT=$data_root
  service_migraphx_cache="${XDG_CONFIG_HOME:-"$HOME/.config"}/vesc-mcp/data/migraphx-cache"
  if [[ -z $profile_migraphx_cache ]]; then
    if [[ -d $service_migraphx_cache ]]; then
      profile_migraphx_cache=$service_migraphx_cache
    else
      profile_migraphx_cache="$data_root/migraphx-cache"
    fi
  fi
  mkdir -p "$profile_migraphx_cache"
}

load_preparation_args() {
  [[ ${1:-} == --profile-config && $# -ge 3 ]] || {
    echo "usage: $0 $command --profile-config CONFIG DATA_ROOT [server args...]" >&2
    exit 2
  }
  configure_profile_data "$2" "$3"
  shift 3
  if (($#)); then
    service_args=("$@")
  else
    service_args=(--profile-initial-training)
  fi
}

load_profile_binary() {
  local wrapper=$profile_package/bin/vesc-mcp-server
  if [[ $profile_package_is_default == true ]]; then
    local expected_package
    expected_package=$(nix eval --raw .#vesc-mcp-migraphx-profile.outPath)
    if [[ $(readlink -f "$profile_package") != "$expected_package" ]]; then
      echo "stale profiling binary; run '$0 build' before profiling" >&2
      exit 1
    fi
  fi
  [[ -x $wrapper ]] || {
    echo "missing profiling binary; run '$0 build'" >&2
    exit 1
  }
  unset ORT_DYLIB_PATH
  while IFS= read -r setup_line; do
    [[ $setup_line == exec\ * ]] && break
    eval "$setup_line"
  done <"$wrapper"
  if [[ -n ${VESC_MCP_PROFILE_DATA_ROOT:-} ]]; then
    export ORT_MIGRAPHX_MODEL_CACHE_PATH="$profile_migraphx_cache"
  fi
  profile_binary=$profile_package/bin/.vesc-mcp-server-wrapped
  [[ -x $profile_binary ]] || {
    echo "missing wrapped profiling binary: $profile_binary" >&2
    exit 1
  }
}

new_output_dir() {
  local command_line
  local -a recorded_args=()
  mkdir -p "$profile_root"
  output_dir=$(mktemp -d "$profile_root/$(date -u +%Y%m%dT%H%M%SZ)-$1.XXXXXX")
  if declare -p profile_args &>/dev/null; then
    recorded_args=("${profile_args[@]}")
  fi
  printf -v command_line '%q ' "$profile_binary" "${recorded_args[@]}"
  {
    echo "git_commit=$(git rev-parse HEAD)"
    echo "git_dirty=$(test -n "$(git status --short)" && echo true || echo false)"
    echo "source_digest=$(profile_source_digest)"
    echo "profile_package=$(readlink -f "$profile_package")"
    echo "profile_binary=$(readlink -f "$profile_binary")"
    echo "profile_binary_sha256=$(sha256sum "$profile_binary" | cut -d' ' -f1)"
    printf 'command=%s\n' "$command_line"
    echo "memory_high=$memory_high"
    echo "memory_max=$memory_max"
    echo "timeout_secs=$timeout_secs"
    echo "data_root=${VESC_MCP_PROFILE_DATA_ROOT:-}"
    echo "migraphx_cache=$profile_migraphx_cache"
    echo "ort_dylib_path=${ORT_DYLIB_PATH:-}"
    echo "ort_migraphx_model_cache_path=${ORT_MIGRAPHX_MODEL_CACHE_PATH:-}"
    echo "vesc_mcp_config=${VESC_MCP_CONFIG:-}"
    while IFS='=' read -r name value; do
      printf '%s=%q\n' "$name" "$value"
    done < <(env | LC_ALL=C sort | sed -n '/^VESC_RAG_[^=]*=/p')
    [[ -z ${profile_wrapper:-} ]] || {
      echo "service_wrapper=$(readlink -f "$profile_wrapper")"
      echo "service_wrapper_sha256=$(sha256sum "$profile_wrapper" | cut -d' ' -f1)"
    }
    [[ -z ${profile_suite:-} ]] || {
      echo "suite=$(readlink -f "$profile_suite")"
      echo "suite_sha256=$(sha256sum "$profile_suite" | cut -d' ' -f1)"
    }
    [[ -z ${profile_config:-} ]] || {
      echo "config=$(readlink -f "$profile_config")"
      echo "config_sha256=$(sha256sum "$profile_config" | cut -d' ' -f1)"
    }
  } >"$output_dir/provenance.env"
  echo "profile output: $output_dir" >&2
}

load_provider_binary() {
  [[ -x $provider_binary ]] || {
    echo "missing provider profiling binary; run '$0 provider-build'" >&2
    exit 1
  }
  require_profile_source_clean
  require_fresh_binary "$provider_binary"
  profile_package=$provider_target
  profile_binary=$provider_binary
}

load_provider_args() {
  provider_model_dir=${1:?usage: $0 $command MODEL_DIR}
  shift
  [[ -f $provider_model_dir/model.onnx ]] || {
    echo "provider model does not exist: $provider_model_dir/model.onnx" >&2
    exit 2
  }
  provider_args=(
    provider-lifecycle
    --semantic-model-dir "$provider_model_dir"
    --semantic-model-id "$provider_model_id"
    --semantic-model-revision "$provider_model_revision"
    --semantic-provider cpu
    --semantic-max-length 512
    --semantic-intra-threads 1
    "$@"
  )
  profile_args=("${provider_args[@]}")
}

load_query_binary() {
  [[ -x $query_binary ]] || {
    echo "missing query profiling binary; run '$0 query-build'" >&2
    exit 1
  }
  require_profile_source_clean
  require_fresh_binary "$query_binary"
  profile_package=$provider_target
  profile_binary=$query_binary
}

load_query_args() {
  local service_wrapper=${1:?usage: $0 $command SERVICE_WRAPPER SUITE}
  local suite=${2:?usage: $0 $command SERVICE_WRAPPER SUITE}
  local config
  local data_root
  [[ -x $service_wrapper ]] || {
    echo "service wrapper is not executable: $service_wrapper" >&2
    exit 2
  }
  [[ -f $suite ]] || {
    echo "query suite does not exist: $suite" >&2
    exit 2
  }
  while IFS= read -r variable; do
    unset "$variable"
  done < <(env | sed -n 's/^\(VESC_RAG_[^=]*\)=.*/\1/p')
  unset VESC_MCP_CONFIG ORT_DYLIB_PATH ORT_MIGRAPHX_MODEL_CACHE_PATH
  while IFS= read -r setup_line; do
    [[ $setup_line == exec\ * ]] && break
    eval "$setup_line"
  done <"$service_wrapper"
  config=${VESC_MCP_CONFIG:-"${XDG_CONFIG_HOME:-"$HOME/.config"}/vesc-mcp/config.toml"}
  profile_wrapper=$service_wrapper
  profile_suite=$suite
  profile_config=$config
  if [[ -z ${ORT_MIGRAPHX_MODEL_CACHE_PATH:-} && -f $config ]]; then
    data_root=$(python3 - "$config" <<'PY'
import pathlib
import sys
import tomllib

with pathlib.Path(sys.argv[1]).open("rb") as config:
    print(tomllib.load(config).get("knowledge", {}).get("data_root", ""))
PY
    )
    if [[ -n $data_root ]]; then
      export ORT_MIGRAPHX_MODEL_CACHE_PATH="$data_root/migraphx-cache"
      mkdir -p "$ORT_MIGRAPHX_MODEL_CACHE_PATH"
    fi
  fi
  query_args=(
    --benchmark-search
    --mode hybrid
    --suite "$suite"
    --warmup 0
    --repetitions 1
    --format json
  )
  profile_args=("${query_args[@]}")
}

case "$command" in
  build)
    stop_service
    exec "${build_scope[@]}" nix build .#vesc-mcp-migraphx-profile \
      --max-jobs 1 --cores 1 --option builders "" --out-link "$profile_package" -L
    ;;
  provider-build)
    stop_service
    "${build_scope[@]}" env \
      CARGO_TARGET_DIR="$provider_target" \
      CARGO_BUILD_RUSTFLAGS="-C force-frame-pointers=yes" \
      cargo build --jobs 1 --profile bench -p vesc-knowledge-index \
        --features semantic-fastembed --bin gen-knowledge-index
    profile_source_digest >"$provider_binary.source-digest"
    ;;
  provider-baseline)
    stop_service
    load_provider_binary
    load_provider_args "$@"
    repetitions=${VESC_MCP_PROFILE_REPETITIONS:-5}
    [[ $repetitions =~ ^[1-9][0-9]*$ ]] || {
      echo "VESC_MCP_PROFILE_REPETITIONS must be a positive integer" >&2
      exit 2
    }
    new_output_dir provider-baseline
    for ((run = 1; run <= repetitions; run++)); do
      "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
        "$VESC_TIME_BIN" -v -o "$output_dir/time-$run.txt" \
        "$provider_binary" "${provider_args[@]}" >"$output_dir/run-$run.json"
      peak_rss_kb=$(awk -F: '/Maximum resident set size/ {gsub(/[[:space:]]/, "", $2); print $2}' "$output_dir/time-$run.txt")
      jq --argjson peak_rss_bytes "$((peak_rss_kb * 1024))" \
        '. + {time_peak_rss_bytes: $peak_rss_bytes}' "$output_dir/run-$run.json" \
        >"$output_dir/run-$run.tmp"
      mv "$output_dir/run-$run.tmp" "$output_dir/run-$run.json"
      validate_provider_report "$output_dir/run-$run.json"
    done
    jq -s '
      def median:
        sort |
        if length % 2 == 1 then .[length / 2 | floor]
        else (.[length / 2 - 1] + .[length / 2]) / 2
        end;
      {
        runs: length,
        model_sha256: (map(.model_sha256) | unique),
        vector_digest: (map(.vector_digest) | unique),
        construction_us_min: (map(.construction_us) | min),
        construction_us_median: (map(.construction_us) | median),
        construction_us_max: (map(.construction_us) | max),
        first_query_us_min: (map(.first_query_us) | min),
        first_query_us_median: (map(.first_query_us) | median),
        first_query_us_max: (map(.first_query_us) | max),
        vector_map_us_min: (map(select(.vector_map_us != null) | .vector_map_us) | if length == 0 then null else min end),
        vector_map_us_median: (map(select(.vector_map_us != null) | .vector_map_us) | if length == 0 then null else median end),
        vector_map_us_max: (map(select(.vector_map_us != null) | .vector_map_us) | if length == 0 then null else max end),
        vector_open_us_min: (map(select(.vector_open_us != null) | .vector_open_us) | if length == 0 then null else min end),
        vector_open_us_median: (map(select(.vector_open_us != null) | .vector_open_us) | if length == 0 then null else median end),
        vector_open_us_max: (map(select(.vector_open_us != null) | .vector_open_us) | if length == 0 then null else max end),
        vector_hash_us_min: (map(select(.vector_hash_us != null) | .vector_hash_us) | if length == 0 then null else min end),
        vector_hash_us_median: (map(select(.vector_hash_us != null) | .vector_hash_us) | if length == 0 then null else median end),
        vector_hash_us_max: (map(select(.vector_hash_us != null) | .vector_hash_us) | if length == 0 then null else max end),
        vector_search_us_min: (map(select(.vector_search_us != null) | .vector_search_us) | if length == 0 then null else min end),
        vector_search_us_median: (map(select(.vector_search_us != null) | .vector_search_us) | if length == 0 then null else median end),
        vector_search_us_max: (map(select(.vector_search_us != null) | .vector_search_us) | if length == 0 then null else max end),
        drop_us_median: (map(.drop_us) | median),
        rss_start_bytes_median: (map(.rss_start_bytes) | median),
        rss_after_vector_open_bytes_median: (map(.rss_after_vector_open_bytes) | median),
        rss_after_construction_bytes_median: (map(.rss_after_construction_bytes) | median),
        rss_after_query_bytes_median: (map(.rss_after_query_bytes) | median),
        rss_after_provider_drop_bytes_median: (map(.rss_after_provider_drop_bytes) | median),
        rss_after_provider_drop_bytes_min: (map(.rss_after_provider_drop_bytes) | min),
        rss_after_provider_drop_bytes_max: (map(.rss_after_provider_drop_bytes) | max),
        rss_after_vector_drop_bytes_median: (map(.rss_after_vector_drop_bytes) | median),
        rss_after_settle_bytes_min: (map(.rss_after_settle_bytes) | min),
        rss_after_settle_bytes_median: (map(.rss_after_settle_bytes) | median),
        rss_after_settle_bytes_max: (map(.rss_after_settle_bytes) | max),
        time_peak_rss_bytes_min: (map(.time_peak_rss_bytes) | min),
        time_peak_rss_bytes_median: (map(.time_peak_rss_bytes) | median),
        time_peak_rss_bytes_max: (map(.time_peak_rss_bytes) | max)
      }
    ' "$output_dir"/run-*.json | tee "$output_dir/summary.json"
    jq -e '.model_sha256 | length == 1' "$output_dir/summary.json" >/dev/null
    jq -e '.vector_digest | length == 1' "$output_dir/summary.json" >/dev/null
    echo "provider baseline: $output_dir" >&2
    ;;
  provider-heaptrack)
    stop_service
    load_provider_binary
    load_provider_args "$@"
    new_output_dir provider-heaptrack
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      heaptrack --use-inject --record-only \
      -o "$output_dir/heaptrack" \
      "$provider_binary" "${provider_args[@]}" >"$output_dir/run.raw"
    sed -n '/^{/,/^}$/p' "$output_dir/run.raw" >"$output_dir/run.json"
    validate_provider_report "$output_dir/run.json"
    trace="$output_dir/heaptrack.zst"
    zstd -t --quiet "$trace"
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      heaptrack_print "$trace" >"$output_dir/heaptrack-report.txt"
    echo "provider heaptrack report: $output_dir/heaptrack-report.txt" >&2
    ;;
  provider-flamegraph)
    stop_service
    load_provider_binary
    load_provider_args "$@"
    new_output_dir provider-flamegraph
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      perf record -F 99 -g --call-graph fp \
      -o "$output_dir/perf.data" \
      -- "$provider_binary" "${provider_args[@]}" >"$output_dir/run.json"
    validate_provider_report "$output_dir/run.json"
    "${scope[@]}" env \
      PERF_DATA="$output_dir/perf.data" \
      FLAMEGRAPH="$output_dir/flamegraph.svg" \
      bash -o pipefail -c \
      'perf script --no-inline -i "$PERF_DATA" | inferno-collapse-perf | inferno-flamegraph >"$FLAMEGRAPH"'
    "${scope[@]}" "$script_dir/parse_perfdata" "$output_dir/perf.data" \
      >"$output_dir/perf-report.txt"
    "${scope[@]}" "$script_dir/parse_flamegraph" \
      "$output_dir/flamegraph.svg" summary >"$output_dir/flamegraph-report.txt"
    echo >>"$output_dir/flamegraph-report.txt"
    "${scope[@]}" "$script_dir/parse_flamegraph" \
      "$output_dir/flamegraph.svg" top 40 0.5 >>"$output_dir/flamegraph-report.txt"
    echo "provider perf report: $output_dir/perf-report.txt" >&2
    echo "provider flamegraph report: $output_dir/flamegraph-report.txt" >&2
    ;;
  query-build)
    stop_service
    "${build_scope[@]}" env \
      CARGO_TARGET_DIR="$provider_target" \
      CARGO_BUILD_RUSTFLAGS="-C force-frame-pointers=yes" \
      cargo build --jobs 1 --profile bench -p vesc-mcp-server \
        --features semantic-fastembed,semantic-migraphx --bin vesc-mcp-server
    profile_source_digest >"$query_binary.source-digest"
    ;;
  query-baseline)
    stop_service
    load_query_binary
    load_query_args "$@"
    repetitions=${VESC_MCP_PROFILE_REPETITIONS:-5}
    [[ $repetitions =~ ^[1-9][0-9]*$ ]] || {
      echo "VESC_MCP_PROFILE_REPETITIONS must be a positive integer" >&2
      exit 2
    }
    new_output_dir query-baseline
    for ((run = 1; run <= repetitions; run++)); do
      "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
        "$VESC_TIME_BIN" -v -o "$output_dir/time-$run.txt" \
        "$query_binary" "${query_args[@]}" >"$output_dir/run-$run.json"
      peak_rss_kb=$(awk -F: '/Maximum resident set size/ {gsub(/[[:space:]]/, "", $2); print $2}' "$output_dir/time-$run.txt")
      jq --argjson peak_rss_bytes "$((peak_rss_kb * 1024))" \
        '. + {time_peak_rss_bytes: $peak_rss_bytes}' "$output_dir/run-$run.json" \
        >"$output_dir/run-$run.tmp"
      mv "$output_dir/run-$run.tmp" "$output_dir/run-$run.json"
      validate_query_report "$output_dir/run-$run.json"
    done
    jq -s '
      def median:
        sort |
        if length % 2 == 1 then .[length / 2 | floor]
        else (.[length / 2 - 1] + .[length / 2]) / 2
        end;
      {
        runs: length,
        modes: (map(.mode) | unique),
        snapshot_ids: (map(.snapshot_ids[]) | unique),
        result_counts: (map(.result_counts[]) | unique),
        response_digest_sets: (map(.response_digests) | unique),
        response_bytes: (map(.response_bytes.p50_bytes) | unique),
        query_us_min: (map(.handler_and_serialization.p50_us) | min),
        query_us_median: (map(.handler_and_serialization.p50_us) | median),
        query_us_max: (map(.handler_and_serialization.p50_us) | max),
        rss_before_bytes_median: (map(.rss_before_queries_bytes) | median),
        rss_after_bytes_median: (map(.rss_after_queries_bytes) | median),
        rss_retained_delta_bytes_median: (map(.rss_retained_delta_bytes) | median),
        time_peak_rss_bytes_min: (map(.time_peak_rss_bytes) | min),
        time_peak_rss_bytes_median: (map(.time_peak_rss_bytes) | median),
        time_peak_rss_bytes_max: (map(.time_peak_rss_bytes) | max)
      }
    ' "$output_dir"/run-*.json | tee "$output_dir/summary.json"
    jq -e '.modes == ["hybrid"]' "$output_dir/summary.json" >/dev/null
    jq -e '.snapshot_ids | length == 1' "$output_dir/summary.json" >/dev/null
    jq -e '.result_counts | all(. > 0)' "$output_dir/summary.json" >/dev/null
    jq -e '.response_digest_sets | length == 1' "$output_dir/summary.json" >/dev/null
    echo "query baseline: $output_dir" >&2
    ;;
  query-heaptrack)
    stop_service
    load_query_binary
    load_query_args "$@"
    new_output_dir query-heaptrack
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      heaptrack --use-inject --record-only \
      -o "$output_dir/heaptrack" \
      "$query_binary" "${query_args[@]}" >"$output_dir/run.raw"
    sed -n '/^{/,/^}$/p' "$output_dir/run.raw" >"$output_dir/run.json"
    validate_query_report "$output_dir/run.json"
    trace="$output_dir/heaptrack.zst"
    zstd -t --quiet "$trace"
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      heaptrack_print "$trace" >"$output_dir/heaptrack-report.txt"
    echo "query heaptrack report: $output_dir/heaptrack-report.txt" >&2
    ;;
  query-flamegraph)
    stop_service
    load_query_binary
    load_query_args "$@"
    new_output_dir query-flamegraph
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      perf record -F 99 -g --call-graph fp \
      -o "$output_dir/perf.data" \
      -- "$query_binary" "${query_args[@]}" >"$output_dir/run.json"
    validate_query_report "$output_dir/run.json"
    "${scope[@]}" env \
      PERF_DATA="$output_dir/perf.data" \
      FLAMEGRAPH="$output_dir/flamegraph.svg" \
      bash -o pipefail -c \
      'perf script --no-inline -i "$PERF_DATA" | inferno-collapse-perf | inferno-flamegraph >"$FLAMEGRAPH"'
    "${scope[@]}" "$script_dir/parse_perfdata" "$output_dir/perf.data" \
      >"$output_dir/perf-report.txt"
    "${scope[@]}" "$script_dir/parse_flamegraph" \
      "$output_dir/flamegraph.svg" summary >"$output_dir/flamegraph-report.txt"
    echo >>"$output_dir/flamegraph-report.txt"
    "${scope[@]}" "$script_dir/parse_flamegraph" \
      "$output_dir/flamegraph.svg" top 40 0.5 >>"$output_dir/flamegraph-report.txt"
    echo "query perf report: $output_dir/perf-report.txt" >&2
    echo "query flamegraph report: $output_dir/flamegraph-report.txt" >&2
    ;;
  prepare-time)
    stop_service
    config=${1:?usage: $0 prepare-time CONFIG DATA_ROOT}
    data_root=${2:?usage: $0 prepare-time CONFIG DATA_ROOT}
    shift 2
    configure_profile_data "$config" "$data_root"
    load_profile_binary
    load_service_args "$@"
    new_output_dir prepare-time
    exec "${scope[@]}" env \
      VESC_MCP_CONFIG="$config" \
      VESC_MCP_PROFILE_DATA_ROOT="$data_root" \
      VESC_MCP_PROFILE_OUTPUT="$output_dir" \
      VESC_MCP_PROFILE_BINARY="$profile_binary" \
      VESC_MCP_PROFILE_TIMEOUT_SECS="$timeout_secs" \
      bash -c '
        set -euo pipefail
        start_ns=$(date +%s%N)
        status="$VESC_MCP_PROFILE_DATA_ROOT/preparation-status.json"
        initial_status_stamp=$(stat -c %y "$status" 2>/dev/null || true)
        "$VESC_MCP_PROFILE_BINARY" "$@" \
          >"$VESC_MCP_PROFILE_OUTPUT/server.stdout" \
          2>"$VESC_MCP_PROFILE_OUTPUT/server.stderr" &
        server_pid=$!
        record_timing() {
          preparation_completed=$1
          timing_outcome=$2
          elapsed_ns=$(( $(date +%s%N) - start_ns ))
          [[ ! -f $status ]] \
            || cp "$status" "$VESC_MCP_PROFILE_OUTPUT/preparation-status.json"
          {
            printf "elapsed_seconds=%d.%09d\n" \
              "$((elapsed_ns / 1000000000))" "$((elapsed_ns % 1000000000))"
            echo "preparation_completed=$preparation_completed"
            echo "timing_outcome=$timing_outcome"
          } | tee "$VESC_MCP_PROFILE_OUTPUT/timing.env"
        }
        cleanup() {
          kill -TERM "$server_pid" 2>/dev/null || return 0
          for _ in {1..20}; do
            kill -0 "$server_pid" 2>/dev/null || {
              wait "$server_pid" 2>/dev/null || true
              return 0
            }
            sleep 0.1
          done
          kill -KILL "$server_pid" 2>/dev/null || true
          wait "$server_pid" 2>/dev/null || true
        }
        trap cleanup EXIT
        deadline=$((SECONDS + VESC_MCP_PROFILE_TIMEOUT_SECS))
        while true; do
          status_stamp=$(stat -c %y "$status" 2>/dev/null || true)
          if [[ $status_stamp != "$initial_status_stamp" ]] \
            && grep -Eq '\''"state":"(ready|stale|failed)"'\'' "$status"; then
            break
          fi
          if ! kill -0 "$server_pid" 2>/dev/null; then
            set +e
            wait "$server_pid"
            server_status=$?
            set -e
            record_timing false server-exited
            echo "server exited before publishing terminal preparation status" >&2
            ((server_status == 0)) && server_status=1
            exit "$server_status"
          fi
          (( SECONDS < deadline )) || {
            record_timing false timeout
            echo "preparation timed out after ${VESC_MCP_PROFILE_TIMEOUT_SECS}s" >&2
            exit 124
          }
          sleep 0.1
        done
        record_timing true terminal-status
        if grep -q '\''"state":"failed"'\'' "$status"; then
          echo "preparation failed" >&2
          exit 1
        fi
      ' bash "${service_args[@]}"
  ;;
  heaptrack)
    stop_service
    load_preparation_args "$@"
    load_profile_binary
    new_output_dir heaptrack
    exec "${scope[@]}" env \
      VESC_MCP_PROFILE_OUTPUT="$output_dir" \
      VESC_MCP_PROFILE_BINARY="$profile_binary" \
      VESC_MCP_PROFILE_TIMEOUT_SECS="$timeout_secs" \
      bash -c '
        set -euo pipefail
        status="$VESC_MCP_PROFILE_DATA_ROOT/preparation-status.json"
        initial_status_stamp=$(stat -c %y "$status" 2>/dev/null || true)
        profiler_pid=
        debuggee_pid=
        finalizer_watchdog=
        forced_debuggee_stop=false
        debuggee_stop=natural
        preparation_outcome=server-exited
        requested_stop=false
        watchdog_marker="$VESC_MCP_PROFILE_OUTPUT/.heaptrack-watchdog-fired"
        cleanup() {
          [[ -z $debuggee_pid ]] || kill -KILL "$debuggee_pid" 2>/dev/null || true
          [[ -z $profiler_pid ]] || kill -TERM "$profiler_pid" 2>/dev/null || true
          [[ -z $finalizer_watchdog ]] || kill "$finalizer_watchdog" 2>/dev/null || true
        }
        trap cleanup EXIT
        heaptrack --use-inject --record-only -o "$VESC_MCP_PROFILE_OUTPUT/heaptrack" \
          "$VESC_MCP_PROFILE_BINARY" "$@" &
        profiler_pid=$!
        for _ in {1..100}; do
          mapfile -t children < <(
            pgrep -P "$profiler_pid" -f -- "$VESC_MCP_PROFILE_BINARY" || true
          )
          if ((${#children[@]} == 1)); then
            debuggee_pid=${children[0]}
            break
          fi
          kill -0 "$profiler_pid" 2>/dev/null || break
          sleep 0.1
        done
        [[ -n $debuggee_pid ]] || {
          echo "heaptrack did not start exactly one server process" >&2
          exit 1
        }
        stop_debuggee() {
          local reason=$1
          kill -TERM "$debuggee_pid" 2>/dev/null || return 0
          requested_stop=true
          forced_debuggee_stop=true
          debuggee_stop="term-$reason"
          for _ in {1..100}; do
            kill -0 "$debuggee_pid" 2>/dev/null || return 0
            sleep 0.1
          done
          debuggee_stop="kill-$reason"
          kill -KILL "$debuggee_pid" 2>/dev/null || true
        }
        deadline=$((SECONDS + VESC_MCP_PROFILE_TIMEOUT_SECS))
        while kill -0 "$debuggee_pid" 2>/dev/null; do
          status_stamp=$(stat -c %y "$status" 2>/dev/null || true)
          if [[ $status_stamp != "$initial_status_stamp" ]] \
            && grep -Eq '\''"state":"(ready|stale|failed)"'\'' "$status"; then
            preparation_outcome=terminal-status
            break
          fi
          if (( SECONDS >= deadline )); then
            preparation_outcome=timeout
            stop_debuggee timeout
            break
          fi
          sleep 0.1
        done
        if [[ $preparation_outcome == terminal-status ]]; then
          natural_exit_deadline=$((SECONDS + 10))
          while kill -0 "$debuggee_pid" 2>/dev/null; do
            if (( SECONDS >= natural_exit_deadline )); then
              stop_debuggee terminal-grace
              break
            fi
            sleep 0.1
          done
        fi
        (
          sleep 60
          : >"$watchdog_marker"
          kill -TERM "$profiler_pid" 2>/dev/null || exit 0
          sleep 5
          kill -KILL "$profiler_pid" 2>/dev/null || true
        ) &
        finalizer_watchdog=$!
        set +e
        wait "$profiler_pid"
        capture_status=$?
        set -e
        profiler_pid=
        kill "$finalizer_watchdog" 2>/dev/null || true
        finalizer_watchdog=
        trap - EXIT
        status_stamp=$(stat -c %y "$status" 2>/dev/null || true)
        preparation_completed=false
        preparation_failed=false
        if [[ $status_stamp != "$initial_status_stamp" ]] \
          && grep -Eq '\''"state":"(ready|stale|failed)"'\'' "$status"; then
          preparation_completed=true
          if grep -q '\''"state":"failed"'\'' "$status"; then
            preparation_failed=true
          fi
        fi
        capture_censored=true
        if [[ $preparation_completed == true \
          && $preparation_failed == false \
          && $debuggee_stop == natural ]]; then
          capture_censored=false
        fi
        if [[ -f $status ]]; then
          cp "$status" "$VESC_MCP_PROFILE_OUTPUT/preparation-status.json"
        fi
        {
          echo "preparation_completed=$preparation_completed"
          echo "preparation_failed=$preparation_failed"
          echo "preparation_outcome=$preparation_outcome"
          echo "heaptrack_status=$capture_status"
          echo "forced_debuggee_stop=$forced_debuggee_stop"
          echo "debuggee_stop=$debuggee_stop"
          echo "capture_censored=$capture_censored"
        } >"$VESC_MCP_PROFILE_OUTPUT/capture.env"
        trace="$VESC_MCP_PROFILE_OUTPUT/heaptrack.zst"
        trace_status=0
        if [[ ! -s $trace ]]; then
          echo "heaptrack produced an empty trace" >&2
          trace_status=1
        elif ! zstd -t --quiet "$trace"; then
          echo "heaptrack produced a corrupt trace" >&2
          trace_status=1
        elif ! heaptrack_print \
          --print-peaks=0 \
          --print-allocators=0 \
          --print-temporary=0 \
          --print-leaks=0 \
          --file "$trace" >/dev/null; then
          echo "heaptrack produced an incomplete trace" >&2
          trace_status=1
        fi

        result_status=0
        if [[ $preparation_outcome == timeout ]]; then
          echo "heaptrack preparation timed out after ${VESC_MCP_PROFILE_TIMEOUT_SECS}s" >&2
          result_status=124
        fi
        if [[ -e $watchdog_marker ]]; then
          echo "heaptrack did not finalize before its watchdog fired" >&2
          if (( result_status == 0 )); then
            result_status=1
          fi
        fi
        if [[ $forced_debuggee_stop == true ]]; then
          echo "profiled server did not stop cleanly" >&2
          if (( result_status == 0 )); then
            result_status=1
          fi
        fi
        if (( capture_status != 0 )) \
          && ! { [[ $requested_stop == true ]] && (( capture_status == 143 )); }; then
          echo "heaptrack failed with status $capture_status" >&2
          if (( result_status == 0 )); then
            result_status=$capture_status
          fi
        fi
        if [[ $preparation_failed == true ]]; then
          echo "preparation failed" >&2
          if (( result_status == 0 )); then
            result_status=1
          fi
        elif [[ $preparation_completed == false ]]; then
          echo "profiled server exited before preparation completed" >&2
          if (( result_status == 0 )); then
            result_status=1
          fi
        fi
        if (( trace_status != 0 && result_status == 0 )); then
          result_status=$trace_status
        fi
        {
          echo "trace_status=$trace_status"
          echo "result_status=$result_status"
        } >>"$VESC_MCP_PROFILE_OUTPUT/capture.env"
        exit "$result_status"
      ' bash "${service_args[@]}"
    ;;
  heaptrack-report)
    stop_service
    trace=${1:?usage: $0 heaptrack-report TRACE.zst}
    report=${2:-"${trace%.zst}.txt"}
    capture="$(dirname -- "$trace")/capture.env"
    capture_complete=false
    if [[ -f $capture ]] \
      && grep -qx "capture_censored=false" "$capture" \
      && ! grep -qx "preparation_failed=true" "$capture" \
      && grep -qx "trace_status=0" "$capture" \
      && grep -qx "result_status=0" "$capture"; then
      capture_complete=true
    fi
    if [[ $capture_complete == true ]]; then
      "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
        heaptrack_print "$trace" >"$report"
      echo "heaptrack report: $report" >&2
    else
      {
        if [[ -f $capture ]] && grep -qx "preparation_failed=true" "$capture"; then
          echo "CAPTURE QUALITY: INVALID (PREPARATION FAILED)"
          echo "Initial training reported failure, so this is not a complete workload."
        elif [[ -f $capture ]] && grep -qx "capture_censored=false" "$capture"; then
          echo "CAPTURE QUALITY: INVALID (CAPTURE FAILED)"
          echo "The profiled workload completed, but trace validation or finalization failed."
        else
          echo "CAPTURE QUALITY: CENSORED"
          echo "Initial training did not complete successfully with a natural process exit."
        fi
        echo "Do not compare total runtime, allocation totals, final live/leaked memory,"
        echo "or treat observed peaks as end-to-end maxima. Prefix allocation stacks remain usable."
        echo "Capture metadata: $capture"
        echo
        "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
          heaptrack_print "$trace" |
          sed \
            -e "s/^total runtime:/CENSORED total runtime (prefix only):/" \
            -e "s/^calls to allocation functions:/CENSORED calls to allocation functions:/" \
            -e "s/^temporary memory allocations:/CENSORED temporary memory allocations:/" \
            -e "s/^peak heap memory consumption:/OBSERVED PREFIX peak heap memory consumption:/" \
            -e "s/^peak RSS /OBSERVED PREFIX peak RSS /" \
            -e "s/^suppressed leaks:/CENSORED suppressed leaks (prefix only):/" \
            -e "s/^total memory leaked:/CENSORED final live memory (not a leak measurement):/"
      } >"$report"
      echo "heaptrack report (censored): $report" >&2
    fi
    ;;
  coz)
    stop_service
    load_preparation_args "$@"
    load_profile_binary
    new_output_dir coz
    service_args+=(--profile-max-progress-points "$coz_progress_points")
    exec "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      coz run --output "$output_dir/profile.coz" \
      --source-scope '/build/source/crates/vesc-knowledge-index/src/%' \
      --- "$profile_binary" "${service_args[@]}"
    ;;
  coz-report)
    stop_service
    trace=${1:?usage: $0 coz-report PROFILE.COZ [REPORT.TXT]}
    report=${2:-"${trace%.coz}.txt"}
    awk '
      function field(name, item, parts) {
        for (item = 2; item <= NF; item++) {
          split($item, parts, "=")
          if (parts[1] == name) return substr($item, length(name) + 2)
        }
        return ""
      }
      /^experiment/ {
        experiments++
        experiment[experiments] = sprintf("%s\t%s\t%s\t%.3f", field("selected"), field("speedup"), field("selected-samples"), field("duration") / 1000000000)
      }
      /^throughput-point/ {
        name = field("name")
        throughput[name] += field("delta")
        throughput_observations[name]++
      }
      /^latency-point/ {
        name = field("name")
        latency_arrivals[name] += field("arrivals")
        latency_departures[name] += field("departures")
        latency_difference[name] += field("difference")
      }
      END {
        print "Causal experiments: " experiments
        print "source\tspeedup\tselected_samples\tduration_seconds"
        for (item = 1; item <= experiments; item++) print experiment[item]
        print ""
        print "Throughput progress points:"
        print "name\ttotal_delta\texperiments_observed"
        for (name in throughput) {
          print name "\t" throughput[name] "\t" throughput_observations[name]
        }
        print ""
        print "Latency progress points:"
        print "name\tarrivals\tdepartures\tdifference"
        for (name in latency_difference) {
          print name "\t" latency_arrivals[name] "\t" latency_departures[name] "\t" latency_difference[name]
        }
      }
    ' "$trace" >"$report"
    echo "coz report: $report" >&2
  ;;
  flamegraph)
    stop_service
    load_preparation_args "$@"
    load_profile_binary
    new_output_dir flamegraph
    set +e
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      perf record -F 99 -g --call-graph fp -o "$output_dir/perf.data" \
      -- "$profile_binary" "${service_args[@]}"
    profile_status=$?
    set -e
    [[ $profile_status == 0 || $profile_status == 124 ]] || exit "$profile_status"
    "${scope[@]}" env \
      PERF_DATA="$output_dir/perf.data" \
      FLAMEGRAPH="$output_dir/flamegraph.svg" \
      bash -o pipefail -c \
      'perf script --no-inline -i "$PERF_DATA" | inferno-collapse-perf | inferno-flamegraph >"$FLAMEGRAPH"'
    "${scope[@]}" "$script_dir/parse_perfdata" "$output_dir/perf.data" \
      >"$output_dir/perf-report.txt"
    "${scope[@]}" "$script_dir/parse_flamegraph" \
      "$output_dir/flamegraph.svg" summary >"$output_dir/flamegraph-report.txt"
    echo >>"$output_dir/flamegraph-report.txt"
    "${scope[@]}" "$script_dir/parse_flamegraph" \
      "$output_dir/flamegraph.svg" top 40 0.5 >>"$output_dir/flamegraph-report.txt"
    echo "perf report: $output_dir/perf-report.txt" >&2
    echo "flamegraph report: $output_dir/flamegraph-report.txt" >&2
    ;;
  perf-report)
    stop_service
    perf_data=${1:?usage: $0 perf-report PERF.DATA [REPORT.TXT]}
    report=${2:-"${perf_data%.data}.txt"}
    "${scope[@]}" "$script_dir/parse_perfdata" "$perf_data" >"$report"
    echo "perf report: $report" >&2
    ;;
  flamegraph-report)
    stop_service
    flamegraph=${1:?usage: $0 flamegraph-report FLAMEGRAPH.SVG [REPORT.TXT]}
    report=${2:-"${flamegraph%.svg}.txt"}
    "${scope[@]}" "$script_dir/parse_flamegraph" "$flamegraph" summary >"$report"
    echo >>"$report"
    "${scope[@]}" "$script_dir/parse_flamegraph" "$flamegraph" top 40 0.5 >>"$report"
    echo "flamegraph report: $report" >&2
    ;;
  *)
    echo "unknown profile command: $command" >&2
    exit 2
    ;;
esac
