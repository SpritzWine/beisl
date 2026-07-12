//! Render `docs/*.1.md` — the single-source user docs, whose tops the binaries
//! embed as `--help` — into man pages under `man/` (the Makefile's `make man`).
//!
//! The inputs are pinned at compile time with `include_str!`, the same embed the
//! binaries use, so a stale working tree can't render pages that disagree with
//! the `--help` baked into the freshly built binaries.

use std::fs;
use std::process::ExitCode;

const PAGES: [(&str, &str); 3] = [
    ("BEISL", include_str!("../../../docs/beisl.1.md")),
    (
        "BEISL-SETUP",
        include_str!("../../../docs/beisl-setup.1.md"),
    ),
    ("BEISL-MCP", include_str!("../../../docs/beisl-mcp.1.md")),
];

fn main() -> ExitCode {
    if let Err(e) = render() {
        eprintln!("docs-gen: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn render() -> std::io::Result<()> {
    fs::create_dir_all("man")?;
    for (title, md) in PAGES {
        // The help-end marker is --help's cut point, not content — drop it so it
        // can't surface as a stray HTML comment in the roff.
        let md = md.replace("<!-- help-end -->", "");
        let out = format!("man/{}.1", title.to_lowercase());
        fs::write(&out, mandown::convert(&md, title, 1))?;
        println!("docs-gen: {out}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_renders_to_roff_with_its_title() {
        for (title, md) in PAGES {
            let roff = mandown::convert(&md.replace("<!-- help-end -->", ""), title, 1);
            assert!(roff.contains(title), "{title}: no .TH title in output");
            assert!(roff.contains(".SH"), "{title}: no sections rendered");
            assert!(
                !roff.contains("help-end"),
                "{title}: marker leaked into roff"
            );
        }
    }

    #[test]
    fn every_page_has_the_help_marker_and_core_sections() {
        for (title, md) in PAGES {
            assert!(
                md.contains("<!-- help-end -->"),
                "{title}: no help-end marker"
            );
            for sect in ["# NAME", "# SYNOPSIS", "# SEE ALSO"] {
                assert!(md.contains(sect), "{title}: missing {sect}");
            }
        }
    }
}
