//! Owner tool: sends one `conversation_import` to a running daemon over its owner socket.
//!
//!   conversation_import --socket <agent.sock> [--created-at-ms <ms>] <restore.json>
//!
//! The file names `item_id`, `origin` and `turns` (`role`, `content`, optional
//! `created_at_ms`); other top-level keys (provenance notes) are ignored. `--created-at-ms`
//! dates every undated turn when the exact time is unknown. Nothing is sent through the mobile
//! gateway, and the daemon refuses anything but restored turns that fit before the item's
//! existing conversation. Re-running with the same file is a no-op.
use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut socket, mut at, mut file) = (None, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = Some(args.next().context("--socket needs a path")?),
            "--created-at-ms" => {
                at = Some(args.next().context("--created-at-ms needs a value")?.parse::<i64>()?)
            }
            _ if file.is_none() && !arg.starts_with("--") => file = Some(arg),
            _ => bail!("usage: conversation_import --socket <agent.sock> [--created-at-ms <ms>] <restore.json>"),
        }
    }
    let socket = std::path::PathBuf::from(socket.context("--socket is required")?);
    let input: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file.context("restore file is required")?)?)?;
    let turns: Vec<serde_json::Value> = input["turns"]
        .as_array()
        .context("turns must be an array")?
        .iter()
        .map(|turn| {
            let mut out = serde_json::json!({ "role": turn["role"], "content": turn["content"] });
            if let Some(ms) = turn.get("created_at_ms").filter(|v| !v.is_null()).cloned().or(at.map(Into::into)) {
                out["created_at_ms"] = ms;
            }
            out
        })
        .collect();
    let command: memex_agent::Command = serde_json::from_value(serde_json::json!({
        "command": "conversation_import",
        "item_id": input["item_id"],
        "origin": input["origin"],
        "turns": turns,
    }))
    .context("restore file does not match the conversation_import shape")?;
    let response = memex_agent::send_command(&socket, &command)?;
    if !response.ok {
        bail!("daemon refused: {}", response.reason_code.as_deref().unwrap_or("unknown"));
    }
    println!("{}", serde_json::to_string(&response.conversation_import)?);
    Ok(())
}
