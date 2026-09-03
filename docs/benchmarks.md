# Benchmarks

## vs alternatives

|  | podup | docker-compose | podman-compose (Python) |
|---|---|---|---|
| Engine | rootless Podman | Docker daemon | Podman |
| Runtime | single static binary | Go binary + Docker daemon | Python + pip packages |
| Root required | no | typically yes (daemon) | no |
| Implementation | Rust | Go | Python |
| Podman API | native libpod REST | n/a | Podman CLI shell-out |
| Systemd Quadlet export | yes (`generate quadlet`) | no | no |
| Platforms | Linux · macOS · Windows (single binary) | Linux · macOS · Windows | wherever Python runs |
| Compose-spec depth | `extends`, profiles, `develop.watch`, inline secrets/configs | full | partial |

## Methodology

All three tools drive **the same rootless Podman**, so this is a pure *tool*
comparison: identical engine, identical digest-pinned and pre-pulled images,
identical compose file per scenario, the same op flags for every tool. Each
number is the median over **10 measured iterations** — 12 runs with the first
2 discarded as warm-up; p95 and standard deviation are in parentheses.

`docker-compose` normally drives dockerd. Pointing it at the Podman socket
through `DOCKER_HOST` is what makes it comparable here — the only difference
left is the orchestrator. Run against a Docker daemon instead, the numbers
would fold in the engine difference and could not be read as tool-versus-tool;
the harness detects which engine it drove and labels the report accordingly.

Reproduce with `bash bench/run.sh` (set `DOCKER_HOST` to the Podman socket to
include docker-compose), then `python3 bench/aggregate.py`. Raw per-iteration
rows land in `bench/results/raw.csv`; the statistics are computed there and
never by the runner.

Timing comes from `bench/timeit`, which forks the command, takes the clock
across it, and reads peak RSS and CPU from `wait4`'s rusage. It replaced
`/usr/bin/time -v`, whose `Elapsed` field resolves to 0.01 s — coarse enough
that the fastest rows here used to publish as `0.000` and podup's entire CPU
column with them.

Each row carries **one unit**, picked from the largest value in that row and
applied to every tool in it, so the tools in a row stay directly comparable.
`bench/results/raw.csv` and `summary.json` keep every figure in seconds.

Measured on podup **5.7.1**, the published `podup-linux-x86_64` asset (static
musl, the binary the installers fetch), against podman-compose 1.5.0 and
docker-compose 5.1.3 on rootless Podman 5.7.0, 16 cores, CPU governor pinned to
`performance`, tools pinned to cores 2-9, no virtual machines running. Podman's
storage held 15 containers, 23 networks, 83 volumes and 73 images at the start,
all of them the host's own projects. 1044 timed runs in ten minutes, none failed.

**This table can be read against the 3.4.1 one, with one caveat.** Podman,
podman-compose and docker-compose are the same versions in both runs; only podup
changed, 3.4.1 to 5.7.1. The caveat is the host: the reference that did not
change, `docker-compose`, moved **-6.5%** (median over the 28 comparable rows),
so the environment was that much faster this time. Against that reference podup
moved **-5.7%**, flat: nine releases on the hot path of `up` cost nothing
measurable. podman-compose moved -17.2% with no change of its own, and the
storage explains it: it shells out to the `podman` binary per call, and a
`podman ps` on this host went from 56 ms to 28 ms once a few hundred networks
and images left over from integration tests were removed before the run. That
cleanup is now part of the method, and the counts above are what "otherwise
idle" means for the engine.

Three rows moved for reasons that are podup's own. `running-ops logs` and
`wide-running-ops logs` fell **-66%** and **-55%** (22.5 ms to 7.5 ms, 22.5 ms
to 10.2 ms), and the two `ps` rows **-28%** and **-22%**: the read path stopped
paying for work it did not need. Peak memory fell in every row, from a median of
8.0 MiB to **5.6 MiB** per command and a worst case of 6.5 MiB against 8.6; the
run's own budget gate (`bench/memory-budget-mib`, 9.0) reads that figure and
would have failed the aggregation above it.

One row reads as a regression and is not. `config-heavy config` prints 13.0 ms
here against 9.4 ms in the 3.4.1 table, +38%. Run head-to-head on this machine,
alternating binaries, 3.4.1 takes 11.7 ms and 5.7.1 11.6 ms. That row sits close
enough to process start that between-run noise reads as double digits, which is
the warning the 3.4.1 table already carried for `running-ops ps`; the number is
published because it was measured, and the head-to-head is what it means.

