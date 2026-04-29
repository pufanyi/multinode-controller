use std::{
    fs::{self, File},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct ChildGuard {
    children: Vec<Child>,
}

impl ChildGuard {
    fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    fn push(&mut self, child: Child) {
        self.children.push(child);
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn authenticated_runtime_rejects_missing_tokens_and_runs_jobs() {
    let root = workspace_root();
    ensure_bins(&root);

    let temp = temp_dir();
    fs::create_dir_all(&temp).unwrap();
    let worker_token = temp.join("worker.token");
    let client_token = temp.join("client.token");
    fs::write(&worker_token, "worker-secret\n").unwrap();
    fs::write(&client_token, "client-secret\n").unwrap();

    let port = free_port();
    let coordinator_url = format!("ws://127.0.0.1:{port}");
    let coordinator = binary(&root, "agent-coordinator");
    let worker = binary(&root, "agent-worker");
    let agentctl = binary(&root, "agentctl");

    let mut children = ChildGuard::new();
    children.push(
        Command::new(&coordinator)
            .args([
                "--listen",
                &format!("127.0.0.1:{port}"),
                "--db",
                temp.join("coordinator.sqlite").to_str().unwrap(),
                "--worker-token-file",
                worker_token.to_str().unwrap(),
                "--client-token-file",
                client_token.to_str().unwrap(),
            ])
            .stdout(log_file(&temp, "coordinator.log"))
            .stderr(log_file(&temp, "coordinator.err"))
            .spawn()
            .unwrap(),
    );

    wait_for_server(&agentctl, &coordinator_url, &client_token);

    children.push(
        Command::new(&worker)
            .args([
                "--coordinator",
                &coordinator_url,
                "--node-name",
                "auth-test-worker",
                "--workspace-root",
                root.to_str().unwrap(),
                "--policy",
                root.join("examples/allow-all.yaml").to_str().unwrap(),
                "--token-file",
                worker_token.to_str().unwrap(),
            ])
            .stdout(log_file(&temp, "worker.log"))
            .stderr(log_file(&temp, "worker.err"))
            .spawn()
            .unwrap(),
    );

    let nodes = wait_for_worker(&agentctl, &coordinator_url, &client_token);
    assert!(nodes.contains("auth-test-worker"), "{nodes}");

    let no_token = agentctl_output(&agentctl, [&coordinator_url, "nodes"], None);
    assert!(!no_token.status.success());
    assert!(combined_output(&no_token).contains("401 Unauthorized"));

    let wrong_token = agentctl_output(
        &agentctl,
        [
            &coordinator_url,
            "--token-file",
            worker_token.to_str().unwrap(),
            "nodes",
        ],
        None,
    );
    assert!(!wrong_token.status.success());
    assert!(combined_output(&wrong_token).contains("401 Unauthorized"));

    let run = agentctl_output(
        &agentctl,
        [
            &coordinator_url,
            "--token-file",
            client_token.to_str().unwrap(),
            "run",
            "--nodes",
            "auth-test-worker",
            "--",
            "sh",
            "-lc",
            "echo auth-test-log",
        ],
        None,
    );
    assert!(run.status.success(), "{}", combined_output(&run));
    let run_text = combined_output(&run);
    let job_id = parse_started_job_id(&run_text);
    assert!(run_text.contains("auth-test-log"), "{run_text}");

    let jobs = agentctl_output(
        &agentctl,
        [
            &coordinator_url,
            "--token-file",
            client_token.to_str().unwrap(),
            "job",
            "list",
        ],
        None,
    );
    assert!(jobs.status.success(), "{}", combined_output(&jobs));
    assert!(combined_output(&jobs).contains(&job_id));

    let tail = agentctl_output(
        &agentctl,
        [
            &coordinator_url,
            "--token-file",
            client_token.to_str().unwrap(),
            "job",
            "tail",
            &job_id,
        ],
        None,
    );
    assert!(tail.status.success(), "{}", combined_output(&tail));
    assert!(combined_output(&tail).contains("auth-test-log"));

    let long_run = agentctl_output(
        &agentctl,
        [
            &coordinator_url,
            "--token-file",
            client_token.to_str().unwrap(),
            "run",
            "--nodes",
            "auth-test-worker",
            "--wait=false",
            "--",
            "sh",
            "-lc",
            "sleep 30",
        ],
        None,
    );
    assert!(long_run.status.success(), "{}", combined_output(&long_run));
    let long_job_id = parse_started_job_id(&combined_output(&long_run));
    let kill = agentctl_output(
        &agentctl,
        [
            &coordinator_url,
            "--token-file",
            client_token.to_str().unwrap(),
            "job",
            "kill",
            &long_job_id,
        ],
        None,
    );
    assert!(kill.status.success(), "{}", combined_output(&kill));
    assert!(combined_output(&kill).contains("kill requested"));

    let _ = fs::remove_dir_all(temp);
}

fn workspace_root() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    while path.file_name().and_then(|name| name.to_str()) != Some("debug") {
        path.pop();
    }
    path.pop();
    path.pop();
    path
}

fn ensure_bins(root: &Path) {
    let status = Command::new("cargo")
        .args(["build", "--workspace", "--bins"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
}

fn binary(root: &Path, name: &str) -> PathBuf {
    let suffix = std::env::consts::EXE_SUFFIX;
    root.join("target")
        .join("debug")
        .join(format!("{name}{suffix}"))
}

fn temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "agent-runtime-auth-test-{}-{nanos}",
        std::process::id()
    ))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn log_file(temp: &std::path::Path, name: &str) -> Stdio {
    Stdio::from(File::create(temp.join(name)).unwrap())
}

fn wait_for_server(agentctl: &Path, coordinator_url: &str, client_token: &Path) {
    for _ in 0..20 {
        let output = agentctl_output(
            agentctl,
            [
                coordinator_url,
                "--token-file",
                client_token.to_str().unwrap(),
                "nodes",
            ],
            None,
        );
        if output.status.success() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("coordinator did not become ready");
}

fn wait_for_worker(agentctl: &Path, coordinator_url: &str, client_token: &Path) -> String {
    for _ in 0..30 {
        let output = agentctl_output(
            agentctl,
            [
                coordinator_url,
                "--token-file",
                client_token.to_str().unwrap(),
                "nodes",
            ],
            None,
        );
        let text = combined_output(&output);
        if output.status.success() && text.contains("auth-test-worker") {
            return text;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("worker did not register");
}

fn agentctl_output<const N: usize>(
    agentctl: &Path,
    args: [&str; N],
    cwd: Option<&PathBuf>,
) -> Output {
    let mut command = Command::new(agentctl);
    command.arg("--coordinator").args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.output().unwrap()
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn parse_started_job_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("started "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing started job id in output:\n{output}"))
        .to_owned()
}
