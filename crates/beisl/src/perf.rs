//! CPU sampling via `perf`, scoped to the traced game's process tree.
//!
//! The *capture* is wine-agnostic — perf samples whatever runs. What differs by wine
//! flavor is *symbolication* (see the `symbol-resolver` crate + DESIGN.md fidelity
//! tiers), which happens afterward on the recorded `perf.data`.

use std::path::{Path, PathBuf};

pub struct Recorder {
    perf_data: PathBuf,
    // TODO: cgroup path for the child subtree; handle to the spawned `perf` process.
}

impl Recorder {
    pub fn new(run_dir: &Path) -> Self {
        Self {
            perf_data: run_dir.join("perf.data"),
        }
    }

    /// Begin sampling the subtree rooted at `pid`.
    pub fn attach(&mut self, pid: u32) {
        // TODO: create a fresh cgroup v2, migrate `pid` into it, then
        //   `perf record --cgroup <cg> -g -o <perf_data>` so we capture the whole
        //   game tree (proton -> wine -> game) and nothing else. Prerequisites to
        //   verify (DESIGN.md "The gate"): perf_event_paranoid low enough, and perf
        //   able to see PIDs inside the pressure-vessel container.
        let _ = (pid, &self.perf_data);
        eprintln!("beisl: perf attach pid={pid} (TODO: cgroup-scoped record)");
    }

    /// Stop sampling and flush `perf.data`.
    pub fn finish(&mut self) {
        // TODO: signal perf to stop, wait, flush.
        eprintln!("beisl: perf finish (TODO)");
    }
}