## Wall-clock (lower is better)

| scenario | op | podman-compose | podup | docker-compose |
|---|---|---|---|---|
| single | up | 343.9 ms (p95 353.5, sd 6.0) | 81.7 ms (p95 93.3, sd 5.2) | 102.6 ms (p95 112.9, sd 4.6) |
| single | down | 350.7 ms (p95 360.7, sd 5.1) | 128.6 ms (p95 149.4, sd 8.9) | 144.5 ms (p95 164.6, sd 8.6) |
| multi-healthcheck | up | 0.856 s (p95 1.804, sd 0.293) | 0.298 s (p95 0.390, sd 0.089) | 0.700 s (p95 0.714, sd 0.009) |
| multi-healthcheck | down | 485.5 ms (p95 646.4, sd 66.5) | 264.4 ms (p95 283.2, sd 14.8) | 277.9 ms (p95 322.9, sd 19.4) |
| deep-chain | up | 1.485 s (p95 1.665, sd 0.122) | 0.344 s (p95 0.376, sd 0.016) | 0.856 s (p95 0.892, sd 0.017) |
| deep-chain | down | 743.7 ms (p95 892.6, sd 64.2) | 378.6 ms (p95 462.7, sd 28.5) | 380.1 ms (p95 411.5, sd 18.2) |
| wide-level | up | 6.649 s (p95 7.069, sd 0.217) | 1.105 s (p95 1.171, sd 0.028) | 1.581 s (p95 1.743, sd 0.063) |
| wide-level | down | 3.917 s (p95 4.493, sd 0.309) | 1.696 s (p95 1.983, sd 0.154) | 2.010 s (p95 2.375, sd 0.248) |
| scale | up | 373.1 ms (p95 395.8, sd 9.7) | 182.1 ms (p95 201.4, sd 9.8) | 378.7 ms (p95 389.0, sd 15.0) |
| scale | down | 306.8 ms (p95 311.6, sd 4.4) | 255.6 ms (p95 271.1, sd 13.1) | 278.7 ms (p95 323.8, sd 18.6) |
| network-ipam | up | 598.7 ms (p95 801.6, sd 93.7) | 103.0 ms (p95 108.3, sd 4.6) | 127.3 ms (p95 141.0, sd 5.9) |
| network-ipam | down | 565.8 ms (p95 653.7, sd 80.5) | 160.2 ms (p95 181.4, sd 19.1) | 194.5 ms (p95 206.4, sd 8.4) |
| volume-heavy | up | 728.2 ms (p95 741.3, sd 6.7) | 99.6 ms (p95 104.1, sd 5.3) | 124.6 ms (p95 132.1, sd 4.3) |
| volume-heavy | down | 490.2 ms (p95 528.8, sd 17.4) | 143.9 ms (p95 153.9, sd 7.2) | 182.9 ms (p95 216.5, sd 15.1) |
| secrets | up | 346.7 ms (p95 378.5, sd 11.9) | 98.8 ms (p95 114.1, sd 6.4) | 111.0 ms (p95 119.7, sd 4.6) |
| secrets | down | 353.2 ms (p95 387.5, sd 14.9) | 139.3 ms (p95 159.4, sd 8.7) | 150.3 ms (p95 172.6, sd 8.6) |
| warm-restart | warm up | 186.8 ms (p95 243.2, sd 17.6) | 32.3 ms (p95 37.6, sd 4.2) | 44.2 ms (p95 50.5, sd 3.5) |
| many-services | up | 1.888 s (p95 1.948, sd 0.023) | 0.366 s (p95 0.394, sd 0.018) | 0.472 s (p95 0.520, sd 0.016) |
| many-services | down | 1.163 s (p95 1.192, sd 0.023) | 0.506 s (p95 0.790, sd 0.099) | 0.508 s (p95 0.637, sd 0.046) |
| running-ops | ps | 114.3 ms (p95 118.5, sd 2.1) | 6.3 ms (p95 7.7, sd 0.7) | 21.6 ms (p95 23.2, sd 0.8) |
| running-ops | logs | 147.0 ms (p95 156.5, sd 5.7) | 7.5 ms (p95 9.4, sd 1.0) | 43.4 ms (p95 46.1, sd 2.2) |
| running-ops | exec | 181.1 ms (p95 194.8, sd 5.5) | 61.8 ms (p95 67.1, sd 2.6) | 72.4 ms (p95 80.9, sd 5.1) |
| running-ops | restart | 272.9 ms (p95 298.7, sd 12.2) | 161.3 ms (p95 187.5, sd 9.2) | 191.0 ms (p95 201.5, sd 9.9) |
| wide-running-ops | ps | 121.8 ms (p95 124.8, sd 1.9) | 9.6 ms (p95 11.5, sd 0.7) | 38.4 ms (p95 43.5, sd 2.1) |
| wide-running-ops | logs | 150.1 ms (p95 159.3, sd 4.7) | 10.2 ms (p95 11.5, sd 0.8) | 45.9 ms (p95 50.4, sd 2.9) |
| wide-running-ops | exec | 185.8 ms (p95 194.2, sd 3.8) | 62.9 ms (p95 70.2, sd 3.3) | 73.6 ms (p95 84.5, sd 4.3) |
| wide-running-ops | restart | 228.7 ms (p95 249.4, sd 10.6) | 123.9 ms (p95 135.1, sd 6.2) | 150.5 ms (p95 165.7, sd 10.5) |
| config-heavy | config | 112.7 ms (p95 117.8, sd 2.6) | 13.0 ms (p95 15.2, sd 0.8) | 38.3 ms (p95 41.3, sd 1.4) |
| build | build | 351.5 ms (p95 373.5, sd 10.2) | 226.1 ms (p95 233.2, sd 6.3) | 277.2 ms (p95 284.0, sd 6.1) |

