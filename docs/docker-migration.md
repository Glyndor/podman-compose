# Migrating from Docker Compose to podup

Point podup at an existing `docker-compose.yml` and run it — no rewrite, no new
file format. The short answer is: **most files work without any changes**.

```sh
cd my-project          # the directory holding docker-compose.yml
podup up
```

podup reads the same `compose.yaml` / `docker-compose.yml` files docker-compose
does and runs them on rootless Podman. The rest of this guide is the full
picture: what works unchanged, what behaves differently under Podman, what is
accepted but has no effect, and what is not yet supported.

## Works out of the box

Every core Compose spec key is supported:

| Category | Keys |
|---|---|
| Images / build | `image`, `build` (context, dockerfile, args, target, cache_from, cache_to, secrets, labels, network) |
| Execution | `command`, `entrypoint`, `working_dir`, `user`, `platform`, `tty`, `stdin_open` |
| Environment | `environment`, `env_file` (dotenv format), variable substitution (`${VAR:-default}`) |
| Networking | `ports`, `expose`, `networks`, `network_mode`, `hostname`, `domainname`, `dns`, `dns_search`, `extra_hosts` |
| Volumes | `volumes` (bind, named, tmpfs, npipe), `tmpfs`, `volumes_from` |
| Secrets / configs | `secrets`, `configs` (file, inline content, environment source, and `external: true` Podman-native secrets) |
| Dependencies | `depends_on` (with `condition:` — `service_started`, `service_healthy`, `service_completed_successfully`) |
| Health checks | `healthcheck` (test, interval, timeout, retries, start_period, disable) |
| Lifecycle hooks | `post_start`, `pre_stop` |
| Restart policies | `restart`, `deploy.restart_policy` (`condition`, `max_attempts`) |
| Replicas / scale | `deploy.replicas`, `scale:` |
| Resource limits | `deploy.resources`, `mem_limit`, `cpus`, `cpu_shares`, `cpu_quota`, `pids_limit`, `ulimits`, `blkio_config` |
| Devices | `devices`, `device_cgroup_rules` |
| GPU | `deploy.resources.reservations.devices`, `gpus:` — see the note below |
| Security | `cap_add`, `cap_drop`, `security_opt`, `read_only`, `privileged`, `userns_mode` |
| Namespaces | `pid`, `ipc`, `uts`, `cgroup`, `shm_size` |
| Metadata | `labels`, `annotations`, `container_name`, `profiles` |
| Logging | `logging` (driver + options) |
| Compose features | `extends`, `include`, YAML anchors, x-extensions, `develop.watch` |

### GPU reservations are host-dependent

`deploy.resources.reservations.devices` and the `gpus:` shorthand are honored,
but only for **NVIDIA** GPUs, and only when the host exposes them through CDI
(the NVIDIA Container Toolkit must be installed and the CDI spec generated).
Reservations for other drivers or capabilities are warned about and skipped.
This is the common case Podman supports natively — but it depends on the host's
GPU driver and CDI setup, not on podup alone.

### External secrets and configs

A secret or config declared `external: true` is mounted from an existing
Podman secret rather than from a value in the project tree — the recommended
pattern for production credentials, since the secret material never appears in
the compose file or its history. (Every other source — inline
`content:`/`environment:` and `file:` alike — is injected as a Podman-native
secret too, never a host bind mount, but its value still lives in the compose
file or next to it.) Create the secret before running, exactly as you would with
`docker secret`:

```sh
printf '%s' "$DB_PASSWORD" | podman secret create db_password -
```

```yaml
services:
  db:
    image: postgres:16
    secrets:
      - db_password
secrets:
  db_password:
    external: true
```

The secret appears at `/run/secrets/db_password` (configs use the long-form
`target:` path). If the named Podman secret does not exist, `podup up` fails
fast rather than starting a container without it. Use a top-level `name:` when
the Podman secret is named differently from the compose reference.

## Behaves differently under rootless Podman

These are Podman behaviours, not podup limitations — they apply equally to any
Podman-based tool. Each one is something to expect, with the workaround.

### `network_mode: bridge`

| | |
|---|---|
| **What to expect** | docker-compose attaches the container to Docker's predefined shared `bridge` network. Podman reads `--network bridge` as "create a fresh, isolated bridge netns", so the container has outbound connectivity but **cannot reach its project siblings** by name or IP. |
| **Workaround** | Remove `network_mode: bridge` and let the container join the project's default network, or declare a shared `networks:` entry the services share. podup emits a warning when it sees `network_mode: bridge`. |

```yaml
# before — siblings unreachable under Podman
services:
  web:
    network_mode: bridge

# after — services share a network and resolve each other by name
services:
  web:
    networks: [app]
  api:
    networks: [app]
networks:
  app: {}
```

### Privileged ports (< 1024)

Rootless containers cannot bind host ports below 1024 unless the kernel allows
it:

