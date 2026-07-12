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

/// One screen — a server binary's help is mostly "how do I wire this in".
const HELP: &str = "\
beisl-mcp — read-only MCP server over recorded beisl traces (JSON-RPC on stdio)

usage:
  beisl-mcp             serve on stdin/stdout (an AI client starts this; it
                        opens no ports and touches nothing without a client)
  beisl-mcp --version   print the version
  beisl-mcp --help      this help

Wire it into an AI client's config:
  { \"mcpServers\": { \"beisl\": { \"command\": \"beisl-mcp\" } } }

Tools: list_traces, get_setup_status, get_artifact, get_report, get_log,
get_crash_summary — all read-only; analyzing and deleting stay in the app.";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("beisl-mcp {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h" | "help") => {
            println!("{HELP}");
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