## Memory + CPU per command (peak RSS / CPU time, median)

This is the **client-side** cost of running the tool, not engine work. podup is
a static binary talking to the Podman service, so work the engine does is not
charged to it. podman-compose is Python that shells out to the `podman` binary
per call and waits on it, so that work *is* charged to it. docker-compose is a
Go binary talking to a socket, like podup.

| scenario | op | podman-compose | podup | docker-compose |
|---|---|---|---|---|
| single | up | 45.9 MiB / 376.6 ms | 5.7 MiB / 6.2 ms | 29.0 MiB / 29.3 ms |
| single | down | 44.2 MiB / 315.6 ms | 5.6 MiB / 6.6 ms | 28.5 MiB / 27.1 ms |
| multi-healthcheck | up | 46.2 MiB / 560.0 ms | 5.6 MiB / 8.2 ms | 29.3 MiB / 32.9 ms |
| multi-healthcheck | down | 44.8 MiB / 419.1 ms | 5.5 MiB / 7.4 ms | 28.5 MiB / 27.7 ms |
| deep-chain | up | 46.7 MiB / 1.187 s | 5.7 MiB / 0.010 s | 29.2 MiB / 0.037 s |
| deep-chain | down | 45.4 MiB / 799.9 ms | 5.6 MiB / 9.3 ms | 28.9 MiB / 31.4 ms |
| wide-level | up | 47.2 MiB / 6.411 s | 6.5 MiB / 0.032 s | 33.8 MiB / 0.090 s |
| wide-level | down | 45.7 MiB / 4.898 s | 6.0 MiB / 0.024 s | 30.3 MiB / 0.066 s |
| scale | up | 46.0 MiB / 417.0 ms | 5.7 MiB / 8.6 ms | 29.4 MiB / 34.6 ms |
| scale | down | 44.8 MiB / 322.0 ms | 5.5 MiB / 8.4 ms | 29.0 MiB / 29.9 ms |
| network-ipam | up | 46.2 MiB / 529.4 ms | 5.6 MiB / 7.0 ms | 29.1 MiB / 31.8 ms |
| network-ipam | down | 45.0 MiB / 429.4 ms | 5.6 MiB / 7.3 ms | 29.0 MiB / 28.0 ms |
| volume-heavy | up | 46.0 MiB / 875.9 ms | 5.5 MiB / 7.4 ms | 29.3 MiB / 32.2 ms |
| volume-heavy | down | 44.7 MiB / 489.1 ms | 5.6 MiB / 7.6 ms | 29.4 MiB / 32.1 ms |
| secrets | up | 46.0 MiB / 383.0 ms | 5.6 MiB / 8.3 ms | 29.0 MiB / 31.4 ms |
| secrets | down | 45.0 MiB / 320.3 ms | 5.6 MiB / 8.2 ms | 28.4 MiB / 27.4 ms |
| warm-restart | warm up | 44.2 MiB / 219.5 ms | 5.6 MiB / 7.3 ms | 29.7 MiB / 29.9 ms |
| many-services | up | 46.9 MiB / 1.858 s | 6.1 MiB / 0.013 s | 30.4 MiB / 0.048 s |
| many-services | down | 45.3 MiB / 1.395 s | 5.7 MiB / 0.012 s | 28.9 MiB / 0.039 s |
| running-ops | ps | 43.9 MiB / 128.8 ms | 5.0 MiB / 4.4 ms | 28.5 MiB / 23.9 ms |
| running-ops | logs | 63.9 MiB / 137.4 ms | 5.3 MiB / 4.8 ms | 28.6 MiB / 25.6 ms |
| running-ops | exec | 43.7 MiB / 137.5 ms | 5.3 MiB / 5.4 ms | 26.8 MiB / 17.8 ms |
| running-ops | restart | 44.5 MiB / 177.4 ms | 5.4 MiB / 5.2 ms | 28.8 MiB / 25.4 ms |
| wide-running-ops | ps | 45.6 MiB / 137.9 ms | 5.0 MiB / 5.2 ms | 29.1 MiB / 38.3 ms |
| wide-running-ops | logs | 64.5 MiB / 142.3 ms | 5.3 MiB / 5.7 ms | 29.4 MiB / 31.0 ms |
| wide-running-ops | exec | 43.8 MiB / 141.2 ms | 5.3 MiB / 6.4 ms | 27.0 MiB / 18.4 ms |
| wide-running-ops | restart | 44.5 MiB / 176.5 ms | 5.4 MiB / 6.2 ms | 29.5 MiB / 31.5 ms |
| config-heavy | config | 34.4 MiB / 115.4 ms | 5.6 MiB / 13.3 ms | 29.4 MiB / 56.0 ms |
| build | build | 51.8 MiB / 378.0 ms | 5.3 MiB / 5.5 ms | 29.7 MiB / 28.7 ms |

