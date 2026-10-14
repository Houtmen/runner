use crate::runtime_status::direct_chat_path;
use crate::session::process::{prepare_headless_fork, ProcessTree};
use crate::shell_path::LoginShellEnv;
use crate::usage::{parse_timestamp, percent, UnavailableReason, UsageWindow, FETCH_TIMEOUT};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

fn parse_codex_window(value: &Value) -> Option<UsageWindow> {
    let duration = value.get("windowDurationMins")?.as_u64()?;
    let name = match duration {
        299..=301 => "5 hours".to_owned(),
        10079..=10081 => "Week".to_owned(),
        _ => format!("{duration} min"),
    };
    Some(UsageWindow {
        name,
        used_percent: percent(value.get("usedPercent"))?,
        resets_at: parse_timestamp(value.get("resetsAt")),
    })
}

pub(crate) fn parse_codex(value: &Value) -> Result<Vec<UsageWindow>, UnavailableReason> {
    let limits = value
        .get("rateLimits")
        .ok_or(UnavailableReason::InvalidResponse)?;
    let mut windows: Vec<_> = ["primary", "secondary"]
        .into_iter()
        .filter_map(|name| limits.get(name).and_then(parse_codex_window))
        .collect();
    windows.sort_by_key(|window| match window.name.as_str() {
        "5 hours" => 0,
        "Week" => 1,
        _ => 2,
    });
    if windows.is_empty() {
        Err(UnavailableReason::InvalidResponse)
    } else {
        Ok(windows)
    }
}

pub(crate) fn fetch_codex(
    command: &str,
    env: &LoginShellEnv,
) -> Result<Vec<UsageWindow>, UnavailableReason> {
    fetch_codex_with_timeout(command, env, FETCH_TIMEOUT)
}

pub(crate) fn fetch_codex_with_timeout(
    command: &str,
    env: &LoginShellEnv,
    timeout: Duration,
) -> Result<Vec<UsageWindow>, UnavailableReason> {
    let mut process = Command::new(command);
    process
        .args([
            "-c",
            "approval_policy=never",
            "-c",
            "features.plugins=false",
            "-s",
            "read-only",
            "-a",
            "never",
            "app-server",
        ])
        .envs(&env.vars)
        .env("PATH", direct_chat_path(env))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = runner_core::app_paths::home_dir() {
        process.current_dir(home);
    }
    #[cfg(test)]
    if crate::golden::record_command(&process, None, timeout) {
        return Err(UnavailableReason::CodexNoAnswer);
    }
    prepare_headless_fork(&mut process);
    let mut child = process.spawn().map_err(|error| {
        log::warn!(
            "Codex usage spawn failed: {:?}, OS code {:?}",
            error.kind(),
            error.raw_os_error()
        );
        UnavailableReason::CodexLaunchFailed
    })?;
    let tree = ProcessTree::adopt(child.id()).map_err(|error| {
        log::warn!(
            "Codex usage process tracking failed: {:?}, OS code {:?}",
            error.kind(),
            error.raw_os_error()
        );
        let _ = child.kill();
        let _ = child.wait();
        UnavailableReason::CodexLaunchFailed
    })?;
    let result = (|| {
        let mut stdin = child.stdin.take().ok_or(UnavailableReason::CodexNoAnswer)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(UnavailableReason::CodexNoAnswer)?;
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        write_rpc(
            &mut stdin,
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"Runner","version":env!("CARGO_PKG_VERSION")}}}),
        ).map_err(|_| UnavailableReason::CodexInitializeFailed)?;
        let deadline = std::time::Instant::now() + timeout;
        let mut expected_id = 1;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(UnavailableReason::CodexTimedOut);
            }
            let phase_error = match expected_id {
                1 => UnavailableReason::CodexInitializeFailed,
                2 => UnavailableReason::CodexAccountFailed,
                _ => UnavailableReason::CodexRateLimitsFailed,
            };
            let line = rx
                .recv_timeout(remaining)
                .map_err(|error| match error {
                    std::sync::mpsc::RecvTimeoutError::Timeout => UnavailableReason::CodexTimedOut,
                    std::sync::mpsc::RecvTimeoutError::Disconnected => phase_error,
                })?
                .map_err(|_| phase_error)?;
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value.get("method").is_some()
                || value.get("id").and_then(Value::as_u64) != Some(expected_id)
            {
                continue;
            }
            if value.get("error").is_some() {
                if expected_id == 3 && value["error"]["code"].as_i64() == Some(-32602) {
                    // Older CLIs may not accept the background-poll parameter.
                    // Retry this read once, within the same deadline; never retry
                    // authentication/provider failures or enable reserve fallback.
                    write_rpc(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":4,"method":"account/rateLimits/read","params":{}}))
                        .map_err(|_| UnavailableReason::CodexRateLimitsFailed)?;
                    expected_id = 4;
                    continue;
                }
                // Only log the numeric protocol code, never account/error text or tokens.
                log::warn!(
                    "Codex usage RPC phase {} failed (code {:?})",
                    expected_id,
                    value["error"]["code"].as_i64()
                );
                return Err(phase_error);
            }
            match expected_id {
                1 => {
                    write_rpc(
                        &mut stdin,
                        serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
                    )
                    .map_err(|_| UnavailableReason::CodexInitializeFailed)?;
                    // Read the account in this exact process/home; never infer authentication
                    // from another role's chat or copy credentials from the standard home.
                    write_rpc(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":2,"method":"account/read","params":{"refreshToken":false}}))
                        .map_err(|_| UnavailableReason::CodexAccountFailed)?;
                    expected_id = 2;
                }
                2 => {
                    match value.get("result").and_then(|result| result.get("account")) {
                        Some(Value::Null) => return Err(UnavailableReason::SignIn),
                        Some(Value::Object(_)) => {}
                        _ => return Err(UnavailableReason::InvalidResponse),
                    }
                    write_rpc(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":3,"method":"account/rateLimits/read","params":{"excludeResetCreditDetails":true}}))
                        .map_err(|_| UnavailableReason::CodexRateLimitsFailed)?;
                    expected_id = 3;
                }
                _ => {
                    return parse_codex(
                        value
                            .get("result")
                            .ok_or(UnavailableReason::InvalidResponse)?,
                    )
                }
            }
        }
    })();
    shutdown_codex(&mut child, &tree);
    result
}

