# Self-hosted runner

Builds beisl releases for Codeberg on your own hardware — and, if you point the labels at
them, the rest of the SpritzWine org too (SpritzWine and SpritzWine-Prater). It lives in
this repo because beisl needed it first, not because it is beisl's alone.

## Why

Codeberg's hosted runners cap a job's **runtime**:

| label | CPU | RAM | timeout |
|-------|-----|-----|---------|
| `codeberg-tiny`   | 1 | 2 GB | 2 min |
| `codeberg-small`  | 2 | 4 GB | 5 min |
| `codeberg-medium` | 4 | 8 GB | 10 min |

The `-lazy` variants only delay when a job *starts* (it may queue for up to 24 h) —
they do **not** extend the runtime. The GUI workspace is ~630 crates (libcosmic/wgpu)
built with thin LTO; it cannot finish in 10 minutes on 4 cores. Caching does not rescue
this: the *cold* build must still fit inside the cap once, and it never can.

So the release runs here instead. No cap, all the host's cores, and a persistent cargo
registry + target dirs — which is the caching we actually wanted, without `actions/cache`
(the job container is bare `rust:1`: no Node, so Node-based actions cannot run in it).

## Setup

1. **Enable Actions**: Codeberg → the repo → Settings → Units → Actions. (Per repo, for
   each repo this runner should serve.)
2. **Create the runner** — and mind *where* you create it, because the scope is fixed at
   registration and **no label can widen it later**:

   | created from | serves |
   |---|---|
   | a **repo**'s Settings → Actions → Runners | that repo, and nothing else |
   | the **org**'s Settings → Actions → Runners | *every* repo in the org |

   A repo-scoped runner does not reject another repo's jobs — it simply never claims
   them, and they queue forever with no error anywhere. If this box is meant to build
   more than beisl, register it from the **organisation** page.

   Either way Codeberg hands you a **uuid** and a **token**:

   ```yaml
   server:
     connections:
       forgejo:
         url: https://codeberg.org/
         uuid: d1d12684-...
         token: ...
   ```

   Runner v12 is *declarative* — those two values are the registration. There is no
   `forgejo-runner register` step any more (that command is deprecated), and the
   container keeps no `.runner` state file.
3. On the build host:

   ```sh
   cp .env.example .env
   $EDITOR .env          # CACHE_DIR + FORGEJO_UUID + FORGEJO_TOKEN + RUNNER_LABELS
   docker compose up -d
   ```

   The entrypoint renders the config from `data/config.template.yml`, injecting the
   token from the environment into a **container-local** `/tmp` file — so the secret
   never lands on the host disk, only in `.env` (gitignored).
4. Check it appears under Settings → Actions → Runners with the labels you configured.

## Serving the whole org

`RUNNER_LABELS` is a comma-separated list, so one daemon can serve every repo. `capacity: 1`
means they never race each other for cores — or, in Prater's case, for its warm work tree.

```
RUNNER_LABELS=beisl-local:docker://docker.io/library/rust:1,\
spritzwine-local:docker://spritzwine-build:fedora44,\
prater-local:docker://spritzwine-prater-build:steamrt4
```

Two things the other repos need from the host, both set in `.env`:

- **`CONTAINER_OPTIONS`** must add `--device /dev/ntsync` for Prater. Its smoke gate runs the
  PE sync fixtures under *both* wine sync backends; without the device the ntsync lane skips
  (loudly, but without failing) and the sync backend goes unguarded — which is exactly the
  thing that has already been misdiagnosed once here. Wants a **≥ 6.14 kernel**.
- **`JOB_TIMEOUT`** wants raising (12h): a *cold* Proton build is hours, not minutes.

The two build images are **local tags** — `force_pull: false` means they are used as-is and
never pulled — so they must exist on this host. Build each from its own repo (`./ci/rebuild.sh`
in SpritzWine, `./prater/ci/rebuild.sh` in Prater).

### Keeping them current (`image-sync.sh`)

A local tag has no upstream, so nothing can pull it and nothing notices it went stale. The CI
jobs *detect* it — they compare the stamp baked into the image against the stamp in the commit
and fail in seconds — but detection alone makes "rebuild the image" a chore a human must
remember, and until they do, **every lane is red**.

`image-sync.sh` is the fixer. Run it on this host after pushing a change to either build image:

```sh
./image-sync.sh
```

It fetches both branches, compares each repo's stamp against the stamp baked into the local
image, and rebuilds **only on drift** — docker caches every unchanged layer, so a no-op costs
seconds. Safe to run while a job is building: the job's container already exists from the old
image, and `docker image prune` only touches dangling (unreferenced) images.

**No timer, on purpose.** Polling the forge every few minutes for something that changes about
twice a year is a poor way to treat a donation-funded host, and it buys little — a stale image
is already a *five-second* failure that prints the exact command to fix it. If you want a safety
net regardless, wrap this script in a systemd timer with a **daily** interval.

The stamp itself hashes the Dockerfile **with comments stripped** (`ci/stamp.sh` /
`prater/ci/stamp.sh`), so editing documentation does not invalidate a 7.7 GB image on every
host that carries it — which it did, twice, on this CI's first day.

## Cutting a release

Unchanged — the workflow triggers on a `v*` tag whose version matches `Cargo.toml`:

```sh
git tag v0.2.0 && git push codeberg v0.2.0
```

If the runner happens to be offline, the job simply waits in Codeberg's queue until it
comes back. Releases are tag-rare, so the host does not need to be always-on.

## The cache

`CACHE_DIR` on the host is bind-mounted into every job container as `/cache`:

| path | holds |
|------|-------|
| `/cache/cargo`       | `CARGO_HOME` — registry + git checkouts |
| `/cache/target-cli`  | CLI workspace target dir (~37 crates) |
| `/cache/target-gui`  | GUI workspace target dir (~630 crates — the bulk) |

The workflow exports these as `CARGO_HOME`, `CLI_TARGET`, `GUI_TARGET`; the Makefile
honours the latter two (defaulting to the in-tree paths, so a plain local `make` is
unchanged). First release build is cold; every later one is incremental.

To reclaim the space, just delete `CACHE_DIR` — it is a pure cache.

## Notes

- `capacity: 1` — one job at a time. A release build wants every core, and two cargo
  builds sharing a target dir would just block on the same lock.
- The runner reaches Codeberg **outbound** over HTTPS; nothing needs to be exposed
  inbound, so this is safe to run on a home machine behind NAT.
- Use `forgejo-runner` (not Gitea's `act_runner`): they share an ancestor, but have
  diverged, and Codeberg is Forgejo.
- The container mounts the host's **docker socket** so job containers run as siblings
  on the host (which is what lets `CACHE_DIR` be a plain host path). That socket is
  root-equivalent on this machine — it is why the runner also runs as root, and why
  this belongs on a box you trust, not a shared one. The alternative (docker-in-docker)
  would isolate it, at the cost of the cache mount and a lot of complexity.
- Build on the arch you ship: the Makefile names the tarball after `uname -m`, so an
  arm64 host would happily publish an `aarch64` tarball that beisl's x86-64 audience
  (wine/Proton gaming rigs) cannot run.
