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

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("beisl-mcp {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
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
