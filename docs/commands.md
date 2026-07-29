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
| `--profile <NAMES>` | `COMPOSE_PROFILES` | Active profiles, comma-separated. |
| `--project-directory <PATH>` | | Base directory for relative paths (env_file, build context, bind mounts, config/secret sources). Defaults to the compose file's directory. |
| `--ansi <WHEN>` | | Colour output: `auto`, `always` or `never`. `always` forces colour even into a pipe or file. | 
| `--env-file <PATH>` | | Env file(s) for interpolation. Repeatable; later files win. **Replaces** a project `.env` rather than adding to it — when this is given, `.env` is not read. The process environment still takes precedence over both. |

## Lifecycle

### `up`
Create and start all services (or only the named ones, plus their transitive
`depends_on`). Accepts a trailing service list.

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
`up --wait` does not stay attached to the logs afterwards — matching
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
| `--progress <STYLE>` | `auto`, `plain` or `tty`. Validated but inert — see [accepted for compatibility](#accepted-for-compatibility). | `auto` |
| `--push` | Push each built image to its registry after a successful build. | off |
| `-q, --quiet` | Suppress the build output. | off |

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

### `ls`
List podup compose projects on the host. Needs no compose file.

| Flag | Description | Default |
|---|---|---|
| `-a, --all` | Include stopped projects. | running only |
| `-q, --quiet` | Print project names only. | off |
| `--format <FMT>` | `table` or `json`. | `table` |
| `--filter <FILTER>` | Keep only projects matching a predicate: `name=<NAME>` or `status=<running\|exited>`. Repeatable. | none |

### `logs [SERVICE...]`
View container output for the named services (or all).

| Flag | Description | Default |
|---|---|---|
| `-f, --follow` | Stream new output. | off |
| `-n, --tail <N>` | Show the last N lines. | all |
| `--since <TIME>` | Show logs since a timestamp or relative time (e.g. `10m`). | start |
| `--until <TIME>` | Show logs before a timestamp or relative time. | end |
| `-t, --timestamps` | Prefix each line with an RFC3339 timestamp. | off |
| `--no-color` | Monochrome prefix even on a colour-capable stdout. | off |
| `--no-log-prefix` | Drop the `{service} \| ` tag entirely. | off |

### `events`
Stream Podman events for this project's containers.

| Flag | Description | Default |
|---|---|---|
| `--format <FMT>` | `table` (a `TYPE ACTION NAME` summary) or `json` (one object per line). | `table` |
| `--filter <FILTER>` | Keep only events matching a predicate (`KEY=VALUE`, e.g. `event=start`). Repeatable. | none |
| `--since <TIME>` | Only stream events at or after this timestamp or relative time (e.g. `-30m`). | stream start |
| `--until <TIME>` | End of the window. Only closes the feed when paired with `--since` and already elapsed. | no end |

`--json` is a hidden deprecated alias for `--format json`.

**Bounding a feed needs both flags.** Measured against Podman 5.4.2:
`--since -2h --until -1h` ends the feed; `--until` alone, `--since` alone, and
any `--until` in the future all leave it following indefinitely. podup warns
when `--until` is given without `--since`. This also decides the exit code — see
[Exit status](#exit-status).

### `top [SERVICE...]`
Show the running processes of service containers.

| Flag | Description | Default |
|---|---|---|
| `--format <FMT>` | `table` or `json` (an array of `{Container, Titles, Processes}`). | `table` |

### `stats [SERVICE...]`
Live resource usage (CPU, memory, network, block I/O, PIDs) for service
containers.

| Flag | Description | Default |
|---|---|---|
| `--no-stream` | Print one snapshot and exit. | stream |
| `-a, --all` | Include non-running containers as zeroed rows. | running only |
| `--no-trunc` | Do not truncate long container names. | truncate at 32 |
| `--format <FMT>` | `table` or `json`. While streaming, `json` is NDJSON — one compact array per line. | `table` |

### `port <SERVICE> <PRIVATE_PORT>`
Print the public binding for a port.

| Flag | Description | Default |
|---|---|---|
| `--proto <PROTO>` (alias `--protocol`) | `tcp` or `udp`. | `tcp` |
| `--index <N>` | Target this replica (1-based) of a scaled service. | 1 |

### `images`
List images used by services.

| Flag | Description | Default |
|---|---|---|
| `-q, --quiet` | Print image IDs only. | off |
| `--format <FMT>` | `table` or `json`. | `table` |

### `volumes [SERVICE...]`
List the project's named volumes (a trailing service list narrows it to volumes
those services mount).

| Flag | Description | Default |
|---|---|---|
| `-q, --quiet` | Print volume names only. | off |
| `--format <FMT>` | `table` or `json`. | `table` |

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
> inspect afterwards is gone — pass `--no-rm` to keep it.

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
container exits or you detach. Output only — stdin is never attached.

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
Block until the named service containers (default: all) stop, printing each
container's exit code as it does.

### `scale <SERVICE=N>...`
Set the number of running containers for one or more services, creating missing
replicas and removing surplus ones. A service that publishes a **fixed host
port** cannot be scaled past one replica — the command fails fast and tells you
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
skipped). Credentials come from `podman login`.

| Flag | Description | Default |
|---|---|---|
| `-q, --quiet` | Suppress the push progress output. | off |
| `--ignore-push-failures` | Continue after a failure. | off |
| `--tls-verify <BOOL>` | Verify the registry TLS cert; `false` allows an insecure/HTTP registry. | Podman default |

## Generate

### `generate quadlet`
Translate the compose file into Podman Quadlet unit files — one `.container` per
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
dropped — the same thing `docker compose config` does. Before 3.1.0 the key was
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
mismatch aborts before the installed binary is touched. See
[self-update.md](self-update.md) for the trust model.

