# Self-hosted release runner

Builds beisl releases for Codeberg on your own hardware.

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

1. **Enable Actions** on the repo: Codeberg → beisl → Settings → Units → Actions.
2. **Create the runner**: Settings → Actions → Runners → *Create new runner*. Codeberg
   hands you a **uuid** and a **token**:

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
   $EDITOR .env          # CACHE_DIR (a disk with ~10-15 GB free) + FORGEJO_UUID + FORGEJO_TOKEN
   docker compose up -d
   ```

   The entrypoint renders the config from `data/config.template.yml`, injecting the
   token from the environment into a **container-local** `/tmp` file — so the secret
   never lands on the host disk, only in `.env` (gitignored).
4. Check it appears under Settings → Actions → Runners with the label `beisl-local`.

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
