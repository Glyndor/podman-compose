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

Measured on podup **3.4.1** built from the release tag, against
podman-compose 1.5.0 and docker-compose 5.1.3 on rootless Podman 5.7.0, 16
cores, CPU governor pinned to `performance`, tools pinned to cores 2-9, host
otherwise idle with no virtual machines running. 1044 timed runs, none failed.

**Do not read this table against the 3.2.1 one as a release comparison.** Three
things changed between the two runs, not one: podup 3.2.1 to 3.4.1, Podman 5.4.2
to 5.7.0, and podman-compose 1.3.0 to 1.5.0. A difference cannot be attributed to
any one of them.

What can be said is bounded by the component that did **not** change.
`docker-compose` is 5.1.3 in both runs, and its median moved **+1.0%** across the
29 comparable rows, so the environment is comparable between them. Against that
reference podup moved **+0.8%** — flat, which is the useful result: 3.4.1 landed
two concurrency changes on the hot path of `up`, and a reader is entitled to ask
whether they cost anything. podman-compose moved +10.4%, but its own version
changed too, so that figure is confounded and is not evidence about the engine.

Two rows deserve a caveat rather than a reading. `multi-healthcheck up` moved
about +55% for podup and +68% for podman-compose; it is gated on a
`depends_on: service_healthy` poll, so it measures healthcheck interval
granularity more than tool speed. And `running-ops ps` at 8.7 ms sits close
enough to process-spawn cost that a few tenths of a millisecond read as double
digits in percent.

## Wall-clock (lower is better)

| scenario | op | podman-compose | podup | docker-compose |
|---|---|---|---|---|
| single | up | 477.5 ms (p95 540.2, sd 24.8) | 99.5 ms (p95 109.9, sd 5.6) | 125.6 ms (p95 132.2, sd 5.6) |
| single | down | 453.3 ms (p95 483.6, sd 15.5) | 138.7 ms (p95 164.9, sd 11.1) | 157.9 ms (p95 170.8, sd 7.7) |
| multi-healthcheck | up | 1.023 s (p95 1.076, sd 0.024) | 0.359 s (p95 0.388, sd 0.070) | 0.716 s (p95 0.733, sd 0.010) |
| multi-healthcheck | down | 594.1 ms (p95 668.2, sd 43.3) | 254.0 ms (p95 309.4, sd 25.8) | 279.7 ms (p95 309.0, sd 16.0) |
| deep-chain | up | 1.839 s (p95 1.963, sd 0.077) | 0.334 s (p95 0.360, sd 0.014) | 0.870 s (p95 0.887, sd 0.014) |
| deep-chain | down | 0.847 s (p95 1.056, sd 0.094) | 0.390 s (p95 0.418, sd 0.009) | 0.404 s (p95 0.429, sd 0.017) |
| wide-level | up | 9.672 s (p95 9.967, sd 0.125) | 1.130 s (p95 1.253, sd 0.048) | 1.571 s (p95 1.746, sd 0.065) |
| wide-level | down | 4.771 s (p95 5.319, sd 0.194) | 1.636 s (p95 1.899, sd 0.144) | 2.169 s (p95 2.874, sd 0.293) |
| scale | up | 470.6 ms (p95 513.4, sd 21.1) | 194.5 ms (p95 212.7, sd 10.1) | 402.2 ms (p95 424.9, sd 13.9) |
| scale | down | 382.6 ms (p95 403.9, sd 11.7) | 270.9 ms (p95 319.1, sd 22.3) | 285.1 ms (p95 348.7, sd 22.0) |
| network-ipam | up | 716.5 ms (p95 747.6, sd 24.2) | 114.9 ms (p95 135.9, sd 6.9) | 150.7 ms (p95 154.4, sd 3.2) |
| network-ipam | down | 569.6 ms (p95 602.8, sd 32.3) | 172.8 ms (p95 203.2, sd 15.0) | 202.6 ms (p95 226.1, sd 13.0) |
| volume-heavy | up | 0.993 s (p95 1.087, sd 0.034) | 0.107 s (p95 0.111, sd 0.004) | 0.139 s (p95 0.183, sd 0.014) |
| volume-heavy | down | 624.8 ms (p95 647.1, sd 12.8) | 144.3 ms (p95 169.3, sd 13.5) | 197.0 ms (p95 227.8, sd 9.8) |
| secrets | up | 542.2 ms (p95 576.6, sd 19.6) | 104.8 ms (p95 113.3, sd 5.4) | 132.2 ms (p95 164.8, sd 11.1) |
| secrets | down | 463.5 ms (p95 514.2, sd 20.9) | 144.2 ms (p95 167.0, sd 14.6) | 164.7 ms (p95 178.0, sd 8.5) |
| warm-restart | warm up | 226.9 ms (p95 254.4, sd 13.2) | 37.9 ms (p95 47.4, sd 4.6) | 63.1 ms (p95 99.9, sd 14.0) |
| many-services | up | 2.925 s (p95 3.090, sd 0.091) | 0.390 s (p95 0.416, sd 0.017) | 0.511 s (p95 0.577, sd 0.023) |
| many-services | down | 1.511 s (p95 1.616, sd 0.040) | 0.552 s (p95 0.693, sd 0.068) | 0.559 s (p95 0.655, sd 0.060) |
| running-ops | ps | 122.6 ms (p95 134.7, sd 5.1) | 8.7 ms (p95 9.3, sd 0.5) | 24.8 ms (p95 49.6, sd 7.6) |
| running-ops | logs | 139.9 ms (p95 147.9, sd 3.5) | 22.4 ms (p95 28.3, sd 2.8) | 37.7 ms (p95 53.7, sd 5.1) |
| running-ops | exec | 184.9 ms (p95 192.9, sd 3.4) | 60.0 ms (p95 69.0, sd 3.8) | 72.6 ms (p95 84.1, sd 4.5) |
| running-ops | restart | 273.7 ms (p95 309.3, sd 15.7) | 147.2 ms (p95 165.5, sd 8.0) | 181.7 ms (p95 198.9, sd 10.6) |
| wide-running-ops | ps | 131.8 ms (p95 139.1, sd 4.1) | 12.3 ms (p95 13.8, sd 0.8) | 42.9 ms (p95 85.8, sd 13.1) |
| wide-running-ops | logs | 142.4 ms (p95 156.9, sd 5.4) | 22.5 ms (p95 27.7, sd 2.0) | 41.9 ms (p95 74.2, sd 10.9) |
| wide-running-ops | exec | 191.2 ms (p95 199.6, sd 3.8) | 63.3 ms (p95 75.1, sd 5.1) | 71.9 ms (p95 81.4, sd 4.2) |
| wide-running-ops | restart | 242.5 ms (p95 258.7, sd 10.9) | 114.8 ms (p95 133.8, sd 8.4) | 147.9 ms (p95 165.0, sd 9.7) |
| config-heavy | config | 108.1 ms (p95 114.0, sd 2.4) | 9.4 ms (p95 9.8, sd 0.2) | 48.8 ms (p95 53.8, sd 3.3) |
| build | build | 377.9 ms (p95 407.1, sd 16.6) | 239.2 ms (p95 260.2, sd 9.6) | 318.1 ms (p95 441.6, sd 45.0) |

