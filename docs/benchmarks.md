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

Measured on podup **3.2.0** installed from apt — the same binary a user gets,
not a local build — against podman-compose 1.3.0 and docker-compose 5.1.3 on
rootless Podman 5.4.2, 16 cores. Six unrelated containers of my own were running
on the host throughout, for every tool alike; that is a fair comparison but not an
idle machine, and it is part of why the teardown rows spread the way they do.

## Wall-clock (seconds, lower is better)

| scenario | op | podman-compose | podup | docker-compose |
|---|---|---|---|---|
| single | up | 0.380 (p95 0.430, sd 0.017) | 0.090 (p95 0.100, sd 0.006) | 0.110 (p95 0.120, sd 0.006) |
| single | down | 0.360 (p95 0.440, sd 0.025) | 0.130 (p95 0.150, sd 0.010) | 0.150 (p95 0.170, sd 0.010) |
| multi-healthcheck | up | 0.600 (p95 0.690, sd 0.033) | 0.355 (p95 0.370, sd 0.072) | 0.695 (p95 0.720, sd 0.010) |
| multi-healthcheck | down | 0.500 (p95 0.530, sd 0.016) | 0.260 (p95 0.300, sd 0.028) | 0.270 (p95 0.290, sd 0.011) |
| deep-chain | up | 1.180 (p95 1.190, sd 0.009) | 0.380 (p95 0.420, sd 0.019) | 0.840 (p95 0.860, sd 0.011) |
| deep-chain | down | 0.690 (p95 0.770, sd 0.033) | 0.380 (p95 0.400, sd 0.010) | 0.390 (p95 0.410, sd 0.013) |
| wide-level | up | 6.745 (p95 6.820, sd 0.045) | 1.040 (p95 1.100, sd 0.033) | 1.500 (p95 1.710, sd 0.081) |
| wide-level | down | 4.500 (p95 5.210, sd 0.399) | 1.900 (p95 2.490, sd 0.251) | 2.165 (p95 2.640, sd 0.284) |
| scale | up | 0.400 (p95 0.420, sd 0.008) | 0.180 (p95 0.190, sd 0.006) | 0.380 (p95 0.390, sd 0.011) |
| scale | down | 0.390 (p95 0.470, sd 0.025) | 0.250 (p95 0.280, sd 0.012) | 0.295 (p95 0.320, sd 0.018) |
| network-ipam | up | 0.560 (p95 0.620, sd 0.027) | 0.100 (p95 0.110, sd 0.005) | 0.130 (p95 0.150, sd 0.008) |
| network-ipam | down | 0.475 (p95 0.510, sd 0.020) | 0.170 (p95 0.180, sd 0.011) | 0.195 (p95 0.220, sd 0.012) |
| volume-heavy | up | 0.850 (p95 0.920, sd 0.023) | 0.105 (p95 0.170, sd 0.021) | 0.130 (p95 0.130, sd 0.005) |
| volume-heavy | down | 0.555 (p95 0.600, sd 0.017) | 0.150 (p95 0.160, sd 0.010) | 0.195 (p95 0.210, sd 0.012) |
| secrets | up | 0.390 (p95 0.520, sd 0.041) | 0.100 (p95 0.100, sd 0.005) | 0.110 (p95 0.120, sd 0.006) |
| secrets | down | 0.375 (p95 0.390, sd 0.009) | 0.135 (p95 0.150, sd 0.007) | 0.155 (p95 0.160, sd 0.008) |
| warm-restart | warm up | 0.330 (p95 0.370, sd 0.014) | 0.030 (p95 0.040, sd 0.006) | 0.040 (p95 0.050, sd 0.004) |
| many-services | up | 2.090 (p95 2.110, sd 0.021) | 0.375 (p95 0.410, sd 0.023) | 0.475 (p95 0.520, sd 0.017) |
| many-services | down | 1.235 (p95 1.600, sd 0.135) | 0.495 (p95 0.880, sd 0.119) | 0.565 (p95 0.670, sd 0.050) |
| running-ops | ps | 0.120 (p95 0.120, sd 0.005) | 0.000 (p95 0.010, sd 0.003) | 0.020 (p95 0.020, sd 0.000) |
| running-ops | logs | 0.140 (p95 0.140, sd 0.003) | 0.020 (p95 0.030, sd 0.005) | 0.030 (p95 0.040, sd 0.004) |
| running-ops | exec | 0.180 (p95 0.190, sd 0.004) | 0.060 (p95 0.070, sd 0.004) | 0.070 (p95 0.070, sd 0.003) |
| running-ops | restart | 0.280 (p95 0.300, sd 0.009) | 0.165 (p95 0.180, sd 0.010) | 0.180 (p95 0.200, sd 0.009) |
| wide-running-ops | ps | 0.120 (p95 0.130, sd 0.005) | 0.010 (p95 0.010, sd 0.003) | 0.040 (p95 0.040, sd 0.004) |
| wide-running-ops | logs | 0.150 (p95 0.170, sd 0.009) | 0.020 (p95 0.030, sd 0.003) | 0.035 (p95 0.040, sd 0.005) |
| wide-running-ops | exec | 0.195 (p95 0.210, sd 0.008) | 0.060 (p95 0.080, sd 0.006) | 0.070 (p95 0.080, sd 0.003) |
| wide-running-ops | restart | 0.240 (p95 0.260, sd 0.009) | 0.115 (p95 0.140, sd 0.012) | 0.140 (p95 0.150, sd 0.006) |
| config-heavy | config | 0.110 (p95 0.110, sd 0.000) | 0.000 (p95 0.010, sd 0.003) | 0.040 (p95 0.040, sd 0.005) |
| build | build | 0.355 (p95 0.370, sd 0.010) | 0.205 (p95 0.220, sd 0.007) | 0.265 (p95 0.270, sd 0.007) |

