use std::{collections::BTreeMap, path::PathBuf};

use agent_auth::{read_optional_token, websocket_request, AuthRole};
use agent_protocol::{
    ClientRequest, ClientResponse, CreateLeaseRequest, DiagnoseJobRequest, DurationSpec,
    JobDiagnosis, JobId, JobStatusRequest, JobSummary, KillJobRequest, LeaseId, LeaseSummary,
    ListJobsRequest, NodeId, NodeSummary, ReleaseLeaseRequest, RunProcessRequest, TailJobRequest,
    WireMessage,
};
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Parser, Debug)]
#[command(name = "agentctl", about = "CLI for the multinode agent runtime")]
struct Args {
    #[arg(long, global = true, default_value = "ws://127.0.0.1:8765")]
    coordinator: String,

    /// Bearer token file used to authenticate to the coordinator.
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Nodes,
    Inventory {
        #[command(subcommand)]
        command: InventoryCommand,
    },
    Health {
        #[command(subcommand)]
        command: HealthCommand,
    },
    Ml {
        #[command(subcommand)]
        command: MlCommand,
    },
    Lease {
        #[command(subcommand)]
        command: LeaseCommand,
    },
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
    Run {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,

        #[arg(long)]
        cwd: Option<PathBuf>,

        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        wait: bool,

        #[arg(long)]
        timeout_seconds: Option<u64>,

        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum InventoryCommand {
    Nodes,
    Gpu {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,
    },
    Cuda {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,
    },
    Torch {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum HealthCommand {
    Gpu {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,
    },
    Torch {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,
    },
    Nccl {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,

        #[arg(long)]
        lease: Option<String>,

        #[arg(long, default_value_t = 1)]
        gpus_per_node: u32,

        #[arg(long)]
        master_addr: Option<String>,

        #[arg(long, default_value_t = 29500)]
        master_port: u16,

        #[arg(long)]
        cwd: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum MlCommand {
    Torchrun {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,

        #[arg(long)]
        lease: Option<String>,

        #[arg(long, default_value_t = 1)]
        gpus_per_node: u32,

        #[arg(long)]
        master_addr: Option<String>,

        #[arg(long, default_value_t = 29500)]
        master_port: u16,

        #[arg(long)]
        cwd: Option<PathBuf>,

        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        wait: bool,

        #[arg(long)]
        timeout_seconds: Option<u64>,

        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum LeaseCommand {
    Create {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,

        #[arg(long)]
        count: Option<usize>,

        #[arg(long)]
        gpus_per_node: Option<u32>,

        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        exclusive: bool,
    },
    List,
    Release {
        lease_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum JobCommand {
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Tail {
        job_id: String,

        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    Kill {
        job_id: String,
    },
    Status {
        job_id: String,
    },
    Watch {
        job_id: String,

        #[arg(long, default_value_t = 5)]
        interval: u64,

        #[arg(long, default_value_t = 20)]
        lines: usize,
    },
    Diagnose {
        job_id: String,

        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = read_optional_token(args.token_file.as_ref())?;

    match args.command {
        Command::Nodes => {
            let nodes = fetch_nodes(&args.coordinator, token.as_deref()).await?;
            print_nodes(nodes);
        }
        Command::Inventory { command } => {
            handle_inventory(command, &args.coordinator, token.as_deref()).await?;
        }
        Command::Health { command } => {
            handle_health(command, &args.coordinator, token.as_deref()).await?;
        }
        Command::Ml { command } => {
            handle_ml(command, &args.coordinator, token.as_deref()).await?;
        }
        Command::Lease { command } => {
            handle_lease(command, &args.coordinator, token.as_deref()).await?;
        }
        Command::Run {
            nodes,
            cwd,
            wait,
            timeout_seconds,
            argv,
        } => {
            let run_request = ClientRequest::RunProcess(RunProcessRequest {
                nodes: nodes.into_iter().map(NodeId::from).collect(),
                argv,
                cwd,
                env: BTreeMap::new(),
                node_env: BTreeMap::new(),
                lease_id: None,
                timeout: timeout_seconds.map(|seconds| DurationSpec { seconds }),
                wait,
            });
            let responses = request(&args.coordinator, token.as_deref(), run_request).await?;
            print_run_responses(responses)?;
        }
        Command::Job { command } => match command {
            JobCommand::List { limit } => {
                let responses = request(
                    &args.coordinator,
                    token.as_deref(),
                    ClientRequest::ListJobs(ListJobsRequest { limit }),
                )
                .await?;
                for response in responses {
                    match response {
                        ClientResponse::Jobs(jobs) => {
                            println!("JOB\tSTATUS\tTASKS\tUPDATED");
                            for job in jobs {
                                let updated = job
                                    .updated_at
                                    .map(|time| time.to_rfc3339())
                                    .unwrap_or_else(|| "-".to_owned());
                                println!(
                                    "{}\t{}\t{}\t{}",
                                    job.job_id,
                                    job.status,
                                    job.tasks.len(),
                                    updated
                                );
                            }
                        }
                        ClientResponse::Error(message) => bail!(message),
                        _ => {}
                    }
                }
            }
            JobCommand::Tail { job_id, lines } => {
                let responses = request(
                    &args.coordinator,
                    token.as_deref(),
                    ClientRequest::TailJob(TailJobRequest {
                        job_id: JobId::from(job_id),
                        lines,
                    }),
                )
                .await?;
                for response in responses {
                    match response {
                        ClientResponse::Logs(logs) => {
                            for line in logs {
                                println!(
                                    "[{} {} {}] {}",
                                    line.node_id, line.task_id, line.stream, line.line
                                );
                            }
                        }
                        ClientResponse::Error(message) => bail!(message),
                        _ => {}
                    }
                }
            }
            JobCommand::Kill { job_id } => {
                let responses = request(
                    &args.coordinator,
                    token.as_deref(),
                    ClientRequest::KillJob(KillJobRequest {
                        job_id: JobId::from(job_id),
                    }),
                )
                .await?;
                for response in responses {
                    match response {
                        ClientResponse::Ack => println!("kill requested"),
                        ClientResponse::Error(message) => bail!(message),
                        _ => {}
                    }
                }
            }
            JobCommand::Status { job_id } => {
                let job =
                    fetch_job(&args.coordinator, token.as_deref(), JobId::from(job_id)).await?;
                print_job_status(&job);
            }
            JobCommand::Watch {
                job_id,
                interval,
                lines,
            } => {
                watch_job(
                    &args.coordinator,
                    token.as_deref(),
                    JobId::from(job_id),
                    interval,
                    lines,
                )
                .await?;
            }
            JobCommand::Diagnose { job_id, lines } => {
                let responses = request(
                    &args.coordinator,
                    token.as_deref(),
                    ClientRequest::DiagnoseJob(DiagnoseJobRequest {
                        job_id: JobId::from(job_id),
                        lines,
                    }),
                )
                .await?;
                for response in responses {
                    match response {
                        ClientResponse::JobDiagnosis(diagnosis) => {
                            print_job_diagnosis(diagnosis);
                        }
                        ClientResponse::Error(message) => bail!(message),
                        _ => {}
                    }
                }
            }
        },
    }

    Ok(())
}

async fn handle_inventory(
    command: InventoryCommand,
    coordinator: &str,
    token: Option<&str>,
) -> Result<()> {
    match command {
        InventoryCommand::Nodes => {
            let nodes = fetch_nodes(coordinator, token).await?;
            print_nodes(nodes);
        }
        InventoryCommand::Gpu { nodes } => {
            run_and_print(
                coordinator,
                token,
                nodes,
                None,
                inventory_gpu_argv(),
                BTreeMap::new(),
                None,
            )
            .await?;
        }
        InventoryCommand::Cuda { nodes } => {
            run_and_print(
                coordinator,
                token,
                nodes,
                None,
                inventory_cuda_argv(),
                BTreeMap::new(),
                None,
            )
            .await?;
        }
        InventoryCommand::Torch { nodes } => {
            run_and_print(
                coordinator,
                token,
                nodes,
                None,
                inventory_torch_argv(),
                BTreeMap::new(),
                None,
            )
            .await?;
        }
    }
    Ok(())
}

async fn handle_health(
    command: HealthCommand,
    coordinator: &str,
    token: Option<&str>,
) -> Result<()> {
    match command {
        HealthCommand::Gpu { nodes } => {
            run_and_print(
                coordinator,
                token,
                nodes,
                None,
                health_gpu_argv(),
                BTreeMap::new(),
                None,
            )
            .await?;
        }
        HealthCommand::Torch { nodes } => {
            run_and_print(
                coordinator,
                token,
                nodes,
                None,
                health_torch_argv(),
                BTreeMap::new(),
                None,
            )
            .await?;
        }
        HealthCommand::Nccl {
            nodes,
            lease,
            gpus_per_node,
            master_addr,
            master_port,
            cwd,
        } => {
            let selected_nodes =
                resolve_target_nodes(coordinator, token, nodes, lease.as_deref()).await?;
            let node_summaries = fetch_nodes(coordinator, token).await?;
            let master_addr = master_addr
                .unwrap_or_else(|| default_master_addr(&selected_nodes, &node_summaries));
            let (argv, node_env) = torchrun_request_parts(
                selected_nodes.clone(),
                gpus_per_node,
                &master_addr,
                master_port,
                nccl_smoke_argv(),
            );
            run_and_print_with_request(
                coordinator,
                token,
                RunProcessRequest {
                    nodes: selected_nodes,
                    argv,
                    cwd,
                    env: BTreeMap::new(),
                    node_env,
                    lease_id: lease.map(LeaseId::from),
                    timeout: None,
                    wait: true,
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn handle_ml(command: MlCommand, coordinator: &str, token: Option<&str>) -> Result<()> {
    match command {
        MlCommand::Torchrun {
            nodes,
            lease,
            gpus_per_node,
            master_addr,
            master_port,
            cwd,
            wait,
            timeout_seconds,
            argv,
        } => {
            let selected_nodes =
                resolve_target_nodes(coordinator, token, nodes, lease.as_deref()).await?;
            let node_summaries = fetch_nodes(coordinator, token).await?;
            let master_addr = master_addr
                .unwrap_or_else(|| default_master_addr(&selected_nodes, &node_summaries));
            let (argv, node_env) = torchrun_request_parts(
                selected_nodes.clone(),
                gpus_per_node,
                &master_addr,
                master_port,
                argv,
            );
            run_and_print_with_request(
                coordinator,
                token,
                RunProcessRequest {
                    nodes: selected_nodes,
                    argv,
                    cwd,
                    env: BTreeMap::new(),
                    node_env,
                    lease_id: lease.map(LeaseId::from),
                    timeout: timeout_seconds.map(|seconds| DurationSpec { seconds }),
                    wait,
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn handle_lease(command: LeaseCommand, coordinator: &str, token: Option<&str>) -> Result<()> {
    match command {
        LeaseCommand::Create {
            nodes,
            count,
            gpus_per_node,
            exclusive,
        } => {
            let responses = request(
                coordinator,
                token,
                ClientRequest::CreateLease(CreateLeaseRequest {
                    nodes: nodes.into_iter().map(NodeId::from).collect(),
                    count,
                    gpus_per_node,
                    exclusive,
                }),
            )
            .await?;
            for response in responses {
                match response {
                    ClientResponse::LeaseCreated(lease) => print_leases(vec![lease]),
                    ClientResponse::Error(message) => bail!(message),
                    _ => {}
                }
            }
        }
        LeaseCommand::List => {
            let leases = fetch_leases(coordinator, token).await?;
            print_leases(leases);
        }
        LeaseCommand::Release { lease_id } => {
            let responses = request(
                coordinator,
                token,
                ClientRequest::ReleaseLease(ReleaseLeaseRequest {
                    lease_id: LeaseId::from(lease_id),
                }),
            )
            .await?;
            for response in responses {
                match response {
                    ClientResponse::Ack => println!("lease released"),
                    ClientResponse::Error(message) => bail!(message),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn run_and_print(
    coordinator: &str,
    token: Option<&str>,
    nodes: Vec<String>,
    cwd: Option<PathBuf>,
    argv: Vec<String>,
    node_env: BTreeMap<NodeId, BTreeMap<String, String>>,
    lease_id: Option<LeaseId>,
) -> Result<()> {
    run_and_print_with_request(
        coordinator,
        token,
        RunProcessRequest {
            nodes: nodes.into_iter().map(NodeId::from).collect(),
            argv,
            cwd,
            env: BTreeMap::new(),
            node_env,
            lease_id,
            timeout: None,
            wait: true,
        },
    )
    .await
}

async fn run_and_print_with_request(
    coordinator: &str,
    token: Option<&str>,
    request_data: RunProcessRequest,
) -> Result<()> {
    let responses = request(coordinator, token, ClientRequest::RunProcess(request_data)).await?;
    print_run_responses(responses)
}

async fn fetch_nodes(coordinator: &str, token: Option<&str>) -> Result<Vec<NodeSummary>> {
    let responses = request(coordinator, token, ClientRequest::ListNodes).await?;
    for response in responses {
        match response {
            ClientResponse::Nodes(nodes) => return Ok(nodes),
            ClientResponse::Error(message) => bail!(message),
            _ => {}
        }
    }
    Ok(Vec::new())
}

async fn fetch_job(coordinator: &str, token: Option<&str>, job_id: JobId) -> Result<JobSummary> {
    let responses = request(
        coordinator,
        token,
        ClientRequest::JobStatus(JobStatusRequest { job_id }),
    )
    .await?;
    for response in responses {
        match response {
            ClientResponse::Job(job) => return Ok(job),
            ClientResponse::Error(message) => bail!(message),
            _ => {}
        }
    }
    bail!("coordinator did not return a job status")
}

async fn fetch_leases(coordinator: &str, token: Option<&str>) -> Result<Vec<LeaseSummary>> {
    let responses = request(coordinator, token, ClientRequest::ListLeases).await?;
    for response in responses {
        match response {
            ClientResponse::Leases(leases) => return Ok(leases),
            ClientResponse::Error(message) => bail!(message),
            _ => {}
        }
    }
    Ok(Vec::new())
}

async fn resolve_target_nodes(
    coordinator: &str,
    token: Option<&str>,
    nodes: Vec<String>,
    lease: Option<&str>,
) -> Result<Vec<NodeId>> {
    if !nodes.is_empty() {
        return Ok(nodes.into_iter().map(NodeId::from).collect());
    }
    if let Some(lease_id) = lease {
        let leases = fetch_leases(coordinator, token).await?;
        let Some(lease) = leases
            .into_iter()
            .find(|lease| lease.lease_id == LeaseId::from(lease_id))
        else {
            bail!("lease {lease_id} was not found");
        };
        return Ok(lease.nodes);
    }

    let nodes = fetch_nodes(coordinator, token).await?;
    Ok(nodes.into_iter().map(|node| node.node_id).collect())
}

fn print_nodes(nodes: Vec<NodeSummary>) {
    if nodes.is_empty() {
        println!("no workers connected");
    } else {
        println!("NODE\tHOSTNAME\tONLINE\tTASKS\tLAST_SEEN");
        for node in nodes {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                node.node_id,
                node.hostname,
                node.online,
                node.running_tasks,
                node.last_seen.to_rfc3339()
            );
        }
    }
}

async fn watch_job(
    coordinator: &str,
    token: Option<&str>,
    job_id: JobId,
    interval: u64,
    lines: usize,
) -> Result<()> {
    let interval = interval.max(1);
    loop {
        let job = fetch_job(coordinator, token, job_id.clone()).await?;
        print_job_status(&job);
        if lines > 0 {
            let responses = request(
                coordinator,
                token,
                ClientRequest::TailJob(TailJobRequest {
                    job_id: job_id.clone(),
                    lines,
                }),
            )
            .await?;
            for response in responses {
                match response {
                    ClientResponse::Logs(logs) => {
                        for line in logs {
                            println!(
                                "[{} {} {}] {}",
                                line.node_id, line.task_id, line.stream, line.line
                            );
                        }
                    }
                    ClientResponse::Error(message) => bail!(message),
                    _ => {}
                }
            }
        }
        if job.status != "running" {
            break;
        }
        sleep(Duration::from_secs(interval)).await;
    }
    Ok(())
}

fn print_job_status(job: &JobSummary) {
    let updated = job
        .updated_at
        .map(|time| time.to_rfc3339())
        .unwrap_or_else(|| "-".to_owned());
    println!("JOB	STATUS	TASKS	UPDATED");
    println!(
        "{}	{}	{}	{}",
        job.job_id,
        job.status,
        job.tasks.len(),
        updated
    );
    println!("TASK	NODE	STATUS	EXIT");
    for task in &job.tasks {
        let exit = task
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "-".to_owned());
        println!("{}	{}	{}	{}", task.task_id, task.node_id, task.status, exit);
    }
}

fn print_leases(leases: Vec<LeaseSummary>) {
    if leases.is_empty() {
        println!("no active leases");
        return;
    }
    println!("LEASE\tNODES\tGPUS_PER_NODE\tEXCLUSIVE\tCREATED");
    for lease in leases {
        let nodes = lease
            .nodes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let gpus = lease
            .gpus_per_node
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "{}\t{}\t{}\t{}\t{}",
            lease.lease_id,
            nodes,
            gpus,
            lease.exclusive,
            lease.created_at.to_rfc3339()
        );
    }
}

fn print_run_responses(responses: Vec<ClientResponse>) -> Result<()> {
    for response in responses {
        match response {
            ClientResponse::RunStarted(started) => {
                let tasks = started
                    .tasks
                    .iter()
                    .map(|task| format!("{}:{}", task.node_id, task.task_id))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("started {} ({tasks})", started.job_id);
            }
            ClientResponse::TaskStarted(started) => {
                if let Some(pid) = started.pid {
                    println!("[{} {}] pid {}", started.node_id, started.task_id, pid);
                }
            }
            ClientResponse::LogLine(line) => {
                println!(
                    "[{} {} {}] {}",
                    line.node_id, line.task_id, line.stream, line.line
                );
            }
            ClientResponse::TaskExited(exited) => {
                println!(
                    "[{} {}] exited success={} code={:?}",
                    exited.node_id, exited.task_id, exited.success, exited.exit_code
                );
            }
            ClientResponse::JobFinished(done) => {
                println!("finished {} success={}", done.job_id, done.success);
                if !done.success {
                    std::process::exit(1);
                }
            }
            ClientResponse::Error(message) => bail!(message),
            _ => {}
        }
    }
    Ok(())
}

fn print_job_diagnosis(diagnosis: JobDiagnosis) {
    println!("JOB\tSTATUS\tSUMMARY");
    println!(
        "{}\t{}\t{}",
        diagnosis.job_id, diagnosis.status, diagnosis.summary
    );
    println!("TASK\tNODE\tSTATUS\tEXIT");
    for task in diagnosis.tasks {
        let exit = task
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "{}\t{}\t{}\t{}",
            task.task_id, task.node_id, task.status, exit
        );
    }
    println!("HINTS");
    for hint in diagnosis.hints {
        println!("- {hint}");
    }
    if !diagnosis.recent_errors.is_empty() {
        println!("RECENT_ERRORS");
        for line in diagnosis.recent_errors {
            println!(
                "[{} {} {}] {}",
                line.node_id, line.task_id, line.stream, line.line
            );
        }
    }
}

fn inventory_gpu_argv() -> Vec<String> {
    vec![
        "nvidia-smi".to_owned(),
        "--query-gpu=index,name,memory.total,memory.free,memory.used,utilization.gpu,driver_version".to_owned(),
        "--format=csv,noheader".to_owned(),
    ]
}

fn inventory_cuda_argv() -> Vec<String> {
    vec![
        "sh".to_owned(),
        "-lc".to_owned(),
        "echo hostname=$(hostname); command -v nvcc >/dev/null 2>&1 && nvcc --version | tail -n 1 || echo nvcc=missing; command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -n 1 || echo nvidia-smi=missing"
            .to_owned(),
    ]
}

fn inventory_torch_argv() -> Vec<String> {
    vec![
        "python3".to_owned(),
        "-c".to_owned(),
        "import json, socket\ntry:\n import torch\n print(json.dumps({'hostname': socket.gethostname(), 'torch': torch.__version__, 'cuda_available': torch.cuda.is_available(), 'cuda_version': torch.version.cuda, 'gpu_count': torch.cuda.device_count(), 'devices': [torch.cuda.get_device_name(i) for i in range(torch.cuda.device_count())]}))\nexcept Exception as e:\n print(json.dumps({'hostname': socket.gethostname(), 'torch': 'missing', 'error': str(e)}))"
            .to_owned(),
    ]
}

fn health_gpu_argv() -> Vec<String> {
    vec![
        "sh".to_owned(),
        "-lc".to_owned(),
        "set -eu; hostname; nvidia-smi -L; nvidia-smi --query-gpu=index,name,memory.free,memory.used,utilization.gpu --format=csv,noheader"
            .to_owned(),
    ]
}

fn health_torch_argv() -> Vec<String> {
    vec![
        "python3".to_owned(),
        "-c".to_owned(),
        "import socket, torch\nprint('hostname=' + socket.gethostname())\nprint('torch=' + torch.__version__)\nprint('cuda_available=' + str(torch.cuda.is_available()))\nassert torch.cuda.is_available(), 'torch cuda is not available'\nx=torch.ones((1024,1024), device='cuda')\ny=(x @ x).sum().item()\ntorch.cuda.synchronize()\nprint('cuda_matmul_sum=' + str(y))"
            .to_owned(),
    ]
}

fn nccl_smoke_argv() -> Vec<String> {
    vec![
        "python3".to_owned(),
        "-c".to_owned(),
        "import os, socket, torch, torch.distributed as dist\nbackend='nccl'\ndist.init_process_group(backend=backend)\nrank=dist.get_rank(); world=dist.get_world_size()\ntorch.cuda.set_device(int(os.environ.get('LOCAL_RANK','0')))\nx=torch.ones(1, device='cuda') * (rank + 1)\ndist.all_reduce(x)\nexpected=world*(world+1)/2\nassert int(x.item()) == int(expected), (x.item(), expected)\nprint(f'hostname={socket.gethostname()} rank={rank} world={world} nccl_all_reduce={x.item()}')\ndist.destroy_process_group()"
            .to_owned(),
    ]
}

fn torchrun_request_parts(
    nodes: Vec<NodeId>,
    gpus_per_node: u32,
    master_addr: &str,
    master_port: u16,
    user_argv: Vec<String>,
) -> (Vec<String>, BTreeMap<NodeId, BTreeMap<String, String>>) {
    let nnodes = nodes.len().max(1);
    let command = format!(
        "exec torchrun --nnodes {} --nproc-per-node {} --node-rank \"$ML_NODE_RANK\" --master-addr {} --master-port {} {}",
        nnodes,
        gpus_per_node,
        shell_quote(master_addr),
        master_port,
        user_argv
            .iter()
            .map(|part| shell_quote(part))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut node_env = BTreeMap::new();
    for (rank, node) in nodes.iter().enumerate() {
        let mut env = BTreeMap::new();
        env.insert("ML_NODE_RANK".to_owned(), rank.to_string());
        env.insert("ML_NNODES".to_owned(), nnodes.to_string());
        env.insert("ML_GPUS_PER_NODE".to_owned(), gpus_per_node.to_string());
        env.insert("ML_MASTER_ADDR".to_owned(), master_addr.to_owned());
        env.insert("ML_MASTER_PORT".to_owned(), master_port.to_string());
        node_env.insert(node.clone(), env);
    }
    (vec!["sh".to_owned(), "-lc".to_owned(), command], node_env)
}

fn default_master_addr(nodes: &[NodeId], summaries: &[NodeSummary]) -> String {
    nodes
        .first()
        .and_then(|node_id| {
            summaries
                .iter()
                .find(|summary| &summary.node_id == node_id)
                .map(|summary| summary.hostname.clone())
        })
        .or_else(|| nodes.first().map(ToString::to_string))
        .unwrap_or_else(|| "127.0.0.1".to_owned())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn request(
    coordinator: &str,
    token: Option<&str>,
    client_request: ClientRequest,
) -> Result<Vec<ClientResponse>> {
    let request = websocket_request(coordinator, AuthRole::Client, token)?;
    let (ws, _) = connect_async(request).await?;
    let (mut write, mut read) = ws.split();
    write
        .send(Message::Text(
            WireMessage::ClientRequest(client_request).to_text()?.into(),
        ))
        .await?;

    let mut responses = Vec::new();
    while let Some(message) = read.next().await {
        let message = message?;
        if message.is_close() {
            break;
        }
        let text = message.into_text()?;
        let wire = WireMessage::from_text(&text)?;
        let WireMessage::ClientResponse(response) = wire else {
            continue;
        };
        let done = matches!(
            response,
            ClientResponse::Nodes(_)
                | ClientResponse::Jobs(_)
                | ClientResponse::Job(_)
                | ClientResponse::Logs(_)
                | ClientResponse::Leases(_)
                | ClientResponse::LeaseCreated(_)
                | ClientResponse::JobDiagnosis(_)
                | ClientResponse::JobFinished(_)
                | ClientResponse::Error(_)
                | ClientResponse::Ack
        );
        responses.push(response);
        if done {
            break;
        }
    }

    Ok(responses)
}