### `autostart` (alias `boot`)
Manage a boot-time autostart unit for this compose project — rootless,
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
| `x-podman-on-failure` | under a service's `healthcheck:` | `none`, `kill`, `restart` or `stop` — what Podman does when the check flips to unhealthy. Default `none`. | yes |
| `noexec`, `nosuid`, `nodev` | under a long-form volume's `volume:` | mount-hardening flags; see [Per-mount hardening options](docker-migration.md#per-mount-hardening-options-noexec-nosuid-nodev). The short form carries them as raw mount options. | no |

### Healthcheck timing on a `service_healthy` gate

When `up` waits on `depends_on: {condition: service_healthy}`, podup drives the
check itself — Podman schedules its own runs through systemd transient timers,
which never fire on a host without systemd, so a purely passive wait would block
until the whole budget elapsed.

| | |
|---|---|
| how often the check is **run** | the healthcheck's `interval`, floored at **100ms** |
| how often the status is **read** | every 150ms |
| how long the wait lasts | `interval × retries` plus `start_period`, extended by `--wait-timeout` |

Running a check executes a command *inside* the container, so it happens no
faster than `interval` and the floor keeps `interval: 1ms` from becoming a
thousand executions a second. Reading the status is a plain inspect — it runs no
command — so it is cheap and frequent, and it is what notices promptly when
Podman's own timer flips the status between podup's runs.

A container that fails during the wait is reported as soon as the next read sees
it, rather than at the end of the budget.

Without it, a compose healthcheck detects a sick container and does nothing
about it: a restart policy reacts to the process *exiting*, not to the container
being unhealthy, so an app that hangs without dying stays in rotation
indefinitely.

> **`kill` and a restart policy do not combine the way you would expect.**
> `--health-on-failure=kill` with `restart: unless-stopped` leaves the container
> **exited and never revived** — the kill is not the kind of exit the restart
> policy acts on. Use `restart` if you want it to come back.
>
> podman-run(1) suggests `kill` or `stop` "when running inside of a systemd
> unit… to make use of systemd's restart policy". That advice assumes the unit
> restarts the container. `autostart --mode service` writes a
> `Type=oneshot` + `RemainAfterExit=yes` unit, which does **not** — so under
> podup's own service-mode unit that recommendation turns a degraded container
> into a stopped one.

An invalid value is rejected by `up`/`create`; `generate quadlet` warns and
omits the key instead, because an unrecognised `HealthOnFailure=` makes Quadlet
drop the whole unit at daemon-reload.

## Accepted for compatibility

These flags parse and are validated, so a script written against docker compose
runs unchanged — but podup does not act on them. They are listed here because
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
| other | `run` propagates the container's own exit code verbatim. |

An attached `up` also exits `1` when a log stream dies while its container is
still running, and `events` exits `1` when an unbounded feed ends at all. Both
are new in 3.3.0 and both used to exit `0`; see the streaming rule below.

`exec` propagates the command's exit code the same way `run` does, and `wait`
returns the last non-zero code it saw.

**`130` for SIGTERM as well as SIGINT.** The signal number would suggest 143 for
SIGTERM, but `docker compose up` returns 130 for both and podup matches it
(measured against v5.1.3 pointed at the same Podman socket). The project is
still torn down before the code is returned, so an interrupted `up` leaves
nothing running.

This matters most in CI: a job that runs `podup up` in the foreground and is
cancelled — by a timeout, by an operator, by the runner shutting down — used to
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
with a chunked terminator. A lost terminator — a dropped connection, or a
version that omits it — is indistinguishable from a real mid-stream break at the
transport layer, so the transport is not asked. Four of the five re-check
whether the container is still running: still running means live output was
truncated, and the command exits `1`. `run` reports the transport error instead
of an exit code, since the command it was running never produced one.

This matters for anything scraping them. `logs -f` used to return success after
losing its socket, so a monitor could not tell "the container finished" from "my
connection died" — and a script reading that `0` was already wrong, it just had
no way to know. `docker compose logs -f` exits `1` on the same failure (measured
against the same Podman socket), so the old `0` was a divergence rather than
parity.

A stream that ends because its containers stopped still exits `0`, as does
`logs` without `-f` and a `logs -f` whose reader closes the pipe (`| head`).
When the re-check itself cannot be made — usually because the same severed
connection is needed for it — the command fails rather than assuming the end was
clean.

**`events` decides it differently**, because a feed is project-scoped and
follows no single container, so there is nothing to re-check. What the caller
asked for answers it instead:

- **A bounded window** — `--since` and `--until` together, both already elapsed
  — closes on its own, so reaching the end exits `0`.
- **Anything else** is an unbounded feed. libpod never ends one, so *any* end
  means the stream was lost and the command exits `1` — including a clean end,
  which no check on the error shape could have caught.
- **A transport failure always fails**, bounded or not. A severed socket is not
  made expected by having asked for a window.
- Ctrl-C or SIGTERM kills the process by signal, as before; no error is
  invented.

Note that a window needs **both** ends and both must already have elapsed.
Measured against Podman 5.4.2: `--since -2h --until -1h` closes the feed, while
either flag alone leaves it open, as does any `--until` in the future. So
`--until 5m` follows indefinitely rather than stopping in five minutes. podup
warns when `--until` is passed without `--since`.

**`watch` is the exception.** A sync, rebuild, restart or exec that fails during
a watch session is reported as a warning and the session keeps going; `watch`
exits 0 unless it cannot start at all. This matches `docker compose watch` — a
long-running developer loop should not die because one rebuild failed — but it
does mean the exit code of a `watch` session says nothing about whether every
action in it succeeded. Read the warnings, not the status, and do not gate
automation on it.
