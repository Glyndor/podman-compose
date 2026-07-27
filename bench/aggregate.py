#!/usr/bin/env python3
"""Aggregate the raw benchmark rows into honest statistics.

Reads results/raw.csv (written by run.sh), discards warm-up rows and any row
whose command failed (rc != 0), and reports median / p95 / stdev / n per
(tool, scenario, op) for three metrics: wall-clock seconds, peak resident memory
(max RSS), and CPU time. Emits results/report.md and results/summary.json.

No number is invented here: every statistic is computed from the measured rows,
and a losing result is printed exactly like a winning one.
"""
import csv
import json
import os
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "results", "raw.csv")
MD = os.path.join(HERE, "results", "report.md")
JSON = os.path.join(HERE, "results", "summary.json")
ENGINE = os.path.join(HERE, "results", "engine")


def read_engine():
	"""Which engine docker-compose drove, as recorded by run.sh next to raw.csv.

	Empty when the file is absent — an older results directory, or a run where
	docker-compose was not measured at all.
	"""
	try:
		with open(ENGINE) as f:
			return f.read().strip()
	except OSError:
		return ""

# Preferred ordering only. Anything measured but not listed here is appended
# rather than dropped: this list silently discarded four scenarios' worth of
# results (config-heavy, wide-running-ops, deep-chain, wide-level) because it was
# a filter, not an order — 972 rows measured, four scenarios never printed.
SCEN_ORDER = [
	"single", "multi-healthcheck", "deep-chain", "wide-level", "scale",
	"network-ipam", "volume-heavy", "secrets", "warm-restart", "many-services",
	"running-ops", "wide-running-ops", "config-heavy", "build",
]
OP_ORDER = ["up", "reup", "down", "config", "ps", "logs", "exec", "restart", "build"]
OP_LABEL = {
	"up": "up", "down": "down", "reup": "warm up", "ps": "ps", "logs": "logs",
	"exec": "exec", "restart": "restart", "build": "build", "config": "config",
}

# Inline sample rows, shaped exactly like raw.csv, for `--self-test`. bench/results/
# is a local, git-ignored artifact directory (real numbers only come from the
# controlled, self-hosted benchmark run), so a shared-runner smoke check has no
# raw.csv to read. These rows exercise the same filtering and statistics path
# (warm-up discarded, failed rows discarded, median/p95/stdev computed) without
# depending on committed benchmark data or a real Podman engine.
SELF_TEST_ROWS = [
	{"tool": "podup", "scenario": "single", "op": "up", "iter": "0", "phase": "warmup", "seconds": "0.520", "max_rss_kb": "10240", "cpu_s": "0.110", "rc": "0"},
	{"tool": "podup", "scenario": "single", "op": "up", "iter": "1", "phase": "measured", "seconds": "0.500", "max_rss_kb": "10000", "cpu_s": "0.100", "rc": "0"},
	{"tool": "podup", "scenario": "single", "op": "up", "iter": "2", "phase": "measured", "seconds": "0.510", "max_rss_kb": "10100", "cpu_s": "0.105", "rc": "0"},
	{"tool": "podup", "scenario": "single", "op": "down", "iter": "1", "phase": "measured", "seconds": "0.200", "max_rss_kb": "9500", "cpu_s": "0.050", "rc": "0"},
	{"tool": "podup", "scenario": "single", "op": "down", "iter": "2", "phase": "measured", "seconds": "0.210", "max_rss_kb": "9600", "cpu_s": "0.052", "rc": "0"},
	{"tool": "podman-compose", "scenario": "single", "op": "up", "iter": "1", "phase": "measured", "seconds": "0.800", "max_rss_kb": "30000", "cpu_s": "0.300", "rc": "0"},
	{"tool": "podman-compose", "scenario": "single", "op": "up", "iter": "2", "phase": "measured", "seconds": "0.001", "max_rss_kb": "1", "cpu_s": "0.001", "rc": "1"},
]


def pct(values, p):
	"""Nearest-rank percentile; honest for small n."""
	if not values:
		return float("nan")
	s = sorted(values)
	k = max(0, min(len(s) - 1, round(p / 100 * (len(s) - 1))))
	return s[k]


def stats(values):
	return {
		"n": len(values),
		"median": statistics.median(values) if values else float("nan"),
		"p95": pct(values, 95),
		"stdev": statistics.pstdev(values) if len(values) > 1 else 0.0,
		"min": min(values) if values else float("nan"),
	}


def filter_measured(rows):
	"""Keep only completed, successful iterations (drop warm-up and failures)."""
	return [r for r in rows if r["phase"] == "measured" and int(r["rc"]) == 0]


def load(path):
	with open(path, newline="") as f:
		return filter_measured(list(csv.DictReader(f)))


