// Copyright (C) 2022 Naim A. <naim@abda.nl>
// forked and change by dDostalker <dDostalker@foxmail.com>

#![forbid(unsafe_code)]
#![warn(unused_crate_dependencies)]
#![deny(clippy::all)]

use clap::Arg;
use log::*;
use server::do_lumen;
use std::sync::Arc;

mod server;
mod web;

use common::config;

fn setup_logger() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", concat!(env!("CARGO_PKG_NAME"), "=info"));
    }
    pretty_env_logger::init_timed();
}

/// Ensure `LUMINA_TLS=false` for the current user.
///
/// - If unset: write `LUMINA_TLS=false`
/// - If set to `true`: rewrite as `false`
/// - Otherwise: leave alone
///
/// The change is persisted to the user environment so that newly-launched IDA
/// processes pick it up:
/// - Windows: via the built-in `setx` command.
/// - Linux:   by writing `export LUMINA_TLS=false` into the shell rc file.
///
/// A reminder is printed to the terminal whenever a change is made.
fn ensure_lumina_tls_disabled() {
    let current = std::env::var("LUMINA_TLS").ok();
    let needs_change = match current.as_deref() {
        None => true,
        Some(v) => v.trim().eq_ignore_ascii_case("true"),
    };
    if !needs_change {
        return;
    }

    // Update the current process env as well.
    std::env::set_var("LUMINA_TLS", "false");

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        match persist_lumina_tls_user_env() {
            Ok(detail) => info!("ADD LUMINA_TLS to USER PATH{detail}"),
            Err(e) => warn!("ADD LUMINA_TLS Fail: {e}, Add path LUMINA_TLS = false by self"),
        }
    }
}

#[cfg(target_os = "windows")]
fn persist_lumina_tls_user_env() -> Result<String, std::io::Error> {
    use std::process::Command;
    let out = Command::new("setx").args(["LUMINA_TLS", "false"]).output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("setx: {}", stderr.trim()),
        ));
    }
    Ok(String::new())
}

#[cfg(target_os = "linux")]
fn persist_lumina_tls_user_env() -> Result<String, std::io::Error> {
    let home = std::env::var("HOME")
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME Set"))?;

    let shell = std::env::var("SHELL").unwrap_or_default();
    let rc_rel = if shell.contains("zsh") { ".zshrc" } else { ".bashrc" };
    let rc_path = std::path::PathBuf::from(&home).join(rc_rel);

    let export_line = "export LUMINA_TLS=false";
    let needle = "export LUMINA_TLS=";
    let comment_needle = "#export LUMINA_TLS=";

    let content = std::fs::read_to_string(&rc_path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let mut found = false;

    for line in lines.iter_mut() {
        let t = line.trim_start();
        if t.starts_with(comment_needle) {
            continue;
        }
        if t.starts_with(needle) {
            *line = export_line.to_string();
            found = true;
            break;
        }
    }

    if !found {
        if !content.is_empty() && !content.ends_with('\n') {
            lines.push(String::new());
        }
        lines.push(export_line.to_string());
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }

    std::fs::write(&rc_path, out)?;
    Ok(format!("（writen {}）", rc_path.display()))
}

#[tokio::main]
async fn main() {
    setup_logger();
    ensure_lumina_tls_disabled();
    let matches = clap::Command::new("lumen")
        .version(env!("CARGO_PKG_VERSION"))
        .about("lumen is a private Lumina server for IDA.\nVisit https://github.com/naim94a/lumen/ for updates.")
        .author("Naim A. <naim@abda.nl>")
        .arg(
            Arg::new("config")
                .short('c')
                .default_value("config.toml")
                .help("Configuration file path")
        )
        .get_matches();

    let config = {
        config::load_config(
            std::fs::File::open(matches.get_one::<String>("config").unwrap())
                .expect("failed to read config"),
        )
    };
    let config = Arc::new(config);

    do_lumen(config).await;
}
