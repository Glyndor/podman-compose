#!/usr/bin/env bash
# Fair compose-tool benchmark runner.
#
# Drives each tool through the same scenario suite, the same number of times,
# on the same machine, against digest-pinned, pre-pulled images. Each timed run
# goes through bench/timeit, so every row records wall-clock, peak resident
# memory and CPU time of the orchestrator process; the statistics (median / p95 /
# stdev) are computed by aggregate.py, never here.
#
# Fairness is the whole point: identical compose inputs, identical lifecycle,
# warm-up iterations discarded, every scenario reported, the same op flags for
# every tool. Same-engine tools (podup, podman-compose) drive Podman and are a
# pure tool comparison; docker-compose drives dockerd and is only run when a
# Docker daemon is present, always labelled as an end-to-end stack comparison.
#
# Note on the memory/CPU columns: they are the resource use of the tool process
# and the processes it directly spawns and waits on (getrusage). podup is a thin
# client to the long-running Podman service, so engine-side work is not charged
# to it; podman-compose shells out to the `podman` binary per call, whose work it
# waits on and is charged for. The columns therefore measure client-side cost per
# command, what running the tool costs on your machine, not engine work.
#
# Usage: bench/run.sh [--iters N] [--warmup W] [--cores CPUSET] [--smoke]
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCEN_DIR="$HERE/scenarios"
OUT_DIR="$HERE/results"
RAW="$OUT_DIR/raw.csv"
TIMEIT_DIR="$HERE/timeit"
TIMEIT="$TIMEIT_DIR/target/release/timeit"

ITERS=12
WARMUP=2
CORES=""
SMOKE=0
PODUP_BIN="${PODUP_BIN:-podup}"

while [ $# -gt 0 ]; do
	case "$1" in
		--iters) ITERS="$2"; shift 2 ;;
		--warmup) WARMUP="$2"; shift 2 ;;
		--cores) CORES="$2"; shift 2 ;;
		--smoke) SMOKE=1; ITERS=1; WARMUP=0; shift ;;
		*) echo "unknown arg: $1" >&2; exit 2 ;;
	esac
done

mkdir -p "$OUT_DIR"

# The timer is on the measured path, so build it before the first scenario
# rather than discovering it is missing after half an hour of empty rows.
if [ ! -x "$TIMEIT" ]; then
	echo ">>> building the timer (bench/timeit)"
	cargo build --release --manifest-path "$TIMEIT_DIR/Cargo.toml" >/dev/null || {
		echo "bench: could not build $TIMEIT_DIR" >&2
		exit 2
	}
fi

# Scenario list and the op-group each one measures.
#   updown  : time `up -d` and `down -v`
#   scale   : like updown but `up -d --scale app=5`
#   reup    : time a warm second `up -d`
#   running : bring up untimed, then time `ps`, `logs`, `exec -T`, `restart`
#   build   : time `build --no-cache`
SCENARIOS=(single multi-healthcheck deep-chain wide-level scale network-ipam volume-heavy secrets warm-restart many-services running-ops wide-running-ops config-heavy build)
declare -A OP=(
	[single]=updown [multi-healthcheck]=updown [scale]=scale
	[network-ipam]=updown [volume-heavy]=updown [secrets]=updown [warm-restart]=reup
	[many-services]=updown [running-ops]=running [build]=build
	[config-heavy]=parse [wide-running-ops]=running
	[deep-chain]=updown [wide-level]=updown
)

# Every scenario must have an op, or the run dies partway through with an
# unbound-variable error under `set -u`. deep-chain and wide-level were added to
# SCENARIOS in #1123 and never added here, so the suite has been unable to
# complete since; nobody noticed because those two were only ever run by hand,
# one at a time, to measure the scheduler change that introduced them.
for _s in "${SCENARIOS[@]}"; do
	[ -n "${OP[$_s]:-}" ] || { echo "bench: scenario '$_s' has no entry in OP" >&2; exit 2; }
done
if [ "$SMOKE" -eq 1 ]; then SCENARIOS=(single running-ops); fi

TOOLS=(podup podman-compose)
# docker compose is the tool podup targets for parity, so it is the comparison
# that matters most, but WHICH engine it drives changes what the numbers mean.
#
#   against Podman  a pure tool comparison: same engine, so the difference is
#                   the orchestrator and nothing else. This is the fair one, and
#                   it needs no Docker installed, only DOCKER_HOST pointed at
#                   the Podman socket.
#   against dockerd a whole-stack comparison: engine differences are folded in,
#                   so it cannot be read as "tool A is faster than tool B".
#
# Both are reported; the label says which, because a reader seeing
# "docker-compose" will assume dockerd.
# Which engine docker-compose drove is written NEXT TO THE RESULTS, not only
# exported. An env var dies with this process, and the documented flow is two
# commands (`bash bench/run.sh`, then `python3 bench/aggregate.py`), so the
# aggregator never saw it and fell back to assuming dockerd, printing a
# same-engine measurement under a heading that says "different daemon". The file
# travels with raw.csv, which is the only thing that can still be read afterwards.
ENGINE_FILE="$OUT_DIR/engine"
rm -f "$ENGINE_FILE"
if command -v docker-compose >/dev/null 2>&1; then
	if docker info >/dev/null 2>&1; then
		TOOLS+=(docker-compose)
		export BENCH_DOCKER_ENGINE=docker
		echo docker > "$ENGINE_FILE"
		echo "note: Docker Engine present; docker-compose measured as a CROSS-ENGINE (whole-stack) run."
	elif [ -n "${DOCKER_HOST:-}" ] && docker-compose ls >/dev/null 2>&1; then
		TOOLS+=(docker-compose)
		# Recorded so the report puts these rows under the right heading; the
		# tool's name does not say which engine it drove.
		export BENCH_DOCKER_ENGINE=podman
		echo podman > "$ENGINE_FILE"
		echo "note: docker-compose driving Podman via DOCKER_HOST; measured as a SAME-ENGINE (pure tool) run."
	else
		echo "note: docker-compose found but no reachable engine; NOT measured. Set DOCKER_HOST to the Podman socket to include it."
	fi
