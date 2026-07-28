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

Measured on podup **3.2.1** installed from apt — the same binary a user gets,
not a local build — against podman-compose 1.3.0 and docker-compose 5.1.3 on
rootless Podman 5.4.2, 16 cores, with the tools pinned to cores 2-9. The host
was otherwise idle this time, unlike the 3.2.0 run.

**Do not read this table against the 3.2.0 one as a release comparison.** Two
things changed between them, the instrument and the version, so a difference
cannot be attributed to either. Compare releases by running them head to head,
as described below.

## Wall-clock (lower is better)

| scenario | op | podman-compose | podup | docker-compose |
|---|---|---|---|---|
| single | up | 389.9 ms (p95 416.0, sd 8.8) | 96.0 ms (p95 99.1, sd 3.6) | 119.7 ms (p95 128.2, sd 6.1) |
| single | down | 384.5 ms (p95 407.2, sd 11.3) | 134.0 ms (p95 152.1, sd 11.0) | 174.3 ms (p95 271.8, sd 34.5) |
| multi-healthcheck | up | 609.9 ms (p95 647.7, sd 16.3) | 231.4 ms (p95 411.0, sd 84.1) | 769.4 ms (p95 818.0, sd 41.9) |
| multi-healthcheck | down | 507.7 ms (p95 549.8, sd 18.5) | 242.0 ms (p95 315.4, sd 30.9) | 322.1 ms (p95 441.8, sd 51.9) |
| deep-chain | up | 1.220 s (p95 1.553, sd 0.122) | 0.393 s (p95 0.448, sd 0.021) | 0.872 s (p95 0.904, sd 0.017) |
| deep-chain | down | 0.738 s (p95 1.082, sd 0.146) | 0.395 s (p95 0.412, sd 0.009) | 0.400 s (p95 0.454, sd 0.027) |
| wide-level | up | 6.978 s (p95 7.035, sd 0.054) | 1.098 s (p95 1.144, sd 0.023) | 1.611 s (p95 1.871, sd 0.112) |
| wide-level | down | 4.505 s (p95 4.932, sd 0.405) | 1.794 s (p95 2.224, sd 0.196) | 2.440 s (p95 3.356, sd 0.501) |
| scale | up | 393.9 ms (p95 452.2, sd 23.1) | 197.5 ms (p95 212.0, sd 10.3) | 400.1 ms (p95 413.7, sd 9.9) |
| scale | down | 398.4 ms (p95 448.9, sd 21.8) | 269.9 ms (p95 312.0, sd 17.3) | 295.3 ms (p95 362.4, sd 23.0) |
| network-ipam | up | 545.5 ms (p95 557.9, sd 7.2) | 113.6 ms (p95 125.4, sd 6.0) | 142.2 ms (p95 154.7, sd 8.3) |
| network-ipam | down | 470.3 ms (p95 478.3, sd 6.1) | 182.4 ms (p95 424.2, sd 96.5) | 194.5 ms (p95 220.4, sd 12.5) |
| volume-heavy | up | 879.2 ms (p95 973.8, sd 33.1) | 108.9 ms (p95 117.9, sd 6.6) | 131.0 ms (p95 140.7, sd 5.8) |
| volume-heavy | down | 565.8 ms (p95 584.0, sd 9.8) | 148.1 ms (p95 169.6, sd 9.2) | 200.9 ms (p95 223.0, sd 12.0) |
| secrets | up | 425.4 ms (p95 465.4, sd 13.8) | 104.0 ms (p95 116.1, sd 5.4) | 122.3 ms (p95 140.2, sd 7.4) |
| secrets | down | 408.9 ms (p95 417.2, sd 9.3) | 140.0 ms (p95 162.1, sd 11.1) | 161.7 ms (p95 202.3, sd 15.6) |
| warm-restart | warm up | 343.1 ms (p95 396.7, sd 20.0) | 36.7 ms (p95 44.4, sd 4.0) | 50.6 ms (p95 59.8, sd 4.1) |
| many-services | up | 2.214 s (p95 2.300, sd 0.048) | 0.391 s (p95 0.424, sd 0.015) | 0.499 s (p95 0.530, sd 0.014) |
| many-services | down | 1.370 s (p95 1.445, sd 0.065) | 0.551 s (p95 0.610, sd 0.049) | 0.593 s (p95 0.665, sd 0.072) |
| running-ops | ps | 125.2 ms (p95 139.8, sd 5.1) | 7.1 ms (p95 8.3, sd 0.5) | 24.0 ms (p95 35.2, sd 3.5) |
| running-ops | logs | 142.8 ms (p95 165.8, sd 8.9) | 20.1 ms (p95 23.0, sd 1.8) | 37.6 ms (p95 44.2, sd 3.3) |
| running-ops | exec | 196.8 ms (p95 239.5, sd 14.3) | 60.8 ms (p95 72.6, sd 5.3) | 75.1 ms (p95 79.2, sd 3.8) |
| running-ops | restart | 287.0 ms (p95 317.7, sd 10.9) | 166.8 ms (p95 177.9, sd 7.4) | 177.5 ms (p95 200.8, sd 8.7) |
| wide-running-ops | ps | 133.6 ms (p95 148.0, sd 6.2) | 11.1 ms (p95 11.7, sd 0.3) | 40.1 ms (p95 58.5, sd 6.1) |
| wide-running-ops | logs | 150.1 ms (p95 164.0, sd 6.6) | 19.8 ms (p95 21.4, sd 1.1) | 37.4 ms (p95 59.2, sd 6.8) |
| wide-running-ops | exec | 198.3 ms (p95 208.0, sd 5.3) | 62.9 ms (p95 70.4, sd 5.0) | 77.4 ms (p95 91.2, sd 5.8) |
| wide-running-ops | restart | 245.4 ms (p95 261.1, sd 12.5) | 117.6 ms (p95 128.0, sd 6.3) | 147.4 ms (p95 168.0, sd 9.8) |
| config-heavy | config | 117.5 ms (p95 126.4, sd 4.5) | 9.0 ms (p95 9.4, sd 0.2) | 46.3 ms (p95 47.7, sd 1.0) |
| build | build | 352.7 ms (p95 379.9, sd 15.8) | 215.0 ms (p95 235.9, sd 8.8) | 286.0 ms (p95 322.3, sd 13.9) |