## Memory + CPU per command (peak RSS / CPU time, median)

This is the **client-side** cost of running the tool, not engine work. podup is
a static binary talking to the Podman service, so work the engine does is not
charged to it. podman-compose is Python that shells out to the `podman` binary
per call and waits on it, so that work *is* charged to it. docker-compose is a
Go binary talking to a socket, like podup.

| scenario | op | podman-compose | podup | docker-compose |
|---|---|---|---|---|
| single | up | 52.5 MiB / 0.450 s | 7.7 MiB / 0.000 s | 28.4 MiB / 0.020 s |
| single | down | 49.8 MiB / 0.370 s | 7.4 MiB / 0.000 s | 28.1 MiB / 0.020 s |
| multi-healthcheck | up | 53.0 MiB / 0.665 s | 7.7 MiB / 0.000 s | 28.8 MiB / 0.030 s |
| multi-healthcheck | down | 50.2 MiB / 0.490 s | 7.4 MiB / 0.000 s | 28.2 MiB / 0.020 s |
| deep-chain | up | 53.1 MiB / 1.300 s | 7.7 MiB / 0.000 s | 28.7 MiB / 0.030 s |
| deep-chain | down | 50.2 MiB / 0.850 s | 7.4 MiB / 0.000 s | 28.6 MiB / 0.020 s |
| wide-level | up | 53.3 MiB / 7.070 s | 8.1 MiB / 0.020 s | 34.6 MiB / 0.090 s |
| wide-level | down | 50.7 MiB / 5.800 s | 7.5 MiB / 0.010 s | 31.4 MiB / 0.065 s |
| scale | up | 52.1 MiB / 0.445 s | 7.6 MiB / 0.000 s | 28.7 MiB / 0.030 s |
| scale | down | 50.0 MiB / 0.370 s | 7.3 MiB / 0.000 s | 28.5 MiB / 0.020 s |
| network-ipam | up | 52.5 MiB / 0.610 s | 7.7 MiB / 0.000 s | 28.6 MiB / 0.025 s |
| network-ipam | down | 50.1 MiB / 0.480 s | 7.4 MiB / 0.000 s | 28.1 MiB / 0.020 s |
| volume-heavy | up | 52.4 MiB / 1.085 s | 7.6 MiB / 0.000 s | 28.5 MiB / 0.020 s |
| volume-heavy | down | 49.8 MiB / 0.585 s | 7.4 MiB / 0.000 s | 28.7 MiB / 0.020 s |
| secrets | up | 52.3 MiB / 0.450 s | 7.6 MiB / 0.000 s | 28.5 MiB / 0.020 s |
| secrets | down | 49.8 MiB / 0.365 s | 7.4 MiB / 0.000 s | 28.3 MiB / 0.020 s |
| warm-restart | warm up | 50.8 MiB / 0.440 s | 7.5 MiB / 0.000 s | 28.9 MiB / 0.020 s |
| many-services | up | 53.1 MiB / 2.195 s | 7.8 MiB / 0.000 s | 30.5 MiB / 0.040 s |
| many-services | down | 50.2 MiB / 1.720 s | 7.4 MiB / 0.000 s | 29.3 MiB / 0.030 s |
| running-ops | ps | 48.6 MiB / 0.130 s | 7.2 MiB / 0.000 s | 28.4 MiB / 0.010 s |
| running-ops | logs | 66.4 MiB / 0.130 s | 7.4 MiB / 0.000 s | 28.3 MiB / 0.015 s |
| running-ops | exec | 47.9 MiB / 0.140 s | 7.5 MiB / 0.000 s | 26.8 MiB / 0.010 s |
| running-ops | restart | 48.7 MiB / 0.180 s | 7.4 MiB / 0.000 s | 28.3 MiB / 0.010 s |
| wide-running-ops | ps | 49.8 MiB / 0.140 s | 7.1 MiB / 0.000 s | 29.3 MiB / 0.030 s |
| wide-running-ops | logs | 66.1 MiB / 0.140 s | 7.4 MiB / 0.000 s | 28.8 MiB / 0.020 s |
| wide-running-ops | exec | 48.3 MiB / 0.140 s | 7.5 MiB / 0.000 s | 27.0 MiB / 0.000 s |
| wide-running-ops | restart | 48.6 MiB / 0.180 s | 7.2 MiB / 0.000 s | 28.5 MiB / 0.020 s |
| config-heavy | config | 38.0 MiB / 0.110 s | 7.2 MiB / 0.000 s | 28.9 MiB / 0.050 s |
| build | build | 58.8 MiB / 0.390 s | 7.5 MiB / 0.000 s | 29.1 MiB / 0.020 s |

