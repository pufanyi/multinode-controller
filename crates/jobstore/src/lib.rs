use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use agent_protocol::{LogLine, NodeInfo, TaskExited, TaskStarted};
use anyhow::Result;
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

    pub fn record_task_started(&self, event: &TaskStarted) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        conn.execute(
            r#"
            INSERT INTO tasks (task_id, job_id, node_id, status, started_at)
            VALUES (?1, ?2, ?3, 'running', ?4)
            ON CONFLICT(task_id) DO UPDATE SET
                status = 'running',
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
        conn.execute(
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
        Ok(())
    }
}