## Memory + CPU per command (peak RSS / CPU time, median)

This is the **client-side** cost of running the tool, not engine work. podup is
a static binary talking to the Podman service, so work the engine does is not
charged to it. podman-compose is Python that shells out to the `podman` binary
per call and waits on it, so that work *is* charged to it. docker-compose is a
Go binary talking to a socket, like podup.

| scenario | op | podman-compose | podup | docker-compose |
|---|---|---|---|---|
| single | up | 52.9 MiB / 469.7 ms | 8.1 MiB / 6.0 ms | 29.8 MiB / 30.4 ms |
| single | down | 51.4 MiB / 393.1 ms | 7.9 MiB / 5.9 ms | 29.3 MiB / 26.5 ms |
| multi-healthcheck | up | 52.8 MiB / 698.0 ms | 8.2 MiB / 6.9 ms | 30.1 MiB / 32.5 ms |
| multi-healthcheck | down | 51.7 MiB / 509.5 ms | 7.9 MiB / 6.2 ms | 29.4 MiB / 27.7 ms |
| deep-chain | up | 53.1 MiB / 1.386 s | 8.2 MiB / 0.008 s | 30.0 MiB / 0.037 s |
| deep-chain | down | 51.8 MiB / 866.4 ms | 7.9 MiB / 7.5 ms | 29.5 MiB / 31.4 ms |
| wide-level | up | 53.9 MiB / 7.659 s | 8.6 MiB / 0.025 s | 34.5 MiB / 0.092 s |
| wide-level | down | 52.0 MiB / 5.541 s | 8.1 MiB / 0.020 s | 31.2 MiB / 0.067 s |
| scale | up | 52.6 MiB / 503.4 ms | 8.1 MiB / 7.5 ms | 29.7 MiB / 36.7 ms |
| scale | down | 51.4 MiB / 384.5 ms | 7.7 MiB / 6.8 ms | 29.7 MiB / 31.8 ms |
| network-ipam | up | 53.0 MiB / 648.5 ms | 8.2 MiB / 6.2 ms | 30.0 MiB / 31.7 ms |
| network-ipam | down | 51.5 MiB / 506.3 ms | 7.9 MiB / 6.2 ms | 29.6 MiB / 28.4 ms |
| volume-heavy | up | 52.8 MiB / 1.155 s | 8.2 MiB / 0.006 s | 29.9 MiB / 0.033 s |
| volume-heavy | down | 51.6 MiB / 617.8 ms | 8.0 MiB / 6.6 ms | 30.2 MiB / 31.6 ms |
| secrets | up | 52.6 MiB / 477.6 ms | 8.2 MiB / 7.3 ms | 29.6 MiB / 32.8 ms |
| secrets | down | 51.6 MiB / 394.1 ms | 7.9 MiB / 6.3 ms | 29.4 MiB / 28.2 ms |
| warm-restart | warm up | 50.0 MiB / 261.9 ms | 8.1 MiB / 5.8 ms | 30.0 MiB / 33.8 ms |
| many-services | up | 53.7 MiB / 2.368 s | 8.3 MiB / 0.011 s | 31.0 MiB / 0.049 s |
| many-services | down | 51.9 MiB / 1.705 s | 7.9 MiB / 0.009 s | 29.9 MiB / 0.039 s |
| running-ops | ps | 50.4 MiB / 140.4 ms | 7.8 MiB / 4.1 ms | 29.8 MiB / 24.9 ms |
| running-ops | logs | 68.6 MiB / 140.1 ms | 7.9 MiB / 4.3 ms | 29.3 MiB / 25.6 ms |
| running-ops | exec | 48.7 MiB / 139.2 ms | 7.9 MiB / 4.9 ms | 27.8 MiB / 17.8 ms |
| running-ops | restart | 49.4 MiB / 181.7 ms | 8.0 MiB / 4.6 ms | 29.5 MiB / 25.8 ms |
| wide-running-ops | ps | 51.3 MiB / 150.1 ms | 7.7 MiB / 4.6 ms | 30.3 MiB / 39.7 ms |
| wide-running-ops | logs | 68.5 MiB / 144.3 ms | 8.0 MiB / 4.6 ms | 29.6 MiB / 30.6 ms |
| wide-running-ops | exec | 48.8 MiB / 145.1 ms | 8.0 MiB / 5.2 ms | 27.9 MiB / 18.0 ms |
| wide-running-ops | restart | 49.6 MiB / 182.0 ms | 8.0 MiB / 5.0 ms | 29.6 MiB / 30.9 ms |
| config-heavy | config | 34.9 MiB / 110.3 ms | 7.9 MiB / 7.6 ms | 30.4 MiB / 61.3 ms |
| build | build | 65.5 MiB / 410.5 ms | 8.1 MiB / 6.0 ms | 30.5 MiB / 29.3 ms |

