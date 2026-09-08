use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, State};

mod git;
mod mock_agent;
mod model;
mod ollama;
mod runner;
mod store;

use model::{
    AgentsConfig, DiffResult, OllamaModelInfo, Run, SpecVersion, Task, Ticket, WorkspaceConfig,
};
use runner::RunRegistry;
use store::{new_id, now_ms, Store};

pub struct AppState {
    pub store: Store,
    pub registry: Arc<RunRegistry>,
}

#[cfg(windows)]
fn kill_pid(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

#[cfg(not(windows))]
fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output();
}

fn ws_path(state: &State<AppState>) -> Result<PathBuf, String> {
    let cfg = state.store.load_workspace()?;
    match cfg.project_path {
        Some(p) if !p.is_empty() => Ok(PathBuf::from(p)),
        _ => Err("Configura primero la carpeta del proyecto en Ajustes".into()),
    }
}

// ---------- workspace ----------

#[tauri::command]
fn set_workspace(path: String, state: State<AppState>) -> Result<WorkspaceConfig, String> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err("la carpeta no existe".into());
    }
    let mut cfg = state.store.load_workspace()?;
    cfg.project_path = Some(p.to_string_lossy().to_string());
    state.store.save_workspace(&cfg)?;
    Ok(cfg)
}

#[tauri::command]
fn get_workspace(state: State<AppState>) -> Result<WorkspaceConfig, String> {
    state.store.load_workspace()
}

// ---------- ollama ----------

#[tauri::command]
fn list_ollama_models(state: State<AppState>) -> Result<Vec<OllamaModelInfo>, String> {
    let cfg = state.store.load_workspace()?;
    ollama::list_models(&cfg.ollama_url)
}

#[tauri::command]
fn set_ollama(url: String, model: String, state: State<AppState>) -> Result<WorkspaceConfig, String> {
    let mut cfg = state.store.load_workspace()?;
    cfg.ollama_url = url;
    cfg.ollama_model = model;
    state.store.save_workspace(&cfg)?;
    Ok(cfg)
}

// ---------- tasks ----------

#[tauri::command]
fn list_tasks(state: State<AppState>) -> Result<Vec<Task>, String> {
    state.store.list_tasks()
}

#[tauri::command]
fn create_task(title: String, intent: String, state: State<AppState>) -> Result<Task, String> {
    let now = now_ms();
    let task = Task {
        id: new_id("task"),
        title,
        intent,
        status: "planning".into(),
        created_at: now,
        updated_at: now,
        spec_current: None,
        spec_versions: Vec::new(),
        tickets: Vec::new(),
    };
    state.store.save_task(&task)?;
    Ok(task)
}

#[tauri::command]
fn get_task(id: String, state: State<AppState>) -> Result<Task, String> {
    state.store.load_task(&id)
}

#[tauri::command]
fn update_task(task: Task, state: State<AppState>) -> Result<Task, String> {
    let mut t = task;
    t.updated_at = now_ms();
    state.store.save_task(&t)?;
    Ok(t)
}

#[tauri::command]
fn delete_task(id: String, state: State<AppState>) -> Result<(), String> {
    state.store.delete_task(&id)
}

// ---------- tickets ----------

#[tauri::command]
fn save_ticket(task_id: String, ticket: Ticket, state: State<AppState>) -> Result<Task, String> {
    let mut task = state.store.load_task(&task_id)?;
    match task.tickets.iter_mut().find(|t| t.id == ticket.id) {
        Some(slot) => *slot = ticket,
        None => task.tickets.push(ticket),
    }
    task.updated_at = now_ms();
    state.store.save_task(&task)?;
    Ok(task)
}

#[tauri::command]
fn delete_ticket(task_id: String, ticket_id: String, state: State<AppState>) -> Result<Task, String> {
    let mut task = state.store.load_task(&task_id)?;
    task.tickets.retain(|t| t.id != ticket_id);
    task.updated_at = now_ms();
    state.store.save_task(&task)?;
    Ok(task)
}

#[tauri::command]
fn set_tickets(task_id: String, tickets: Vec<Ticket>, state: State<AppState>) -> Result<Task, String> {
    let mut task = state.store.load_task(&task_id)?;
    task.tickets = tickets;
    task.updated_at = now_ms();
    state.store.save_task(&task)?;
    Ok(task)
}

// ---------- spec ----------

#[tauri::command]
fn set_spec(task_id: String, content: String, state: State<AppState>) -> Result<Task, String> {
    let mut task = state.store.load_task(&task_id)?;
    let version = task.spec_versions.iter().map(|v| v.version).max().unwrap_or(0) + 1;
    task.spec_versions.push(SpecVersion {
        version,
        content: content.clone(),
        created_at: now_ms(),
    });
    task.spec_current = Some(content);
    task.updated_at = now_ms();
    state.store.save_task(&task)?;
    Ok(task)
}

// ---------- runs ----------

#[tauri::command]
fn list_runs(task_id: String, state: State<AppState>) -> Result<Vec<Run>, String> {
    state.store.list_runs(&task_id)
}

#[tauri::command]
fn get_run(task_id: String, run_id: String, state: State<AppState>) -> Result<Run, String> {
    state.store.load_run(&task_id, &run_id)
}

