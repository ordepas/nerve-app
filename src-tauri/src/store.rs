use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::model::{AgentDef, AgentsConfig, Run, RunEvent, Task, WorkspaceConfig};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn new_id(prefix: &str) -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}_{}", prefix, n)
}

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        fs::create_dir_all(&root).expect("no se pudo crear el directorio del store");
        let conn = Connection::open(root.join("nerve.db"))
            .expect("no se pudo abrir nerve.db");
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        init_schema(&conn);
        let store = Self {
            root,
            conn: Arc::new(Mutex::new(conn)),
        };
        store.migrate_json_files();
        store
    }

    // ---------- config (JSON legible a propósito) ----------

    pub fn load_agents(&self) -> Result<AgentsConfig, String> {
        let p = self.root.join("config").join("agents.json");
        if p.exists() {
            let mut cfg: AgentsConfig = self.read_json(&p)?;
            // migración: agente Ollama (integrado después de la fase 1)
            if !cfg.agents.iter().any(|a| a.id == "ollama") {
                cfg.agents.push(AgentDef {
                    id: "ollama".into(),
                    label: "Ollama (local)".into(),
                    bin: String::new(),
                    kind: "ollama".into(),
                    enabled: true,
                    plan_args: vec![],
                    exec_args: vec![],
                });
                self.write_json(&p, &cfg)?;
            }
            // migración fase 2: reemplaza claude/codex deshabilitados (placeholder) por
            // las definiciones reales, y añade gemini si falta
            let stale: Vec<String> = cfg
                .agents
                .iter()
                .filter(|a| (a.id == "claude" || a.id == "codex") && (a.kind == "disabled" || !a.enabled))
                .map(|a| a.id.clone())
                .collect();
            if !stale.is_empty() {
                let fresh: AgentsConfig =
                    serde_json::from_str("{}").map_err(|e| e.to_string())?;
                for id in stale {
                    if let Some(d) = fresh.agents.iter().find(|a| a.id == id) {
                        match cfg.agents.iter_mut().find(|a| a.id == id) {
                            Some(slot) => *slot = d.clone(),
                            None => cfg.agents.push(d.clone()),
                        }
                    }
                }
                self.write_json(&p, &cfg)?;
            }
            if !cfg.agents.iter().any(|a| a.id == "gemini") {
                let fresh: AgentsConfig =
                    serde_json::from_str("{}").map_err(|e| e.to_string())?;
                if let Some(d) = fresh.agents.iter().find(|a| a.id == "gemini") {
                    cfg.agents.push(d.clone());
                    self.write_json(&p, &cfg)?;
                }
            }
            Ok(cfg)
        } else {
            // "{}" activa el #[serde(default)] con la lista real de agentes;
            // AgentsConfig::default() del derive produciría un Vec vacío.
            let cfg: AgentsConfig = serde_json::from_str("{}").map_err(|e| e.to_string())?;
            self.write_json(&p, &cfg)?;
            Ok(cfg)
        }
    }

    pub fn save_agents(&self, cfg: &AgentsConfig) -> Result<(), String> {
        self.write_json(&self.root.join("config").join("agents.json"), cfg)
    }

    pub fn load_workspace(&self) -> Result<WorkspaceConfig, String> {
        let p = self.root.join("config").join("workspace.json");
        if p.exists() {
            self.read_json(&p)
        } else {
            Ok(WorkspaceConfig::default())
        }
    }

    pub fn save_workspace(&self, cfg: &WorkspaceConfig) -> Result<(), String> {
        self.write_json(&self.root.join("config").join("workspace.json"), cfg)
    }

    // ---------- tasks ----------

    pub fn list_tasks(&self) -> Result<Vec<Task>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT data FROM tasks ORDER BY updated_at DESC")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            let data: String = row.map_err(sql_err)?;
            match serde_json::from_str::<Task>(&data) {
                Ok(t) => out.push(t),
                Err(e) => eprintln!("nerve: task corrupto en sqlite: {}", e),
            }
        }
        Ok(out)
    }

    pub fn save_task(&self, task: &Task) -> Result<(), String> {
        let data = serde_json::to_string(task).map_err(|e| e.to_string())?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status, created_at, updated_at, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               title=excluded.title, status=excluded.status,
               created_at=excluded.created_at, updated_at=excluded.updated_at,
               data=excluded.data",
            rusqlite::params![
                task.id,
                task.title,
                task.status,
                task.created_at as i64,
                task.updated_at as i64,
                data
            ],
        )
        .map_err(sql_err)?;
        Ok(())
    }

    pub fn load_task(&self, id: &str) -> Result<Task, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT data FROM tasks WHERE id = ?1", [id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| format!("task {} no encontrada", id))
        .and_then(|data| serde_json::from_str(&data).map_err(|e| e.to_string()))
    }

    pub fn delete_task(&self, id: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM runs WHERE task_id = ?1", [id])
            .map_err(sql_err)?;
        conn.execute("DELETE FROM tasks WHERE id = ?1", [id])
            .map_err(sql_err)?;
        Ok(())
    }

    // ---------- runs ----------

    pub fn save_run(&self, run: &Run) -> Result<(), String> {
        // events van en su propia tabla; se serializa el run sin ellos
        let mut clean = run.clone();
        clean.events = Vec::new();
        let data = serde_json::to_string(&clean).map_err(|e| e.to_string())?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO runs (id, task_id, agent, mode, status, started_at, finished_at, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
               agent=excluded.agent, mode=excluded.mode, status=excluded.status,
               started_at=excluded.started_at, finished_at=excluded.finished_at,
               data=excluded.data",
            rusqlite::params![
                run.id,
                run.task_id,
                run.agent,
                run.mode,
                run.status,
                run.started_at as i64,
                run.finished_at.map(|f| f as i64),
                data
            ],
        )
        .map_err(sql_err)?;
        Ok(())
    }

    pub fn load_run(&self, task_id: &str, run_id: &str) -> Result<Run, String> {
        let conn = self.conn.lock().unwrap();
        let data: String = conn
            .query_row(
                "SELECT data FROM runs WHERE task_id = ?1 AND id = ?2",
                [task_id, run_id],
                |row| row.get(0),
            )
            .map_err(|_| format!("run {} no encontrado", run_id))?;
        let mut run: Run = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        run.events = load_events_impl(&conn, run_id)?;
        Ok(run)
    }

    pub fn list_runs(&self, task_id: &str) -> Result<Vec<Run>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM runs WHERE task_id = ?1 ORDER BY started_at DESC")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([task_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (run_id, data) = row.map_err(sql_err)?;
            if let Ok(mut r) = serde_json::from_str::<Run>(&data) {
                r.events = load_events_impl(&conn, &run_id)?;
                out.push(r);
            }
        }
        Ok(out)
    }

    pub fn append_event(&self, task_id: &str, run_id: &str, ev: &RunEvent) -> Result<(), String> {
        let data = serde_json::to_string(ev).map_err(|e| e.to_string())?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events (task_id, run_id, ts, kind, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                task_id,
                run_id,
                ev.ts as i64,
                ev.kind,
                data
            ],
        )
        .map_err(sql_err)?;
        Ok(())
    }

    /// Marca los runs "running" como failed (la app se cerró con runs activos)
    /// y devuelve las tasks afectadas al estado ready.
    pub fn fail_stale_running_runs(&self) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().unwrap();
        let mut task_ids: Vec<String> = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT DISTINCT task_id FROM runs WHERE status = 'running'")
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sql_err)?;
            for row in rows {
                task_ids.push(row.map_err(sql_err)?);
            }
        }
        for tid in &task_ids {
            let mut stmt = conn
                .prepare("SELECT data FROM runs WHERE task_id = ?1 AND status = 'running'")
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([tid], |row| row.get::<_, String>(0))
                .map_err(sql_err)?;
            for row in rows {
                let data: String = row.map_err(sql_err)?;
                if let Ok(mut run) = serde_json::from_str::<Run>(&data) {
                    run.status = "failed".into();
                    run.finished_at = Some(now_ms());
                    run.summary = Some("interrumpido: la app se cerró mientras corría".into());
                    if let Ok(data) = serde_json::to_string(&run) {
                        let _ = conn.execute(
                            "UPDATE runs SET status='failed', finished_at=?2, data=?3 WHERE id=?1",
                            rusqlite::params![run.id, run.finished_at.map(|f| f as i64), data],
                        );
                    }
                }
            }
            let _ = conn.execute(
                "UPDATE tasks SET status='ready', data=json_set(data, '$.status', 'ready'), updated_at=?2 WHERE id=?1 AND status='in_dev'",
                rusqlite::params![tid, now_ms() as i64],
            );
        }
        Ok(task_ids)
    }

    // ---------- json helpers / migración ----------

    fn write_json<T: Serialize>(&self, path: &PathBuf, value: &T) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let data = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
        fs::write(path, data).map_err(|e| e.to_string())
    }

    fn read_json<T: DeserializeOwned>(&self, path: &PathBuf) -> Result<T, String> {
        let data = fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&data).map_err(|e| e.to_string())
    }

    /// Importa tasks/runs/eventos del layout JSON anterior (tasks/*/task.json)
    /// a SQLite. Al terminar renombra el directorio a tasks_imported.
    fn migrate_json_files(&self) {
        let dir = self.root.join("tasks");
        if !dir.exists() {
            return;
        }
        let imported = self.root.join("tasks_imported");
        if imported.exists() {
            return; // migración ya hecha antes
        }
        let conn = self.conn.lock().unwrap();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let tdir = entry.path();
            let task_path = tdir.join("task.json");
            if !task_path.exists() {
                continue;
            }
            let Ok(data) = fs::read_to_string(&task_path) else {
                continue;
            };
            let Ok(task) = serde_json::from_str::<Task>(&data) else {
                continue;
            };
            if conn
                .execute(
                    "INSERT INTO tasks (id, title, status, created_at, updated_at, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(id) DO UPDATE SET data=excluded.data",
                    rusqlite::params![
                        task.id,
                        task.title,
                        task.status,
                        task.created_at as i64,
                        task.updated_at as i64,
                        data
                    ],
                )
                .is_err()
            {
                continue;
            }
            let runs_dir = tdir.join("runs");
            if let Ok(runs) = fs::read_dir(&runs_dir) {
                for rentry in runs.flatten() {
                    let rpath = rentry.path();
                    if rpath.extension().map(|e| e != "json").unwrap_or(true) {
                        continue;
                    }
                    let Some(stem) = rpath.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if stem.ends_with(".events") {
                        continue;
                    }
                    let Ok(rdata) = fs::read_to_string(&rpath) else {
                        continue;
                    };
                    let Ok(mut run) = serde_json::from_str::<Run>(&rdata) else {
                        continue;
                    };
                    run.events = Vec::new();
                    let Ok(rdata) = serde_json::to_string(&run) else {
                        continue;
                    };
                    let _ = conn.execute(
                        "INSERT OR REPLACE INTO runs (id, task_id, agent, mode, status, started_at, finished_at, data)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        rusqlite::params![
                            run.id,
                            run.task_id,
                            run.agent,
                            run.mode,
                            run.status,
                            run.started_at as i64,
                            run.finished_at.map(|f| f as i64),
                            rdata
                        ],
                    );
                    // eventos del JSONL antiguo
                    let ev_path = runs_dir.join(format!("{}.events.jsonl", stem));
                    if let Ok(evdata) = fs::read_to_string(&ev_path) {
                        for line in evdata.lines() {
                            if line.trim().is_empty() {
                                continue;
                            }
                            if let Ok(ev) = serde_json::from_str::<RunEvent>(line) {
                                let _ = conn.execute(
                                    "INSERT INTO events (task_id, run_id, ts, kind, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                                    rusqlite::params![
                                        run.task_id,
                                        run.id,
                                        ev.ts as i64,
                                        ev.kind,
                                        serde_json::to_string(&ev).unwrap_or_default()
                                    ],
                                );
                            }
                        }
                    }
                }
            }
        }
        drop(conn);
        let _ = fs::rename(&dir, &imported);
    }
}

