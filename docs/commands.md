# Command reference

This page lists every `podup` command, its options, and what it does. Run
`podup <command> --help` for the same information at the terminal. The
[global options](#global-options) below apply to every command.

```
podup [GLOBAL OPTIONS] <COMMAND> [COMMAND OPTIONS] [SERVICE...]
```

## Global options

These appear before the subcommand and may also come from the environment.

| Flag | Env | Description |
|---|---|---|
| `-f, --file <PATH>` | `COMPOSE_FILE` | Compose file. Repeatable; later files merge over earlier ones. When unset, the compose-spec precedence list is probed: `compose.yaml`, `compose.yml`, `docker-compose.yaml`, `docker-compose.yml`. |
| `-p, --project <NAME>` | `COMPOSE_PROJECT_NAME` | Project name, prefixing container/network/volume names. When unset: the top-level `name:`, then the sanitized project-directory basename. |
| `--socket <PATH>` | `PODMAN_SOCKET` | Podman socket path; overrides auto-detection. |
| `--connection-pool-size <N>` | `PODUP_LIBPOD_POOL` | Maximum HTTP/1.1 connections the libpod client keeps open to the Podman socket for reuse. Streaming calls each take a dedicated connection outside this cap. Default: 8. The earlier spelling `PODUP_LIBCOD_POOL` (a typo of `LIBPOD`) is read as a fallback when the new name is unset, so an existing script that exports the old name keeps working. |
| `--profile <NAMES>` | `COMPOSE_PROFILES` | Active profiles, comma-separated. |

| `--project-directory <PATH>` | | Base directory for relative paths (env_file, build context, bind mounts, config/secret sources). Defaults to the compose file's directory. |
| `--ansi <WHEN>` | | Colour output: `auto`, `always` or `never`. `always` forces colour even into a pipe or file. | 
| `--env-file <PATH>` | | Env file(s) for interpolation. Repeatable; later files win. **Replaces** a project `.env` rather than adding to it: when this is given, `.env` is not read. The process environment still takes precedence over both. |
| `--no-warn` | | Suppress the host-binding / privilege-escalation warnings the engine emits during `up`/`create`/`run`/`exec` (e.g. `network_mode: host`, `privileged: true`, `pid: host`, `container:<id>` namespace sharing). Operators who wrote the compose file deliberately use this to silence the per-run warning. `podup config` still surfaces the active modes at default log level, since that command is the "show me what will happen" path, where the warning is the whole point. | off |

**Profiles activate their dependencies.** A service left out by profile
filtering is still started when a service that *is* running declares
`depends_on` on it, transitively, so a retained service never points at a
dropped one. docker compose rejects that file instead. Give the dependant the
same profile if you want neither to start. See
[docker-migration.md](docker-migration.md#a-depends_on-target-behind-an-inactive-profile).

**Identity colours.** Each service gets its own colour, so a name is
recognisable across `ps`, `logs`, `images`, `stats` and the progress lines.
Twenty colours are available, assigned in sorted order, so no two services in
a project share one until the twenty-first. Each was picked to stay readable
on a light terminal and a dark one alike, and to sit away from the red, green
and yellow that carry status meaning, so a service name never reads as a
state.

The wide palette needs a terminal that announces 256-colour support through
`COLORTERM` or `TERM` (or Windows, where virtual-terminal processing is
enabled directly). Where it does not, podup falls back to six basic ANSI
colours (cyan, magenta, blue and their bright variants), which render
everywhere but cannot fully clear that bar: of the sixteen standard ANSI
colours, only cyan and bright magenta stay readable on both a light and a dark
background, and four of the fallback's own six fall short (magenta and blue
against black, bright cyan against white, bright blue against black). ANSI-16
colours have no fixed RGB (a terminal theme picks its own), so these figures,
like the wide palette's, are relative to the reference palette this branch's
tests pin (`internal/ui/palette_tests.rs`, the standard VGA 16-colour set),
not a universal guarantee. The fallback stays anyway, on the view that
distinguishing six services imperfectly beats distinguishing two well, and any
terminal from the last decade qualifies for the wide palette instead. Both
palettes obey `--ansi`, `NO_COLOR` and TTY detection: if colour is off,
neither is emitted.

Colour elsewhere carries meaning rather than identity, and each family is used
for one thing only: green for something that now exists or is healthy, red for
something gone or failed, yellow for something stopped that survives, dim for a
default or unremarkable answer. That is why `volumes` marks an external volume
yellow rather than green (it is not healthy or unhealthy, it is the one podup
will not delete), and why `autostart status` reports an uninstalled unit dim
throughout instead of colouring systemd's `not-found` red.

## Defaults

Defaults podup applies when a compose file does not specify them. Override
each with the compose-spec field shown.

| Compose key | Default | Notes |
|---|---|---|
| `logging` | `driver: k8s-file` + `max-size: 10m` + `max-file: 5` | Rotation policy on every container podup creates. Without an explicit `logging:` block, libpod logs would grow unbounded and eventually fill the host. To delegate rotation to journald instead: `logging: { driver: journald }`. To opt out of rotation: `logging: { driver: k8s-file, options: { max-file: "0" } }`. The same default is applied by `generate quadlet`, so a generated unit behaves the same as an `up`-managed container. |

## Lifecycle

### `up`
Create and start all services (or only the named ones, plus their transitive
`depends_on`). Accepts a trailing service list.

A container that already exists is left in place when two facts both hold:
its recorded config hash equals what the compose file renders now, and the
image it is bound to is still the image its service resolves to. Either
changing replaces it: an edited service definition, a rebuilt or re-pulled
image, or a tag moved by `podman tag`. The image comparison is what a
`build:` service relies on, since the config hash does not cover the build
context, and it is the same rule docker compose applies. `--no-recreate`
keeps an existing container regardless; `--force-recreate` replaces it
regardless.

| Flag | Description | Default |
|---|---|---|
| `-d, --detach` | Run containers in the background. | off |
| `--build` | Build images before starting. | off |
| `-w, --watch` | After starting, watch for changes per `develop.watch`. | off |
| `--remove-orphans` | Remove containers for services no longer in the file. | off |
| `--no-recreate` | Leave already-running containers in place. | off |
| `--force-recreate` | Recreate containers even if their config is unchanged. | off |
| `--no-deps` | Do not start the `depends_on` services of the named services. | off |
| `-t, --timeout <SECS>` | Seconds to wait for a container to stop when recreating. | Podman default |
| `--scale <SERVICE=N>` | Override a service's replica count for this run. Repeatable. | from file |
| `--pull <POLICY>` | Pull policy before starting: `always`, `missing`, `never`, `newer`, `build`. (`newer` is Podman's extension.) | per service |
| `--no-build` | Do not build images, even for services with a `build:` section. | off |
| `--quiet-pull` | Suppress image-pull progress output. | off |
| `--wait` | Wait until services are running/healthy before returning. | off |
| `--wait-timeout <SECS>` | Maximum seconds to wait with `--wait` before giving up. | no limit |

**`--wait` implies `-d`.** The flag means "return once the services are up", so
`up --wait` does not stay attached to the logs afterwards, matching
`docker compose up --wait`.

**Under `missing`, an image already present is not pulled again.** The effective
policy is `missing` unless `--pull` or a service's `pull_policy` says otherwise;
in that case podup checks the image once per invocation and skips the pull for
every service using it, so a warm `up` prints no `Pulling` line and issues no
pull request. `always` and `newer` always go to the registry, and a service
pinning `platform:` always pulls, because presence is matched on the image
reference and that carries no architecture.
| `--no-start` | Create the containers but do not start them. | off |
| `--timestamps` | Prefix attached log lines with a timestamp (ignored with `-d`). | off |
| `-V, --renew-anon-volumes` | Recreate anonymous volumes instead of keeping the previous ones. | off |
| `--abort-on-container-exit` | Stop every container as soon as any of them exits; the process exit status is that container's exit code. Cannot be combined with `-d`, `--wait`, or `--watch`. | off |
| `--exit-code-from SERVICE` | Return the named service's exit code as podup's own. Implies `--abort-on-container-exit` (with the same combination rules); a service that does not exist in the compose file is rejected before any container is created. | off |

```bash
podup up -d --build
```

### `down`
Stop and remove containers, networks, and (with `-v`) volumes.

| Flag | Description | Default |
|---|---|---|
| `-v, --volumes` | Also remove named volumes declared in the compose file. | off |
| `--remove-orphans` | Remove containers for services no longer in the file. | off |
| `--rmi <SCOPE>` | Also remove service images: `all`, or `local` (only those built from a `build:` section). | keep images |
| `-t, --timeout <SECS>` | Seconds to wait for containers to stop before killing them. | Podman default |

### `create`
Create the containers for services without starting them (like `up` stopped at
the create step). Accepts a trailing service list. A later `up`/`start` runs the
created containers.

| Flag | Description | Default |
|---|---|---|
| `--build` | Build images before creating containers. | off |
| `--force-recreate` | Recreate containers even if their config is unchanged. | off |
| `--no-recreate` | Leave existing containers in place. | off |
| `--no-deps` | Do not create the `depends_on` services of the named ones. | off |
| `--pull <POLICY>` | Pull policy before creating: `always`, `missing`, `never`, `newer`, `build`. | Podman default |

### `start`
Start existing stopped containers. Accepts a trailing service list.

| Flag | Description | Default |
|---|---|---|
| `--wait` | Wait until services are running/healthy before returning. | off |
| `--wait-timeout <SECS>` | Maximum seconds to wait with `--wait` before giving up. | no limit |

### `stop`
Stop running containers without removing them. Accepts a trailing service list.

| Flag | Description | Default |
|---|---|---|
| `-t, --timeout <SECS>` | Seconds to wait for containers to stop before killing them. | Podman default |

### `restart`
Restart service containers (default: all, or the named ones).

| Flag | Description | Default |
|---|---|---|
| `-t, --timeout <SECS>` | Seconds to wait for containers to stop before killing them. | Podman default |
| `--no-deps` | Do not cascade-restart dependents that declare a `depends_on` restart condition. | off |

### `build`
Build or rebuild service images (optionally only the named services).

| Flag | Description | Default |
|---|---|---|
| `--no-cache` | Do not use the build cache. | off |
| `--pull` | Always attempt to pull a newer base image. | off |
| `--build-arg <KEY=VAL>` | Set a build-time variable. Repeatable. | none |
| `--progress <STYLE>` | `auto`, `plain` or `tty`. Validated but inert; see [accepted for compatibility](#accepted-for-compatibility). | `auto` |
| `--push` | Push each built image to its registry after a successful build. | off |
| `-q, --quiet` | Suppress the build output. | off |

On a terminal `build` draws a board of its own, one row per image, `Building`, then `Building n/m` as each
`STEP n/m` line arrives, then `Built` (or `Failed`). The buildah stream
itself is folded: while a row builds, its last four lines sit dimmed under
the row and vanish when it finishes; on failure the whole stream is
printed once, above the error, so the reason is on screen.
`up --build` shares the `up` board with the build phase: the image rows
sit at the top, in `build`'s order, and the network and container rows
follow on the same board. The build verbs (`Building`/`Building n/m`/
`Built`) land on the same rows the rest of `up` is drawing on, so a single
`[+] Running N/M` count covers everything. An `up` that finds a service's
image missing builds it on the `up` board too, with the image row just
above the service's container row, in the missing-image case `up` has
handled since #1681.

In a pipe every stream line goes to stderr prefixed with `<image-tag> | `,
the way `logs` prefixes container output. The image id of the freshly built
image goes to stdout only when stdout is not a terminal, so a script
piping `podup build | awk '{print $1}'` can pluck it; on a terminal the
row says `Built` and the id is dropped so the row is the record.

## Inspection

### `ps`
List project containers.

| Flag | Description | Default |
|---|---|---|
| `-a, --all` | Include stopped containers. | running only |
| `-q, --quiet` | Print container IDs only. | off |
| `--format <FMT>` | `table` or `json`. | `table` |
| `--status <STATE>` | Show only containers in this state. Repeatable; folded together with any `status=` from `--filter`. | all |
| `--filter <KEY=VAL>` | `name=<NAME>` or `status=<STATE>`. An unknown key is an error. | none |
| `--services` | Print service names only. | off |
| `-s, --size` | Add a SIZE column with each container's on-disk footprint. | off |

An empty result prints `no containers` on stderr and leaves stdout empty, so
a script capturing stdout (`podup ps | awk …`) can tell an empty project from
a non-empty one without parsing headers.

### `ls`
List podup compose projects on the host. Needs no compose file.

| Flag | Description | Default |
|---|---|---|
| `-a, --all` | Include stopped projects. | running only |
| `-q, --quiet` | Print project names only. | off |
| `--format <FMT>` | `table` or `json`. | `table` |
| `--filter <FILTER>` | Keep only projects matching a predicate: `name=<NAME>` or `status=<running\|exited>`. Repeatable. | none |

An empty result prints `no projects` on stderr and leaves stdout empty, so a
script capturing stdout (`podup ls | awk …`) can tell an empty host from a
non-empty one without parsing headers.

### `logs [SERVICE...]`
View container output for the named services (or all).

| Flag | Description | Default |
|---|---|---|
| `-f, --follow` | Stream new output. | off |
| `-n, --tail <N>` | Show the last N lines. `all` opts back into the full stream. | 100 |
| `--since <TIME>` | Show logs since a timestamp or relative time (e.g. `10m`). | start |
| `--until <TIME>` | Show logs before a timestamp or relative time. | end |
| `-t, --timestamps` | Prefix each line with an RFC3339 timestamp. | off |
| `--no-color` | Monochrome prefix even on a colour-capable stdout. | off |
| `--no-log-prefix` | Drop the `{service} \| ` tag entirely. | off |

### `events`
Stream Podman events for this project's containers, under a `TYPE ACTION NAME`
header with the columns aligned to a fixed width (rows arrive over time, so
there is no complete set to size against). `--format json` prints no header.

| Flag | Description | Default |
|---|---|---|
| `--format <FMT>` | `table` (a `TYPE ACTION NAME` summary) or `json` (one object per line). | `table` |
| `--filter <FILTER>` | Keep only events matching a predicate (`KEY=VALUE`, e.g. `event=start`). Repeatable. | none |
| `--since <TIME>` | Only stream events at or after this timestamp or relative time (e.g. `-30m`). | stream start |
| `--until <TIME>` | End of the window. Only closes the feed when paired with `--since` and already elapsed. | no end |

`--json` is a hidden deprecated alias for `--format json`.

**Bounding a feed needs both flags.** Measured against Podman 5.4.2 on 2026-07-29:
`--since -2h --until -1h` ends the feed; `--until` alone, `--since` alone, and
any `--until` in the future all leave it following indefinitely. podup warns
when `--until` is given without `--since`. This also decides the exit code; see
[Exit status](#exit-status).

### `top [SERVICE...]`
Show the running processes of service containers. Each block is headed by the
container name in its identity colour; within the table the bookkeeping columns
(`UID`, `PPID`, `C`, `STIME`, `TTY`, `TIME`) are dimmed so the command line
stands out.

| Flag | Description | Default |
|---|---|---|
| `--format <FMT>` | `table` or `json` (an array of `{Container, Titles, Processes}`). | `table` |

### `stats [SERVICE...]`
Live resource usage (CPU, memory, network, block I/O, PIDs) for service
containers. On a terminal the table repaints in place; anywhere else each frame
is appended, so a redirected `stats` stays a file of readable frames rather than
a file of cursor moves.

CPU and memory percentages are coloured by band: dim below 5%, green to 50,
yellow to 85, red above, so an idle container recedes and the one in trouble is
the one that catches the eye. The absolute figures and the PID count are
secondary detail and stay dim.

`--format json` never repaints, whatever the terminal is: NDJSON while
streaming, one pretty array with `--no-stream`.

| Flag | Description | Default |
|---|---|---|
| `--no-stream` | Print one snapshot and exit. | stream |
| `-a, --all` | Include non-running containers as zeroed rows. | running only |
| `--no-trunc` | Do not truncate long container names. | truncate at 32 |
| `--format <FMT>` | `table` or `json`. While streaming, `json` is NDJSON: one compact array per line. | `table` |

### `port <SERVICE> <PRIVATE_PORT>`
Print the public binding for a port.

| Flag | Description | Default |
|---|---|---|
| `--proto <PROTO>` (alias `--protocol`) | `tcp` or `udp`. | `tcp` |
| `--index <N>` | Target this replica (1-based) of a scaled service. | 1 |

### `images`
List images used by services.

`CREATED` is how long ago the image was built, as a moment (`2 hours ago`,
`81 days ago`). `SIZE` is the image's on-disk size, rendered in decimal units at three
significant digits (`98.2MB`, `805kB`) so the column lines up with what `podman
images` and `docker compose images` print. An image that is not present locally
has an empty `SIZE` and an empty `IMAGE ID`. Under `--format json` the size is
the raw byte count, not the rendered string.

| Flag | Description | Default |
|---|---|---|
| `-q, --quiet` | Print image IDs only. | off |
| `--format <FMT>` | `table` or `json`. | `table` |

An empty result (no services with `image:` or `build:`) prints `no images` on
stderr and leaves stdout empty.

### The ps CREATED and STATUS columns

`STATUS` reports how long a running container has been up (`Up 2h 5m 3s`), with
its health in parentheses when it has a healthcheck (`Up 13h (healthy)`). A
stopped container reports its exit code instead (`Exited (7)`).

`CREATED` is how long ago the container was made. It differs from `STATUS` after
a restart, which is the point of having both: a container created three days ago
and started four seconds ago reads `3 days ago` / `Up 4s`.

`SIZE` appears only with `-s/--size`. It reads `143kB (virtual 225MB)`: the bytes
the container has written on top of its image, then the image's own size, the
same shape `podman ps -s` and `docker ps -s` print, and `virtual` is the image
size rather than the total of the two.

It is opt-in because libpod has to walk each container's writable layer to
answer, which costs real time as a project grows: measured across 59 containers
on Podman 5.7.0 on 2026-08-03, asking for it took the underlying call from 21 ms to 109 ms. On
a small project the difference is below noise. Under `--format json` the field is
`null` when the size was not requested, so a consumer can tell "not asked" from
"empty".

`CREATED` reads as a moment, one unit, the largest that fits: `3 seconds ago`,
`1 minute ago`, `2 hours ago`, `3 days ago`. The `STATUS` span is largest-first
and up to three components, skipping units that are empty (`1h 5m 3s`, `5s`).
A year is 365 days and a month is 30.
Under `--format json` the raw wire values are passed through instead: `Created`
is the RFC 3339 string and `StartedAt` is Unix seconds.

### Volume size accounting

`SIZE` is what the volume occupies; `RECLAIMABLE` is what removing it would free.
The two differ and both are worth seeing: a volume a container still uses reports
its full size and **zero** reclaimable, which is the fact that matters when you
are clearing disk space.

A volume declared in the compose file but never created renders both cells empty
rather than `0B`: an empty cell says it is not there, while `0B` would claim it
exists and holds nothing.

**It is opt-in because it is slow, not because it is verbose.** No libpod
endpoint reports a single volume's size; the only one that knows is `system/df`,
which accounts for every image, container and volume on the host. Measured on
Podman 5.7.0 with 46 volumes, 2026-08-03: 1.2 s, against 10 ms for the plain list. podup
makes that call once per table, never once per row.

Under `--format json` each entry gains a `Usage` object with the raw byte counts
and the container link count, and `Usage` is `null` when `--size` was not given.

### Event timestamps

The `TIME` column is the reader's own wall clock, with the offset that applied at
that instant: `2026-08-02 23:43:35 -05:00`. That matches what `podman events`
prints, so a podup line and a podman line for the same event read the same.

The offset is resolved per event rather than once, so a `--since` window
spanning a daylight-saving change renders each side correctly. If the platform
cannot determine a zone, the time is shown in UTC and marked `Z` rather than
left ambiguous.

`--format json` is untouched: it passes libpod's own event object straight
through.

### `volumes [SERVICE...]`
List the project's named volumes (a trailing service list narrows it to volumes
those services mount). `EXTERNAL` is highlighted when it reads `yes`: podup
neither creates nor deletes an external volume, so those are the ones a
`down -v` leaves standing.

| Flag | Description | Default |
|---|---|---|
| `-q, --quiet` | Print volume names only. | off |
| `-s, --size` | Add SIZE and RECLAIMABLE columns. Slow; see below. | off |
| `--format <FMT>` | `table` or `json`. | `table` |

An empty result prints `no volumes` on stderr and leaves stdout empty.

## Container operations

### `run <SERVICE> [COMMAND...]`
Run a one-off command in a new container for the service.

| Flag | Description | Default |
|---|---|---|
| `--rm` | Remove the container after it exits. | on |
| `--no-rm` | Keep the one-off container after it exits. | off |
| `-d, --detach` | Run in the background. | off |
| `-e, --env <KEY=VAL>` | Set an environment variable. Repeatable. | none |
| `--name <NAME>` | Override the container name. | generated |
| `-P, --service-ports` | Publish the service's declared ports. | off |
| `-l, --label <KEY=VAL>` | Add a label to the one-off container. Repeatable. | none |
| `-u, --user <NAME\|UID[:GID]>` | Run the command as this user. | image default |
| `-w, --workdir <PATH>` | Working directory inside the container. | image default |
| `--entrypoint <CMD>` | Override the image entrypoint. | image default |
| `-v, --volume <SPEC>` | Bind-mount an extra volume (`HOST:CONTAINER[:OPTS]` or `NAME:CONTAINER`). Repeatable. | none |
| `-p, --publish <SPEC>` | Publish an extra port (`HOST:CONTAINER[/PROTO]`). Repeatable. | none |
| `-i, --interactive` | Keep the container's STDIN open (`stdin_open`). Whether a live terminal is attached is decided by stdin/stdout being terminals and by `-T`, not by this flag. | off |
| `-T, --no-TTY` (alias `--no-tty`) | Disable pseudo-TTY allocation. | off |
| `--no-deps` | Do not start the `depends_on` services before running. | off |

```bash
podup run --rm web sh -c 'echo hello'
```

> **Differs from docker on purpose.** `run` removes the container by default
> here; `docker compose run` keeps it unless you pass `--rm`. Migrating a script
> means its existing `--rm` becomes a no-op and a container it expected to
> inspect afterwards is gone; pass `--no-rm` to keep it.

`run` allocates a pseudo-TTY and attaches your stdin when stdin is a
terminal, so `podup run -it app sh` drops you into an interactive session that
follows your window size. Like `docker compose run`, a TTY on both ends is the
default and `-T` is how you turn it off; `-d` never allocates one, since there
is nobody to be interactive with.

It engages **only** when *both* stdin and stdout are terminals, so a script, a
pipeline or a redirect keeps the plain streaming behaviour with no change to
output framing:

```bash
podup run --rm app echo hello > out.txt     # stdout is a file  -> streams, no TTY
echo x | podup run --rm app ./migrate.sh    # stdin is a pipe   -> streams, no TTY
podup run --rm -T app ./migrate.sh          # -T                -> streams, no TTY
```

Requiring stdout matters because a pty **merges stdout and stderr and writes
CRLF**. Checking stdin alone would mean `podup run app cmd > out.txt`, typed at
a shell, silently wrote different bytes into that file than the same command in
a script.

### `exec <SERVICE> <COMMAND...>`
Execute a command in a running service container.

| Flag | Description | Default |
|---|---|---|
| `-e, --env <KEY=VAL>` | Set an environment variable. Repeatable. | none |
| `-u, --user <NAME\|UID[:GID]>` | Run the command as this user. | container default |
| `-w, --workdir <PATH>` | Working directory inside the container. | container default |
| `--privileged` | Give extended privileges to the command. | off |
| `-d, --detach` | Run the command in the background. | off |
| `-T, --no-tty` (alias `--no-TTY`) | Disable pseudo-TTY allocation. | off |
| `--index <N>` | Target this replica (1-based) of a scaled service. | 1 |

`exec` allocates a pseudo-TTY and attaches your stdin when both stdin and stdout
are terminals, so `podup exec -it db psql` drops you into an interactive session
that follows your window size. It is not on `-i`: like `docker compose exec`, a
TTY on both ends is the default, and `-T` is how you turn it off.

Requiring stdout too, not just stdin, keeps a redirect clean: a pty merges
stdout and stderr and writes CRLF, so interactivity engages **only** when both
ends are terminals, and a script, a pipeline or a redirect keeps the plain
streaming behaviour with no change to output framing:

```bash
podup exec db psql -c 'select 1' > out.txt   # stdout is a file -> streams, no TTY
echo 'select 1' | podup exec -T db psql      # stdin is a pipe  -> streams, no TTY
```

```bash
podup exec -u root web sh
```

### `cp <SRC> <DST>`
Copy files between a container and the host. Use `SERVICE:PATH` for the
container side, e.g. `podup cp web:/app/data ./local`.

| Flag | Description | Default |
|---|---|---|
| `--index <N>` | Target this replica (1-based) of a scaled service. | 1 |
| `-L, --follow-link` | Follow symlinks in the host source before copying into the container. | off |
| `-a, --archive` | Accepted for compatibility (no effect under rootless Podman). | off |

### `attach <SERVICE>`
Attach to a service container's output (stdout/stderr), streaming it until the
container exits or you detach. Output only; stdin is never attached.

| Flag | Description | Default |
|---|---|---|
| `--index <N>` | Target this replica (1-based) of a scaled service. | 1 |
| `--no-stdin` | Accepted for compatibility; stdin is never attached anyway. | off |
| `--sig-proxy [<BOOL>]` | Accepted for compatibility; no effect. Takes docker's bare form or an explicit value. | off |
| `--detach-keys <KEYS>` | Accepted for compatibility; no effect. | none |

### `kill [SERVICE...]`
Send a signal to service containers.

| Flag | Description | Default |
|---|---|---|
| `-s, --signal <SIG>` | Signal to send. | `SIGKILL` |
| `--remove-orphans` | Then remove containers for services no longer in the file. | off |

### `rm [SERVICE...]`
Remove stopped service containers.

| Flag | Description | Default |
|---|---|---|
| `-f, --force` | Remove even running containers (stop first). | off |
| `-v, --volumes` | Also remove anonymous volumes attached to them. | off |
| `-s, --stop` | Stop the containers (gracefully) before removing them. | off |

### `pause [SERVICE...]` / `unpause [SERVICE...]`
Pause running service containers, or resume paused ones. `resume` is an alias
for `unpause`.

### `wait [SERVICE...]`
Block until the named service containers (default: all) stop, printing one line
per container as it exits: the container's name and its exit code, in aligned
columns, with a non-zero code in red. A scaled service reports each replica
separately. The command's own exit status is the last non-zero code it saw.

| Flag | Description | Default |
|---|---|---|
| `--format <FORMAT>` | `table` for the aligned columns, or `json` for one NDJSON object (`Container`, `ExitCode`) per container, emitted as that container exits rather than after the last one. | table |

A project with nothing to wait on prints nothing and exits 0.

### `scale <SERVICE=N>...`
Set the number of running containers for one or more services, creating missing
replicas and removing surplus ones. A service that publishes a **fixed host
port** cannot be scaled past one replica; the command fails fast and tells you
to drop the host port (`- "80"`, so Podman assigns one per replica), front it
with a reverse proxy, or stay at one replica.

### `commit <SERVICE> <IMAGE>`
Commit a service container's current state to a new image reference
(`repo[:tag]`).

| Flag | Description | Default |
|---|---|---|
| `--index <N>` | Select a replica (1-based) of a scaled service. | 1 |
| `-m, --message <MSG>` | Commit message recorded on the image. | none |
| `-a, --author <AUTHOR>` | Author recorded on the image. | none |
| `-c, --change <INSTRUCTION>` | Apply a Dockerfile instruction to the created image. Repeatable. | none |
| `-p, --pause [<BOOL>]` | Pause the container during commit for a consistent snapshot. `--pause=false` snapshots it live. | **on** |

### `export <SERVICE>`
Export a service container's filesystem as a tar archive.

| Flag | Description | Default |
|---|---|---|
| `-o, --output <FILE>` | Write to a file instead of stdout. | stdout |
| `--index <N>` | Select a replica (1-based) of a scaled service. | 1 |

## Images

### `pull [SERVICE...]`
Pull images for the named services, or all services if none are given.

| Flag | Description | Default |
|---|---|---|
| `-q, --quiet` | Suppress image-pull progress output. | off |
| `--ignore-pull-failures` | Continue pulling the remaining services after a failure. | off |
| `--include-deps` | Also pull images for the named services' `depends_on` services. | off |
| `--policy <POLICY>` | Pull policy, overriding per-service `pull_policy`: `always`, `missing`, `never`, `newer`, `build`. | per service |

### `push [SERVICE...]`
Push each service's `image:` to its registry (services without an image are
skipped). Credentials come from `podman login`. Each image is reported on stderr
as it starts and finishes, leaving stdout a clean pipe.

| Flag | Description | Default |
|---|---|---|
| `-q, --quiet` | Suppress the push progress output. | off |
| `--ignore-push-failures` | Continue after a failure. | off |
| `--tls-verify <BOOL>` | Verify the registry TLS cert; `false` allows an insecure/HTTP registry. | Podman default |

## Generate

### `generate quadlet`
Translate the compose file into Podman Quadlet unit files: one `.container` per
service plus `.network` and `.volume` units. `gen` is an alias for `generate`.

| Flag | Description | Default |
|---|---|---|
| `-o, --output <DIR>` | Directory to write the unit files into. Omit to print to stdout. | stdout |

```bash
podup generate quadlet -o ~/.config/containers/systemd
```

Quadlet units are consumed by systemd, so they only run on Linux. Generating
them on macOS or Windows is allowed (e.g. to deploy to a remote Linux host) but
prints a `podup: warning:` to stderr noting the files will not run on the host.

## Watch

### `watch`
Watch for file changes and react as configured by each service's
`develop.watch` rules. (`up --watch` does the same after starting the stack.)
The `action` of each rule may be:

| Action | Effect on change |
|---|---|
| `sync` | Copy the changed files into the running container. |
| `rebuild` | Rebuild the image and recreate the container. |
| `restart` | Restart the container without rebuilding. |
| `sync+restart` | Sync the files, then restart the container. |
| `sync+exec` | Sync the files, then run the rule's `exec` command in the container. |

## Maintenance

### `config`
Print the resolved compose file (after substitution, extends, include, and
`env_file`). `convert` is an alias.

`env_file` entries are read and folded into `environment:`, and the key is
dropped, the same thing `docker compose config` does. Before 3.1.0 the key was
printed unresolved, so a service taking its whole environment from a file
rendered with no `environment:` at all, and `config` pointed away from the answer
it is meant to give. `environment:` still wins over `env_file:`, a later file
still wins over an earlier one, and a bare `KEY` stays valueless because it means
"inherit from the host".

| Flag | Description | Default |
|---|---|---|
| `--format <FMT>` | `yaml` or `json`. | `yaml` |
| `--services` | List service names, one per line. | off |
| `--volumes` | List named volumes, one per line. | off |
| `--images` | List the images services use, one per line. | off |
| `--profiles` | List the profiles the file declares, one per line. | off |
| `--hash <SERVICES>` | Print a stable per-service config hash for the given comma-separated services, or `'*'` for all. | none |
| `--no-normalize` | Accepted for compatibility; `config` always emits the normalized form. | off |
| `-q, --quiet` | Only validate; print nothing. | off |
| `--no-interpolate` | Leave `${VAR}` placeholders literal. | off |
| `--resolve-image-digests` | Rewrite each service `image:` to its registry digest (`repo@sha256:...`). | off |

### `audit`

Print one row per service listing every hardening gap the compose file leaves
open. The `audit` command is read-only and never contacts Podman; no check
changes what `up` does. The same loading path as `config` is used, so
`--profile`, `--env-file` and `-f` resolve identically, and `audit` reports
what `up` would actually start.

The default output is a table:

```
$ podup audit
SERVICE FINDINGS
api     writable_root no_new_privileges_off secret_in_environment
db      -
  api: writable_root: read_only is not true: the container's root filesystem is writable
  api: no_new_privileges_off: security_opt is missing no-new-privileges:true: setuid binaries may regain privileges
  api: secret_in_environment: environment: DB_PASSWORD carries a hard-coded value; move it to secrets:
```

A service with no findings shows `-` in the `FINDINGS` column; a project
with no findings at all prints `no findings` and nothing else.

`--format json` emits a single machine-readable object so CI can pin the
shape and grep the `check` ids:

```
{"findings":[{"check":"writable_root","reason":"...","service":"api"}, ...]}
```

The keys are alphabetically ordered; an empty list is `{"findings":[]}`,
never `null`.

`--strict` exits 1 when at least one finding is present (otherwise exits
0), so a CI job can gate on `podup audit --strict` and fail when hardening
gaps are introduced.

The eleven checks and what they look for:

| Check id | Fires when | Notes |
|---|---|---|
| `privileged` | `privileged: true` | Grants extended host privileges; under rootless Podman reduced but never incidental. |
| `host_namespace` | `network_mode: host`, or `pid`/`ipc`/`uts`/`cgroup`/`userns_mode: host` | One finding per active mode. |
| `dangerous_capability` | `cap_add` contains `SYS_ADMIN` or `ALL` | Effectively grants root inside the container. |
| `writable_root` | `read_only` is not `true` | Compose's default is writable. |
| `no_cap_drop_all` | `cap_drop` does not contain `ALL` | Without it the runtime's default capability set stays. |
| `no_new_privileges_off` | `security_opt` lacks `no-new-privileges:true` | Both spellings (`no-new-privileges:true` and `no-new-privileges` alone, the Podman form) are accepted. |
| `no_pids_limit` | `pids_limit` unset | A fork bomb can exhaust the host's process table. |
| `no_memory_limit` | Neither `mem_limit` nor `deploy.resources.limits.memory` set | A leak can OOM the host. |
| `no_userns` | `userns_mode` unset | Without it Podman's `auto` applies; the reason links to `docs/docker-migration.md`. |
| `secret_in_environment` | `environment:` key matching `PASSWORD`/`SECRET`/`TOKEN`/`KEY` (case-insensitive) with a non-empty literal value | Bare keys (host inheritance) and `${VAR}` placeholders are not flagged. |
| `unpinned_image` | `image:` with no tag, with tag `latest`, or `latest` without a digest | An `@sha256:` digest counts as pinning regardless of the tag. |

| Flag | Description | Default |
|---|---|---|
| `--format <FMT>` | `table` (default) or `json`. | `table` |
| `--strict` | Exit 1 when any finding is present, 0 otherwise. | off |

### `completions <SHELL>`
Print a shell completion script to stdout for `bash`, `zsh`, `fish`,
`powershell`, or `elvish`. The Debian package installs the bash/zsh/fish files
automatically; otherwise source the output from your shell startup:

```bash
mkdir -p ~/.local/share/bash-completion/completions
podup completions bash > ~/.local/share/bash-completion/completions/podup
podup completions fish > ~/.config/fish/completions/podup.fish
```

For zsh, write to a directory on your `$fpath` (run this from zsh, where
`fpath` is defined):

```zsh
podup completions zsh > "${fpath[1]}/_podup"
```

### `update`
Replace the running binary with the latest signed release.

| Flag | Description | Default |
|---|---|---|
| `--check` | Report whether a newer release exists; install nothing. | off |
| `--force` | Reinstall even if the latest release is not newer. | off |

Verification fails closed: a missing key, bad Ed25519 signature, or SHA-256
mismatch aborts before the installed binary is touched. After the binary is
written, a self-test runs `<binary> --version` and refuses the install if the
reported version does not match the resolved release tag (strict equality,
optional single `v` prefix). A CDN or proxy that replays an older,
*legitimately* signed release passes the signature and digest checks but fails
this one, and the previous binary is restored. The shell installers
(`install.sh`, `install.ps1`) run the same gate. See
[self-update.md](self-update.md) for the trust model.

### `autostart` (alias `boot`)
Manage a boot-time autostart unit for this compose project: rootless,
user-scope `systemctl --user` (enable lingering with
`loginctl enable-linger` so the unit starts without a login session). See
[Rootless autostart](autostart.md) for the full setup, the two backends, and
running it under an isolated service account.

| Subcommand | Description |
|---|---|
| `install` | Install (and, by default, start) the autostart unit(s) for this project. Writes only under `${XDG_CONFIG_HOME:-~/.config}`. |
| `uninstall` | Remove whichever mode is installed (auto-detected). `--purge` also tears the stack down and drops its volumes. |
| `status` | Report this project's unit and session state. |
| `rebuild [service]` | Quadlet mode only: rebuild the built image(s) and restart the container(s). Omit the argument to rebuild every built service. |

| Flag (`install`) | Description | Default |
|---|---|---|
| `--mode <MODE>` | Autostart backend: `service` (one `Type=oneshot` unit running `podup up -d` at boot, `podup stop` on shutdown) or `quadlet` (one native Podman Quadlet unit per service, owned by systemd directly). | `service` |
| `--no-start` | Install the unit(s) but do not start them. | off |
| `--dry-run` | Print what would be written and run; change nothing. | off |

### `version`
Print version information, like `docker compose version`. `podup --version`
prints the same.

| Flag | Description | Default |
|---|---|---|
| `--short` | Print only the version number. | off |
| `--format <FMT>` | `pretty` or `json`. | `pretty` |

## Progress output

`up`, `down`, `pull` and `build` report every resource they touch, on
**stderr**, so stdout stays a clean pipe. What that looks like depends on where
it is going.

**On a terminal**, a live region at the tail of the output shows the whole set
up front and repaints it as work proceeds. Finished rows scroll up and stay:

```
[+] Running 3/6
 ✔ Network   myapp_default  Created        0.1s
 ⠹ Image     postgres:16    Pulling        2.9s
 ⠹ Container myapp-db-1     Starting       2.9s
 ⠸ Container myapp-api-1    Creating       0.4s
 ⠿ Container myapp-web-1    Pending
```

A tail region rather than a full-screen takeover, deliberately: `up` is a
command that finishes, and handing the screen back blank would destroy the
record of what it did.

**Anywhere else** (a pipe, a file, CI, `NO_COLOR`, `--ansi never`), the same
events come out as plain append-only lines with no escape sequences at all:

**Progress lines on stderr never contradict themselves.** A transitional
verb (`Creating`, `Starting`, `Pulling`, `Removing`, …) is held until the final
one arrives. When the final verb reports work (`Created`, `Started`, `Pulled`,
`Removed`), both lines are printed in order, so a log shows when the work
started; when it reports that nothing was done (`Exists`, `Running`, `Absent`,
`Skipped`), only that line is printed, so a piped `up -d` against a network
that already exists says `Network … Exists` and not the pair `Creating` /
`Exists`. A transitional verb whose final never arrives (a crash mid-way) is
still flushed at the end of the command, so the log records what was in
flight.

```
 Network myapp_default  Creating
 Network myapp_default  Created
 Container myapp-web-1  Starting
 Container myapp-web-1  Started
```

Both renderers see every event, so a log says *more* than it used to rather than
less. Animation in a CI log is a defect, and so is a CI log missing what the
terminal showed.

Only what actually happened is reported: re-running `up` over existing
resources reports no creation, and `down` on a project whose networks or volumes
were never created reports no removal. A command that acted on nothing says so:
`no containers to stop`, `no containers to start (project not created)`, rather
than exiting silently, which is indistinguishable from success.

A container that is replaced reads differently from one that is created:
`Recreating`/`Recreated` for a container `up` or `create` removed and built
again (a changed config or image, `--force-recreate`), `Starting`/`Started` or
`Creating`/`Created` for one that did not exist, and `Running` or `Exists` for
one left alone. Recreation destroys the container's writable layer, so the word
is the operator's only signal at the moment it happens, and it is what makes
`--force-recreate` and `--no-recreate` verifiable from the output.

The live region needs stderr to be a terminal, colour to be on, and the terminal
size to be readable. If any of those is missing it falls back to the plain
lines, which is also what `--ansi never` and `NO_COLOR` select.

## Diagnostics

podup writes warnings and errors to **stderr**, prefixed with `podup:` (so the
emitter is identifiable in journald and multi-tool logs) while stdout stays a
clean pipe (e.g. the YAML from `config`, the units from `generate quadlet`).
Forward-compatibility warnings about unknown or unsupported compose fields are
shown by default; set `RUST_LOG=debug` for verbose tracing. An unexpected
internal error prints a `podup: internal error:` notice with a bug-report link
and a reminder to redact secrets before sharing logs.

## Environment

Every environment variable `podup` reads, in one place. Each compose variable
has an equivalent flag (see [Global options](#global-options)); the flag wins
when both are set.

| Variable | Description |
|---|---|
| `COMPOSE_FILE` | Path-separator-delimited list of compose files (`--file`). |
| `COMPOSE_PROJECT_NAME` | Default project name (`--project`). |
| `COMPOSE_PROFILES` | Default active profiles (`--profile`). |
| `PODMAN_SOCKET` | Podman socket path (`--socket`). |
| `PODUP_LIBPOD_POOL` | HTTP/1.1 connection-pool size for the libpod client (`--connection-pool-size`). Default 8. `PODUP_LIBCOD_POOL` (the earlier typo'd name) is still read when the new name is unset. |
| `DOCKER_HOST` | Docker-compatible fallback for the Podman socket, used only when `PODMAN_SOCKET` is unset. Must be a local `unix://` socket (or `npipe://` on Windows); a remote `tcp://`/`ssh://` value is rejected. |
| `RUST_LOG` | Log verbosity filter. Unset shows warnings and errors; e.g. `RUST_LOG=podup=info` or `RUST_LOG=podup=debug` for more detail. |

## Podman extensions

Podman does more than the Compose Specification, and a few of those extras
change behaviour rather than just observability. Most sit under the spec's
reserved `x-` prefix, so a file using one **stays a valid compose file**: docker
compose ignores an unknown `x-` key instead of erroring, and the same file still
runs there, it just does not act on the extra. Two extensions are not `x-`
prefixed and so are **not** portable back to docker compose, which rejects the
keys; they are called out below.

| Key | Where | What it does | Portable |
|---|---|---|---|
| `x-podman-on-failure` | under a service's `healthcheck:` | `none`, `kill`, `restart` or `stop`, naming what Podman does when the check flips to unhealthy. Default `none`. | yes |
| `x-podman-pod` | top level | `true` puts every service of the project into one Podman pod named after the project; see [Pods](#pods). | yes |
| `x-podman-autoupdate` | under a service | `registry` or `local`; see [Auto-update](#auto-update). | yes |
| `noexec`, `nosuid`, `nodev` | under a long-form volume's `volume:` | mount-hardening flags; see [Per-mount hardening options](docker-migration.md#per-mount-hardening-options-noexec-nosuid-nodev). The short form carries them as raw mount options. | no |

### Auto-update

The Compose Spec has no equivalent. Podman's `auto-update` (driven by
`podman-auto-update.timer`) only sees containers that carry the
`io.containers.autoupdate` label, and podup does not emit it on its own.

`x-podman-autoupdate` adds the label and arranges for the registry to be
checked, so a stack started by `podup up` is no longer invisible to Podman's
auto-update, and a Quadlet exported by `podup generate quadlet` carries
`AutoUpdate=<value>` for systemd to set the same label.

| Value | What it does |
|---|---|
| `registry` | The container carries `io.containers.autoupdate=registry`, and `podup up` pulls the image with policy `newer` so a moved tag recreates the container. `--pull <policy>` on the command line wins over the extension. |
| `local` | The container carries `io.containers.autoupdate=local`: Podman's auto-update compares the container's image with the local image of the same name and restarts the unit when they differ, which is what a `podman build` that moved the tag looks like. `podup up` keeps the existing pull behaviour, and the same rebuilt image recreates the container through the config-hash and image-ID comparison it already does. |

On `generate quadlet`, the value lands in the `[Container]` section as
`AutoUpdate=<value>`. Quadlet derives the `io.containers.autoupdate` label
itself, so the generator must not also emit a `Label=io.containers.autoupdate=...`
line, which would duplicate the label and the unit would silently disagree
with the Quadlet side.

`podup autostart --auto-update <hourly|daily|weekly>` is the executor for
service-mode stacks; Quadlet mode already uses `podman-auto-update.timer`,
and start mode has no compose front-end on the boot path. See
[docs/autostart.md](autostart.md#auto-update) for which executor runs where.

### Pods

The Compose Spec has no equivalent. `x-podman-pod: true` at the top level
puts every service of the project into one Podman pod, named after the
project, with a shared network namespace. Nothing else in the file changes
meaning; without the key `up` creates the containers on the project network
as before.

What changes inside the pod:

- Services reach each other on `localhost`. `up` adds one `<service>:127.0.0.1`
  host entry per service to the pod, so a compose file that says `db:5432`
  keeps working; two services listening on the same container port now
  collide, since they share the namespace, and podup cannot see that before
  Podman does.
- `ports:` are published by the pod, as the union of every service's list.
  A container inside a pod cannot publish its own.
- Only the network namespace is shared. UTS and IPC stay per container, so
  `hostname:` keeps working. The user namespace is the pod's: a `userns_mode`
  every service declares alike (`auto`, say) is applied to the pod, and a
  member cannot carry its own.
- The pod carries the project's networks, the same set the containers would
  have joined.
- `up` creates the pod before the first container, prints `Creating`/`Created`
  for it, and records a hash of the port set, the network set and the host
  entries as a label. When a later `up` computes a different hash it recreates
  the pod, prints `Recreating`/`Recreated`, and creates every container
  afresh, since removing a pod removes its members. A change that leaves the
  hash alone, such as a service's command or image, recreates only that
  container, as today.
- `down` removes the containers, then the pod (which takes the infra
  container with it), then networks and volumes. `down --remove-orphans` also
  removes a pod left behind under the project's label.
- `ps` does not list the infra container.
- `generate quadlet` writes one `<project>.pod` unit with the ports, the
  networks and the host entries, and each `.container` unit references it
  with `Pod=` and drops its own `PublishPort=` and `Network=` lines.

What is refused, before anything is created, with the service and the key in
the message:

- `network_mode` on any service;
- a service whose `networks:` set differs from another service's (every
  service declares the same set, or none and gets the project default);
- two services publishing the same host port, whichever host IPs they bind;
- services that disagree on `userns_mode` (one sets it and another does not,
  or they set different values).

What it costs and what it saves, measured on 2026-09-03 with the `wide-level`
benchmark scenario (42 services), 10 measured runs after 2 warm-up, twice,
same binary, cores pinned: `up -d` 1.13 s on the project network against
1.50 s in a pod; `down -v` 1.76 to 1.91 s against 1.39 to 1.40 s. Creation is
slower inside a pod and teardown faster. The same 42 containers created one
at a time from the `podman` CLI went the other way (4.7 s against 3.3 s), so
the cost sits in how many creates run at once: `up` starts a level's
containers concurrently, and a pod does not take that in parallel the way a
network does. Choose a pod for `localhost` between services, one namespace to
audit, and one place ports are published; not for a faster `up`.

### Healthcheck timing on a `service_healthy` gate

When `up` waits on `depends_on: {condition: service_healthy}`, podup drives the
check itself: Podman schedules its own runs through systemd transient timers,
which never fire on a host without systemd, so a purely passive wait would block
until the whole budget elapsed.

| | |
|---|---|
| how often the check is **run** | the healthcheck's `interval`, floored at **100ms** |
| how often the status is **read** | every 150ms |
| how long the wait lasts | `interval × retries` plus `start_period`, extended by `--wait-timeout` |

Running a check executes a command *inside* the container, so it happens no
faster than `interval` and the floor keeps `interval: 1ms` from becoming a
thousand executions a second. Reading the status is a plain inspect that runs no
command, so it is cheap and frequent, and it is what notices promptly when
Podman's own timer flips the status between podup's runs.

A container that fails during the wait is reported as soon as the next read sees
it, rather than at the end of the budget.

Without it, a compose healthcheck detects a sick container and does nothing
about it: a restart policy reacts to the process *exiting*, not to the container
being unhealthy, so an app that hangs without dying stays in rotation
indefinitely.

> **`kill` and a restart policy do not combine the way you would expect.**
> `--health-on-failure=kill` with `restart: unless-stopped` leaves the container
> **exited and never revived**: the kill is not the kind of exit the restart
> policy acts on. Use `restart` if you want it to come back.
>
> podman-run(1) suggests `kill` or `stop` "when running inside of a systemd
> unit… to make use of systemd's restart policy". That advice assumes the unit
> restarts the container. `autostart --mode service` writes a
> `Type=oneshot` + `RemainAfterExit=yes` unit, which does **not**, so under
> podup's own service-mode unit that recommendation turns a degraded container
> into a stopped one.

An invalid value is rejected by `up`/`create`; `generate quadlet` warns and
omits the key instead, because an unrecognised `HealthOnFailure=` makes Quadlet
drop the whole unit at daemon-reload.

## Accepted for compatibility

These flags parse and are validated, so a script written against docker compose
runs unchanged, but podup does not act on them. They are listed here because
`--help` says "accepted for compatibility" without saying which flags that
covers, and the only other way to find out was to read the dispatch code.

| Flag | Why it does nothing |
|---|---|
| `build --progress <STYLE>` | podup renders build output one way. The value is still validated, so a typo is rejected rather than silently ignored. |
| `config --no-normalize` | `config` always emits the normalized form. |
| `cp -a, --archive` | Ownership/permission preservation is not meaningful for a rootless copy. |
| `attach --no-stdin`, `--sig-proxy`, `--detach-keys` | `attach` streams output only; stdin is never attached. Use `exec`/`run` for an interactive session. |

Everything else that parses does something. An **unknown** `--filter` predicate
is rejected outright rather than dropped: a filter that silently does not apply
returns the whole set, which a script reads as a match.

## Exit status

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1` | A command failed (Podman error, runtime failure). |
| `2` | Command-line usage error (unknown flag, bad argument). |
| `3` | `update` failed to verify or install a release. |
| `126` | `run`/`exec`: the command exists but is not executable. |
| `127` | `run`/`exec`: the command was not found. |
| `130` | An attached `up` was ended by SIGINT or SIGTERM. |
| other | `run` propagates the container's own exit code verbatim. `up --abort-on-container-exit` does the same with the first container to exit; `up --exit-code-from SERVICE` does the same with the named service (which may have been SIGKILLed during the abort, yielding 137). |

An attached `up` also exits `1` when a log stream dies while its container is
still running, and `events` exits `1` when an unbounded feed ends at all. Both
are new in 3.3.0 and both used to exit `0`; see the streaming rule below.

`exec` propagates the command's exit code the same way `run` does, and `wait`
returns the last non-zero code it saw.

**`up --abort-on-container-exit` and `up --exit-code-from`** make a foreground
`up` a usable CI test harness: the project is brought up, the named service
(or the first to exit) runs to completion, the rest are stopped, and the
container's exit code is the process exit code, matching `docker compose v5.1.3`
on the same Podman socket. The teardown is `stop`, not `down`: containers
remain in `Exited` state, ready for the next `up`/`down`. A zero exit
propagates as `0`, not `1`, so a clean run still reports success.

**`130` for SIGTERM as well as SIGINT.** The signal number would suggest 143 for
SIGTERM, but `docker compose up` returns 130 for both and podup matches it
(measured against v5.1.3 pointed at the same Podman socket). The project is
still torn down before the code is returned, so an interrupted `up` leaves
nothing running.

This matters most in CI: a job that runs `podup up` in the foreground and is
cancelled (by a timeout, by an operator, by the runner shutting down) used to
report **success**. Anything gating on that exit status could not tell a
completed run from an abandoned one.

**`stats --format json` differs from docker on purpose.** podup emits numbers
(`"CPUPerc": 12.5`) where docker emits preformatted strings (`"12.50%"`), and
splits `NetIO`/`BlockIO` into separate input/output fields. Raw numbers are
exact and need no parsing, but it does mean a docker-compose JSON consumer needs
adapting rather than working unchanged.

**A streaming command that loses its connection fails.** This covers five
commands: `logs`, `stats`, an attached `up`, `run`, and `events`.

A stream ends when the container it follows stops, and libpod marks that end
with a chunked terminator. A lost terminator (a dropped connection, or a
version that omits it) is indistinguishable from a real mid-stream break at the
transport layer, so the transport is not asked. Four of the five re-check
whether the container is still running: still running means live output was
truncated, and the command exits `1`. `run` reports the transport error instead
of an exit code, since the command it was running never produced one.

This matters for anything scraping them. `logs -f` used to return success after
losing its socket, so a monitor could not tell "the container finished" from "my
connection died", and a script reading that `0` was already wrong, it just had
no way to know. `docker compose logs -f` exits `1` on the same failure (measured
against the same Podman socket), so the old `0` was a divergence rather than
parity.

A stream that ends because its containers stopped still exits `0`, as does
`logs` without `-f` and a `logs -f` whose reader closes the pipe (`| head`).
When the re-check itself cannot be made, usually because the same severed
connection is needed for it, the command fails rather than assuming the end was
clean.

**`events` decides it differently**, because a feed is project-scoped and
follows no single container, so there is nothing to re-check. What the caller
asked for answers it instead:

- **A bounded window** (`--since` and `--until` together, both already elapsed)
  closes on its own, so reaching the end exits `0`.
- **Anything else** is an unbounded feed. libpod never ends one, so *any* end
  means the stream was lost and the command exits `1`, including a clean end,
  which no check on the error shape could have caught.
- **A transport failure always fails**, bounded or not. A severed socket is not
  made expected by having asked for a window.
- Ctrl-C or SIGTERM kills the process by signal, as before; no error is
  invented.

Note that a window needs **both** ends and both must already have elapsed.
Measured against Podman 5.4.2 on 2026-07-29: `--since -2h --until -1h` closes the feed, while
either flag alone leaves it open, as does any `--until` in the future. So
`--until 5m` follows indefinitely rather than stopping in five minutes. podup
warns when `--until` is passed without `--since`.

**`watch` is the exception.** A sync, rebuild, restart or exec that fails during
a watch session is reported as a warning and the session keeps going; `watch`
exits 0 unless it cannot start at all. This matches `docker compose watch`: a
long-running developer loop should not die because one rebuild failed. But it
does mean the exit code of a `watch` session says nothing about whether every
action in it succeeded. Read the warnings, not the status, and do not gate
automation on it.