#[tauri::command]
fn cancel_run(run_id: String, state: State<AppState>) -> Result<bool, String> {
    let reg = state.registry.lock().unwrap();
    if let Some(cs) = reg.get(&run_id) {
        cs.flag.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(pid) = *cs.pid.lock().unwrap() {
            kill_pid(pid);
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_run(
    app: AppHandle,
    state: &State<AppState>,
    task_id: String,
    agent_id: String,
    mode: String,
    kind: &str,
) -> Result<Run, String> {
    let task = state.store.load_task(&task_id)?;
    if matches!(kind, "exec") {
        let any_running = state
            .store
            .list_runs(&task_id)?
            .iter()
            .any(|r| r.status == "running");
        if any_running {
            return Err("ya hay una ejecución en curso para esta task".into());
        }
    }
    let agents = state.store.load_agents()?;
    let agent = runner::resolve_agent(&agents.agents, &agent_id)?;
    let ws = ws_path(&state)?;
    let store = state.store.clone();
    let registry = state.registry.clone();
    let kind = kind.to_string();
    let run_id = new_id("run");
    let run_id_c = run_id.clone();
    let mode_c = mode.clone();
    let agent_id_c = agent.id.clone();

    let app2 = app.clone();
    std::thread::spawn(move || {
        let run = if kind == "plan" {
            runner::run_plan(app2.clone(), &store, &registry, &run_id_c, &task, &agent, &mode_c, &ws)
        } else {
            runner::run_exec(app2.clone(), &store, &registry, &run_id_c, &task, &agent, &mode_c, &ws)
        };
        let _ = app2.emit("run-finished", &run);
    });

    Ok(Run {
        id: run_id,
        task_id,
        agent: agent_id_c,
        mode,
        worktree: None,
        worktree_path: None,
        base_sha: None,
        checkpoint_sha: None,
        started_at: now_ms(),
        finished_at: None,
        status: "running".into(),
        session_id: None,
        summary: None,
        events: Vec::new(),
    })
}

#[tauri::command]
fn start_plan_run(
    app: AppHandle,
    task_id: String,
    agent_id: String,
    mode: String,
    state: State<AppState>,
) -> Result<Run, String> {
    spawn_run(app, &state, task_id, agent_id, mode, "plan")
}

#[tauri::command]
fn start_exec_run(
    app: AppHandle,
    task_id: String,
    agent_id: String,
    mode: String,
    state: State<AppState>,
) -> Result<Run, String> {
    spawn_run(app, &state, task_id, agent_id, mode, "exec")
}

// ---------- diff / git ----------

#[tauri::command]
fn get_diff(task_id: String, run_id: String, state: State<AppState>) -> Result<DiffResult, String> {
    let run = state.store.load_run(&task_id, &run_id)?;
    let ws = ws_path(&state)?;
    if let Some(wt) = &run.worktree_path {
        let dir = PathBuf::from(wt);
        if !dir.exists() {
            return Err("el worktree ya no existe".into());
        }
        git::diff(&dir, run.base_sha.as_deref())
    } else {
        git::diff(&ws, run.base_sha.as_deref())
    }
}

#[tauri::command]
fn list_worktrees(state: State<AppState>) -> Result<Vec<(String, String)>, String> {
    let ws = ws_path(&state)?;
    git::list_nerve_worktrees(&ws)
}

#[tauri::command]
fn discard_worktree(id: String, state: State<AppState>) -> Result<(), String> {
    let ws = ws_path(&state)?;
    git::remove_worktree(&ws, &id, true)
}

#[tauri::command]
fn merge_worktree(id: String, state: State<AppState>) -> Result<String, String> {
    let ws = ws_path(&state)?;
    if git::diff(&ws, Some("HEAD")).map(|d| !d.files.is_empty()).unwrap_or(false) {
        return Err("el workspace tiene cambios sin confirmar; confírmalos antes de fusionar".into());
    }
    git::merge_worktree(&ws, &id)?;
    git::remove_worktree(&ws, &id, true)?;
    git::head_sha(&ws)
}

#[tauri::command]
fn revert_run(task_id: String, run_id: String, state: State<AppState>) -> Result<(), String> {
    let run = state.store.load_run(&task_id, &run_id)?;
    if run.mode != "workspace" {
        return Err("revertir solo aplica a ejecuciones en modo workspace".into());
    }
    let base = run
        .base_sha
        .ok_or("el run no tiene sha base registrado")?;
    let ws = ws_path(&state)?;
    git::reset_hard(&ws, &base)
}

// ---------- agents ----------

#[tauri::command]
fn list_agents(state: State<AppState>) -> Result<AgentsConfig, String> {
    state.store.load_agents()
}

#[tauri::command]
fn save_agents(cfg: AgentsConfig, state: State<AppState>) -> Result<AgentsConfig, String> {
    state.store.save_agents(&cfg)?;
    Ok(cfg)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            store: Store::default(),
            registry: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
        .invoke_handler(tauri::generate_handler![
            set_workspace,
            get_workspace,
            list_ollama_models,
            set_ollama,
            list_tasks,
            create_task,
            get_task,
            update_task,
            delete_task,
            save_ticket,
            delete_ticket,
            set_tickets,
            set_spec,
            list_runs,
            get_run,
            cancel_run,
            start_plan_run,
            start_exec_run,
            get_diff,
            list_worktrees,
            discard_worktree,
            merge_worktree,
            revert_run,
            list_agents,
            save_agents,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}