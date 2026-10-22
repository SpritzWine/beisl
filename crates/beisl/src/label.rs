//! `beisl label`: say what a spike really was, so the likely causes learn from it.
//!
//! The headless door to the same label store the session window and beisl-mcp write.
//! A label from here is the user's own: full weight, and ground truth for the gate that
//! decides which learned layer may speak.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use artifact::TraceArtifact;
use hypothesis::{Cause, Judge, Label, LabelSource, LabelStore, ModelStore, Review};

use crate::out::out;

pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None => usage(),
        Some("--stats") => stats(),
        Some("--retrain") => retrain(),
        Some(run) => {
            let Some(dir) = run_dir(run) else {
                eprintln!("beisl: no trace run at {run}");
                return ExitCode::from(2);
            };
            let art = match TraceArtifact::read_json(&dir.join(session::library::ARTIFACT_FILE)) {
                Ok(a) => a,
                Err(_) => {
                    eprintln!(
                        "beisl: {} is not analyzed yet; run `beisl symbols {}` first",
                        dir.display(),
                        dir.display()
                    );
                    return ExitCode::from(2);
                }
            };
            match &args[1..] {
                [] => list(&dir, &art),
                [n, rest @ ..] => {
                    let Some(i) = n.parse::<usize>().ok().filter(|&i| i >= 1) else {
                        eprintln!("beisl: {n} is not a spike number (1, 2, ...)");
                        return ExitCode::from(2);
                    };
                    record(&dir, &art, i - 1, rest)
                }
            }
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "beisl: usage: beisl label <run> [<n> <cause> [--note TEXT] | <n> --confirm]\n       \
         beisl label --stats | --retrain\n\ncauses: {}",
        causes()
    );
    ExitCode::from(2)
}

fn causes() -> String {
    Cause::ALL.map(Cause::as_str).join(", ")
}

/// A run directory, or a bare `run-<n>` in the trace library.
fn run_dir(arg: &str) -> Option<PathBuf> {
    let p = Path::new(arg);
    if p.join(session::library::ARTIFACT_FILE).is_file() || p.join("perf.data").is_file() {
        return Some(p.to_path_buf());
    }
    let named = session::library::default_root().join(arg);
    named.is_dir().then_some(named)
}

fn run_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn list(dir: &Path, art: &TraceArtifact) -> ExitCode {
    let judge = Judge::load_default();
    out!(
        "{} | {} | {} worst spike(s)",
        run_name(dir),
        art.game,
        art.spikes.len()
    );
    if art.spikes.is_empty() {
        out!("\nNo spikes in this run: nothing to label.");
        return ExitCode::SUCCESS;
    }
    if art.spikes.iter().all(|s| s.features.is_empty()) {
        out!(
            "\nThis run was analyzed before beisl measured its spikes, so there is nothing \
             to label. Re-analyze it with `beisl symbols {}`; a run recorded before beisl \
             put frametimes and perf on one clock stays unmeasured.",
            dir.display()
        );
        return ExitCode::SUCCESS;
    }
    for (n, s) in art.spikes.iter().enumerate() {
        out!(
            "\n{:>2}  {:6.1} ms at {:.1}s",
            n + 1,
            s.frametime_ms,
            s.t_seconds
        );
        if s.features.is_empty() {
            out!("    not measured");
            continue;
        }
        let v = judge.judge(&s.features, &art.context());
        let ask = match v.review {
            Review::Confirm => "sure: confirm with --confirm",
            Review::Checklist => "unsure: which is it?",
        };
        out!("    {}  [{ask}]", v.line());
        if let Some(h) = &v.hypothesis {
            for e in &h.evidence {
                out!("      - {e}");
            }
        }
        if v.review == Review::Checklist {
            if v.candidates.is_empty() {
                out!(
                    "      the rules see no candidate here; say what it was: {}",
                    causes()
                );
            }
            for c in &v.candidates {
                out!("      [ ] {} ({:.0}%)", c.cause.as_str(), c.score * 100.0);
                for e in &c.evidence {
                    out!("            {e}");
                }
            }
        }
    }
    out!(
        "\nLabel one:   beisl label {} <n> <cause>   ({})\nConfirm one: beisl label {} <n> --confirm",
        run_name(dir),
        causes(),
        run_name(dir)
    );
    ExitCode::SUCCESS
}

