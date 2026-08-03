#!/usr/bin/env python3
"""Aggregate the raw benchmark rows into honest statistics.

Reads results/raw.csv (written by run.sh), discards warm-up rows and any row
whose command failed (rc != 0), and reports median / p95 / stdev / n per
(tool, scenario, op) for three metrics: wall-clock seconds, peak resident memory
(max RSS), and CPU time. Emits results/report.md and results/summary.json.

raw.csv and summary.json stay in seconds at full precision — one canonical unit.
Only the report picks a readable one, per row (see row_unit).

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
BUDGET = os.path.join(os.path.dirname(os.path.abspath(__file__)), "memory-budget-mib")
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
	# Sub-10 ms rows, the ones /usr/bin/time could not see. They exercise the
	# millisecond branch of row_unit, which no whole-second fixture reaches.
	{"tool": "podup", "scenario": "running-ops", "op": "ps", "iter": "1", "phase": "measured", "seconds": "0.008521", "max_rss_kb": "9100", "cpu_s": "0.003812", "rc": "0"},
	{"tool": "podup", "scenario": "running-ops", "op": "ps", "iter": "2", "phase": "measured", "seconds": "0.008904", "max_rss_kb": "9150", "cpu_s": "0.003907", "rc": "0"},
	{"tool": "podman-compose", "scenario": "running-ops", "op": "ps", "iter": "1", "phase": "measured", "seconds": "0.412", "max_rss_kb": "29000", "cpu_s": "0.240", "rc": "0"},
	{"tool": "podman-compose", "scenario": "running-ops", "op": "ps", "iter": "2", "phase": "measured", "seconds": "0.421", "max_rss_kb": "29100", "cpu_s": "0.244", "rc": "0"},
	# Multi-second rows, so the seconds branch of row_unit is exercised too. A
	# fixture set that never crosses 1 s would leave half the formatting untested.
	{"tool": "podup", "scenario": "wide-level", "op": "up", "iter": "1", "phase": "measured", "seconds": "6.745", "max_rss_kb": "12800", "cpu_s": "1.040", "rc": "0"},
	{"tool": "podup", "scenario": "wide-level", "op": "up", "iter": "2", "phase": "measured", "seconds": "6.802", "max_rss_kb": "12900", "cpu_s": "1.061", "rc": "0"},
	{"tool": "podman-compose", "scenario": "wide-level", "op": "up", "iter": "1", "phase": "measured", "seconds": "41.220", "max_rss_kb": "61000", "cpu_s": "18.400", "rc": "0"},
	{"tool": "podman-compose", "scenario": "wide-level", "op": "up", "iter": "2", "phase": "measured", "seconds": "42.007", "max_rss_kb": "61200", "cpu_s": "18.910", "rc": "0"},
]


def pct(values, p):
	"""Nearest-rank percentile; honest for small n."""
	if not values:
		return float("nan")
	s = sorted(values)
	k = max(0, min(len(s) - 1, round(p / 100 * (len(s) - 1))))
	return s[k]


def row_unit(cells, metric):
	"""Pick one time unit for a whole report row, from its largest value.

	Returns (suffix, multiplier, decimals).

	One unit per row, applied to every tool in it. Choosing per cell would break
	the comparison the reader actually makes — one tool against another on the
	same operation — by putting "90 ms" next to "0.11 s". Across rows the
	workloads differ anyway, so a row is the widest scope where a shared unit
	still means something.

	p95 counts towards the choice, not just the median: a row whose median is a
	few milliseconds but whose p95 is over a second reads better in seconds than
	as a four-digit millisecond figure.
	"""
	values = []
	for cell in cells:
		if not cell:
			continue
		s = cell[metric]
		values += [v for v in (s["median"], s["p95"]) if v == v]
	if values and max(values) < 1.0:
		return ("ms", 1000.0, 1)
	# Above a minute, seconds stop being readable at a glance: a scenario that
	# takes two minutes printed as `120.000 s` makes the reader do the division.
	# No published row reaches this yet — the slowest is `wide-level up` at 9.7 s
	# for docker-compose — but the tier belongs here before a long scenario is
	# added rather than after, when the fix competes with reading the results.
	if values and max(values) >= 60.0:
		return ("min", 1.0 / 60.0, 2)
	return ("s", 1.0, 3)


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
				row = [summary.get(c, {}).get(scen, {}).get(op) for c in cols]
				cells = [fmt(cell, row) for cell in row]
				lines.append(f"| {scen} | {OP_LABEL[op]} | " + " | ".join(cells) + " |")
		lines.append("")

	def wall(cell, row):
		if not cell:
			return "—"
		suffix, mult, dec = row_unit(row, "seconds")
		s = cell["seconds"]
		def q(v):
			return f"{v * mult:.{dec}f}"
		return f"{q(s['median'])} {suffix} (p95 {q(s['p95'])}, sd {q(s['stdev'])})"

	def mem(cell, row):
		if not cell:
			return "—"
		# CPU time gets the same treatment as wall clock: rusage resolves to
		# microseconds, so a `ps` costing 4 ms of CPU no longer has to publish as
		# 0.004 s next to a build costing seconds.
		suffix, mult, dec = row_unit(row, "cpu_s")
		r, c = cell["rss_mib"], cell["cpu_s"]
		return f"{r['median']:.1f} MiB / {c['median'] * mult:.{dec}f} {suffix}"

	lines.append("All numbers are over the measured iterations (warm-up "
				 "discarded), same machine, same digest-pinned pre-pulled images, "
				 "same compose file per scenario.\n")

	metric_table(
		"Wall-clock — pure tool comparison (all drive Podman)",
		"Lower is better. Median with p95 and stdev in parentheses. Each row "
		"carries one unit, picked from the largest value in it, so the tools in "
		"a row stay directly comparable; raw.csv and summary.json keep every "
		"figure in seconds. Identical engine, so the only difference is the "
		"compose tool. "
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
		# Printed, not just built: the fixtures exist to exercise the formatting,
		# and a table nobody looks at cannot show that a row picked the wrong
		# unit or that a cell came out empty.
		print("\n".join(lines))
		# Exercise the budget gate in both directions. The fixture rows are
		# synthetic, so they are not measured against the real budget — but a gate
		# nobody has watched fail is decoration, and this is the only place the
		# shared-CI smoke run can watch it.
		# The unit tiers, exercised rather than assumed: a row is rendered in one
		# unit chosen from its largest value, and each boundary decides a report
		# column nobody re-reads once it is published.
		def unit_of(seconds):
			return row_unit([{"seconds": {"median": seconds, "p95": seconds}}], "seconds")[0]

		for value, want in ((0.5, "ms"), (1.0, "s"), (59.9, "s"), (60.0, "min"), (600.0, "min")):
			got = unit_of(value)
			if got != want:
				print(f"self-test FAILED: {value}s rendered in {got}, expected {want}", file=sys.stderr)
				return 1

		under = {"podup": {"single": {"up": {"rss_mib": {"median": 1.0}}}}}
		over = {"podup": {"single": {"up": {"rss_mib": {"median": 10_000.0}}}}}
		if check_memory_budget(under) != 0:
			print("self-test FAILED: the budget rejected a value under it", file=sys.stderr)
			return 1
		if check_memory_budget(over) != 1:
			print("self-test FAILED: the budget accepted a value over it", file=sys.stderr)
			return 1
		print(f"self-test ok ({len(rows)} fixture rows); {MD} and {JSON} left untouched")
		return 0

	with open(MD, "w") as f:
		f.write("\n".join(lines))
	print(f"wrote {MD} and {JSON}")
	return check_memory_budget(summary)


def check_memory_budget(summary):
	"""Fail when podup's peak median RSS exceeds the budget in bench/memory-budget-mib.

	The releases standard asks for a size budget per artifact and treats an
	unexplained growth as a regression to investigate rather than ship. There was
	none for memory, so the drift from 7.5 MiB at 2.1.0 to 8.1 at 3.4.1 was
	noticed by a human reading a published table months later instead of by a red
	run on the day it landed.

	The budget is a file, not a literal here, for the same reason
	.github/podman-baseline-tests is: raising it is a reviewable commit that says
	someone decided to, not a number that drifts inside a script.
	"""
	if not os.path.exists(BUDGET):
		print(f"no memory budget at {BUDGET} — skipping the check", file=sys.stderr)
		return 0
	with open(BUDGET) as f:
		budget = float(f.read().strip())
	peaks = [
		(scen, op, cell["rss_mib"]["median"])
		for scen, ops in summary.get("podup", {}).items()
		for op, cell in ops.items()
	]
	if not peaks:
		print("no podup rows to check against the memory budget", file=sys.stderr)
		return 0
	scen, op, worst = max(peaks, key=lambda t: t[2])
	# Report the number either way: a budget nobody sees the margin on is one
	# nobody notices tightening around them.
	print(f"memory: peak median {worst:.2f} MiB ({scen} {op}), budget {budget:.2f} MiB")
	if worst > budget:
		print(
			f"::error::podup peak median RSS {worst:.2f} MiB exceeds the "
			f"{budget:.2f} MiB budget in bench/memory-budget-mib "
			f"(worst: {scen} {op}). Attribute the growth, or raise the budget "
			f"deliberately in its own commit.",
			file=sys.stderr,
		)
		return 1
	return 0


if __name__ == "__main__":
	sys.exit(main())