def main():
	self_test = "--self-test" in sys.argv
	if self_test:
		rows = filter_measured(SELF_TEST_ROWS)
	else:
		if not os.path.exists(RAW):
			print(f"no raw data at {RAW}", file=sys.stderr)
			return 1
		rows = load(RAW)
	tools = sorted({r["tool"] for r in rows})
	measured = {r["scenario"] for r in rows}
	# Ordered by preference, then anything else that was measured. A scenario
	# absent from SCEN_ORDER used to vanish from the report with no warning,
	# which is worse than an ugly order: the run costs half an hour and the
	# missing rows look like they were never measured.
	scenarios = [s for s in SCEN_ORDER if s in measured]
	scenarios += sorted(measured - set(scenarios))

	# summary[tool][scenario][op] = {seconds:..., rss_mib:..., cpu_s:...}
	summary = {}
	for tool in tools:
		for scen in scenarios:
			for op in OP_ORDER:
				sel = [r for r in rows if r["tool"] == tool
					   and r["scenario"] == scen and r["op"] == op]
				if not sel:
					continue
				cell = {
					"seconds": stats([float(r["seconds"]) for r in sel]),
					"rss_mib": stats([int(r["max_rss_kb"]) / 1024 for r in sel]),
					"cpu_s": stats([float(r["cpu_s"]) for r in sel]),
				}
				summary.setdefault(tool, {}).setdefault(scen, {})[op] = cell

	if not self_test:
		with open(JSON, "w") as f:
			json.dump(summary, f, indent="\t", sort_keys=True)

	# Which engine docker-compose drove decides which table it belongs in, and
	# that is a property of the RUN, not of the tool's name. run.sh records it;
	# assuming "docker-compose means dockerd" printed a same-engine measurement
	# under a heading that said "different daemon", which is the report saying
	# the opposite of what happened.
	# The env var only reaches here when the aggregator runs inside run.sh's own
	# process; the documented flow is two separate commands, so the file run.sh
	# leaves beside raw.csv is the path that actually works. Env wins when set, so
	# a hand-driven run can still override it.
	dc_engine = os.environ.get("BENCH_DOCKER_ENGINE", "") or read_engine()
	dc_same = dc_engine == "podman"
	same = [t for t in tools if t in ("podup", "podman-compose")]
	if dc_same:
		same += [t for t in tools if t == "docker-compose"]
	cross = [] if dc_same else [t for t in tools if t == "docker-compose"]
	lines = []

	def metric_table(title, intro, cols, fmt):
		if not cols:
			return
		lines.append(f"### {title}\n")
		if intro:
			lines.append(intro + "\n")
		lines.append("| scenario | op | " + " | ".join(cols) + " |")
		lines.append("|" + "---|" * (len(cols) + 2))
		for scen in scenarios:
			for op in OP_ORDER:
				if not any(op in summary.get(c, {}).get(scen, {}) for c in cols):
					continue
				cells = [fmt(summary.get(c, {}).get(scen, {}).get(op)) for c in cols]
				lines.append(f"| {scen} | {OP_LABEL[op]} | " + " | ".join(cells) + " |")
		lines.append("")

	def wall(cell):
		if not cell:
			return "—"
		s = cell["seconds"]
		return f"{s['median']:.3f} (p95 {s['p95']:.3f}, sd {s['stdev']:.3f})"

	def mem(cell):
		if not cell:
			return "—"
		r, c = cell["rss_mib"], cell["cpu_s"]
		return f"{r['median']:.1f} MiB / {c['median']:.3f} s"

	lines.append("All numbers are over the measured iterations (warm-up "
				 "discarded), same machine, same digest-pinned pre-pulled images, "
				 "same compose file per scenario.\n")

	metric_table(
		"Wall-clock — pure tool comparison (all drive Podman)",
		"Seconds, lower is better. Median with p95 and stdev in parentheses. "
		"Identical engine, so the only difference is the compose tool. "
		"docker-compose appears here when it was pointed at the Podman socket "
		"rather than at a Docker daemon.",
		same, wall)
	metric_table(
		"Memory + CPU — pure tool comparison (all drive Podman)",
		"Peak resident memory (max RSS) and CPU time of the tool process per "
		"command, median. This is the client-side cost of running the tool: "
		"podup is a static binary talking to the Podman service, podman-compose "
		"is Python shelling out to `podman`.",
		same, mem)
	metric_table(
		"Wall-clock — cross-engine stack (different daemon)",
		"docker-compose drives dockerd, so these are an end-to-end stack "
		"comparison, not pure-tool. Only present when a Docker Engine was "
		"available on the benchmark host.",
		cross, wall)
	if not cross and not dc_same:
		lines.append("> docker-compose was not measured against a Docker daemon "
					 "on this host, so the cross-engine comparison is left blank "
					 "rather than estimated.\n")

	if self_test:
		# The self-test runs on six fixture rows. Writing them out would replace a
		# real report and summary — the output of a benchmark that takes the better
		# part of an hour and cannot be recomputed, since raw.csv is the only copy.
		print(f"self-test ok ({len(rows)} fixture rows); {MD} and {JSON} left untouched")
		return 0

	with open(MD, "w") as f:
		f.write("\n".join(lines))
	print(f"wrote {MD} and {JSON}")
	return 0


if __name__ == "__main__":
	sys.exit(main())