fn init_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            data TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_updated ON tasks(updated_at DESC);
        CREATE TABLE IF NOT EXISTS runs (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL,
            agent TEXT NOT NULL,
            mode TEXT NOT NULL,
            status TEXT NOT NULL,
            started_at INTEGER NOT NULL,
            finished_at INTEGER,
            data TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_runs_task ON runs(task_id, started_at DESC);
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            ts INTEGER NOT NULL,
            kind TEXT NOT NULL,
            data TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_run ON events(run_id, id);",
    )
    .expect("no se pudo crear el esquema de nerve.db");
}

fn load_events_impl(conn: &Connection, run_id: &str) -> Result<Vec<RunEvent>, String> {
    let mut stmt = conn
        .prepare("SELECT data FROM events WHERE run_id = ?1 ORDER BY id ASC")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([run_id], |row| row.get::<_, String>(0))
        .map_err(sql_err)?;
    let mut out = Vec::new();
    for row in rows {
        let data: String = row.map_err(sql_err)?;
        if let Ok(ev) = serde_json::from_str::<RunEvent>(&data) {
            out.push(ev);
        }
    }
    Ok(out)
}

fn sql_err(e: rusqlite::Error) -> String {
    e.to_string()
}

impl Default for Store {
    fn default() -> Self {
        Self::new(store_root())
    }
}

pub fn store_root() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".nerve".to_string());
        PathBuf::from(base)
            .join("com.pedro.nerve-app")
            .join("store")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("com.pedro.nerve-app")
            .join("store")
    }
}