//! `rex-nvim`: the Rust half of rex-nvim (file picker, daemon inventory,
//! config bridge, doctor wrapper). The nvim plugin and the Rex action/state
//! machine live in `lua/rex-nvim` and `rex/`.

mod candidates;
mod config;
mod extract;
mod fff;
mod gitscan;
mod inventory;
mod paths;
mod picker;
mod rex;
mod sessions;

use std::path::Path;
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: rex-nvim <command> [options]

commands:
  pick --session S --window W --block B --kind K --origin O --cwd C
  daemons [--json]
  daemons stop <key> | --all | --orphans [--force]
  config --json
  doctor [--with-agent NAME]";

enum CliError {
    Usage(String),
    Nvim(String),
    Runtime(anyhow::Error),
}

impl From<anyhow::Error> for CliError {
    fn from(err: anyhow::Error) -> Self {
        CliError::Runtime(err)
    }
}

impl From<serde_json::Error> for CliError {
    fn from(err: serde_json::Error) -> Self {
        CliError::Runtime(err.into())
    }
}

fn main() -> ExitCode {
    prepend_path();
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(CliError::Usage(message)) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        Err(CliError::Nvim(message)) => {
            eprintln!("rex-nvim: {message}");
            ExitCode::from(3)
        }
        Err(CliError::Runtime(err)) => {
            eprintln!("rex-nvim: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<u8, CliError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        return Err(CliError::Usage(USAGE.to_owned()));
    };
    match command.as_str() {
        "pick" => {
            ensure_nvim()?;
            let args = pick_args(&args[1..])?;
            Ok(picker::pick(args)? as u8)
        }
        "daemons" => {
            ensure_nvim()?;
            Ok(inventory::daemons_cmd(&args[1..])? as u8)
        }
        "config" => {
            if args[1..].iter().any(|arg| arg != "--json") {
                return Err(CliError::Usage("usage: rex-nvim config --json".to_owned()));
            }
            let config = config::load();
            println!("{}", serde_json::to_string(&config::to_json(&config))?);
            Ok(0)
        }
        "doctor" => doctor(&args[1..]),
        other => Err(CliError::Usage(format!("unknown command {other}\n{USAGE}"))),
    }
}

fn pick_args(args: &[String]) -> Result<picker::PickArgs, CliError> {
    let mut session = None;
    let mut window = None;
    let mut block = None;
    let mut kind = None;
    let mut origin = None;
    let mut cwd = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let Some(value) = iter.next() else {
            return Err(CliError::Usage(format!(
                "missing value for {flag}\n{USAGE}"
            )));
        };
        match flag.as_str() {
            "--session" => session = Some(value.clone()),
            "--window" => window = Some(value.clone()),
            "--block" => block = Some(value.clone()),
            "--kind" => kind = Some(value.clone()),
            "--origin" => origin = Some(value.clone()),
            "--cwd" => cwd = Some(value.clone()),
            other => return Err(CliError::Usage(format!("unknown option {other}\n{USAGE}"))),
        }
    }
    Ok(picker::PickArgs {
        session: required(session, "--session")?,
        window: required(window, "--window")?,
        block: required(block, "--block")?,
        kind: required(kind, "--kind")?,
        origin: required(origin, "--origin")?,
        cwd: required(cwd, "--cwd")?,
    })
}

fn required(value: Option<String>, flag: &str) -> Result<String, CliError> {
    value.ok_or_else(|| CliError::Usage(format!("missing {flag}\n{USAGE}")))
}

fn doctor(args: &[String]) -> Result<u8, CliError> {
    let mut with_agent: Option<String> = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        if flag == "--with-agent" {
            let Some(value) = iter.next() else {
                return Err(CliError::Usage(
                    "usage: rex-nvim doctor [--with-agent NAME]".to_owned(),
                ));
            };
            with_agent = Some(value.clone());
        } else {
            return Err(CliError::Usage(
                "usage: rex-nvim doctor [--with-agent NAME]".to_owned(),
            ));
        }
    }
    let install = paths::load_install();
    let root = install.root.ok_or_else(|| {
        CliError::Runtime(anyhow::anyhow!(
            "install.json is missing; run install.sh first (looked in {})",
            paths::install_json_path(&paths::state_dir()).display()
        ))
    })?;
    let script = Path::new(&root).join("rex/doctor.lua");
    let mut command = Command::new(rex::bin());
    command.arg("do").arg(&script);
    if let Some(agent) = with_agent {
        command.arg(format!("with_agent={agent}"));
    }
    let status = command
        .status()
        .map_err(|err| CliError::Runtime(anyhow::anyhow!("failed to run {}: {err}", rex::bin())))?;
    Ok(status.code().unwrap_or(1).clamp(0, 255) as u8)
}

fn ensure_nvim() -> Result<(), CliError> {
    let bin = config::load().sidebar.nvim_bin;
    if find_executable(&bin) {
        Ok(())
    } else {
        Err(CliError::Nvim(format!(
            "nvim not found: {bin:?} is not executable (set [sidebar] nvim_bin in {} or install nvim)",
            config::config_file_display()
        )))
    }
}

fn find_executable(bin: &str) -> bool {
    if bin.contains('/') {
        return Path::new(bin).is_file();
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

/// Rex may inherit a minimal PATH; the daemons, picker, and probes need the
/// usual tool locations.
fn prepend_path() {
    let mut parts: Vec<std::path::PathBuf> = [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect();
    if let Some(home) = paths::home_dir() {
        parts.push(home.join(".local/bin"));
        parts.push(home.join(".cargo/bin"));
    }
    if let Some(existing) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(parts) {
        std::env::set_var("PATH", joined);
    }
}
