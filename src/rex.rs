//! Thin transport over the `rex` CLI: control-API calls, block-method calls,
//! pane capture, and OSC 9 notifications.

use std::process::Command;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::paths::load_install;

/// The `rex` executable: `install.json`'s recorded path, else PATH.
pub fn bin() -> String {
    load_install()
        .rex
        .filter(|rex| !rex.is_empty())
        .unwrap_or_else(|| "rex".to_owned())
}

fn run(args: &[&str]) -> Result<std::process::Output> {
    let output = Command::new(bin())
        .args(args)
        .output()
        .with_context(|| format!("failed to run {} {}", bin(), args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "rex {} failed ({}): {}{}",
            args.join(" "),
            output.status,
            stderr.trim(),
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                ""
            }
        );
    }
    Ok(output)
}

/// Call a control-API method attached to `session`.
pub fn api_call(session: &str, method: &str, args: &Value) -> Result<Value> {
    let text = serde_json::to_string(args)?;
    let output = run(&["api", "call", "-s", session, method, &text])?;
    parse_json(&output.stdout, method)
}

/// Call a method with no session attachment (`session.list`, `session.create`).
pub fn api_call_unattached(method: &str, args: &Value) -> Result<Value> {
    let text = serde_json::to_string(args)?;
    let output = run(&["api", "call", method, &text])?;
    parse_json(&output.stdout, method)
}

/// The block's screen + scrollback as plain text.
pub fn capture(session: &str, block: &str) -> Result<String> {
    let output = run(&["capture", "-s", session, "-b", block, "--trim", "--unwrap"])?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Show a desktop notification for `session`.
///
/// Writing OSC 9 into a block's terminal goes through the *input* direction and
/// pollutes whatever program runs there, so instead this creates an ephemeral
/// unplaced block whose program prints the OSC 9 on its output; the server turns
/// that into a `desktop_notification` and the block removes itself on exit.
/// `block` is unused: the notification is session-scoped.
pub fn notify(session: &str, _block: &str, message: &str) -> Result<()> {
    api_call_unattached("session.new_block", &notify_request(session, message))?;
    Ok(())
}

/// The `session.new_block` request that renders `message` as an OSC 9
/// notification. The text rides in argv (`$1`) so quotes, backslashes, and
/// newlines need no shell escaping; control characters are replaced because
/// BEL and ESC would end the OSC sequence early.
pub fn notify_request(session: &str, message: &str) -> Value {
    let body = sanitize_message(message);
    json!({
        "session_id": session,
        "flavor": "com.superlogical.terminal.shell",
        "label": "rex-nvim notify",
        "options": {
            "command": [
                "sh",
                "-c",
                "printf '\\033]9;%s\\007' \"$1\"; sleep 0.3",
                "sh",
                body,
            ],
            "shell": "none",
        },
    })
}

/// Replace control characters with spaces: they are invisible in a
/// notification and BEL/ESC terminate or corrupt the OSC 9 sequence.
fn sanitize_message(message: &str) -> String {
    message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn parse_json(bytes: &[u8], what: &str) -> Result<Value> {
    serde_json::from_slice(bytes).with_context(|| {
        format!(
            "{what} returned non-JSON output: {}",
            String::from_utf8_lossy(bytes)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_request_puts_message_in_argv_and_strips_controls() {
        let req = notify_request(
            "session:abc",
            "a \"quoted\" \\ path\nline2\u{7}and\u{1b}esc",
        );
        assert_eq!(req["session_id"], "session:abc");
        assert_eq!(req["flavor"], "com.superlogical.terminal.shell");
        assert_eq!(req["label"], "rex-nvim notify");
        assert_eq!(req["options"]["shell"], "none");

        let command = req["options"]["command"].as_array().unwrap();
        assert_eq!(command.len(), 5);
        assert_eq!(command[0], "sh");
        assert_eq!(command[1], "-c");
        assert_eq!(command[3], "sh");
        // Only the message argument carries the text; quotes, backslashes and
        // newlines survive verbatim (newlines become spaces).
        assert_eq!(command[4], "a \"quoted\" \\ path line2 and esc");
        let script = command[2].as_str().unwrap();
        assert!(script.contains("printf"));
        assert!(script.contains("\"$1\""));
        assert!(!script.contains("quoted"));

        // The JSON layer must escape, then restore, every shell-hostile byte.
        let encoded = serde_json::to_string(&req).unwrap();
        let decoded: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            decoded["options"]["command"][4],
            "a \"quoted\" \\ path line2 and esc"
        );
    }

    #[test]
    fn notify_body_sanitizes_every_control_char() {
        assert_eq!(sanitize_message("plain"), "plain");
        assert_eq!(sanitize_message("\u{1b}]9;evil\u{7}"), "]9;evil");
        assert_eq!(sanitize_message("  padded\n"), "padded");
        assert_eq!(sanitize_message("a\rb\tc"), "a b c");
    }
}
