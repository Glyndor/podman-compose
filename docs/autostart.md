# Rootless autostart

`podup autostart` keeps a compose stack running across reboots, entirely
**rootless and user-scope**: it writes only under `${XDG_CONFIG_HOME:-~/.config}`
and drives everything through `systemctl --user`. Nothing touches `/etc`, the
system systemd, or root. The stack runs as an unprivileged user, and systemd
brings it up at boot.

## Prerequisite: lingering

A user's systemd manager normally exists only while that user has a session, and
`/run/user/<uid>` is created at login and torn down at logout. A service account
never logs in, so without lingering there is no user manager at boot and the stack
never starts. Enable it once, as root:

```bash
sudo loginctl enable-linger appuser
loginctl show-user appuser --property=Linger   # → Linger=yes
```

Lingering starts the user manager at boot and creates `/run/user/<uid>` up front,
independent of any login. `podup autostart install` warns if lingering is off,
because the unit it writes will not start at boot until you enable it.

## Picking a mode

`--mode` selects the backend. All three are rootless and user-scope; they differ
in who owns the containers, and in whether the boot path reconciles.

| Mode | What it installs | Choose it when |
|---|---|---|
| `service` (default) | One `Type=oneshot` unit that runs `podup up -d` at boot and `podup stop` on shutdown. | You want the whole stack managed as a unit, the simplest option — one thing to enable, one to remove. |
| `quadlet` | One native Podman Quadlet unit per service (`.container`/`.build`/`.volume`/`.network`), which systemd owns directly. | You want per-container supervision — systemd restarts, ordering and status for each service independently. |
| `start` | One `Type=oneshot` unit whose `ExecStart` is `podman start`. Single-service projects only. | You want the boot to resume the container that already exists, with nothing else on the path. |

### Reconcile or restore

This is the axis that actually separates them, and it is worth being explicit
because two of the three sit on the same side of it.

`service` and `quadlet` both make the world match the file at boot. Service mode
keeps the compose front-end (`.env`, interpolation, profiles) on the runtime path:
systemd starts `podup`, and `podup` reads the compose file. Quadlet mode renders
the stack to systemd units once, at install time, and hands them over, after which
systemd runs the containers with no `podup` process in the loop, though Podman
still reconciles each `.container` against its unit.

`start` does neither. Podman is daemonless and its store survives a reboot, so
every setting was baked into the container definition when it was created.
`podman start` restores the lot with no compose file, no `.env`, no registry and
no build on the path.

The failure semantics are the reason to choose it rather than a side effect. A
container missing at boot means a deploy went wrong, and booting cannot fix a
broken deploy: `start` fails loudly in the journal, where `up -d` would rebuild it
silently. Deploy reconciles; boot restores.

### What `start` costs

**Single-service projects only.** `podman start` waits for nothing, so a project
with `depends_on` (and especially `condition: service_healthy`) needs ordering
between units, which is exactly what quadlet mode derives from the compose file.
Rather than reimplement that, `start` refuses a project with more than one service,
or one scaled past a single replica, and the error names the mode to use instead.

**Drift is caught at install, not at boot.** If `compose.yaml` changes after the
container was created, a boot would resume the old configuration. `podup` compares
the container's `podup.config-hash` label against what the file renders and refuses
to install over a mismatch, or over a container that does not exist yet.

That check runs when you install, because there is no `podup` at boot to run it —
which is the point of the mode, and therefore also its limit. Editing the compose
file after installing leaves the unit starting the old container silently. Run
`podup up -d` after any change to the file, exactly as you would to deploy it.

The three cannot coexist for one project: each install refuses if another is
present, since they would all bring the same stack up at boot.

## Commands

```bash
podup autostart install                  # service mode (default)
podup autostart install --mode quadlet   # quadlet mode
podup autostart install --no-start       # write the unit(s) but don't start yet
podup autostart install --dry-run        # print what would be written/run, change nothing
podup autostart install --mode service --auto-update daily   # service mode + a sibling timer

podup autostart status                   # this project's unit and session state
podup autostart uninstall                # remove whichever mode is installed
podup autostart uninstall --purge        # also tear the stack down and drop its volumes

podup autostart rebuild                   # quadlet only: rebuild every built image + restart
podup autostart rebuild web               # rebuild just one service
```

## Auto-update

`x-podman-autoupdate` on a service asks Podman's auto-update to keep the
image fresh. The three autostart modes pick a different executor, and which
executor runs where is the part that matters:

| Mode | Executor | How it is installed |
|---|---|---|
| `quadlet` | `podman-auto-update.timer` (ships with Podman) | nothing to do: Quadlet sets `AutoUpdate=<value>` on each `.container` and the bundled timer fires it. |
| `service` | a per-project `<unit>-update.timer` (`hourly`/`daily`/`weekly`) | `podup autostart install --mode service --auto-update <hourly\|daily\|weekly>`. Adds `<unit>-update.service` (oneshot that runs `podup up -d`) and the timer that fires it; uninstall removes both. |
| `start` | none | the boot path runs `podman start`, not `podup up`. `--auto-update` is rejected with `--mode start`. |

For stacks that are not under autostart at all (no `podup autostart
install` was run), the schedule is the same `podup up -d` line as the
service-mode timer, dropped in cron:

```
0 3 * * *  cd /srv/app && podup up -d
```

Without `--auto-update`, the install path produces exactly what it did before
the feature existed. The timer pair only appears when the flag is given, and
`autostart uninstall` removes all three units together.

`uninstall` detects which mode is installed and removes that one; you never pass
`--mode` to it. `rebuild` applies to quadlet mode: a Quadlet `.build` unit is
`Type=oneshot`, so an image only rebuilds when its build service is restarted, and
the container is then restarted to pick it up. Service mode has no `rebuild` — it
builds at deploy time, whenever you run `podup up`.

## Why `--user` and `default.target`

The unit is a `--user` unit wired into `default.target`, not the system
`multi-user.target`. `multi-user.target` is a system-manager concept and is inert
in the user instance, so ordering against it would imply a boot gate that never
fires. The same is true of `network-online.target`, which is why neither mode
names it.

Waiting for the network still has to happen somewhere, and Podman ships the piece
that makes it possible from a user unit:
`podman-user-wait-network-online.service`, a `Type=oneshot` unit that polls
`systemctl is-active network-online.target` until the system target comes up.
Both modes order against that shim. Quadlet mode gets it from Podman's generator
(`man podman-systemd.unit`, under *Implicit network dependencies*), which adds
`Wants=` and `After=` to every `.container` unit it converts. Service mode writes
its final unit itself, so `podup` puts the same two lines in directly:

```ini
[Unit]
Description=podup <project>
Wants=podman-user-wait-network-online.service
After=podman-user-wait-network-online.service
```

The wait earns its place in service mode more than it does under Quadlet, not
less. Quadlet's `ExecStart` starts a container; this one is `podup up -d`, which
may pull an image, and under rootless Podman pasta builds the container network
at start time.

Measured 2026-08-30: the shim first ships in Podman 5.3.0, while `podup`'s floor
is 5.0. On 5.0 through 5.2 systemd finds no such unit, drops the `Wants=` and
`After=` with `LoadState=not-found`, and starts the unit clean with
`Result=success` and nothing in the journal. Nothing regresses on those versions;
they simply get no ordering, which is what they had.

That silence is the problem with leaving it there. The unit file *reads* as
though it waits for the network, and on those versions it does not, with nothing
anywhere to say so. `podup autostart status` therefore asks:

```
network wait: podman-user-wait-network-online.service is loaded
```

or, when it is not:

```
network wait: podman-user-wait-network-online.service is not loadable, so the
              unit's network ordering is dropped silently (Podman ships it from 5.3.0)
```

Note for anyone changing that check: `systemctl show <unit> -p LoadState` exits
**0 whether or not the unit exists**, and reports the answer only in the
`LoadState=` string. That is the opposite of `is-active` and `is-enabled`, which
both exit 4 for an unknown unit. A guard written against the exit code reports
the shim as present in exactly the case it is missing.

## Running `systemctl --user` for a login-less account

An isolated service account has no login shell, so you cannot open a session for
it — `su - appuser` and `machinectl shell appuser@` both bounce, because they try
to launch a login shell that does not exist. With no session there is no D-Bus
session bus, and `systemctl --user` without a bus fails:

```
$ systemctl --user is-system-running
Failed to connect to user scope bus via local transport:
$DBUS_SESSION_BUS_ADDRESS and $XDG_RUNTIME_DIR not defined
```

Lingering already created the runtime directory, so the fix is to point
`systemctl` at it explicitly:

```bash
uid=$(id -u appuser)
ls -d /run/user/$uid          # exists thanks to lingering

sudo -u appuser env XDG_RUNTIME_DIR=/run/user/$uid \
     podup autostart install --mode quadlet
```

Every `systemctl --user` / `podup autostart` invocation for that account needs
`XDG_RUNTIME_DIR=/run/user/<uid>` in its environment. The same applies over SSH:
a non-login SSH command has no runtime dir set, so export it before running
`podup autostart`.

## One-time rootless setup

For a dedicated service account the account itself needs the usual rootless
Podman groundwork, done once:

- **Subordinate UID/GID ranges** — rootless Podman maps container users into the
  host user's subordinate ranges. Ensure the account has entries in
  `/etc/subuid` and `/etc/subgid` (e.g. `appuser:100000:65536`).
- **`podman system migrate` as the user** — run it **as the account**, never via
  `sudo` as root, so the migration writes the account's own storage config rather
  than root's:

  ```bash
  sudo -u appuser env XDG_RUNTIME_DIR=/run/user/$(id -u appuser) \
       podman system migrate
  ```

- **The Podman API socket** — `podman` is daemonless and needs no socket, so a
  fresh account can run `podman` fine and still have every `podup` command fail
  with a connection error. podup speaks the libpod API, so the socket has to be
  listening:

  ```bash
  sudo -u appuser env XDG_RUNTIME_DIR=/run/user/$(id -u appuser) \
       systemctl --user enable --now podman.socket
  ```

  The `env XDG_RUNTIME_DIR=…` is the same requirement as above and for the same
  reason: an account with no login shell has no user session, so `systemctl
  --user` cannot find the manager without being told where it lives.

After that, `podup autostart install` writes the unit(s), reloads the user
manager, and starts the stack; a reboot brings it back on its own.