## Reading these numbers honestly

podup is fastest in every row of both tables in this run. **Three of those wins
are not real**, and they are worth naming rather than counting:

| row | podup | best of the others | gap | podup's own sd |
|---|---|---|---|---|
| multi-healthcheck down | 0.260 | 0.270 | 0.010 | 0.028 |
| deep-chain down | 0.380 | 0.390 | 0.010 | 0.010 |
| many-services down | 0.495 | 0.565 | 0.070 | 0.119 |

Each gap is inside podup's own standard deviation on that row, so those three are
coin tosses that happened to land this way. `many-services down` landed the other
way in the 3.0.1 run — docker-compose ahead by 0.015 s, also inside the noise —
and nothing about either tool changed in between. Teardown is where this benchmark
is noisiest, and a row that flips between runs is telling you the spread, not the
winner.

`running-ops ps` and `config-heavy config` show podup at **0.000s**. That is the
floor of `/usr/bin/time -v`, which resolves to 10 ms. Timed separately over 50
invocations, `ps` against a project that does not exist takes 7.7 ms and `config`
on this two-file scenario 8.3 ms, and 2.0 ms of that is process start — the binary
spawning, building its command tree and its async runtime, before any work. It is
not zero, it is below what the instrument can see.

`multi-healthcheck up` is the noisiest row in the suite (sd 0.072 here, 0.087 in
the 3.0.1 run) and the one most likely to be misread across releases: it reads
0.355 s here against 0.280 s for 3.0.1, which looks like a regression and is not
one. Run head-to-head on the same machine, alternating binaries per iteration,
3.2.0 came out at 0.295 s against 3.0.1's 0.350 s — the two numbers in this table
are further apart than the two binaries are. Compare releases by running them
against each other, never by subtracting two published tables.

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