## Memory + CPU per command (peak RSS / CPU time, median)

This is the **client-side** cost of running the tool, not engine work. podup is
a static binary talking to the Podman service, so work the engine does is not
charged to it. podman-compose is Python that shells out to the `podman` binary
per call and waits on it, so that work *is* charged to it. docker-compose is a
Go binary talking to a socket, like podup.

| scenario | op | podman-compose | podup | docker-compose |
|---|---|---|---|---|
| single | up | 53.2 MiB / 431.7 ms | 7.7 MiB / 5.5 ms | 28.7 MiB / 29.4 ms |
| single | down | 50.7 MiB / 361.6 ms | 7.5 MiB / 5.5 ms | 28.6 MiB / 27.5 ms |
| multi-healthcheck | up | 53.8 MiB / 631.3 ms | 7.7 MiB / 6.9 ms | 29.4 MiB / 43.4 ms |
| multi-healthcheck | down | 50.8 MiB / 475.3 ms | 7.5 MiB / 5.9 ms | 28.5 MiB / 35.9 ms |
| deep-chain | up | 54.0 MiB / 1.237 s | 7.8 MiB / 0.009 s | 29.1 MiB / 0.039 s |
| deep-chain | down | 51.1 MiB / 820.4 ms | 7.6 MiB / 7.2 ms | 28.5 MiB / 33.0 ms |
| wide-level | up | 54.3 MiB / 6.782 s | 8.5 MiB / 0.033 s | 33.6 MiB / 0.093 s |
| wide-level | down | 51.6 MiB / 5.279 s | 7.8 MiB / 0.021 s | 30.2 MiB / 0.068 s |
| scale | up | 53.3 MiB / 432.4 ms | 7.9 MiB / 7.4 ms | 29.6 MiB / 36.1 ms |
| scale | down | 50.9 MiB / 366.8 ms | 7.4 MiB / 6.8 ms | 28.6 MiB / 30.3 ms |
| network-ipam | up | 53.6 MiB / 581.1 ms | 7.8 MiB / 6.2 ms | 29.2 MiB / 31.0 ms |
| network-ipam | down | 50.8 MiB / 465.3 ms | 7.5 MiB / 5.9 ms | 28.6 MiB / 27.8 ms |
| volume-heavy | up | 53.5 MiB / 1.047 s | 7.7 MiB / 0.006 s | 28.9 MiB / 0.032 s |
| volume-heavy | down | 51.0 MiB / 564.1 ms | 7.4 MiB / 6.3 ms | 29.1 MiB / 32.1 ms |
| secrets | up | 53.1 MiB / 438.0 ms | 7.7 MiB / 8.0 ms | 28.8 MiB / 31.9 ms |
| secrets | down | 50.7 MiB / 364.9 ms | 7.6 MiB / 6.6 ms | 28.6 MiB / 28.8 ms |
| warm-restart | warm up | 51.6 MiB / 423.2 ms | 7.8 MiB / 6.2 ms | 29.2 MiB / 31.3 ms |
| many-services | up | 53.9 MiB / 2.201 s | 7.9 MiB / 0.012 s | 30.1 MiB / 0.049 s |
| many-services | down | 50.8 MiB / 1.713 s | 7.7 MiB / 0.009 s | 28.8 MiB / 0.039 s |
| running-ops | ps | 49.6 MiB / 142.3 ms | 7.3 MiB / 4.2 ms | 28.7 MiB / 24.3 ms |
| running-ops | logs | 67.5 MiB / 143.7 ms | 7.4 MiB / 4.4 ms | 28.2 MiB / 26.5 ms |
| running-ops | exec | 48.9 MiB / 152.0 ms | 7.7 MiB / 4.7 ms | 26.8 MiB / 17.8 ms |
| running-ops | restart | 49.3 MiB / 191.1 ms | 7.5 MiB / 4.6 ms | 28.4 MiB / 26.0 ms |
| wide-running-ops | ps | 50.6 MiB / 154.0 ms | 7.3 MiB / 4.7 ms | 29.2 MiB / 38.0 ms |
| wide-running-ops | logs | 67.6 MiB / 148.6 ms | 7.5 MiB / 4.6 ms | 28.6 MiB / 30.7 ms |
| wide-running-ops | exec | 48.9 MiB / 153.0 ms | 7.5 MiB / 5.0 ms | 26.8 MiB / 18.4 ms |
| wide-running-ops | restart | 49.5 MiB / 190.5 ms | 7.4 MiB / 4.8 ms | 28.6 MiB / 33.0 ms |
| config-heavy | config | 37.6 MiB / 120.5 ms | 7.4 MiB / 7.8 ms | 29.0 MiB / 60.8 ms |
| build | build | 62.8 MiB / 386.4 ms | 7.6 MiB / 5.6 ms | 29.4 MiB / 29.8 ms |

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