fn record(dir: &Path, art: &TraceArtifact, i: usize, rest: &[String]) -> ExitCode {
    let Some(spike) = art.spikes.get(i) else {
        eprintln!("beisl: this run has {} spike(s)", art.spikes.len());
        return ExitCode::from(2);
    };
    if spike.features.is_empty() {
        eprintln!(
            "beisl: spike {} was not measured; there is nothing to learn from",
            i + 1
        );
        return ExitCode::from(2);
    }
    let note = rest
        .iter()
        .position(|a| a == "--note")
        .and_then(|k| rest.get(k + 1))
        .cloned()
        .unwrap_or_default();
    let cause = match rest.first().map(String::as_str) {
        Some("--confirm") => match Judge::load_default()
            .judge(&spike.features, &art.context())
            .hypothesis
        {
            Some(h) => h.cause,
            None => {
                eprintln!(
                    "beisl: nothing to confirm for spike {}: no likely cause yet. Say \
                         what it was: beisl label {} {} <cause>",
                    i + 1,
                    run_name(dir),
                    i + 1
                );
                return ExitCode::from(2);
            }
        },
        Some(c) => match Cause::parse(c) {
            Some(c) => c,
            None => {
                eprintln!("beisl: unknown cause {c}; one of {}", causes());
                return ExitCode::from(2);
            }
        },
        None => return usage(),
    };
    let label = Label {
        run: run_name(dir),
        t_seconds: spike.t_seconds,
        frametime_ms: spike.frametime_ms,
        features: spike.features.clone(),
        context: art.context(),
        cause,
        source: LabelSource::User,
        confirmed: true,
        note,
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    let store = LabelStore::default_path();
    if let Err(e) = store.append(&label) {
        eprintln!(
            "beisl: could not save the label in {} ({e})",
            store.path().display()
        );
        return ExitCode::FAILURE;
    }
    out!(
        "Spike {} of {} labelled {} ({}).",
        i + 1,
        run_name(dir),
        cause.as_str(),
        cause.describe()
    );
    stats()
}

fn stats() -> ExitCode {
    let store = LabelStore::default_path();
    let loaded = store.load();
    let count = |s: LabelSource| loaded.labels.iter().filter(|l| l.source == s).count();
    let (user, agent, harness) = (
        count(LabelSource::User),
        count(LabelSource::Agent),
        count(LabelSource::Harness),
    );
    let unconfirmed = loaded
        .labels
        .iter()
        .filter(|l| l.source == LabelSource::Agent && !l.confirmed)
        .count();
    let judge = Judge::load(&store, &ModelStore::default_path());
    let g = judge.gate();
    out!(
        "{} label(s): {user} yours, {agent} from an agent ({unconfirmed} unconfirmed), \
         {harness} from the harness  [{}]",
        g.labels,
        store.path().display()
    );
    if loaded.skipped > 0 {
        out!(
            "{} line(s) in it could not be read (a newer beisl, or damage) and are kept as they are",
            loaded.skipped
        );
    }
    let say = |on: bool, s: &hypothesis::learn::Score, what: &str| {
        let record = format!(
            "{} of {} right on your labels, the rules {} on the same",
            s.right, s.answered, s.rules_right
        );
        if on {
            format!("{what}: speak ({record})")
        } else if s.answered == 0 {
            format!("{what}: silent (no record yet)")
        } else {
            format!("{what}: silent ({record})")
        }
    };
    if judge.trained_at().is_none() {
        if g.labels > 0 {
            out!(
                "never trained: nothing is graded and the learned layers stay silent until \
                 `beisl label --retrain`"
            );
        }
        return ExitCode::SUCCESS;
    }
    out!(
        "rules: answered {} of your {} label(s), right on {}",
        g.rules.answered,
        g.trusted,
        g.rules.right
    );
    out!("{}", say(g.neighbours_on, &g.neighbours, "neighbours"));
    if g.trusted < hypothesis::learn::MIN_FOR_MODEL {
        out!(
            "model: silent (graded from {} of your labels; you have {})",
            hypothesis::learn::MIN_FOR_MODEL,
            g.trusted
        );
    } else {
        out!("{}", say(g.model_on, &g.model, "model"));
    }
    match judge.trained_at() {
        None if g.labels > 0 => {
            out!("never trained: the learned layers stay silent until `beisl label --retrain`")
        }
        None => {}
        Some(at) => {
            let new = judge.untrained_labels();
            out!(
                "trained {} ago{}",
                ago(at),
                if new > 0 {
                    format!(
                        "; {new} label(s) since already vote as neighbours, and count \
                         everywhere after `beisl label --retrain`"
                    )
                } else {
                    String::new()
                }
            );
        }
    }
    ExitCode::SUCCESS
}

/// Train on every label and save it: the slow part, only when asked for.
fn retrain() -> ExitCode {
    let (labels, model) = (LabelStore::default_path(), ModelStore::default_path());
    let started = std::time::Instant::now();
    match Judge::retrain(&labels, &model) {
        Ok(j) => {
            out!(
                "Trained on {} label(s) in {} ms.",
                j.gate().labels,
                started.elapsed().as_millis()
            );
            stats()
        }
        Err(e) => {
            eprintln!(
                "beisl: could not save the training to {} ({e})",
                model.path().display()
            );
            ExitCode::FAILURE
        }
    }
}

fn ago(at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let s = now.saturating_sub(at);
    match s {
        0..=119 => format!("{s} s"),
        120..=7199 => format!("{} min", s / 60),
        7200..=172_799 => format!("{} h", s / 3600),
        _ => format!("{} days", s / 86_400),
    }
}
