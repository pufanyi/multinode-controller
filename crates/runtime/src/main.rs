use std::{
    env,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
#[cfg(unix)]
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    flag as signal_flag,
};

#[derive(Parser, Debug)]
#[command(
    name = "agent-runtime",
    version,
    about = "Environment-driven launcher for the multinode agent runtime"
)]
struct Args {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    Start,
    Stop,
    Status,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Role {
    Auto,
    Master,
    Worker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Mode {
    Foreground,
    Tmux,
}

#[derive(Clone, Debug)]
struct Config {
    rank: String,
    role: Role,
    mode: Mode,
    session: String,
    port: String,
    listen: String,
    coordinator_url: String,
    runtime_dir: PathBuf,
    worker_token_file: PathBuf,
    client_token_file: PathBuf,
    db: PathBuf,
    node_name: String,
    policy: Option<PathBuf>,
    coordinator_bin: String,
    worker_bin: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResolvedRole {
    Master,
    Worker,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let config = Config::from_env()?;

    match args.command {
        CommandKind::Start => start(config),
        CommandKind::Stop => stop(config),
        CommandKind::Status => status(config),
    }
}

fn start(config: Config) -> Result<()> {
    let role = config.resolved_role()?;
    fs::create_dir_all(&config.runtime_dir)?;

    if role == ResolvedRole::Master {
        ensure_token(&config.worker_token_file)?;
        ensure_token(&config.client_token_file)?;
    } else {
        require_file(&config.worker_token_file, "worker token")?;
    }

    match config.mode {
        Mode::Foreground => start_foreground(&config, role),
        Mode::Tmux => start_tmux(&config, role),
    }
}

fn stop(config: Config) -> Result<()> {
    if config.mode != Mode::Tmux {
        bail!("agent-runtime stop is only available with MODE=tmux");
    }
    require_tmux()?;
    let status = Command::new("tmux")
        .args(["kill-session", "-t", &tmux_target(&config.session)])
        .status()
        .context("failed to run tmux kill-session")?;
    if !status.success() {
        bail!("tmux session {} was not stopped", config.session);
    }
    Ok(())
}

fn status(config: Config) -> Result<()> {
    if config.mode != Mode::Tmux {
        bail!("agent-runtime status is only available with MODE=tmux");
    }
    require_tmux()?;
    let status = Command::new("tmux")
        .args(["list-windows", "-t", &tmux_target(&config.session)])
        .status()
        .context("failed to run tmux list-windows")?;
    if !status.success() {
        bail!("tmux session {} was not found", config.session);
    }
    Ok(())
}

fn start_foreground(config: &Config, role: ResolvedRole) -> Result<()> {
    let terminate_requested = install_signal_handlers()?;
    match role {
        ResolvedRole::Master => {
            reset_log(&config.runtime_dir.join("coordinator.log"))?;
            reset_log(&worker_log_path(config))?;
            let mut coordinator = spawn_logged(
                coordinator_command(config),
                "coordinator",
                &config.runtime_dir.join("coordinator.log"),
            )?;
            thread::sleep(Duration::from_secs(1));
            let mut worker =
                spawn_logged(worker_command(config), "worker", &worker_log_path(config))?;
            println!("started role=master mode=foreground");
            print_runtime_info(config, true);
            wait_for_children(&mut coordinator, &mut worker, &terminate_requested)
        }
        ResolvedRole::Worker => {
            reset_log(&worker_log_path(config))?;
            println!("started role=worker mode=foreground");
            print_runtime_info(config, false);
            let mut worker =
                spawn_logged(worker_command(config), "worker", &worker_log_path(config))?;
            wait_for_child(&mut worker, "worker", &terminate_requested)
        }
    }
}

fn start_tmux(config: &Config, role: ResolvedRole) -> Result<()> {
    require_tmux()?;
    if tmux_has_session(&config.session)? {
        bail!("tmux session {} already exists", config.session);
    }

    match role {
        ResolvedRole::Master => {
            reset_log(&config.runtime_dir.join("coordinator.log"))?;
            reset_log(&worker_log_path(config))?;
            tmux_new_session(
                &config.session,
                "coordinator",
                &shell_command(
                    coordinator_command(config),
                    &config.runtime_dir.join("coordinator.log"),
                ),
            )?;
            thread::sleep(Duration::from_secs(1));
            tmux_new_window(
                &config.session,
                "worker",
                &shell_command(worker_command(config), &worker_log_path(config)),
            )?;
            println!("started role=master mode=tmux");
            print_runtime_info(config, true);
        }
        ResolvedRole::Worker => {
            reset_log(&worker_log_path(config))?;
            tmux_new_session(
                &config.session,
                "worker",
                &shell_command(worker_command(config), &worker_log_path(config)),
            )?;
            println!("started role=worker mode=tmux");
            print_runtime_info(config, false);
        }
    }

    Ok(())
}

impl Config {
    fn from_env() -> Result<Self> {
        let rank = env_or("RANK", "0");
        let port = env::var("PORT")
            .or_else(|_| env::var("MASTER_PORT"))
            .unwrap_or_else(|_| "8765".to_owned());
        let coordinator_addr = env::var("COORDINATOR_ADDR")
            .or_else(|_| env::var("MASTER_ADDR"))
            .unwrap_or_else(|_| "127.0.0.1".to_owned());
        let coordinator_url = env::var("COORDINATOR_URL")
            .unwrap_or_else(|_| format!("ws://{coordinator_addr}:{port}"));
        let listen = env::var("LISTEN").unwrap_or_else(|_| format!("0.0.0.0:{port}"));
        let runtime_dir = env_path("RUNTIME_DIR").unwrap_or_else(default_runtime_dir);
        let worker_token_file =
            env_path("WORKER_TOKEN_FILE").unwrap_or_else(|| runtime_dir.join("worker.token"));
        let client_token_file =
            env_path("CLIENT_TOKEN_FILE").unwrap_or_else(|| runtime_dir.join("client.token"));
        let db = env_path("DB").unwrap_or_else(|| runtime_dir.join("coordinator.sqlite"));
        let node_name =
            env::var("NODE_NAME").unwrap_or_else(|_| format!("{}-rank-{rank}", hostname_string()));
        let policy = env_path("POLICY");
        let session = env::var("SESSION").unwrap_or_else(|_| format!("agent-runtime-{port}"));
        let role = env_value("ROLE", Role::Auto)?;
        let mode = env_value("MODE", Mode::Foreground)?;
        let coordinator_bin = env_or("AGENT_COORDINATOR_BIN", "agent-coordinator");
        let worker_bin = env_or("AGENT_WORKER_BIN", "agent-worker");

        Ok(Self {
            rank,
            role,
            mode,
            session,
            port,
            listen,
            coordinator_url,
            runtime_dir,
            worker_token_file,
            client_token_file,
            db,
            node_name,
            policy,
            coordinator_bin,
            worker_bin,
        })
    }

    fn resolved_role(&self) -> Result<ResolvedRole> {
        match self.role {
            Role::Master => Ok(ResolvedRole::Master),
            Role::Worker => Ok(ResolvedRole::Worker),
            Role::Auto if self.rank == "0" => Ok(ResolvedRole::Master),
            Role::Auto => Ok(ResolvedRole::Worker),
        }
    }
}

fn coordinator_command(config: &Config) -> Vec<String> {
    vec![
        config.coordinator_bin.clone(),
        "--listen".to_owned(),
        config.listen.clone(),
        "--db".to_owned(),
        config.db.display().to_string(),
        "--worker-token-file".to_owned(),
        config.worker_token_file.display().to_string(),
        "--client-token-file".to_owned(),
        config.client_token_file.display().to_string(),
    ]
}

fn worker_command(config: &Config) -> Vec<String> {
    let mut command = vec![
        config.worker_bin.clone(),
        "--coordinator".to_owned(),
        config.coordinator_url.clone(),
        "--node-name".to_owned(),
        config.node_name.clone(),
        "--token-file".to_owned(),
        config.worker_token_file.display().to_string(),
    ];
    if let Some(policy) = &config.policy {
        command.push("--policy".to_owned());
        command.push(policy.display().to_string());
    }
    command
}

fn spawn_logged(command: Vec<String>, label: &str, log_path: &Path) -> Result<Child> {
    let Some((program, args)) = command.split_first() else {
        bail!("empty {label} command");
    };
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open {label} log {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("failed to clone {label} log {}", log_path.display()))?;
    Command::new(program)
        .args(args)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("failed to start {label} with {program}"))
}

fn wait_for_children(
    first: &mut Child,
    second: &mut Child,
    terminate_requested: &AtomicBool,
) -> Result<()> {
    loop {
        if termination_requested(terminate_requested) {
            terminate_child(first);
            terminate_child(second);
            bail!("received shutdown signal");
        }
        if let Some(status) = first.try_wait()? {
            terminate_child(second);
            bail!("coordinator exited with {status}");
        }
        if let Some(status) = second.try_wait()? {
            terminate_child(first);
            bail!("worker exited with {status}");
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_child(child: &mut Child, label: &str, terminate_requested: &AtomicBool) -> Result<()> {
    loop {
        if termination_requested(terminate_requested) {
            terminate_child(child);
            bail!("received shutdown signal");
        }
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("{label} exited with {status}");
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn termination_requested(terminate_requested: &AtomicBool) -> bool {
    terminate_requested.load(Ordering::SeqCst)
}

#[cfg(unix)]
fn install_signal_handlers() -> Result<Arc<AtomicBool>> {
    let terminate_requested = Arc::new(AtomicBool::new(false));
    signal_flag::register(SIGINT, Arc::clone(&terminate_requested))
        .context("failed to install SIGINT handler")?;
    signal_flag::register(SIGTERM, Arc::clone(&terminate_requested))
        .context("failed to install SIGTERM handler")?;
    Ok(terminate_requested)
}

#[cfg(not(unix))]
fn install_signal_handlers() -> Result<Arc<AtomicBool>> {
    Ok(Arc::new(AtomicBool::new(false)))
}

fn shell_command(command: Vec<String>, log_path: &Path) -> String {
    let command = command
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{command} >> {} 2>&1",
        shell_quote(&log_path.display().to_string())
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn tmux_new_session(session: &str, window: &str, command: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", session, "-n", window, command])
        .status()
        .context("failed to run tmux new-session")?;
    if !status.success() {
        bail!("failed to create tmux session {session}");
    }
    Ok(())
}

fn tmux_new_window(session: &str, window: &str, command: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args([
            "new-window",
            "-t",
            &tmux_target(session),
            "-n",
            window,
            command,
        ])
        .status()
        .context("failed to run tmux new-window")?;
    if !status.success() {
        bail!("failed to create tmux window {window}");
    }
    Ok(())
}

fn tmux_has_session(session: &str) -> Result<bool> {
    let status = Command::new("tmux")
        .args(["has-session", "-t", &tmux_target(session)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run tmux has-session")?;
    Ok(status.success())
}

fn require_tmux() -> Result<()> {
    let status = Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run tmux")?;
    if status.success() {
        Ok(())
    } else {
        bail!("tmux is required when MODE=tmux")
    }
}

fn tmux_target(session: &str) -> String {
    format!("={session}")
}

fn ensure_token(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() && fs::metadata(path)?.len() > 0 {
        return Ok(());
    }

    let token = generate_token()?;
    fs::write(path, token)?;
    set_private_permissions(path)?;
    Ok(())
}

fn generate_token() -> Result<String> {
    let output = Command::new("python3")
        .args(["-c", "import secrets; print(secrets.token_urlsafe(32))"])
        .output();
    match output {
        Ok(output) if output.status.success() => Ok(String::from_utf8(output.stdout)?),
        _ => {
            let output = Command::new("openssl")
                .args(["rand", "-base64", "32"])
                .output()
                .context("failed to generate token with python3 or openssl")?;
            if !output.status.success() {
                bail!("failed to generate token with python3 or openssl");
            }
            Ok(String::from_utf8(output.stdout)?)
        }
    }
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn require_file(path: &Path, label: &str) -> Result<()> {
    if path.exists() && fs::metadata(path)?.len() > 0 {
        Ok(())
    } else {
        bail!("missing {label}: {}", path.display())
    }
}

fn reset_log(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    File::create(path)?;
    Ok(())
}

fn worker_log_path(config: &Config) -> PathBuf {
    config
        .runtime_dir
        .join(format!("worker-{}.log", config.node_name))
}

fn print_runtime_info(config: &Config, master: bool) {
    println!("coordinator: {}", config.coordinator_url);
    println!("node: {}", config.node_name);
    println!("runtime dir: {}", config.runtime_dir.display());
    println!("port: {}", config.port);
    if master {
        println!("client token: {}", config.client_token_file.display());
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key).map(PathBuf::from)
}

fn env_value<T>(key: &str, default: T) -> Result<T>
where
    T: ValueEnum + Clone,
{
    let Ok(value) = env::var(key) else {
        return Ok(default);
    };
    T::from_str(&value, true).map_err(|_| anyhow!("invalid {key}={value}"))
}

fn default_runtime_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agent-runtime")
}

fn hostname_string() -> String {
    hostname::get()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|_| "node".to_owned())
}
