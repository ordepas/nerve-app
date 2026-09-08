use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{atomic::Ordering, Arc};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::model::{AgentDef, Run, Task, Ticket};
use crate::runner::RunRegistry;
use crate::store::{new_id, now_ms, Store};

/// Epic = grupo de tasks organizadas en fases (capas del DAG de dependencias
/// del plan maestro) con gates de aprobación entre fases. El epic planifica
/// una "master task" con la intención global; sus tickets con `depends_on`
/// definen el orden: cada capa del DAG se convierte en una fase de tasks
/// reales que se planifican/ejecutan con el motor normal, gate a gate.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct EpicStage {
    pub title: String,
    pub tasks: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct EpicDef {
    pub id: String,
    pub title: String,
    pub intent: String,
    pub plan_task_id: String,
    pub agent_id: String,
    pub skill_id: String,
    #[serde(default)]
    pub stages: Vec<EpicStage>,
    #[serde(default)]
    pub current_stage: u32,
    pub status: String, // draft | running | awaiting_gate | done | failed | cancelled
    // qué se está aprobando en el gate: master | plan | fase
    #[serde(default)]
    pub gate: String,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub current_run: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

fn epics_dir(store: &Store) -> PathBuf {
    store.root.join("epics")
}

pub fn save_epic(store: &Store, epic: &EpicDef) -> Result<(), String> {
    let dir = epics_dir(store);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(
        dir.join(format!("{}.json", epic.id)),
        serde_json::to_string_pretty(epic).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub fn load_epic(store: &Store, id: &str) -> Result<EpicDef, String> {
    let p = epics_dir(store).join(format!("{}.json", id));
    let data = fs::read_to_string(&p).map_err(|e| format!("no se pudo leer el epic: {}", e))?;
    serde_json::from_str(&data).map_err(|e| e.to_string())
}

pub fn list_epics(store: &Store) -> Result<Vec<EpicDef>, String> {
    let mut out = Vec::new();
    let dir = epics_dir(store);
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }
            if let Ok(data) = fs::read_to_string(&p) {
                if let Ok(epic) = serde_json::from_str::<EpicDef>(&data) {
                    out.push(epic);
                }
            }
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

pub fn delete_epic(store: &Store, id: &str) -> Result<(), String> {
    let p = epics_dir(store).join(format!("{}.json", id));
    if p.exists() {
        fs::remove_file(&p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Al arrancar la app: epics que quedaron "running" → failed.
pub fn fail_stale_running(store: &Store) -> Result<usize, String> {
    let mut n = 0;
    for mut epic in list_epics(store)? {
        if epic.status == "running" {
            epic.status = "failed".into();
            epic.last_error = Some("interrumpido: la app se cerró mientras el epic corría".into());
            epic.current_run = None;
            epic.updated_at = now_ms();
            save_epic(store, &epic)?;
            n += 1;
        }
    }
    Ok(n)
}

fn new_task(store: &Store, title: &str, intent: &str) -> Result<Task, String> {
    let now = now_ms();
    let task = Task {
        id: new_id("task"),
        title: title.to_string(),
        intent: intent.to_string(),
        status: "planning".into(),
        created_at: now,
        updated_at: now,
        spec_current: None,
        spec_versions: Vec::new(),
        tickets: Vec::new(),
    };
    store.save_task(&task)?;
    Ok(task)
}

pub fn create_epic(
    store: &Store,
    title: &str,
    intent: &str,
    agent_id: &str,
    skill_id: &str,
) -> Result<EpicDef, String> {
    let master = new_task(store, title, intent)?;
    let now = now_ms();
    let epic = EpicDef {
        id: new_id("epic"),
        title: title.to_string(),
        intent: intent.to_string(),
        plan_task_id: master.id,
        agent_id: agent_id.to_string(),
        skill_id: skill_id.to_string(),
        stages: Vec::new(),
        current_stage: 0,
        status: "draft".into(),
        gate: String::new(),
        last_error: None,
        current_run: None,
        created_at: now,
        updated_at: now,
    };
    save_epic(store, &epic)?;
    Ok(epic)
}

fn ticket_intent(t: &Ticket) -> String {
    let mut s = String::new();
    s.push_str(&t.title);
    if !t.description.is_empty() {
        s.push_str("\n\n");
        s.push_str(&t.description);
    }
    if !t.acceptance.is_empty() {
        s.push_str("\n\nCriterios de aceptación:\n");
        for a in &t.acceptance {
            s.push_str("- ");
            s.push_str(a);
            s.push('\n');
        }
    }
    if let Some(v) = &t.verify_command {
        if !v.trim().is_empty() {
            s.push_str("\n\nComando de verificación: ");
            s.push_str(v);
        }
    }
    s
}

/// Capas topológicas del DAG de tickets (Kahn). Ciclos y huérfanos caen en
/// la última capa para no bloquear el epic.
fn layers_from_tickets(tickets: &[Ticket]) -> Vec<Vec<usize>> {
    let n = tickets.len();
    let idx: HashMap<&str, usize> = tickets
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.as_str(), i))
        .collect();
    let mut deps = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, t) in tickets.iter().enumerate() {
        for d in &t.depends_on {
            if let Some(&j) = idx.get(d.as_str()) {
                if j != i {
                    deps[i] += 1;
                    dependents[j].push(i);
                }
            }
        }
    }
    let mut layers: Vec<Vec<usize>> = Vec::new();
    let mut done = vec![false; n];
    loop {
        let layer: Vec<usize> = (0..n).filter(|&i| !done[i] && deps[i] == 0).collect();
        if layer.is_empty() {
            break;
        }
        for &i in &layer {
            done[i] = true;
        }
        for &i in &layer {
            for &d in &dependents[i] {
                deps[d] -= 1;
            }
        }
        layers.push(layer);
    }
    let rest: Vec<usize> = (0..n).filter(|&i| !done[i]).collect();
    if !rest.is_empty() {
        match layers.last_mut() {
            Some(last) => last.extend(rest),
            None => layers.push(rest),
        }
    }
    layers
}

fn stages_from_tickets(store: &Store, tickets: &[Ticket]) -> Result<Vec<EpicStage>, String> {
    let mut task_of: HashMap<String, String> = HashMap::new();
    for t in tickets {
        let task = new_task(store, &t.title, &ticket_intent(t))?;
        task_of.insert(t.id.clone(), task.id.clone());
    }
    let mut stages = Vec::new();
    for (k, layer) in layers_from_tickets(tickets).into_iter().enumerate() {
        let ids: Vec<String> = layer
            .iter()
            .filter_map(|&i| tickets.get(i).and_then(|t| task_of.get(&t.id)).cloned())
            .collect();
        if ids.is_empty() {
            continue;
        }
        stages.push(EpicStage {
            title: format!("Fase {}", k + 1),
            tasks: ids,
        });
    }
    Ok(stages)
}

fn tickets_from_run(run: &Run) -> Result<Vec<Ticket>, String> {
    let ev = run
        .events
        .iter()
        .rev()
        .find(|e| e.kind == "plan" && e.text.is_some())
        .ok_or("el run de plan no dejó bloque JSON de tickets")?;
    let v: serde_json::Value = serde_json::from_str(ev.text.as_deref().unwrap_or("{}"))
        .map_err(|e| format!("JSON del plan inválido: {}", e))?;
    let list = v
        .get("tickets")
        .cloned()
        .ok_or("el bloque JSON del plan no tiene tickets")?;
    serde_json::from_value::<Vec<Ticket>>(list).map_err(|e| e.to_string())
}

/// Tickets del plan maestro: del run de planificación más reciente.
fn master_tickets(store: &Store, plan_task_id: &str) -> Option<Vec<Ticket>> {
    let runs = store.list_runs(plan_task_id).ok()?;
    for r in &runs {
        if r.mode != "plan" {
            continue;
        }
        if let Ok(tks) = tickets_from_run(r) {
            if !tks.is_empty() {
                return Some(tks);
            }
        }
    }
    store
        .load_task(plan_task_id)
        .ok()
        .filter(|t| !t.tickets.is_empty())
        .map(|t| t.tickets)
}

fn agent_and_ws(store: &Store, agent_id: &str) -> Result<(AgentDef, PathBuf), String> {
    let agents = store.load_agents()?;
    let agent = crate::runner::resolve_agent(&agents.agents, agent_id)?;
    let cfg = store.load_workspace()?;
    let ws = match cfg.project_path {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => return Err("Configura primero la carpeta del proyecto en Ajustes".into()),
    };
    Ok((agent, ws))
}

fn set_status(
    store: &Store,
    epic: &mut EpicDef,
    status: &str,
    error: Option<String>,
) -> Result<(), String> {
    epic.status = status.to_string();
    epic.last_error = error;
    epic.updated_at = now_ms();
    save_epic(store, epic)
}

/// Arranca el motor del epic en un hilo (solo desde los estados `from`).
pub fn kick(
    app: &AppHandle,
    store: &Store,
    registry: &Arc<RunRegistry>,
    epic_id: &str,
    from: &[&str],
) -> Result<EpicDef, String> {
    let mut epic = load_epic(store, epic_id)?;
    if !from.contains(&epic.status.as_str()) {
        return Err(format!(
            "el epic está en estado '{}' y no admite esta acción ahora",
            epic.status
        ));
    }
    epic.status = "running".into();
    epic.last_error = None;
    epic.updated_at = now_ms();
    save_epic(store, &epic)?;
    let _ = app.emit("epic-updated", &epic);

    let app2 = app.clone();
    let app3 = app.clone();
    let store2 = store.clone();
    let registry2 = registry.clone();
    let id = epic_id.to_string();
    std::thread::spawn(move || {
        if let Err(e) = engine_loop(app2, &store2, &registry2, &id) {
            let _ = fail_epic(&app3, &store2, &id, e);
        }
    });
    Ok(epic)
}

fn fail_epic(app: &AppHandle, store: &Store, epic_id: &str, e: String) -> Result<EpicDef, String> {
    let mut epic = load_epic(store, epic_id)?;
    if epic.status == "cancelled" {
        return Ok(epic);
    }
    epic.status = "failed".into();
    epic.last_error = Some(e);
    epic.current_run = None;
    epic.updated_at = now_ms();
    save_epic(store, &epic)?;
    let _ = app.emit("epic-updated", &epic);
    Ok(epic)
}

pub fn cancel_epic(
    store: &Store,
    registry: &Arc<RunRegistry>,
    epic_id: &str,
) -> Result<EpicDef, String> {
    let mut epic = load_epic(store, epic_id)?;
    if let Some(rid) = epic.current_run.take() {
        let reg = registry.lock().unwrap();
        if let Some(cs) = reg.get(&rid) {
            cs.flag.store(true, Ordering::Relaxed);
            if let Some(pid) = *cs.pid.lock().unwrap() {
                crate::kill_pid(pid);
            }
        }
    }
    set_status(store, &mut epic, "cancelled", None)?;
    Ok(epic)
}

/// Aprueba los tickets pendientes de la task (el gate de fase los autoriza).
fn approve_tickets(store: &Store, task_id: &str, app: &AppHandle) -> Result<Task, String> {
    let mut t = store.load_task(task_id)?;
    let mut changed = false;
    for tk in t.tickets.iter_mut() {
        if !tk.approved && tk.status != "done" {
            tk.approved = true;
            changed = true;
        }
    }
    if changed {
        t.updated_at = now_ms();
        store.save_task(&t)?;
        let _ = app.emit("task-updated", &t);
    }
    Ok(t)
}

/// Lanza un run de planificación para `task` dentro del epic (bloqueante).
/// El Run devuelto se recarga del store para incluir sus eventos (el plan
/// JSON queda persistido como evento "plan").
fn plan_one(
    app: &AppHandle,
    store: &Store,
    registry: &Arc<RunRegistry>,
    epic: &mut EpicDef,
    task: &Task,
) -> Result<Run, String> {
    let (agent, ws) = agent_and_ws(store, &epic.agent_id)?;
    let run_id = new_id("run");
    epic.current_run = Some(run_id.clone());
    epic.updated_at = now_ms();
    save_epic(store, epic)?;
    let _ = app.emit("epic-updated", &*epic);
    let run = crate::runner::run_plan(
        app.clone(),
        store,
        registry,
        &run_id,
        task,
        &agent,
        "plan",
        &ws,
        false,
        &epic.skill_id,
    );
    epic.current_run = None;
    epic.updated_at = now_ms();
    save_epic(store, epic)?;
    let _ = app.emit("epic-updated", &*epic);
    Ok(store.load_run(&run.task_id, &run.id)?)
}

/// Ejecuta una task del epic en worktree propio (bloqueante).
fn exec_one(
    app: &AppHandle,
    store: &Store,
    registry: &Arc<RunRegistry>,
    epic: &mut EpicDef,
    task: &Task,
) -> Result<Run, String> {
    let (agent, ws) = agent_and_ws(store, &epic.agent_id)?;
    let run_id = new_id("run");
    epic.current_run = Some(run_id.clone());
    epic.updated_at = now_ms();
    save_epic(store, epic)?;
    let _ = app.emit("epic-updated", &*epic);
    let run = crate::runner::run_exec(
        app.clone(),
        store,
        registry,
        &run_id,
        task,
        &agent,
        "worktree",
        &ws,
        false,
    );
    epic.current_run = None;
    epic.updated_at = now_ms();
    save_epic(store, epic)?;
    let _ = app.emit("epic-updated", &*epic);
    Ok(run)
}

fn gate_if_running(
    app: &AppHandle,
    store: &Store,
    epic: &mut EpicDef,
    gate: &str,
) -> Result<(), String> {
    if load_epic(store, &epic.id)?.status == "cancelled" {
        return Ok(());
    }
    set_status(store, epic, "awaiting_gate", None)?;
    epic.gate = gate.to_string();
    epic.updated_at = now_ms();
    save_epic(store, epic)?;
    let _ = app.emit("epic-updated", &*epic);
    Ok(())
}

/// Cierra un run dentro del motor: true = seguir; false = parar (ya marcado).
fn handle_run_end(
    app: &AppHandle,
    store: &Store,
    epic_id: &str,
    epic: &mut EpicDef,
    run: &Run,
    fail_msg: &str,
) -> Result<bool, String> {
    match run.status.as_str() {
        "done" => Ok(true),
        "cancelled" => {
            set_status(store, epic, "cancelled", None)?;
            Ok(false)
        }
        _ => {
            fail_epic(
                &app,
                store,
                epic_id,
                run.summary
                    .clone()
                    .unwrap_or_else(|| fail_msg.to_string()),
            )?;
            Ok(false)
        }
    }
}

/// Motor del epic (hilo dedicado, bloqueante). Un segmento por kick:
/// 1) planifica el plan maestro y crea las fases (→ gate "master"),
/// 2) planifica la siguiente task pendiente de la fase en curso (→ gate "plan"),
/// 3) construye una task aprobada de la fase (→ gate "fase"),
/// 4) fase completa → avanza en silencio y sigue con la siguiente.
fn engine_loop(
    app: AppHandle,
    store: &Store,
    registry: &Arc<RunRegistry>,
    epic_id: &str,
) -> Result<(), String> {
    loop {
        let mut epic = load_epic(store, epic_id)?;
        if epic.status != "running" {
            return Ok(()); // cancelado por el usuario
        }

        // 1) plan maestro + creación de fases
        if epic.stages.is_empty() {
            let tickets = match master_tickets(store, &epic.plan_task_id) {
                Some(t) if !t.is_empty() => t,
                _ => {
                    let task = store.load_task(&epic.plan_task_id)?;
                    let run = plan_one(&app, store, registry, &mut epic, &task)?;
                    if !handle_run_end(&app, store, epic_id, &mut epic, &run, "la planificación del epic falló")? {
                        return Ok(());
                    }
                    tickets_from_run(&run)?
                }
            };
            epic.stages = stages_from_tickets(store, &tickets)?;
            epic.current_stage = 0;
            gate_if_running(&app, store, &mut epic, "master")?;
            return Ok(());
        }

        // 2) fase en curso
        let Some(stage_tasks) = epic
            .stages
            .get(epic.current_stage as usize)
            .map(|s| s.tasks.clone())
        else {
            set_status(store, &mut epic, "done", None)?;
            epic.gate = String::new();
            epic.updated_at = now_ms();
            save_epic(store, &epic)?;
            let _ = app.emit("epic-updated", &epic);
            return Ok(());
        };

        // tasks borradas por el usuario: se purgan de la fase
        let kept: Vec<String> = stage_tasks
            .iter()
            .filter(|tid| store.load_task(tid).is_ok())
            .cloned()
            .collect();
        if kept.len() != stage_tasks.len() {
            if let Some(st) = epic.stages.get_mut(epic.current_stage as usize) {
                st.tasks = kept;
            }
            epic.updated_at = now_ms();
            save_epic(store, &epic)?;
            continue;
        }

        // estados inesperados (p. ej. blocked por verificación fallida) → parar
        for tid in &stage_tasks {
            if let Ok(t) = store.load_task(tid) {
                if matches!(t.status.as_str(), "planning" | "ready" | "in_dev" | "done") {
                    continue;
                }
                fail_epic(
                    &app,
                    store,
                    epic_id,
                    format!(
                        "la task '{}' está en estado '{}'; revísala y reintenta el epic",
                        t.title, t.status
                    ),
                )?;
                return Ok(());
            }
        }

        // 3) planificar la siguiente task de la fase sin plan
        for tid in &stage_tasks {
            if let Ok(t) = store.load_task(tid) {
                if t.status == "planning" {
                    let run = plan_one(&app, store, registry, &mut epic, &t)?;
                    if !handle_run_end(&app, store, epic_id, &mut epic, &run, "la planificación de la fase falló")? {
                        return Ok(());
                    }
                    // plan sin tickets → task completa (solo documentación)
                    let fresh = store.load_task(&t.id)?;
                    if fresh.tickets.is_empty() {
                        let mut f2 = fresh;
                        f2.status = "done".into();
                        f2.updated_at = now_ms();
                        store.save_task(&f2)?;
                        let _ = app.emit("task-updated", &f2);
                    }
                    gate_if_running(&app, store, &mut epic, "plan")?;
                    return Ok(());
                }
            }
        }

        // 4) construir la siguiente task aprobada de la fase
        for tid in &stage_tasks {
            if let Ok(mut t) = store.load_task(tid) {
                if t.status == "ready" || t.status == "in_dev" {
                    t = approve_tickets(store, &t.id, &app)?;
                    if t.tickets.is_empty() {
                        // task sin tickets: nada que construir
                        t.status = "done".into();
                        t.updated_at = now_ms();
                        store.save_task(&t)?;
                        let _ = app.emit("task-updated", &t);
                        continue;
                    }
                    let all_done = t.tickets.iter().all(|tk| tk.status == "done");
                    if all_done {
                        t.status = "done".into();
                        t.updated_at = now_ms();
                        store.save_task(&t)?;
                        let _ = app.emit("task-updated", &t);
                        continue;
                    }
                    let run = exec_one(&app, store, registry, &mut epic, &t)?;
                    if !handle_run_end(&app, store, epic_id, &mut epic, &run, "la ejecución de la fase falló")? {
                        return Ok(());
                    }
                    let fresh = store.load_task(&t.id)?;
                    if fresh.status == "blocked" {
                        fail_epic(
                            &app,
                            store,
                            epic_id,
                            format!(
                                "la verificación falló en '{}'; revísala y reintenta el epic",
                                fresh.title
                            ),
                        )?;
                        return Ok(());
                    }
                    gate_if_running(&app, store, &mut epic, "fase")?;
                    return Ok(());
                }
            }
        }

        // 5) fase completa → avanzar en silencio y seguir
        let all_done = stage_tasks
            .iter()
            .all(|tid| store.load_task(tid).map(|t| t.status == "done").unwrap_or(false));
        if all_done {
            epic.current_stage += 1;
            epic.updated_at = now_ms();
            save_epic(store, &epic)?;
            continue;
        }
        // nada pendiente pero no todo done: parar para revisión humana
        gate_if_running(&app, store, &mut epic, "fase")?;
        return Ok(());
    }
}