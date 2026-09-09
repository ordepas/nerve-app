use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager, State};

mod agents_md;
mod epic;
mod git;
mod mock_agent;
mod model;
mod ollama;
mod runner;
mod skills;
mod store;

use model::{
    AgentsConfig, AnswerInput, DiffResult, OllamaModelInfo, Run, SpecVersion, Task, Ticket,
    WorkspaceConfig,
};
use runner::RunRegistry;
use store::{new_id, now_ms, Store};

pub struct AppState {
    pub store: Store,
    pub registry: Arc<RunRegistry>,
}

#[cfg(windows)]
pub fn kill_pid(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

#[cfg(not(windows))]
pub fn kill_pid(pid: u32) {
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

#[tauri::command]
fn set_security(
    max_steps: u64,
    command_allowlist: Vec<String>,
    agents_md_enabled: bool,
    exec_agent: Option<String>,
    state: State<AppState>,
) -> Result<WorkspaceConfig, String> {
    let mut cfg = state.store.load_workspace()?;
    cfg.max_steps = max_steps;
    cfg.command_allowlist = command_allowlist
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    cfg.agents_md_enabled = agents_md_enabled;
    // "" = sin perfil de ejecución
    cfg.exec_agent = exec_agent.filter(|s| !s.trim().is_empty());
    state.store.save_workspace(&cfg)?;
    Ok(cfg)
}

// ---------- skills ----------

#[tauri::command]
fn list_skills(state: State<AppState>) -> Result<Vec<skills::SkillDef>, String> {
    skills::list_skills(&state.store)
}

#[tauri::command]
fn save_skill(
    id: String,
    label: String,
    description: String,
    template: String,
    state: State<AppState>,
) -> Result<String, String> {
    skills::save_skill(
        &state.store,
        skills::SkillDef {
            id,
            label,
            description,
            template,
        },
    )
}

#[tauri::command]
fn delete_skill(id: String, state: State<AppState>) -> Result<(), String> {
    skills::delete_skill(&state.store, &id)
}

#[tauri::command]
fn import_skill(path: String, state: State<AppState>) -> Result<String, String> {
    skills::import_skill(&state.store, &path)
}

// ---------- epics ----------

#[tauri::command]
fn list_epics(state: State<AppState>) -> Result<Vec<epic::EpicDef>, String> {
    epic::list_epics(&state.store)
}

#[tauri::command]
fn create_epic(
    title: String,
    intent: String,
    agent_id: String,
    skill_id: String,
    state: State<AppState>,
) -> Result<epic::EpicDef, String> {
    epic::create_epic(&state.store, &title, &intent, &agent_id, &skill_id)
}

#[tauri::command]
fn start_epic(
    app: AppHandle,
    id: String,
    state: State<AppState>,
) -> Result<epic::EpicDef, String> {
    epic::kick(&app, &state.store, &state.registry, &id, &["draft", "awaiting_gate", "failed"])
}

#[tauri::command]
fn continue_epic(
    app: AppHandle,
    id: String,
    state: State<AppState>,
) -> Result<epic::EpicDef, String> {
    epic::kick(&app, &state.store, &state.registry, &id, &["awaiting_gate"])
}

#[tauri::command]
fn cancel_epic(
    id: String,
    state: State<AppState>,
) -> Result<epic::EpicDef, String> {
    epic::cancel_epic(&state.store, &state.registry, &id)
}

#[tauri::command]
fn delete_epic(id: String, state: State<AppState>) -> Result<(), String> {
    epic::delete_epic(&state.store, &id)
}

#[tauri::command]
fn set_epic_yolo(
    app: AppHandle,
    id: String,
    yolo: bool,
    state: State<AppState>,
) -> Result<epic::EpicDef, String> {
    let e = epic::set_epic_yolo(&state.store, &id, yolo)?;
    let _ = app.emit("epic-updated", &e);
    Ok(e)
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
        plan_artifacts: Vec::new(),
        pending_docs: Vec::new(),
        tickets: Vec::new(),
        chat_messages: Vec::new(),
        review_comments: Vec::new(),
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

// ---------- artefactos de planificación (brief/arquitectura/flujos) ----------

#[tauri::command]
fn generate_doc(
    app: AppHandle,
    task_id: String,
    agent_id: String,
    kind: String,
    state: State<AppState>,
) -> Result<Run, String> {
    if !matches!(kind.as_str(), "brief" | "architecture" | "flows" | "spec") {
        return Err(format!("kind de documento no soportado: {}", kind));
    }
    let task = state.store.load_task(&task_id)?;
    let any_running = state
        .store
        .list_runs(&task_id)?
        .iter()
        .any(|r| r.status == "running");
    if any_running {
        return Err("ya hay una ejecución en curso para esta task".into());
    }
    let agents = state.store.load_agents()?;
    let agent = runner::resolve_agent(&agents.agents, &agent_id)?;
    let ws = ws_path(&state)?;
    let store = state.store.clone();
    let registry = state.registry.clone();
    let run_id = new_id("run");
    let run_id_c = run_id.clone();
    let kind_c = kind.clone();
    let agent_label = agent.id.clone();
    std::thread::spawn(move || {
        let run = runner::run_doc(app.clone(), &store, &registry, &run_id_c, &task, &agent, &ws, &kind_c);
        let _ = app.emit("run-finished", &run);
    });
    Ok(Run {
        id: run_id,
        task_id,
        agent: agent_label,
        mode: "doc".into(),
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
fn delete_doc(
    app: AppHandle,
    task_id: String,
    kind: String,
    state: State<AppState>,
) -> Result<Task, String> {
    let mut t = state.store.load_task(&task_id)?;
    t.plan_artifacts.retain(|a| a.kind != kind);
    t.pending_docs.retain(|p| p.kind != kind);
    t.updated_at = now_ms();
    state.store.save_task(&t)?;
    let _ = app.emit("task-updated", &t);
    Ok(t)
}

/// Fase de preguntas: el agente formula 2-4 preguntas para el documento
/// indicado y el usuario responde antes de generarlo.
#[tauri::command]
fn ask_doc(
    app: AppHandle,
    task_id: String,
    agent_id: String,
    kind: String,
    state: State<AppState>,
) -> Result<Run, String> {
    if !matches!(kind.as_str(), "brief" | "architecture" | "flows" | "spec") {
        return Err(format!("kind de documento no soportado: {}", kind));
    }
    let task = state.store.load_task(&task_id)?;
    let any_running = state
        .store
        .list_runs(&task_id)?
        .iter()
        .any(|r| r.status == "running");
    if any_running {
        return Err("ya hay una ejecución en curso para esta task".into());
    }
    let agents = state.store.load_agents()?;
    let agent = runner::resolve_agent(&agents.agents, &agent_id)?;
    let ws = ws_path(&state)?;
    let store = state.store.clone();
    let registry = state.registry.clone();
    let run_id = new_id("run");
    let run_id_c = run_id.clone();
    let kind_c = kind.clone();
    let agent_label = agent.id.clone();
    std::thread::spawn(move || {
        let run = runner::run_questions(app.clone(), &store, &registry, &run_id_c, &task, &agent, &ws, &kind_c);
        let _ = app.emit("run-finished", &run);
    });
    Ok(Run {
        id: run_id,
        task_id,
        agent: agent_label,
        mode: "ask".into(),
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

/// Guarda las respuestas del usuario a las preguntas de un documento.
#[tauri::command]
fn answer_doc(
    app: AppHandle,
    task_id: String,
    kind: String,
    answers: Vec<AnswerInput>,
    state: State<AppState>,
) -> Result<Task, String> {
    let mut t = state.store.load_task(&task_id)?;
    match t.pending_docs.iter_mut().find(|p| p.kind == kind) {
        None => return Err(format!("no hay preguntas pendientes para {}", kind)),
        Some(p) => {
            for a in answers {
                if let Some(q) = p.questions.iter_mut().find(|q| q.id == a.id) {
                    q.answer = Some(a.text.trim().to_string());
                }
            }
        }
    }
    t.updated_at = now_ms();
    state.store.save_task(&t)?;
    let _ = app.emit("task-updated", &t);
    Ok(t)
}

/// Envía un mensaje del usuario al agente (chat en conversación abierta).
#[tauri::command]
fn send_chat(
    app: AppHandle,
    task_id: String,
    agent_id: String,
    message: String,
    state: State<AppState>,
) -> Result<Run, String> {
    let msg = message.trim().to_string();
    if msg.is_empty() {
        return Err("el mensaje está vacío".into());
    }
    let task = state.store.load_task(&task_id)?;
    let any_running = state
        .store
        .list_runs(&task_id)?
        .iter()
        .any(|r| r.status == "running");
    if any_running {
        return Err("ya hay una ejecución en curso para esta task".into());
    }
    // registra el mensaje del usuario en el historial antes de lanzar
    let mut t = task.clone();
    t.chat_messages.push(crate::model::ChatMessage {
        role: "user".into(),
        text: msg.clone(),
        created_at: now_ms(),
    });
    t.updated_at = now_ms();
    state.store.save_task(&t)?;
    let _ = app.emit("task-updated", &t);

    let agents = state.store.load_agents()?;
    let agent = runner::resolve_agent(&agents.agents, &agent_id)?;
    let ws = ws_path(&state)?;
    let store = state.store.clone();
    let registry = state.registry.clone();
    let run_id = new_id("run");
    let run_id_c = run_id.clone();
    let msg_c = msg.clone();
    let agent_label = agent.id.clone();
    std::thread::spawn(move || {
        let run = runner::run_chat(app.clone(), &store, &registry, &run_id_c, &t, &agent, &ws, &msg_c);
        let _ = app.emit("run-finished", &run);
    });
    Ok(Run {
        id: run_id,
        task_id,
        agent: agent_label,
        mode: "chat".into(),
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

/// Borra el historial de chat de la tarea.
#[tauri::command]
fn clear_chat(
    app: AppHandle,
    task_id: String,
    state: State<AppState>,
) -> Result<Task, String> {
    let mut t = state.store.load_task(&task_id)?;
    t.chat_messages.clear();
    t.updated_at = now_ms();
    state.store.save_task(&t)?;
    let _ = app.emit("task-updated", &t);
    Ok(t)
}

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
    resume_session: bool,
    skill_id: String,
    yolo: bool,
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
        if yolo {
            // YOLO: auto-aprueba los tickets pendientes antes de construir
            let mut t = task.clone();
            let mut changed = false;
            for tk in t.tickets.iter_mut() {
                if !tk.approved && tk.status != "done" {
                    tk.approved = true;
                    changed = true;
                }
            }
            if changed {
                t.updated_at = now_ms();
                state.store.save_task(&t)?;
                let _ = app.emit("task-updated", &t);
            }
        }
    }
    let agents = state.store.load_agents()?;
    let agent = runner::resolve_agent(&agents.agents, &agent_id)?;
    let ws = ws_path(&state)?;
    // contexto AGENTS.md si está activado
    let agents_md = crate::agents_md::load(&ws, &state.store);
    // perfil por paso: para exec se usa el agente del perfil si está configurado
    let agent = if kind == "exec" {
        let exec_id = state.store.load_workspace()?.exec_agent;
        match exec_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => runner::resolve_agent(&agents.agents, id)?,
            None => agent,
        }
    } else {
        agent
    };
    let store = state.store.clone();
    let registry = state.registry.clone();
    let kind = kind.to_string();
    let run_id = new_id("run");
    let run_id_c = run_id.clone();
    let mode_c = mode.clone();
    let agent_id_c = agent.id.clone();
    let skill_c = skill_id.clone();

    let app2 = app.clone();
    std::thread::spawn(move || {
        let run = if kind == "plan" {
            runner::run_plan(app2.clone(), &store, &registry, &run_id_c, &task, &agent, &mode_c, &ws, resume_session, &skill_c, agents_md.as_ref())
        } else {
            runner::run_exec(app2.clone(), &store, &registry, &run_id_c, &task, &agent, &mode_c, &ws, resume_session, agents_md.as_ref())
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
    resume_session: Option<bool>,
    skill_id: Option<String>,
    state: State<AppState>,
) -> Result<Run, String> {
    spawn_run(
        app,
        &state,
        task_id,
        agent_id,
        mode,
        "plan",
        resume_session.unwrap_or(false),
        skill_id.unwrap_or_default(),
        false,
    )
}

#[tauri::command]
fn start_exec_run(
    app: AppHandle,
    task_id: String,
    agent_id: String,
    mode: String,
    resume_session: Option<bool>,
    yolo: Option<bool>,
    state: State<AppState>,
) -> Result<Run, String> {
    spawn_run(app, &state, task_id, agent_id, mode, "exec", resume_session.unwrap_or(false), String::new(), yolo.unwrap_or(false))
}

// ---------- verificación (review comments) ----------

#[tauri::command]
fn start_verify_run(
    app: AppHandle,
    task_id: String,
    run_id: String,
    agent_id: String,
    state: State<AppState>,
) -> Result<Run, String> {
    let task = state.store.load_task(&task_id)?;
    let target = state.store.load_run(&task_id, &run_id)?;
    if target.mode == "plan" {
        return Err("solo se verifican runs de ejecución".into());
    }
    let any_running = state
        .store
        .list_runs(&task_id)?
        .iter()
        .any(|r| r.status == "running");
    if any_running {
        return Err("ya hay una ejecución en curso para esta task".into());
    }
    let agents = state.store.load_agents()?;
    let agent = runner::resolve_agent(&agents.agents, &agent_id)?;
    let ws = ws_path(&state)?;
    let store = state.store.clone();
    let registry = state.registry.clone();
    let new_run_id = new_id("run");
    let new_run_id_c = new_run_id.clone();
    let app2 = app.clone();
    let agent_c = agent.clone();
    let target_c = target.clone();
    std::thread::spawn(move || {
        let run = runner::verify_run(
            app2.clone(),
            &store,
            &registry,
            &new_run_id_c,
            &task,
            &target_c,
            &agent_c,
            &ws,
        );
        let _ = app2.emit("run-finished", &run);
    });
    Ok(Run {
        id: new_run_id,
        task_id,
        agent: agent.id.clone(),
        mode: "verify".into(),
        worktree: None,
        worktree_path: target.worktree_path.clone(),
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

/// "Fix all": un run de corrección con los comentarios abiertos como prompt.
#[tauri::command]
fn fix_comments(
    app: AppHandle,
    task_id: String,
    mode: String,
    agent_id: String,
    target_run_id: String,
    state: State<AppState>,
) -> Result<Run, String> {
    let task = state.store.load_task(&task_id)?;
    let target = state.store.load_run(&task_id, &target_run_id)?;
    if target.mode == "plan" {
        return Err("solo se corrigen runs de ejecución".into());
    }
    if target.status != "done" {
        return Err("solo se corrigen runs terminados".into());
    }
    let open: Vec<crate::model::ReviewComment> = task
        .review_comments
        .iter()
        .filter(|c| !c.resolved)
        .cloned()
        .collect();
    if open.is_empty() {
        return Err("no hay comentarios abiertos que corregir".into());
    }
    let mut intent = String::from(
        "Corrige los siguientes problemas detectados en la verificación (mantén el plan original):\n",
    );
    for c in &open {
        let file = c.file.clone().unwrap_or_default();
        let file_part = if file.is_empty() {
            String::new()
        } else {
            format!("({}) ", file)
        };
        intent.push_str(&format!(
            "- [{}] {}{}: {}\n",
            c.severity,
            file_part,
            c.title,
            c.detail
        ));
    }
    // la corrección corre sobre la task existente (sus tickets aprobados se
    // mantienen) y el prompt llega como intención adicional del run exec
    let any_running = state
        .store
        .list_runs(&task_id)?
        .iter()
        .any(|r| r.status == "running");
    if any_running {
        return Err("ya hay una ejecución en curso para esta task".into());
    }
    let agents = state.store.load_agents()?;
    let agent = runner::resolve_agent(&agents.agents, &agent_id)?;
    let ws = ws_path(&state)?;
    let agents_md = crate::agents_md::load(&ws, &state.store);
    let store = state.store.clone();
    let registry = state.registry.clone();
    let run_id = new_id("run");
    let mode_c = mode.clone();
    let task_c = task.clone();
    let intent_c = intent.clone();
    // la corrección corre dentro del worktree del run ejecutado: ahí viven los
    // cambios que revisó la verificación (ver run_fix_run en runner.rs)
    let target_c = target.clone();
    let app2 = app.clone();
    let agent_c = agent.clone();
    let run_id_c = run_id.clone();
    std::thread::spawn(move || {
        let run = runner::run_fix_run(
            app2.clone(),
            &store,
            &registry,
            &run_id_c,
            &task_c,
            &agent_c,
            &mode_c,
            &ws,
            &intent_c,
            Some(&target_c),
            agents_md.as_ref(),
        );
        let _ = app2.emit("run-finished", &run);
    });
    Ok(Run {
        id: run_id,
        task_id,
        agent: agent.id.clone(),
        mode: "fix".into(),
        worktree: None,
        worktree_path: target.worktree_path.clone(),
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

/// Marca un comentario como resuelto manualmente.
#[tauri::command]
fn resolve_comment(task_id: String, comment_id: String, state: State<AppState>) -> Result<Task, String> {
    let mut t = state.store.load_task(&task_id)?;
    let mut changed = false;
    for c in t.review_comments.iter_mut() {
        if c.id == comment_id {
            c.resolved = true;
            changed = true;
        }
    }
    if changed {
        t.updated_at = now_ms();
        state.store.save_task(&t)?;
    }
    Ok(t)
}

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
        .setup(move |app| {
            // runs que quedaron "running" de una sesión anterior → failed
            let state: State<AppState> = app.state();
            match state.store.fail_stale_running_runs() {
                Ok(ids) if !ids.is_empty() => {
                    eprintln!("nerve: {} run(s) huérfano(s) marcados como failed", ids.len());
                }
                Err(e) => eprintln!("nerve: no se pudieron limpiar runs huérfanos: {}", e),
                _ => {}
            }
            match epic::fail_stale_running(&state.store) {
                Ok(n) if n > 0 => {
                    eprintln!("nerve: {} epic(s) huérfano(s) marcados como failed", n);
                }
                Err(e) => eprintln!("nerve: no se pudieron limpiar epics huérfanos: {}", e),
                _ => {}
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            set_workspace,
            get_workspace,
            list_ollama_models,
            set_ollama,
            set_security,
            list_tasks,
            create_task,
            get_task,
            update_task,
            delete_task,
            save_ticket,
            delete_ticket,
            set_tickets,
            set_spec,
            generate_doc,
            delete_doc,
            ask_doc,
            answer_doc,
            send_chat,
            clear_chat,
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
            list_skills,
            save_skill,
            delete_skill,
            import_skill,
            list_epics,
            create_epic,
            start_epic,
            continue_epic,
            cancel_epic,
            delete_epic,
            set_epic_yolo,
            start_verify_run,
            fix_comments,
            resolve_comment,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}