use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use agent_protocol::{
    JobId, JobSummary, LogLine, LogStream, NodeId, NodeInfo, TaskExited, TaskId, TaskStarted,
    TaskSummary,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

#[derive(Clone)]
pub struct SqliteJobStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteJobStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let store = Self {
            conn: Arc::new(Mutex::new(Connection::open_in_memory()?)),
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS nodes (
                node_id TEXT PRIMARY KEY,
                node_name TEXT NOT NULL,
                last_seen TEXT NOT NULL,
                payload TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tasks (
                task_id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT,
                started_at TEXT,
                exited_at TEXT,
                exit_code INTEGER
            );

            CREATE TABLE IF NOT EXISTS task_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                stream TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                line TEXT NOT NULL,
                offset INTEGER NOT NULL
            );
            "#,
        )?;
        let _ = conn.execute("ALTER TABLE tasks ADD COLUMN created_at TEXT", []);
        Ok(())
    }

    pub fn upsert_node(&self, node: &NodeInfo) -> Result<()> {
        let payload = serde_json::to_string(node)?;
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        conn.execute(
            r#"
            INSERT INTO nodes (node_id, node_name, last_seen, payload)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(node_id) DO UPDATE SET
                node_name = excluded.node_name,
                last_seen = excluded.last_seen,
                payload = excluded.payload
            "#,
            params![
                node.node_id.to_string(),
                node.node_name,
                node.started_at.to_rfc3339(),
                payload
            ],
        )?;
        Ok(())
    }

    pub fn record_task_dispatched(
        &self,
        job_id: &JobId,
        task_id: &TaskId,
        node_id: &NodeId,
        timestamp: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        conn.execute(
            r#"
            INSERT INTO tasks (task_id, job_id, node_id, status, created_at)
            VALUES (?1, ?2, ?3, 'queued', ?4)
            ON CONFLICT(task_id) DO UPDATE SET
                job_id = excluded.job_id,
                node_id = excluded.node_id,
                status = 'queued',
                created_at = COALESCE(tasks.created_at, excluded.created_at)
            "#,
            params![
                task_id.to_string(),
                job_id.to_string(),
                node_id.to_string(),
                timestamp.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn record_task_started(&self, event: &TaskStarted) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        conn.execute(
            r#"
            INSERT INTO tasks (task_id, job_id, node_id, status, created_at, started_at)
            VALUES (?1, ?2, ?3, 'running', ?4, ?4)
            ON CONFLICT(task_id) DO UPDATE SET
                status = 'running',
                created_at = COALESCE(tasks.created_at, excluded.created_at),
                started_at = excluded.started_at
            "#,
            params![
                event.task_id.to_string(),
                event.job_id.to_string(),
                event.node_id.to_string(),
                event.timestamp.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn record_log_line(&self, line: &LogLine) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        conn.execute(
            r#"
            INSERT INTO task_logs
                (job_id, task_id, node_id, stream, timestamp, line, offset)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                line.job_id.to_string(),
                line.task_id.to_string(),
                line.node_id.to_string(),
                line.stream.to_string(),
                line.timestamp.to_rfc3339(),
                line.line,
                line.offset as i64,
            ],
        )?;
        Ok(())
    }

    pub fn record_task_exited(&self, event: &TaskExited) -> Result<()> {
        let status = if event.success { "succeeded" } else { "failed" };
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        let updated = conn.execute(
            r#"
            UPDATE tasks
            SET status = ?2, exited_at = ?3, exit_code = ?4
            WHERE task_id = ?1
            "#,
            params![
                event.task_id.to_string(),
                status,
                event.timestamp.to_rfc3339(),
                event.exit_code,
            ],
        )?;
        if updated == 0 {
            conn.execute(
                r#"
                INSERT INTO tasks
                    (task_id, job_id, node_id, status, created_at, exited_at, exit_code)
                VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)
                "#,
                params![
                    event.task_id.to_string(),
                    event.job_id.to_string(),
                    event.node_id.to_string(),
                    status,
                    event.timestamp.to_rfc3339(),
                    event.exit_code,
                ],
            )?;
        }
        Ok(())
    }

    pub fn list_jobs(&self, limit: usize) -> Result<Vec<JobSummary>> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        let mut stmt = conn.prepare(
            r#"
            SELECT job_id
            FROM tasks
            GROUP BY job_id
            ORDER BY MAX(COALESCE(exited_at, started_at, created_at, '')) DESC
            LIMIT ?1
            "#,
        )?;
        let job_ids = stmt
            .query_map([limit as i64], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut jobs = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            let tasks = self.list_tasks_for_job_locked(&conn, &JobId(job_id))?;
            jobs.push(job_summary(tasks));
        }
        Ok(jobs)
    }

    pub fn get_job(&self, job_id: &JobId) -> Result<Option<JobSummary>> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        let tasks = self.list_tasks_for_job_locked(&conn, job_id)?;
        if tasks.is_empty() {
            Ok(None)
        } else {
            Ok(Some(job_summary(tasks)))
        }
    }

    pub fn tail_logs(&self, job_id: &JobId, lines: usize) -> Result<Vec<LogLine>> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        let mut stmt = conn.prepare(
            r#"
            SELECT job_id, task_id, node_id, stream, timestamp, line, offset
            FROM (
                SELECT id, job_id, task_id, node_id, stream, timestamp, line, offset
                FROM task_logs
                WHERE job_id = ?1
                ORDER BY id DESC
                LIMIT ?2
            )
            ORDER BY id ASC
            "#,
        )?;
        let logs = stmt
            .query_map(params![job_id.to_string(), lines as i64], |row| {
                let stream: String = row.get(3)?;
                let timestamp: String = row.get(4)?;
                Ok(LogLine {
                    job_id: JobId(row.get(0)?),
                    task_id: TaskId(row.get(1)?),
                    node_id: NodeId(row.get(2)?),
                    stream: parse_stream(&stream),
                    timestamp: parse_datetime(&timestamp).unwrap_or_else(|_| Utc::now()),
                    line: row.get(5)?,
                    offset: row.get::<_, i64>(6)? as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(logs)
    }

    fn list_tasks_for_job_locked(
        &self,
        conn: &Connection,
        job_id: &JobId,
    ) -> Result<Vec<TaskSummary>> {
        let mut stmt = conn.prepare(
            r#"
            SELECT job_id, task_id, node_id, status, created_at, started_at, exited_at, exit_code
            FROM tasks
            WHERE job_id = ?1
            ORDER BY COALESCE(created_at, started_at, exited_at, ''), task_id
            "#,
        )?;
        let tasks = stmt
            .query_map([job_id.to_string()], |row| {
                let created_at: Option<String> = row.get(4)?;
                let started_at: Option<String> = row.get(5)?;
                let exited_at: Option<String> = row.get(6)?;
                Ok(TaskSummary {
                    job_id: JobId(row.get(0)?),
                    task_id: TaskId(row.get(1)?),
                    node_id: NodeId(row.get(2)?),
                    status: row.get(3)?,
                    created_at: parse_optional_datetime(created_at),
                    started_at: parse_optional_datetime(started_at),
                    exited_at: parse_optional_datetime(exited_at),
                    exit_code: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tasks)
    }
}

fn job_summary(tasks: Vec<TaskSummary>) -> JobSummary {
    let job_id = tasks
        .first()
        .map(|task| task.job_id.clone())
        .unwrap_or_else(JobId::new);
    let created_at = tasks.iter().filter_map(|task| task.created_at).min();
    let updated_at = tasks
        .iter()
        .flat_map(|task| [task.exited_at, task.started_at, task.created_at])
        .flatten()
        .max();
    let status = if tasks
        .iter()
        .any(|task| matches!(task.status.as_str(), "queued" | "running"))
    {
        "running"
    } else if tasks.iter().all(|task| task.status == "succeeded") {
        "succeeded"
    } else {
        "failed"
    }
    .to_owned();

    JobSummary {
        job_id,
        status,
        created_at,
        updated_at,
        tasks,
    }
}

fn parse_stream(value: &str) -> LogStream {
    match value {
        "stdout" => LogStream::Stdout,
        "stderr" => LogStream::Stderr,
        _ => LogStream::System,
    }
}

fn parse_optional_datetime(value: Option<String>) -> Option<DateTime<Utc>> {
    value.and_then(|value| parse_datetime(&value).ok())
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid stored timestamp {value}"))?
        .with_timezone(&Utc))
}
