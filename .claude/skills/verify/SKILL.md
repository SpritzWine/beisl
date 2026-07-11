---
name: verify
description: Drive the beisl trace wrapper end-to-end without a real game — headless, isolated, scriptable fake game via sh -c.
---

# Verifying beisl

Build: `cargo build -p beisl` (main workspace only; never needs beisl-gui).

Drive the wrapper headlessly with isolated state (never touches the user's real
config/traces):

```sh
SCRATCH=$(mktemp -d)
XDG_DATA_HOME=$SCRATCH/data XDG_CONFIG_HOME=$SCRATCH/cfg BEISL_NO_UI=1 \
  ./target/debug/beisl sh -c '<fake game>'
```

- `BEISL_NO_UI=1` skips the session-window spawn (`spawn_monitor` in run.rs).
- Traces land in `$XDG_DATA_HOME/beisl/traces/run-<ts>/`; inspect `beisl.log`,
  `game.log`, `session.json` there.
- perf really records (needs host cap_perfmon on perf, ~4 MB for a 2 s run).

The fake game can observe its own environment and fake MangoHud behavior:

```sh
sh -c 'echo out; echo err >&2; echo "MHCFG=$MANGOHUD_CONFIG";
       dir="${MANGOHUD_CONFIG#*output_folder=}"; dir="${dir%%,*}";
       touch "$dir/FakeGame_2026-07-11_09-00-00.csv";   # triggers game-name detection
       sleep 2'                                          # outlive one 800 ms poll tick
```

To test GPU-pin paths, write `$XDG_CONFIG_HOME/beisl/config.toml`:

```toml
[gpu_pin]
name = "Intel Arc A770 — intel_icd"
icd_paths = ["/usr/share/vulkan/icd.d/intel_icd.x86_64.json"]
device_select = "8086:56a0!"   # vendor:device, resolved against /sys/class/drm at launch
```

Gotchas:
- `beisl <prog> <args…>` — no subcommand needed; leading `VAR=x` tokens are peeled.
- beisl propagates the child's exit code — don't measure it through a pipeline.
- Multi-GPU with no pin intentionally sets no `pci_dev` (see mangohud.rs) — a "no
  pci_dev appended" observation on such a box is correct behavior, not a failure.
- Game output is *teed*: it must land in `game.log` AND pass through on beisl's own
  stdout/stderr with stream identity preserved (that pass-through is the Lutris log
  window / Steam console). Check it by capturing beisl's stdout and stderr to two
  separate files. Straggler output from launcher descendants is only captured until
  beisl exits (bounded by the direct child + teardown) — by design.
