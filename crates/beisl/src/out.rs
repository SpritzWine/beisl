//! `out!` — `println!` that shrugs off a closed stdout (`beisl --check | head`,
//! `beisl install | …`): the reader hanging up mid-output is not an error worth a
//! panic, and std's `println!` aborts with a broken-pipe panic there. Drop the
//! remaining output, keep the exit code honest. stderr (`eprintln!`) stays as-is —
//! it remains open in the piped case, and errors there should still be loud.

macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}
pub(crate) use out;
