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
  echo "usage: $0 {build|prepare-time|heaptrack|heaptrack-report|coz|coz-report|flamegraph|perf-report|flamegraph-report} [args...]" >&2
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
  mkdir -p "$profile_root"
  output_dir=$(mktemp -d "$profile_root/$(date -u +%Y%m%dT%H%M%SZ)-$1.XXXXXX")
  {
    echo "git_commit=$(git rev-parse HEAD)"
    echo "git_dirty=$(test -n "$(git status --short)" && echo true || echo false)"
    echo "profile_package=$(readlink -f "$profile_package")"
    echo "profile_binary=$(readlink -f "$profile_binary")"
    echo "memory_high=$memory_high"
    echo "memory_max=$memory_max"
    echo "data_root=${VESC_MCP_PROFILE_DATA_ROOT:-}"
    echo "migraphx_cache=$profile_migraphx_cache"
  } >"$output_dir/provenance.env"
  echo "profile output: $output_dir" >&2
}

case "$command" in
  build)
    stop_service
    exec "${build_scope[@]}" nix build .#vesc-mcp-migraphx-profile \
      --max-jobs 1 --cores 1 --option builders "" --out-link "$profile_package" -L
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