## Reading these numbers honestly

podup is fastest in every row of both tables in this run. **Three of those wins
are not real**, and they are worth naming rather than counting:

| row | podup | best of the others | gap | podup's own sd |
|---|---|---|---|---|
| deep-chain down | 379 ms | 380 ms | 1.5 ms | 29 ms |
| many-services down | 506 ms | 508 ms | 2 ms | 99 ms |
| multi-healthcheck down | 264 ms | 278 ms | 13.5 ms | 15 ms |

Each gap is inside podup's own standard deviation on that row, so those three are
coin tosses that happened to land this way. `many-services down` has now landed
on podup's side in two runs and on docker-compose's in one, always inside the
noise, and nothing about either tool's teardown changed in between. Teardown is
where this benchmark is noisiest: the spread on `many-services down` is 99 ms
against a 2 ms gap.

Five more clear the bar by less than two standard deviations and should be read
as "about the same", not as wins: `network-ipam down` (34 ms gap, sd 19),
`scale down` (23 ms, sd 13), `secrets up` (12 ms, sd 6), `secrets down` (11 ms,
sd 9) and `single down` (16 ms, sd 9).

`running-ops ps` and `config-heavy config` used to publish as **0.000 s** here,
the floor of `/usr/bin/time -v`. The current timer reads them directly, at
**6.3 ms** and **13.0 ms**; about 1.7 ms of each is process start, measured on
`--version`. Rows at that scale move by tens of percent between runs for no
reason of the tool's, which is why the `config` row above got a head-to-head
rather than a reading.

`multi-healthcheck up` is the noisiest row in the suite (sd 89 ms here, 70 ms in
the 3.4.1 run, p95 390 ms against a 298 ms median) and the one most likely to be
misread across releases. It is gated on a `depends_on: service_healthy` poll, so
it measures healthcheck interval granularity more than tool speed. Compare
releases by running them against each other on one machine, never by subtracting
two published tables; the -6.5% the unchanged `docker-compose` moved between
these two is the size of the error that subtraction would carry.

The `secrets` scenario is the one place where a correctness fix costs measurable
time. Six `file:` secrets used to be six read-only bind mounts; since 3.1.0 each
is read and created as a Podman-native secret, because a bind mount is denied
outright on an SELinux host while `up` still reports the container as started.
That is three API calls per secret at `up` and two at `down`, and it puts about
10 ms on each direction of this scenario. The fix is worth the 10 ms.
