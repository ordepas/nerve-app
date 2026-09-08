use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn tasks_dir(&self) -> PathBuf {
        self.root.join("tasks")
    }

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

    pub fn list_tasks(&self) -> Result<Vec<Task>, String> {
        let mut out = Vec::new();
        let dir = self.tasks_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            return Ok(out);
        }
        let entries = fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let p = entry.path().join("task.json");
            if p.exists() {
                match self.read_json::<Task>(&p) {
                    Ok(t) => out.push(t),
                    Err(e) => eprintln!("nerve: task corrupto {}: {}", p.display(), e),
                }
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    pub fn save_task(&self, task: &Task) -> Result<(), String> {
        self.write_json(&self.tasks_dir().join(&task.id).join("task.json"), task)
    }

    pub fn load_task(&self, id: &str) -> Result<Task, String> {
        let p = self.tasks_dir().join(id).join("task.json");
        if !p.exists() {
            return Err(format!("task {} no encontrada", id));
        }
        self.read_json(&p)
    }

    pub fn delete_task(&self, id: &str) -> Result<(), String> {
        let dir = self.tasks_dir().join(id);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn save_run(&self, run: &Run) -> Result<(), String> {
        self.write_json(
            &self
                .tasks_dir()
                .join(&run.task_id)
                .join("runs")
                .join(format!("{}.json", run.id)),
            run,
        )
    }

    pub fn load_run(&self, task_id: &str, run_id: &str) -> Result<Run, String> {
        let p = self
            .tasks_dir()
            .join(task_id)
            .join("runs")
            .join(format!("{}.json", run_id));
        if !p.exists() {
            return Err(format!("run {} no encontrado", run_id));
        }
        let mut run: Run = self.read_json(&p)?;
        run.events = self.load_events(task_id, run_id)?;
        Ok(run)
    }

    pub fn list_runs(&self, task_id: &str) -> Result<Vec<Run>, String> {
        let mut out = Vec::new();
        let dir = self.tasks_dir().join(task_id).join("runs");
        if !dir.exists() {
            return Ok(out);
        }
        let entries = fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(mut r) = self.read_json::<Run>(&p) {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        r.events = self.load_events(task_id, stem)?;
                    }
                    out.push(r);
                }
            }
        }
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(out)
    }

    pub fn append_event(&self, task_id: &str, run_id: &str, ev: &RunEvent) -> Result<(), String> {
        use std::io::Write;
        let dir = self.tasks_dir().join(task_id).join("runs");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{}.events.jsonl", run_id)))
            .map_err(|e| e.to_string())?;
        let line = serde_json::to_string(ev).map_err(|e| e.to_string())?;
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        f.write_all(b"\n").map_err(|e| e.to_string())
    }

    fn load_events(&self, task_id: &str, run_id: &str) -> Result<Vec<RunEvent>, String> {
        let p = self
            .tasks_dir()
            .join(task_id)
            .join("runs")
            .join(format!("{}.events.jsonl", run_id));
        if !p.exists() {
            return Ok(Vec::new());
        }
        let data = fs::read_to_string(&p).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for line in data.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<RunEvent>(line) {
                out.push(ev);
            }
        }
        Ok(out)
    }
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