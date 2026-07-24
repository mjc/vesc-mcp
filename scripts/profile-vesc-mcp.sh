#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
cd "$repo_root"

if [[ ${VESC_MCP_PROFILE_SHELL:-} != 1 ]]; then
  exec nix develop -c env VESC_MCP_PROFILE_SHELL=1 "$0" "$@"
fi

command=${1:-}
[[ -n $command ]] || {
  echo "usage: $0 {build|prepare-time|heaptrack|heaptrack-report|coz|coz-report|flamegraph|perf-report|flamegraph-report} [args...]" >&2
  exit 2
}
shift

profile_package=${VESC_MCP_PROFILE_PACKAGE:-"$repo_root/result-profile"}
profile_root=${VESC_MCP_PROFILE_ROOT:-"${XDG_CONFIG_HOME:-"$HOME/.config"}/vesc-mcp/profiles"}
timeout_secs=${VESC_MCP_PROFILE_TIMEOUT_SECS:-600}
scope=(
  systemd-run --user --scope --quiet
  -p MemoryHigh=2G
  -p MemoryMax=3G
  -p MemorySwapMax=0
  -p CPUWeight=10
  -p IOWeight=10
)

stop_service() {
  if systemctl --user cat vesc-mcp.service &>/dev/null; then
    systemctl --user stop vesc-mcp.service
  fi
}

load_profile_binary() {
  local wrapper=$profile_package/bin/vesc-mcp-server
  [[ -x $wrapper ]] || {
    echo "missing profiling binary; run '$0 build'" >&2
    exit 1
  }
  unset ORT_DYLIB_PATH
  while IFS= read -r setup_line; do
    [[ $setup_line == exec\ * ]] && break
    eval "$setup_line"
  done <"$wrapper"
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
  } >"$output_dir/provenance.env"
  echo "profile output: $output_dir" >&2
}

case "$command" in
  build)
    stop_service
    exec "${scope[@]}" env CARGO_BUILD_JOBS=1 \
      nix build .#vesc-mcp-migraphx-profile --max-jobs 1 \
      --out-link "$profile_package" -L
    ;;
  prepare-time)
    stop_service
    load_profile_binary
    config=${1:?usage: $0 prepare-time CONFIG DATA_ROOT}
    data_root=${2:?usage: $0 prepare-time CONFIG DATA_ROOT}
    shift 2
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
            echo "server exited before publishing terminal preparation status" >&2
            ((server_status == 0)) && server_status=1
            exit "$server_status"
          fi
          (( SECONDS < deadline )) || {
            echo "preparation timed out after ${VESC_MCP_PROFILE_TIMEOUT_SECS}s" >&2
            exit 124
          }
          sleep 0.1
        done
        elapsed_ns=$(( $(date +%s%N) - start_ns ))
        cp "$status" "$VESC_MCP_PROFILE_OUTPUT/preparation-status.json"
        printf "elapsed_seconds=%d.%09d\n" \
          "$((elapsed_ns / 1000000000))" "$((elapsed_ns % 1000000000))" \
          | tee "$VESC_MCP_PROFILE_OUTPUT/timing.env"
        if grep -q '\''"state":"failed"'\'' "$status"; then
          echo "preparation failed" >&2
          exit 1
        fi
      ' bash "$@"
    ;;
  heaptrack)
    stop_service
    load_profile_binary
    new_output_dir heaptrack
    exec "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      heaptrack --record-only -o "$output_dir/heaptrack" "$profile_binary" "$@"
    ;;
  heaptrack-report)
    stop_service
    trace=${1:?usage: $0 heaptrack-report TRACE.zst}
    report=${2:-"${trace%.zst}.txt"}
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      heaptrack_print "$trace" >"$report"
    echo "heaptrack report: $report" >&2
    ;;
  coz)
    stop_service
    load_profile_binary
    new_output_dir coz
    exec "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      coz run --output "$output_dir/profile.coz" \
      --source-scope '/build/source/crates/vesc-knowledge-index/src/%' \
      --- "$profile_binary" "$@"
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
    load_profile_binary
    new_output_dir flamegraph
    set +e
    "${scope[@]}" timeout --signal=INT --kill-after=15s "$timeout_secs" \
      perf record -F 99 -g --call-graph fp -o "$output_dir/perf.data" \
      -- "$profile_binary" "$@"
    profile_status=$?
    set -e
    [[ $profile_status == 0 || $profile_status == 124 ]] || exit "$profile_status"
    "${scope[@]}" env \
      PERF_DATA="$output_dir/perf.data" \
      FLAMEGRAPH="$output_dir/flamegraph.svg" \
      bash -o pipefail -c \
      'perf script -i "$PERF_DATA" | inferno-collapse-perf | inferno-flamegraph >"$FLAMEGRAPH"'
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
