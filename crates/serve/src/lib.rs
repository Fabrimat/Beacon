//! `beacon serve` (3, 6.4, 8.1): one daemon per project worktree, shared by every client
//! (VS Code, other LSP editors, MCP agents, later the app). Phase-4 contract.
//!
//! ```text
//! editor / agent --stdio--> `beacon serve --stdio --lsp|--mcp` (shim, [`shim`])
//!                               | named pipe (Windows) / Unix socket, owner-only
//!                               v
//!                           daemon ([`daemon`]): one engine thread owning the Sessions,
//!                           the symbol table, open documents, the API index; one thread
//!                           per connection speaking its channel ([`lsp`], [`mcp`], control)
//! ```
//!
//! - **Handshake**: a client's first line is a [`Hello`] (newline-terminated JSON); the
//!   daemon answers one [`HelloReply`] line; afterwards the stream is the channel's own
//!   protocol: LSP (`Content-Length` framing), MCP (newline-delimited JSON-RPC 2.0, as the
//!   phase-2 `beacon mcp`), or control (newline-delimited [`ControlRequest`] / JSON reply).
//! - **Versions**: an LSP/MCP hello with a different `engine` or `protocol` gets `ok: false`
//!   with `ServeVersionMismatch`. The control channel is accepted across versions (its two
//!   requests are frozen), so the shim can send `stop` (non-forced): an idle daemon (no
//!   LSP/MCP client) stops and the shim starts its own, else the shim fails with that code.
//! - **Trust (5.5)** is decided per connection from the stored decision or the shim's own
//!   command line (`--trust`, which the user configured), never from protocol messages. A
//!   pipe/socket reachable only by the same user cannot escalate: that user can already run
//!   `beacon build --trust`.
//! - **Grants (8.1)**: MCP write tools need the operator's `--allow` on the shim command line
//!   ([`Grant`]); read tools need none.
//! - TCP (+ bearer token, Host/Origin checks) is for the app's webview only: phase 5.

pub mod daemon;
pub mod discovery;
pub mod lsp;
pub mod mcp;
pub mod shim;
pub mod transport;
pub mod watch;

/// Handshake protocol version; bump on any incompatible change of a channel.
pub const PROTOCOL_VERSION: u32 = 1;

/// Engine version a client and the daemon must agree on.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Lsp,
    Mcp,
    /// `beacon serve --status` / `--stop`, version-mismatch restarts.
    Control,
}

/// Operator grants for MCP write tools (`--allow edit,build,release,deploy`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Grant {
    /// Source edits: `rename_symbol`, `fix`, `add_content`.
    Edit,
    /// Writing build outputs: `build`.
    Build,
    /// Release versions in the lockfile: `pack`.
    Release,
    /// Writing into the game's `com.mojang`: `deploy`.
    Deploy,
}

impl Grant {
    /// Parses the shim's `--allow` value: a comma list of `edit`, `build`, `release`,
    /// `deploy`. There is deliberately no `all`: every grant is named by the operator.
    pub fn parse_list(s: &str) -> Result<Vec<Grant>, String> {
        let mut v = s
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(|g| {
                serde_json::from_value(serde_json::Value::String(g.into())).map_err(|_| {
                    format!("unknown grant `{g}` (expected edit, build, release or deploy)")
                })
            })
            .collect::<Result<Vec<Grant>, _>>()?;
        v.sort();
        v.dedup();
        Ok(v)
    }
}

/// Handshake and control lines are small; anything longer is not a client of ours.
pub(crate) const MAX_LINE: usize = 64 << 10;

/// Reads one newline-terminated line byte by byte, so nothing after it is consumed (the
/// channel's own protocol follows the handshake on the same stream). `None` at end of
/// stream before any byte.
pub(crate) fn read_line(r: &mut impl std::io::Read) -> std::io::Result<Option<String>> {
    let mut buf = vec![];
    let mut b = [0u8];
    loop {
        if r.read(&mut b)? == 0 {
            return Ok((!buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned()));
        }
        if b[0] == b'\n' {
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        if buf.len() >= MAX_LINE {
            return Err(std::io::Error::other("line too long"));
        }
        buf.push(b[0]);
    }
}

/// Writes `v` as one JSON line.
pub(crate) fn write_line(
    w: &mut impl std::io::Write,
    v: &impl serde::Serialize,
) -> std::io::Result<()> {
    let mut b = serde_json::to_vec(v)?;
    b.push(b'\n');
    w.write_all(&b)?;
    w.flush()
}

/// First line a client sends.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    /// [`PROTOCOL_VERSION`] of the client.
    pub beacon: u32,
    /// [`ENGINE_VERSION`] of the client.
    pub engine: String,
    pub channel: Channel,
    /// The shim was started with `--trust`.
    #[serde(default)]
    pub trust: bool,
    #[serde(default)]
    pub allow: Vec<Grant>,
}

/// The daemon's answer to [`Hello`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HelloReply {
    pub ok: bool,
    pub engine: String,
    pub protocol: u32,
    /// Set when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<HelloError>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HelloError {
    /// Diagnostic code id, e.g. `BCN0160`.
    pub code: String,
    pub message: String,
}

/// Control channel requests (one JSON line each; the reply is one JSON line).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum ControlRequest {
    /// Reply: `{"pid", "engine", "protocol", "clients", "uptimeMs", "rssBytes"}`.
    Status,
    /// Stops the daemon if no other client is attached (or `force`).
    Stop {
        #[serde(default)]
        force: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_wire_shape() {
        let h: Hello = serde_json::from_str(
            r#"{"beacon":1,"engine":"0.0.0","channel":"mcp","allow":["edit","deploy"]}"#,
        )
        .unwrap();
        assert_eq!(h.channel, Channel::Mcp);
        assert!(!h.trust);
        assert_eq!(h.allow, [Grant::Edit, Grant::Deploy]);
        let c: ControlRequest = serde_json::from_str(r#"{"method":"stop"}"#).unwrap();
        assert_eq!(c, ControlRequest::Stop { force: false });
    }

    #[test]
    fn allow_list() {
        assert_eq!(
            Grant::parse_list("deploy, edit,edit").unwrap(),
            [Grant::Edit, Grant::Deploy]
        );
        assert!(Grant::parse_list("all").is_err());
        assert!(Grant::parse_list("edit,write").is_err());
        assert!(Grant::parse_list("").unwrap().is_empty());
    }

    #[test]
    fn line_stops_at_newline() {
        let mut r: &[u8] = b"{\"a\":1}\nrest";
        assert_eq!(read_line(&mut r).unwrap().as_deref(), Some("{\"a\":1}"));
        assert_eq!(r, b"rest");
        assert_eq!(read_line(&mut r).unwrap().as_deref(), Some("rest"));
        assert_eq!(read_line(&mut r).unwrap(), None);
    }
}