fn shutdown_codex(child: &mut std::process::Child, tree: &ProcessTree) {
    drop(child.stdin.take());
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let exited = child.try_wait().is_ok_and(|status| status.is_some());
        #[cfg(unix)]
        let drained = exited && unsafe { libc::kill(-(child.id() as i32), 0) } != 0;
        #[cfg(windows)]
        let drained = exited;
        if drained {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = tree.terminate();
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.wait();
}

fn write_rpc(writer: &mut impl Write, value: Value) -> Result<(), UnavailableReason> {
    serde_json::to_writer(&mut *writer, &value).map_err(|_| UnavailableReason::CodexNoAnswer)?;
    writer
        .write_all(b"\n")
        .map_err(|_| UnavailableReason::CodexNoAnswer)?;
    writer.flush().map_err(|_| UnavailableReason::CodexNoAnswer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_and_failure_phases_cross_real_piped_transport_without_agents() {
        let cases = [
            ("success", Ok(23.0)),
            ("legacy", Ok(23.0)),
            (
                "legacy-error",
                Err(UnavailableReason::CodexRateLimitsFailed),
            ),
            ("signed-out", Err(UnavailableReason::SignIn)),
            ("initialize", Err(UnavailableReason::CodexInitializeFailed)),
            ("account", Err(UnavailableReason::CodexAccountFailed)),
            ("limits", Err(UnavailableReason::CodexRateLimitsFailed)),
        ];
        for (case, expected) in cases {
            let root = tempfile::tempdir().unwrap();
            let initialize = if case == "initialize" {
                serde_json::json!({"id":1,"error":{"code":-32000}})
            } else {
                serde_json::json!({"id":1,"result":{}})
            };
            let account = match case {
                "signed-out" => serde_json::json!({"id":2,"result":{"account":null}}),
                "account" => serde_json::json!({"id":2,"error":{"code":-32000}}),
                _ => serde_json::json!({"id":2,"result":{"account":{"type":"chatgpt"}}}),
            };
            let limits = if matches!(case, "legacy" | "legacy-error") {
                serde_json::json!({"id":3,"error":{"code":-32602}})
            } else if case == "limits" {
                serde_json::json!({"id":3,"error":{"code":-32000}})
            } else {
                serde_json::json!({"id":3,"result":{"rateLimits":{"primary":{"usedPercent":23,"windowDurationMins":300}}}})
            };
            let compatibility = if case == "legacy" {
                Some(
                    serde_json::json!({"id":4,"result":{"rateLimits":{"primary":{"usedPercent":23,"windowDurationMins":300}}}}),
                )
            } else if case == "legacy-error" {
                Some(serde_json::json!({"id":4,"error":{"code":-32602}}))
            } else {
                None
            };
            #[cfg(unix)]
            let (path, script) = {
                let extra = compatibility
                    .as_ref()
                    .map(|reply| format!("read line\nprintf '%s\\n' '{}'\n", reply))
                    .unwrap_or_default();
                (root.path().join("mock codex"), format!("#!/bin/sh\nread line\nprintf '%s\\n' '{}'\nread line\nread line\nprintf '%s\\n' '{}'\nread line\nprintf '%s\\n' '{}'\n{}", initialize, account, limits, extra))
            };
            #[cfg(windows)]
            let (path, script) = {
                // cmd's repeated set /p can discard buffered pipe lines. Use
                // Console.In's persistent reader behind a realistic batch shim.
                let extra = compatibility.as_ref().map(|reply| format!("[Console]::In.ReadLine() | Out-Null\r\n[Console]::Out.WriteLine('{}')\r\n", reply)).unwrap_or_default();
                let server = format!("[Console]::In.ReadLine() | Out-Null\r\n[Console]::Out.WriteLine('{}')\r\n[Console]::In.ReadLine() | Out-Null\r\n[Console]::In.ReadLine() | Out-Null\r\n[Console]::Out.WriteLine('{}')\r\n[Console]::In.ReadLine() | Out-Null\r\n[Console]::Out.WriteLine('{}')\r\n{}", initialize, account, limits, extra);
                std::fs::write(root.path().join("mock server.ps1"), server).unwrap();
                (root.path().join("mock codex.cmd"), "@echo off\r\npowershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"%~dp0mock server.ps1\"\r\n".to_owned())
            };
            std::fs::write(&path, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let result = fetch_codex_with_timeout(
                path.to_str().unwrap(),
                &LoginShellEnv::default(),
                Duration::from_secs(10),
            )
            .map(|windows| windows[0].used_percent);
            assert_eq!(result, expected, "{case}");
        }
        assert_eq!(
            fetch_codex_with_timeout(
                "runner-missing-codex-fixture",
                &LoginShellEnv::default(),
                Duration::from_secs(1)
            ),
            Err(UnavailableReason::CodexLaunchFailed)
        );
    }
}