## Reading these numbers honestly

podup is fastest in every row of both tables in this run. **Three of those wins
are not real**, and they are worth naming rather than counting:

| row | podup | best of the others | gap | podup's own sd |
|---|---|---|---|---|
| deep-chain down | 395 ms | 400 ms | 5 ms | 9 ms |
| network-ipam down | 182 ms | 195 ms | 12 ms | 97 ms |
| many-services down | 551 ms | 593 ms | 42 ms | 49 ms |

Each gap is inside podup's own standard deviation on that row, so those three are
coin tosses that happened to land this way. `many-services down` landed the other
way in the 3.0.1 run — docker-compose ahead by 15 ms, also inside the noise — and
nothing about either tool changed in between. Teardown is where this benchmark is
noisiest, and a row that flips between runs is telling you the spread, not the
winner. `network-ipam down` is the extreme case: podup's spread on it is eight
times the gap it won by.

Two more are close enough to name: `scale down` (25 ms gap, sd 17 ms) and
`running-ops restart` (11 ms gap, sd 7 ms) clear the bar by less than two standard
deviations. Read them as "about the same", not as wins.

`running-ops ps` and `config-heavy config` used to publish as **0.000 s** here.
That was the floor of `/usr/bin/time -v`, which resolves to 10 ms, and this table
had to explain that separately-timed runs put them at 7.7 ms and 8.3 ms. The
current instrument reads them directly, at **7.1 ms** and **9.0 ms** — close
enough to those hand-timed figures to be a useful check on the new timer. About
2.0 ms of each is process start: the binary spawning, building its command tree
and its async runtime, before any work.

The same floor sat under the whole CPU column, which reported podup at `0.000 s`
in all 29 rows. It now reads between 4.2 ms and 33 ms.

`multi-healthcheck up` is the noisiest row in the suite (sd 84 ms here, 87 ms in
the 3.0.1 run, and its p95 is 411 ms against a 231 ms median) and the one most
likely to be misread across releases. Run head-to-head on the same machine,
alternating binaries per iteration, 3.2.0 came out at 295 ms against 3.0.1's
350 ms, while the published tables for those two releases sat 75 ms apart in the
opposite direction. Compare releases by running them against each other, never by
subtracting two published tables.

What that row does show, against 2.0.0's 1.275 s, is a bug fix rather than a
scheduling trick: podup used to read a container's health status once per
healthcheck `interval` and now reads it every 150 ms between runs, so a container
that turns healthy just after a probe is noticed at once instead of at the end of
the window.

The `secrets` scenario is new here, and it is the one place where a correctness
fix cost measurable time. Six `file:` secrets used to be six read-only bind
mounts; since 3.1.0 each is read and created as a Podman-native secret, because a
bind mount is denied outright on an SELinux host while `up` still reports the
container as started. That is three API calls per secret at `up` and two at
`down`, and it puts about 10 ms on each direction of this scenario. The fix is
worth the 10 ms.