```bash
# Allow ports down to 80 (persists; requires root once):
sudo sysctl net.ipv4.ip_unprivileged_port_start=80

# Or map through a higher port in your compose file:
ports:
  - "8080:80"
```

### UID/GID mapping

Containers run as your host user's UID inside a user namespace. If a container
image writes files with UID 0 (root inside the container), those files appear
owned by your user on the host. Bind-mount permissions reflect your host user's
access.

### `userns_mode: auto` gives each container its own UID range

Rootless Podman maps the container's root onto your user, so two containers of
one project share that mapping: a file one of them writes as UID 0 on a shared
volume is readable by the other, and a process that escaped either lands in the
same user. Podman's `--userns=auto` allocates each container a private range of
subordinate UIDs and GIDs instead. podup forwards the compose key as it is:

```yaml
services:
  web:
    image: registry.example.com/web:1.4.2
    userns_mode: auto
```

Two consequences to know before switching a service. It needs a range in
`/etc/subuid` and `/etc/subgid` for your user (`usermod --add-subuids
100000-165535 --add-subgids 100000-165535 <user>` on a host that has none;
most installs already have one, 65536 wide). And files a container writes to a
named volume are owned by an ID from its private range, so a bind mount the
host user expects to read back needs `:U` (Podman chowns the mount to the
container's mapping) or a volume shared only between containers. Podman
estimates the size of the range from the image; a service whose image expects
more IDs than that declares it, as `userns_mode: auto:size=65536`.

### Volume SELinux labels

On SELinux-enforcing systems, bind mounts require relabeling. Append `:z`
(shared) or `:Z` (private) to the volume spec:

```yaml
volumes:
  - ./data:/app/data:Z
```

### Per-mount hardening options (`noexec`, `nosuid`, `nodev`)

The short form carries these as raw mount options (`cache:/app/cache:noexec`),
and the long form accepts them as booleans under `volume:`:

```yaml
volumes:
  - type: volume
    source: cache
    target: /app/cache
    volume:
      noexec: true
      nosuid: true
      nodev: true
```

The long-form spelling is a podup extension (the Compose Specification defines
no per-mount hardening flags there), so a compose file using it is not portable
back to `docker compose`, which rejects the unknown keys. `generate quadlet`
carries the options into the exported unit.

### `network_mode: host`

Attaches the container to your user's network namespace, not a privileged host
namespace. Traffic is still limited to your user's capabilities.

### `network_mode: none`

Supported. The container gets a loopback interface only.

### `mac_address:` at the service level

The Compose spec deprecated the top-level `mac_address` field in favour of
per-network configuration. podup still honours it (for backward compatibility)
and applies it to the primary network, but logs a deprecation warning. Move it
under `networks:` to silence the warning:

```yaml
# before
services:
  web:
    mac_address: "02:42:ac:11:00:02"

# after
services:
  web:
    networks:
      default:
        mac_address: "02:42:ac:11:00:02"
```

## Accepted, and podup is more permissive

### A `depends_on` target behind an inactive profile

A service can declare `profiles:` so it stays out of a default `up`. If another
service that *is* being started declares `depends_on` on it, the two
declarations contradict each other: one says keep it out, the other says it is
required.

docker compose refuses the project:

```
service "web" depends on undefined service "db": invalid compose project
```

podup **starts the dependency**, and every service transitively reached that
way, so a service that is kept never points at one that was dropped.

```yaml
services:
  db:
    image: postgres:18-alpine
    profiles: ["debug"]      # not started by a plain `up`...
  web:
    image: nginx:alpine
    depends_on:
      - db                   # ...but web needs it, so podup starts it
```

Both forms of `depends_on` behave this way, short and long. When **both**
services carry the profile, podup agrees with docker compose and starts neither
— the profile is doing its job and nothing contradicts it.

**The Compose Specification does not decide this.** It says only that a service
with `profiles` "is only started if the profile is activated", and says nothing
about a dependency whose profile is inactive. Docker's own documentation treats
it as something the author must fix — the dependency has to be "in the same
profile, started separately, or not assigned to any profile" — so its rejection
is an implementation choice in a case the specification leaves open, not a rule
podup is breaking.

**What this means for you.** A compose file relying on this runs under podup and
is rejected by docker compose, so it is not portable back. If you want the
dependency to stay out, or you need the file to work under both tools, give the
dependant the same profile; then neither starts, everywhere. Note also that
docker compose's message is misleading — `db` is not undefined, it is defined
and filtered.

## Accepted but has no effect

These fields parse cleanly so existing compose files validate, but podup cannot
translate them on single-host rootless Podman. **podup emits a warning for each
one it finds**, so nothing is dropped silently. The list below is a set of
**examples, not exhaustive** — when in doubt, run `podup up` and read the
warnings (see [Seeing the warnings](#seeing-the-warnings)).

### Swarm / cluster orchestration

| Field | What it does in Swarm |
|---|---|
| `deploy.mode: global` | Run one replica per cluster node |
| `deploy.placement` | Constrain which nodes a service runs on |
| `deploy.update_config` | Rolling-update parallelism, delay, failure action |
| `deploy.rollback_config` | Automatic rollback behaviour |
| `deploy.endpoint_mode` | VIP vs DNS round-robin load balancing |
| `deploy.restart_policy.delay` / `.window` | No first-class Podman restart delay or attempt-counting window (`condition` and `max_attempts` *are* honored) |
| port long-form `mode:` (`ingress` / `host`) | Swarm ingress routing |

### BuildKit / buildx-only build options

`build.privileged`, `build.ssh`, `build.ulimits`, `build.isolation`,
`build.entitlements`, `build.provenance`, `build.sbom` — these have no libpod
build-API equivalent and are ignored.

### Windows / Hyper-V-only

`cpu_count`, `cpu_percent`, `credential_spec`, `isolation` — no rootless Podman
equivalent.

### Other parsed-but-ignored fields

| Field | Why it has no effect |
|---|---|
| `attach` | podup follows its own attach/detach logic for `up` log streaming |
| `use_api_socket` | no podup equivalent |
| `provider:` / top-level `models:` | podup runs no model runner; the service/model is not honored (see [Not yet supported](#not-yet-supported)) |
| volume long-form `driver_config` | not forwarded to Podman |
| `networks.*.enable_ipv4` | Podman networks enable IPv4 by default and expose no toggle |
| `networks.*.ipam.config[].aux_addresses` | not supported by Podman |
| service `networks.*.gw_priority` | not supported by Podman |
| `secrets`/`configs` `driver` / `template_driver` (on non-`external` defs) | external secret-store plugins (Vault, AWS SM, …) podup does not invoke; the secret/config is not staged |

## Not yet supported

| Feature | Status |
|---|---|
| `env_file.format` values other than `dotenv` | A **warning** is emitted (`format is not honored; podup always parses env files as dotenv`); the file is still read as dotenv |
| `provider:` / model-runner services (`provider`, top-level `models`) | Modeled and parsed, but **not honored** — a not-honored warning is emitted (these are no rootless-Podman equivalent, *not* "unknown key" errors) |
| Real-hardware smoke tests on macOS | Pending; the code paths exist but are untested on physical Apple hardware. Windows was validated end-to-end on real hardware (Windows 11, Podman 5.8.3), including the interactive `exec`/`run` path added in 3.0.0. |

## Nothing is dropped silently

podup follows the Compose spec's forward-compatibility rule — an unknown key is
never a hard error, so `x-*` extensions and newer compose fields keep parsing.
But anything podup cannot translate is **reported**, never silently ignored, so
a typo or an unmapped feature can't hide. At parse time podup warns about:

- unknown keys at the top level and inside every modeled object (services and
  their `healthcheck`, `deploy`, `develop.watch`; top-level `networks`,
  `networks.*.ipam`, and `volumes`) — usually a typo such as `enviroment:`;
- modeled fields it cannot honor on rootless Podman — the
  [accepted-but-has-no-effect](#accepted-but-has-no-effect) fields above.

This is what lets a compose file written for a newer Docker or Podman release
run under podup: unsupported additions surface as warnings instead of vanishing.

`extra_hosts` is accepted in both the list (`["host:ip"]`) and mapping
(`{host: ip}`) forms.

### Seeing the warnings

The `podup` CLI prints these warnings automatically. Run with
`RUST_LOG=podup=debug` to also see network creation and container lifecycle
events. See the [`RUST_LOG` reference in `commands.md`](commands.md#environment)
for the full level table.

If you embed podup as a **library**, `parse_file` itself stays quiet — call
`podup::collect_diagnostics` on the parsed file to obtain the same warnings and
surface them to your users.

## File references and path confinement

Compose files are **trusted input**, like a Makefile. Path-valued keys that the
spec resolves relative to the compose file — `extends.file`, `env_file`,
`label_file`, and `include` — may use `../` to reach files outside the project
directory, matching docker-compose. Absolute paths are accepted too (an
absolute `include:` is used as given, as in docker-compose). This is an
intentional divergence from a fully sandboxed parser: do not feed podup a
compose file from an untrusted source any more than you would `make -f` an
untrusted Makefile.

## Quick compatibility checklist

Before running `podup up` on an existing compose file:

- [ ] Replace `network_mode: bridge` with a shared `networks:` entry if services need to reach each other.
- [ ] Remove or remap any host ports below 1024 if running fully rootless.
- [ ] Add `:Z` to bind mounts on SELinux hosts if containers cannot write to them.
- [ ] Check for `mac_address:` at the service level — move to `networks:` to silence the warning.
- [ ] Check for Swarm-only `deploy.*` fields — they are harmless but can be cleaned up.
- [ ] Verify `env_file` uses dotenv format (key=value lines, `#` comments).
- [ ] For GPUs, confirm the host has an NVIDIA driver + CDI configured.
