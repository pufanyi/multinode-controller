use std::{collections::BTreeMap, path::PathBuf};

use agent_protocol::{ClientRequest, ClientResponse, NodeId, RunProcessRequest, WireMessage};
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Parser, Debug)]
#[command(name = "agentctl", about = "CLI for the multinode agent runtime")]
struct Args {
    #[arg(long, global = true, default_value = "ws://127.0.0.1:8765")]
    coordinator: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Nodes,
    Run {
        #[arg(long, value_delimiter = ',')]
        nodes: Vec<String>,

        #[arg(long)]
        cwd: Option<PathBuf>,

        #[arg(long, default_value_t = true)]
        wait: bool,

        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Nodes => {
            let responses = request(&args.coordinator, ClientRequest::ListNodes).await?;
            for response in responses {
                match response {
                    ClientResponse::Nodes(nodes) => {
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
                    ClientResponse::Error(message) => bail!(message),
                    _ => {}
                }
            }
        }
        Command::Run {
            nodes,
            cwd,
            wait,
            argv,
        } => {
            let run_request = ClientRequest::RunProcess(RunProcessRequest {
                nodes: nodes.into_iter().map(NodeId::from).collect(),
                argv,
                cwd,
                env: BTreeMap::new(),
                wait,
            });
            let responses = request(&args.coordinator, run_request).await?;
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
        }
    }

    Ok(())
}

async fn request(coordinator: &str, request: ClientRequest) -> Result<Vec<ClientResponse>> {
    let (ws, _) = connect_async(coordinator).await?;
    let (mut write, mut read) = ws.split();
    write
        .send(Message::Text(
            WireMessage::ClientRequest(request).to_text()?.into(),
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
