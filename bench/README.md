# Compose-tool benchmark

A reproducible, **fair** wall-clock comparison of compose tools. The point is not
to win; it is to publish honest, equitable numbers across identical scenarios.
A podup loss is published exactly like a podup win.

## What is compared

- **podup** and **podman-compose** both drive **Podman**, so comparing them is a
  pure *tool* comparison: same engine, only the orchestrator differs. This is the
  apples-to-apples result.
- **docker-compose** is pointed at the **Podman socket** through `DOCKER_HOST`,
  which is what makes it comparable: same engine, so the only difference left is
  the orchestrator. Run against a Docker daemon instead, its numbers fold in the
  engine difference and become an end-to-end *stack* comparison; the harness
  detects which engine it drove and labels the report accordingly, so a reader
  is never left guessing. It is never estimated when absent.

## Fairness rules (non-negotiable)

- **Identical inputs.** The same compose file per scenario for every tool; images
  are **pinned by digest and pre-pulled**, so image download is never timed.
- **Statistics, not single runs.** N iterations per cell, warm-up discarded,
  reported as **median + p95 + stdev**. A single number is never published.
- **Controlled environment.** The real run happens on a dedicated/self-hosted
  runner or the maintainer's machine, with the CPU governor pinned and the tool
  process taskset-pinned to reduce variance. **Shared CI runners are too noisy for
  published numbers**: CI only checks the harness (`bash -n`, a Python
  compile, `aggregate.py --self-test` on fixture rows, and a build of `timeit`
  with an assertion that it resolves `/bin/true` below 10 ms), never running the
  scenarios or the numbers in the README.
- **No cherry-picking.** Every scenario is published, whoever wins.

## Scenarios

| scenario | what it exercises |
|---|---|
| `single` | one container, minimal lifecycle cost |
| `multi-healthcheck` | `depends_on: service_healthy` gate on `up` |
| `deep-chain` | a dependency chain behind a fast service: where a level-barrier scheduler loses to a per-service DAG (#1071) |
| `wide-level` | 41 services in one level plus one dependent, the batching cost the two-service `deep-chain` cannot show |
| `scale` | `--scale app=5` replica fan-out |
| `network-ipam` | custom bridge network with explicit IPAM |
| `volume-heavy` | several named volumes created/removed |
| `secrets` | six `file:` secrets, materialised as Podman-native secrets since 3.1.0, which is an API call per secret each way |
| `warm-restart` | a second `up` on an already-running project |
| `many-services` | a 12-service compose file, the realistic upper end |
| `running-ops` | `ps`, `logs`, `exec`, `restart` on a running stack |
| `wide-running-ops` | the same read path across twelve containers, where the work grows with the container count |
| `config-heavy` | `config` over a base + override pair: the one scenario with no engine in it, so no daemon variance |
| `build` | `build --no-cache` from a Dockerfile (base pinned by digest) |

The lifecycle scenarios time `up -d` and `down -v` (`warm-restart` times the warm
second `up`); `running-ops` brings a stack up untimed, then times each of `ps`,
`logs`, `exec -T`, `restart`; `build` times a `--no-cache` image build. `init:
true` is set on idle `sleep` services so teardown measures the tool, not a
container ignoring `SIGTERM` as PID 1.

## Metrics

Every timed run goes through `bench/timeit`, so each row records **wall-clock,
peak resident memory (max RSS) and CPU time** of the tool process. The memory and
CPU figures are the **client-side** cost: the tool process and the processes it
directly spawns and waits on. podup is a thin client to the long-running Podman
service, so engine-side work is not charged to it; podman-compose shells out to
`podman` per call and is charged for the work it waits on. The columns therefore
answer "what does invoking the tool cost on my machine", not "how much work does
the engine do".

`timeit` is a small Rust binary that `fork`s the command, takes the clock across
it with `Instant`, and reads the rest from `wait4`'s rusage. `run.sh` builds it
on first use; it lives outside the workspace, like `fuzz/`, so it is not part of
a podup build.

It replaced `/usr/bin/time -v`, whose `Elapsed` line resolves to 0.01 s. That put
a floor under the suite's two fastest rows (`running-ops ps` and `config-heavy
config`, both under 10 ms), which published as `0.000` while `raw.csv` stored
them as `%.6f` seconds.

**Why the timer is compiled rather than a script.** `ru_maxrss` survives
`execve`: a child inherits its parent's high-water mark, so a wrapper's own
footprint becomes a floor under every memory figure it reports. Measured on
`/bin/true`, whose real cost is about 1.3 MB: `/usr/bin/time -v` 1336 KB, this
binary 1312 KB, a `python3` wrapper 6304 KB with `fork` + `execvp` and 9792 KB
with `posix_spawnp`. A ~6 MB floor under a column that reports podup at about
8.9 MB would leave that column measuring the wrapper. The same run had the
interpreter inflating the clock on short commands, timing `/bin/true` at 1.0 ms
against 0.50 ms here.

## Running it

```sh
# Measure the binary people install: the published musl asset, or a build of
# the same target. A plain `cargo build --release` on a glibc host links
# dynamically and reports about twice the memory of the static binary.
cargo build --release --locked --target x86_64-unknown-linux-musl
PODUP_BIN=target/x86_64-unknown-linux-musl/release/podup \
  bash bench/run.sh --iters 12 --warmup 2 --cores 2-9
python3 bench/aggregate.py
# -> bench/results/report.md and bench/results/summary.json
```

`--smoke` runs a single scenario once, for a quick local check against a real engine (CI no longer uses it; it static-checks the harness instead, see above).

## Output

`run.sh` writes one raw row per timed run to `results/raw.csv`; `aggregate.py`
discards warm-up and failed runs and computes the statistics into
`results/report.md` + `results/summary.json`. Raw, host-specific results are not
committed; the published numbers live in `docs/benchmarks.md`, with the
methodology and host details alongside them, and a short summary in the
repository `README.md`.

The harness is reviewed by `podup-benchmark-fairness-auditor` (the harness is
equitable) and `podup-benchmark-results-reviewer` (the published claims are
supported and honest).

## Budgets

Two ceilings live here, both read by a gate rather than by a reader:

| file | what it bounds | enforced by |
|---|---|---|
| `memory-budget-mib` | peak RSS of a benchmarked command | `aggregate.py`, which fails the run above it |
| `binary-budget-mib` | the release binary on disk | `.github/workflows/binary-budget.yml`, on every pull request |

Both exist because the releases standard asks for a size budget per artifact and
treats unexplained growth as a regression to investigate rather than ship. Before
them, drift was something someone noticed in a published table months later.

**Moving a budget is allowed and is meant to be deliberate.** Raise the number in
the same commit as the growth that needs it, and say in the message what the
growth buys. A budget that is raised silently the moment it fires is not a gate.
