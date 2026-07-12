//! `beisl-mcp` — the MCP server binary. Its own binary rather than a `beisl`
//! subcommand on purpose: the wrapper stays a wrapper for everyone who never uses
//! this, and nothing here runs (or listens anywhere — the transport is this
//! process's own stdio, no ports, no sockets) unless a user wires the binary into
//! their AI client's config:
//!
//! ```json
//! { "mcpServers": { "beisl": { "command": "beisl-mcp" } } }
//! ```

use std::io;
use std::process::ExitCode;

/// The user docs are the single source of truth: this is the man-page source
/// (rendered by `docs-gen` into `man beisl-mcp`), and `--help` prints its top —
/// cut at the help-end marker, `# ` section markers stripped for the terminal.
const DOC: &str = include_str!("../../../docs/beisl-mcp.1.md");

fn help_text() -> String {
    let top = DOC.split("<!-- help-end -->").next().unwrap_or(DOC);
    let lines: Vec<&str> = top
        .lines()
        .map(|l| l.strip_prefix("# ").unwrap_or(l))
        .collect();
    lines.join("\n").trim().to_string()
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("beisl-mcp {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h" | "help") => {
            println!("{}", help_text());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let server = beisl_mcp::Server::new(session::library::default_root());
    match server.serve(io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("beisl-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
