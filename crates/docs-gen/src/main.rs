//! Render the generated packaging artifacts (the Makefile's `make generate`):
//! `docs/*.1.md` - the single-source user docs, whose tops the binaries embed as
//! `--help` - into man pages under `man/`, and the desktop entries into
//! `packaging/`, with a bare `Exec` because a packaged binary is on PATH.
//!
//! The inputs are pinned at compile time with `include_str!`, the same embed the
//! binaries use, so a stale working tree can't render pages that disagree with
//! the `--help` baked into the freshly built binaries.

use std::fs;
use std::process::ExitCode;

const PAGES: [(&str, &str); 4] = [
    ("BEISL", include_str!("../../../docs/beisl.1.md")),
    (
        "BEISL-SETUP",
        include_str!("../../../docs/beisl-setup.1.md"),
    ),
    ("BEISL-MCP", include_str!("../../../docs/beisl-mcp.1.md")),
    (
        "BEISL-COMPAT",
        include_str!("../../../docs/beisl-compat.1.md"),
    ),
];

/// The desktop entries a DISTRO package installs. Same bodies `beisl install`
/// writes, from the same consts, differing only in `Exec`: the package manager put
/// the binary on PATH, so there is no absolute path to bake in.
fn render_desktop() -> std::io::Result<()> {
    fs::create_dir_all("packaging")?;
    for (file, template, exec) in [
        (
            assets::desktop::SETUP_FILE,
            assets::desktop::SETUP,
            "beisl-setup",
        ),
        (
            assets::desktop::SESSION_FILE,
            assets::desktop::SESSION,
            "beisl-setup",
        ),
        (
            assets::desktop::COMPAT_FILE,
            assets::desktop::COMPAT,
            "beisl-compat",
        ),
    ] {
        fs::write(
            format!("packaging/{file}"),
            assets::desktop::render(template, exec),
        )?;
    }
    Ok(())
}

fn main() -> ExitCode {
    if let Err(e) = render_desktop() {
        eprintln!("docs-gen: desktop entries: {e}");
        return ExitCode::FAILURE;
    }
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

    /// The distro recipes carry the version in their own syntax, and a packager
    /// building a stale one ships binaries that disagree with their package. This
    /// is the same drift the metainfo's newest release guards against.
    #[test]
    fn the_packaging_recipes_name_the_version_being_built() {
        let v = env!("CARGO_PKG_VERSION");
        let pkgbuild = include_str!("../../../packaging/arch/PKGBUILD");
        assert!(
            pkgbuild.contains(&format!("pkgver={v}")),
            "PKGBUILD pkgver is not {v}"
        );
        for (name, spec) in [
            (
                "opensuse",
                include_str!("../../../packaging/opensuse/beisl.spec"),
            ),
            (
                "fedora",
                include_str!("../../../packaging/fedora/beisl.spec"),
            ),
        ] {
            assert!(
                spec.contains(&format!("Version:        {v}")),
                "{name} spec Version is not {v}"
            );
        }
    }
}