else
	echo "note: docker-compose not installed; NOT measured."
fi

run() { # tool, compose-file, project, op-args...
	local tool="$1" file="$2" proj="$3"; shift 3
	local pre=(); [ -n "$CORES" ] && pre=(taskset -c "$CORES")
	# `file` may name several compose files separated by spaces, so a scenario can
	# exercise the base+override merge every real project has. One name yields one
	# -f, exactly as before.
	local fargs=(); local f; for f in $file; do fargs+=(-f "$f"); done
	case "$tool" in
		podup)          "${pre[@]}" "$PODUP_BIN" "${fargs[@]}" -p "$proj" "$@" ;;
		podman-compose) "${pre[@]}" podman-compose "${fargs[@]}" -p "$proj" "$@" ;;
		docker-compose) "${pre[@]}" docker-compose "${fargs[@]}" -p "$proj" "$@" ;;
	esac
}

# Builds the real external command for a tool (the timer execs it, so it cannot
# be a shell function) and echoes "wall_s max_rss_kb cpu_s rc".
timed() { # tool, compose-file, project, op-args...
	local tool="$1" file="$2" proj="$3"; shift 3
	local cmd=(); [ -n "$CORES" ] && cmd=(taskset -c "$CORES")
	local fargs=(); local f; for f in $file; do fargs+=(-f "$f"); done
	case "$tool" in
		podup)          cmd+=("$PODUP_BIN" "${fargs[@]}" -p "$proj" "$@") ;;
		podman-compose) cmd+=(podman-compose "${fargs[@]}" -p "$proj" "$@") ;;
		docker-compose) cmd+=(docker-compose "${fargs[@]}" -p "$proj" "$@") ;;
	esac
	LC_ALL=C "$TIMEIT" "${cmd[@]}"
}

teardown() { run "$1" "$2" "$3" down -v >/dev/null 2>&1; }

# Pre-pull the digest-pinned bases so image download is never on the timed path.
#
# LC_ALL=C on the sort: UTF-8 collations ignore punctuation at the primary
# level, so `sort -u` treats two image references that differ only in a hyphen
# as equal and drops one. That image is then pulled during the run instead of
# before it, and the benchmark times a network download rather than a start --
# silently, as a number that is simply larger.
echo ">>> pre-pulling pinned images"
grep -rhoE 'docker\.io/[^ "]+@sha256:[a-f0-9]+' "$SCEN_DIR" | LC_ALL=C sort -u | while read -r img; do
	podman pull -q "$img" >/dev/null 2>&1 || echo "  warning: could not pre-pull $img" >&2
done

echo "tool,scenario,op,iter,phase,seconds,max_rss_kb,cpu_s,rc" > "$RAW"

for tool in "${TOOLS[@]}"; do
	for scen in "${SCENARIOS[@]}"; do
		file="$SCEN_DIR/$scen/compose.yaml"
		# A scenario may ship an override alongside its base file; merging the two
		# is what real projects do and what no single-file scenario can measure.
		# Passed explicitly rather than relying on auto-discovery, since the tools
		# disagree about that and this must compare the same work.
		[ -f "$SCEN_DIR/$scen/compose.override.yaml" ] &&
			file="$file $SCEN_DIR/$scen/compose.override.yaml"
		op="${OP[$scen]}"
		proj="bench_${tool//-/_}_${scen//-/_}"
		echo ">>> $tool / $scen (op=$op)"
		teardown "$tool" "$file" "$proj"
		for ((i=0; i<ITERS; i++)); do
			phase="measured"; [ "$i" -lt "$WARMUP" ] && phase="warmup"
			row() { echo "$tool,$scen,$1,$i,$phase,${2// /,}" >> "$RAW"; }
			case "$op" in
				updown)
					row up   "$(timed "$tool" "$file" "$proj" up -d)"
					row down "$(timed "$tool" "$file" "$proj" down -v)"
					;;
				scale)
					row up   "$(timed "$tool" "$file" "$proj" up -d --scale app=5)"
					row down "$(timed "$tool" "$file" "$proj" down -v)"
					;;
				reup)
					run "$tool" "$file" "$proj" up -d >/dev/null 2>&1
					row reup "$(timed "$tool" "$file" "$proj" up -d)"
					teardown "$tool" "$file" "$proj"
					;;
				running)
					run "$tool" "$file" "$proj" up -d >/dev/null 2>&1
					row ps      "$(timed "$tool" "$file" "$proj" ps)"
					row logs    "$(timed "$tool" "$file" "$proj" logs app)"
					row exec    "$(timed "$tool" "$file" "$proj" exec -T app true)"
					row restart "$(timed "$tool" "$file" "$proj" restart app)"
					teardown "$tool" "$file" "$proj"
					;;
				parse)
					# The only op with no engine on the other side: read, interpolate,
					# merge and re-render. No containers means no daemon variance, so
					# this is the least noisy number the suite produces, and it is
					# what CI runs most, to validate a file before deploying it.
					row config "$(timed "$tool" "$file" "$proj" config)"
					;;
				build)
					row build "$(timed "$tool" "$file" "$proj" build --no-cache)"
					podman rmi -f "podup-bench-build:latest" >/dev/null 2>&1
					;;
			esac
		done
		teardown "$tool" "$file" "$proj"
	done
done

echo "raw rows: $(( $(wc -l < "$RAW") - 1 )) -> $RAW"
