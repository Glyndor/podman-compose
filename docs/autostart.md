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

`--mode` selects the backend. Both are rootless and user-scope; they differ in
who owns the containers.

| Mode | What it installs | Choose it when |
|---|---|---|
| `service` (default) | One `Type=oneshot` unit that runs `podup up -d` at boot and `podup stop` on shutdown. | You want the whole stack managed as a unit, the simplest option — one thing to enable, one to remove. |
| `quadlet` | One native Podman Quadlet unit per service (`.container`/`.build`/`.volume`/`.network`), which systemd owns directly. | You want per-container supervision — systemd restarts, ordering and status for each service independently. |

Service mode keeps the compose front-end (`.env`, interpolation, profiles) on the
runtime path; systemd starts `podup`, and `podup` reads the compose file. Quadlet
mode renders the stack to systemd units once, at install time, and hands them over
— after that systemd runs the containers with no `podup` process in the loop.

The two cannot coexist for one project: each install refuses if the other is
present, since both would bring the same stack up at boot.

## Commands

```bash
podup autostart install                  # service mode (default)
podup autostart install --mode quadlet   # quadlet mode
podup autostart install --no-start       # write the unit(s) but don't start yet
podup autostart install --dry-run        # print what would be written/run, change nothing

podup autostart status                   # this project's unit and session state
podup autostart uninstall                # remove whichever mode is installed
podup autostart uninstall --purge        # also tear the stack down and drop its volumes

podup autostart rebuild                   # quadlet only: rebuild every built image + restart
podup autostart rebuild web               # rebuild just one service
```

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
`Result=success` and nothing in the journal. Those versions behave exactly as they
did before, so nothing regresses on them; they simply get no ordering, which is
what they had.

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
