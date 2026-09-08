use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::git;
use crate::mock_agent;
use crate::model::{AgentDef, Run, RunEvent, Task, Ticket};
use crate::ollama;
use crate::store::{new_id, now_ms, Store};

/// Mata el proceso hijo al soltarlo (cancelación o fin del run).
pub struct KillChild(pub std::process::Child);

impl Drop for KillChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Default, Clone)]
pub struct CancelState {
    pub flag: Arc<AtomicBool>,
    pub pid: Arc<Mutex<Option<u32>>>,
}

impl CancelState {
    pub fn cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

pub type RunRegistry = Mutex<std::collections::HashMap<String, CancelState>>;

pub fn register_run(registry: &RunRegistry, run_id: &str) -> CancelState {
    let st = CancelState::default();
    registry
        .lock()
        .unwrap()
        .insert(run_id.to_string(), st.clone());
    st
}

pub fn unregister_run(registry: &RunRegistry, run_id: &str) {
    registry.lock().unwrap().remove(run_id);
}

pub fn plan_prompt(intent: &str) -> String {
    format!(
        "Eres el planificador de Nerve. A partir de la intención del usuario, inspecciona el repositorio (solo lectura) y produce:\n\n1) Una especificación en Markdown con: propósito, alcance, decisiones de diseño y criterios de aceptación globales.\n2) Una lista de tickets de implementación.\n\nTu respuesta final debe terminar EXACTAMENTE con este bloque JSON en una línea:\n```json\n{{\"tickets\":[{{\"id\":\"T1\",\"title\":\"...\",\"description\":\"...\",\"acceptance\":[\"...\"],\"verify_command\":\"comando opcional o null\",\"depends_on\":[]}}]}}\n```\n\nReglas: tickets pequeños y verificables; usa depends_on con ids si hay orden; no modifiques ningún archivo.\n\nIntención del usuario:\n{intent}"
    )
}

/// Prompt del planificador: plantilla de la skill (si trae) con la intención
/// incrustada; si la plantilla no tiene {intent}, se añade al final.
pub fn plan_prompt_for_skill(intent: &str, template: Option<String>) -> String {
    match template {
        None => plan_prompt(intent),
        Some(t) if t.trim().is_empty() => plan_prompt(intent),
        Some(t) => {
            if t.contains("{intent}") {
                t.replace("{intent}", intent)
            } else {
                format!("{}\n\nIntención del usuario:\n{}", t, intent)
            }
        }
    }
}

pub type ExecTicket = (String, String, String, Vec<String>, Option<String>);

pub fn exec_prompt(task_title: &str, tickets: &[ExecTicket], extra: Option<&str>) -> String {
    let mut list = String::new();
    for (id, title, desc, acc, verify) in tickets {
        list.push_str(&format!(
            "- id: {}\n  título: {}\n  descripción: {}\n  criterios de aceptación: {}\n  verificación: {}\n",
            id,
            title,
            desc,
            if acc.is_empty() {
                "-".to_string()
            } else {
                acc.join("; ")
            },
            verify.clone().unwrap_or_else(|| "-".to_string())
        ));
    }
    let mut s = format!(
        "Eres un agente de ejecución de Nerve. Tarea: {task_title}\n\nImplementa los siguientes tickets, en orden de dependencias:\n{list}\n\nReglas:\n- Trabaja SOLO sobre el directorio actual (es un git worktree aislado).\n- No hagas commits ni nada relacionado con git.\n- Cuando termines cada ticket, imprime una línea exacta: TICKET_DONE: <id>\n- Cuando termines todos, imprime una línea exacta: NERVE_RUN_COMPLETE\n- No imprimas las líneas de control dentro de bloques de código."
    );
    if let Some(ctx) = extra {
        s.push_str(ctx);
    }
    s
}

pub fn extract_json_block(text: &str) -> Option<Value> {
    let candidate = if let Some(start) = text.find("```json") {
        let rest = &text[start + 7..];
        let end = rest.find("```").unwrap_or(rest.len());
        rest[..end].trim().to_string()
    } else {
        text.lines()
            .rev()
            .find(|l| {
                let s = l.trim_start();
                s.starts_with('{') && (s.contains("\"tickets\"") || s.contains("\"comments\""))
            })
            .map(|l| l.trim().to_string())?
    };
    serde_json::from_str(&candidate).ok()
}

pub fn emit(
    app: &AppHandle,
    store: &Store,
    task_id: &str,
    run_id: &str,
    kind: &str,
    text: Option<String>,
    ticket_id: Option<String>,
) {
    let ev = RunEvent {
        ts: now_ms(),
        kind: kind.to_string(),
        text,
        ticket_id,
    };
    let _ = store.append_event(task_id, run_id, &ev);
    let _ = app.emit(
        "run-event",
        json!({"taskId": task_id, "runId": run_id, "event": ev}),
    );
}

pub fn resolve_agent<'a>(agents: &'a [AgentDef], id: &str) -> Result<AgentDef, String> {
    let a = agents
        .iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("agente {} no encontrado", id))?;
    if !a.enabled || a.kind == "disabled" {
        return Err(format!("agente {} deshabilitado", id));
    }
    Ok(a.clone())
}

/// Ejecuta el verify_command del ticket y devuelve (exit_code, salida).
/// Vía .bat temporal para preservar el quoting del comando (ver ollama.rs).
#[cfg(windows)]
fn run_verify(cwd: &Path, command: &str) -> (i32, String) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let bat = std::env::temp_dir().join(format!(
        "nerve-verify-{}-{}.bat",
        now_ms(),
        std::process::id()
    ));
    if fs::write(&bat, format!("@echo off\r\n{}\r\n", command)).is_err() {
        let out = Command::new("cmd")
            .args(["/d", "/s", "/c", command])
            .current_dir(cwd)
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        return match out {
            Ok(o) => (
                o.status.code().unwrap_or(-1),
                format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
            ),
            Err(e) => (-1, format!("no se pudo lanzar el comando: {}", e)),
        };
    }
    let out = Command::new("cmd")
        .arg("/d")
        .arg("/c")
        .arg(bat.to_string_lossy().to_string())
        .current_dir(cwd)
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = fs::remove_file(&bat);
    match out {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
        ),
        Err(e) => (-1, format!("no se pudo lanzar el comando: {}", e)),
    }
}

#[cfg(not(windows))]
fn run_verify(cwd: &Path, command: &str) -> (i32, String) {
    let out = Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .output();
    match out {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
        ),
        Err(e) => (-1, format!("no se pudo lanzar el comando: {}", e)),
    }
}

/// Escribe el prompt en un archivo temporal (lo pasamos por stdin para evitar
/// problemas de quoting y saltos de línea en cmd.exe).
fn write_prompt_file(prompt: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir();
    let p = dir.join(format!("nerve-prompt-{}-{}.txt", now_ms(), new_id("p")));
    fs::write(&p, prompt).map_err(|e| format!("no se pudo escribir el prompt temporal: {}", e))?;
    Ok(p)
}

/// Lanza el CLI del agente en modo headless con stream-json.
#[cfg(windows)]
fn spawn_agent(bin: &str, args: &[String], cwd: &Path, stdin_file: Option<&Path>) -> Result<KillChild, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new("cmd");
    cmd.arg("/c").arg(bin).args(args);
    cmd.current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    match stdin_file {
        Some(f) => {
            let file = fs::File::open(f).map_err(|e| e.to_string())?;
            cmd.stdin(Stdio::from(file));
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    cmd.spawn()
        .map(KillChild)
        .map_err(|e| format!("no se pudo lanzar {}: {}", bin, e))
}

/// Igual que spawn_agent pero mantiene stdin ABIERTO (requerido por codex exec).
#[cfg(windows)]
fn spawn_agent_codex(bin: &str, args: &[String], cwd: &Path, stdin_file: Option<&Path>) -> Result<KillChild, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new("cmd");
    cmd.arg("/c").arg(bin).args(args);
    cmd.current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd
        .spawn()
        .map(KillChild)
        .map_err(|e| format!("no se pudo lanzar {}: {}", bin, e))?;
    if let Some(f) = stdin_file {
        let mut fh = fs::File::open(f).map_err(|e| e.to_string())?;
        let mut stdin = child.0.stdin.take().ok_or("sin stdin del hijo")?;
        std::io::copy(&mut fh, &mut stdin).map_err(|e| e.to_string())?;
        drop(stdin); // EOF: codex procede
    } else {
        drop(child.0.stdin.take());
    }
    Ok(child)
}

#[cfg(not(windows))]
fn spawn_agent(bin: &str, args: &[String], cwd: &Path, stdin_file: Option<&Path>) -> Result<KillChild, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match stdin_file {
        Some(f) => {
            let file = fs::File::open(f).map_err(|e| e.to_string())?;
            cmd.stdin(Stdio::from(file));
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    cmd.spawn()
        .map(KillChild)
        .map_err(|e| format!("no se pudo lanzar {}: {}", bin, e))
}

#[cfg(not(windows))]
fn spawn_agent_codex(bin: &str, args: &[String], cwd: &Path, stdin_file: Option<&Path>) -> Result<KillChild, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map(KillChild)
        .map_err(|e| format!("no se pudo lanzar {}: {}", bin, e))?;
    if let Some(f) = stdin_file {
        let mut fh = fs::File::open(f).map_err(|e| e.to_string())?;
        let mut stdin = child.0.stdin.take().ok_or("sin stdin del hijo")?;
        std::io::copy(&mut fh, &mut stdin).map_err(|e| e.to_string())?;
        drop(stdin);
    } else {
        drop(child.0.stdin.take());
    }
    Ok(child)
}

/// Lee el stream JSONL de codex exec --json. La respuesta final llega vía el
/// archivo de --output-last-message; usamos los eventos solo para el session id.
fn read_stream_codex(child: &mut KillChild, cancel: &CancelState, last_msg_path: &Path) -> Result<(String, Option<String>), String> {
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or("no se pudo leer stdout del agente")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut session_id: Option<String> = None;
    loop {
        if cancel.cancelled() {
            return Err("__cancelled__".into());
        }
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(id) = v.get("thread_id").and_then(Value::as_str).or_else(|| v.get("session_id").and_then(Value::as_str)) {
            session_id = Some(id.to_string());
        }
        if v.get("type").and_then(Value::as_str) == Some("error") {
            return Err(format!("codex: {}", v.get("message").and_then(Value::as_str).unwrap_or("error desconocido")));
        }
    }
    if !child.0.wait().map(|s| s.success()).unwrap_or(false) {
        // aun con exit != 0, si hay mensaje final lo aprovechamos; si no, error
        let text = fs::read_to_string(last_msg_path).unwrap_or_default();
        if text.trim().is_empty() {
            return Err("codex terminó con error (ver log)".into());
        }
        return Ok((text, session_id));
    }
    let text = fs::read_to_string(last_msg_path).unwrap_or_default();
    Ok((text, session_id))
}

/// Presupuesto de pasos del run: cuenta tool calls vistos en el stream y
/// aborta (como cancelación) al agotar el límite (0 = sin límite).
#[derive(Clone)]
pub struct StepBudget {
    max: u64,
    count: Arc<Mutex<u64>>,
    exceeded: Arc<AtomicBool>,
}

impl StepBudget {
    pub fn new(max: u64) -> Self {
        Self { max, count: Arc::new(Mutex::new(0)), exceeded: Arc::new(AtomicBool::new(false)) }
    }
    pub fn tick(&self) -> Result<(), String> {
        if self.max == 0 {
            return Ok(());
        }
        let mut c = self.count.lock().unwrap();
        *c += 1;
        if *c > self.max {
            self.exceeded.store(true, Ordering::Relaxed);
            return Err(format!(
                "presupuesto agotado: el run superó {} pasos del agente",
                self.max
            ));
        }
        Ok(())
    }
    pub fn used(&self) -> u64 {
        *self.count.lock().unwrap()
    }
    pub fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::Relaxed)
    }
}

/// Lee stream-json del hijo hasta el evento result; devuelve (texto_final, session_id).
/// Cada evento intermedio legible (texto parcial, uso de herramientas) se pasa
/// a `on_event` en vivo para que la UI lo muestre mientras el agente trabaja.
fn read_stream(
    child: &mut KillChild,
    cancel: &CancelState,
    budget: &StepBudget,
    allowlist: &[String],
    mut on_event: impl FnMut(String),
) -> Result<(String, Option<String>), String> {
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or("no se pudo leer stdout del agente")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut final_text = String::new();
    let mut session_id: Option<String> = None;
    loop {
        if cancel.cancelled() {
            return Err("__cancelled__".into());
        }
        if budget.exceeded() {
            return Err("__cancelled__".into());
        }
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(desc) = describe_stream_event(&v) {
            on_event(desc.clone());
            // allowlist: comandos del agente fuera de la lista abortan el run
            if command_tool_violation(&v, allowlist) == Some(false) {
                let _ = child.0.kill();
                return Err("comando bloqueado: no está en la allowlist del workspace".into());
            }
            // cada tool call consume presupuesto
            if desc.contains('→') {
                if let Err(e) = budget.tick() {
                    eprintln!("nerve: {}", e);
                    // abortar el run: el loop detecta exceeded() y corta
                    let _ = child.0.kill();
                    return Err(e);
                }
            }
        }
        match v.get("type").and_then(Value::as_str) {
            Some("system") => {
                if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                    session_id = Some(sid.to_string());
                }
            }
            Some("assistant") => {
                if let Some(content) = v.pointer("/message/content").and_then(Value::as_array) {
                    for item in content {
                        if item.get("type").and_then(Value::as_str) == Some("text") {
                            if let Some(t) = item.get("text").and_then(Value::as_str) {
                                final_text.push_str(t);
                            }
                        }
                    }
                }
            }
            Some("result") => {
                if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                    session_id = Some(sid.to_string());
                }
                if let Some(r) = v.get("result").and_then(Value::as_str) {
                    final_text = r.to_string();
                }
                let is_err = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                if is_err {
                    return Err("el agente terminó con error".into());
                }
                return Ok((final_text, session_id));
            }
            _ => {}
        }
    }
    Ok((final_text, session_id))
}

/// Resumen legible de un evento intermedio del stream (funciona con el formato
/// de qwen/claude/gemini: bloques de texto y tool_use dentro de "assistant").
fn describe_stream_event(v: &Value) -> Option<String> {
    let ty = v.get("type").and_then(Value::as_str)?;
    match ty {
        "assistant" => {
            let arr = v.pointer("/message/content").and_then(Value::as_array)?;
            let mut parts: Vec<String> = Vec::new();
            for item in arr {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let t = item.get("text").and_then(Value::as_str).unwrap_or("").trim();
                        if !t.is_empty() {
                            parts.push(t.chars().take(200).collect());
                        }
                    }
                    Some("tool_use") => {
                        let name = item.get("name").and_then(Value::as_str).unwrap_or("herramienta");
                        let brief = tool_input_brief(item.get("input"));
                        if brief.is_empty() {
                            parts.push(format!("→ {}", name));
                        } else {
                            parts.push(format!("→ {} {}", name, brief));
                        }
                    }
                    _ => {}
                }
            }
            if parts.is_empty() { None } else { Some(parts.join(" · ")) }
        }
        // qwen/gemini usan tool_use a nivel raíz en algunos eventos
        "tool_use" => {
            let name = v.get("name").and_then(Value::as_str).unwrap_or("herramienta");
            let brief = tool_input_brief(v.get("input").or_else(|| v.get("args")));
            Some(if brief.is_empty() { format!("→ {}", name) } else { format!("→ {} {}", name, brief) })
        }
        _ => None,
    }
}

/// Primer campo descriptivo del input de una herramienta (archivo, comando…).
fn tool_input_brief(input: Option<&Value>) -> String {
    let Some(Value::Object(m)) = input else { return String::new() };
    for k in ["file_path", "path", "command", "pattern", "url", "description", "prompt"] {
        if let Some(Value::String(s)) = m.get(k) {
            let s = s.replace('\n', " ");
            return s.chars().take(120).collect();
        }
    }
    String::new()
}

/// ¿El comando está permitido por la allowlist? (prefijos; lista vacía = libre)
/// Ignora prefijos triviales de entorno (cd, set) que los CLIs anteponen.
pub fn command_allowed(command: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    let cmd = command.trim_start();
    let first = cmd
        .split_whitespace()
        .find(|w| !w.eq_ignore_ascii_case("cd") || cmd[..cmd.len() - w.len()].contains("&&"))
        .unwrap_or("");
    // toma el primer token real (saltando "cd X &&")
    let mut tokens = cmd.split_whitespace();
    let mut candidate = tokens.next().unwrap_or("");
    let lower = candidate.to_ascii_lowercase();
    if lower == "cd" {
        // salta hasta después del "&&" si existe
        if let Some(pos) = cmd.to_ascii_lowercase().find("&&") {
            candidate = cmd[pos + 2..].trim_start().split_whitespace().next().unwrap_or("");
        }
    }
    let candidate = candidate.trim_matches(|c| c == '"' || c == '\'');
    let _ = first;
    allowlist
        .iter()
        .any(|a| {
            let a = a.trim().trim_matches(|c| c == '"' || c == '\'');
            !a.is_empty()
                && (candidate.eq_ignore_ascii_case(a)
                    || candidate
                        .to_ascii_lowercase()
                        .starts_with(&a.to_ascii_lowercase()))
        })
}

/// Verifica el comando de un tool_use "run_command"/"bash" contra la allowlist.
/// Devuelve None si la herramienta no es de comandos, Some(false) si viola.
fn command_tool_violation(v: &Value, allowlist: &[String]) -> Option<bool> {
    if allowlist.is_empty() {
        return None;
    }
    let obj = v.pointer("/message/content").and_then(Value::as_array)?;
    for item in obj {
        if item.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let is_cmd_tool = ["bash", "shell", "run_command", "execute_command", "cmd", "terminal"]
            .iter()
            .any(|t| name.to_ascii_lowercase().contains(t));
        if !is_cmd_tool {
            continue;
        }
        if let Some(cmd) = item.pointer("/input/command").and_then(Value::as_str) {
            if !command_allowed(cmd, allowlist) {
                return Some(false);
            }
        }
    }
    None
}

pub struct RunPaths {
    pub base_sha: Option<String>,
    pub worktree_path: Option<PathBuf>,
    pub worktree_id: Option<String>,
}

pub fn prepare_paths(task: &Task, run_id: &str, ws: &Path) -> Result<RunPaths, String> {
    let id = format!("{}-{}", task.id, &run_id[..12.min(run_id.len())]);
    let dir = git::add_worktree(ws, &id)?;
    Ok(RunPaths {
        base_sha: Some(git::head_sha(ws)?),
        worktree_path: Some(dir),
        worktree_id: Some(id),
    })
}

fn finish(app: &AppHandle, store: &Store, registry: &RunRegistry, run: &mut Run, err: Option<String>) {
    match err {
        None => run.status = "done".into(),
        Some(e) => {
            if e == "__cancelled__" {
                run.status = "cancelled".into();
            } else {
                run.status = "failed".into();
                run.summary = Some(e.clone());
                emit(app, store, &run.task_id, &run.id, "error", Some(e), None);
            }
        }
    }
    run.finished_at = Some(now_ms());
    let _ = store.save_run(run);
    unregister_run(registry, &run.id);
}

/// Ejecución del PLAN: solo lectura, siempre sobre el workspace (sin worktree).
pub fn run_plan(
    app: AppHandle,
    store: &Store,
    registry: &RunRegistry,
    run_id: &str,
    task: &Task,
    agent: &AgentDef,
    _mode: &str,
    ws: &Path,
    resume_session: bool,
    skill_id: &str,
    agents_md: Option<&crate::agents_md::AgentsMd>,
) -> Run {
    let cancel = register_run(registry, run_id);
    // plantilla de la skill elegida (None = plan estándar)
    let skill_template = if agent.kind == "mock" || agent.kind == "ollama" {
        None
    } else {
        match crate::skills::resolve_template(store, skill_id) {
            Ok(t) => t,
            Err(e) => {
                let mut run = Run {
                    id: run_id.to_string(),
                    task_id: task.id.clone(),
                    agent: agent.id.clone(),
                    mode: "plan".into(),
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
                };
                let _ = store.save_run(&run);
                finish(&app, store, registry, &mut run, Some(e));
                return run;
            }
        }
    };
    // --resume: continúa la sesión previa del mismo agente en esta task
    // (solo qwen -r / claude --resume; mock/ollama no tienen sesiones)
    let resume_id: Option<String> = if resume_session
        && (agent.kind == "qwen" || agent.kind == "claude")
    {
        store
            .list_runs(&task.id)
            .ok()
            .and_then(|runs| {
                runs.iter()
                    .find(|r| r.agent == agent.id && r.status == "done" && r.session_id.is_some())
                    .and_then(|r| r.session_id.clone())
            })
    } else {
        None
    };
    let mut run = Run {
        id: run_id.to_string(),
        task_id: task.id.clone(),
        agent: agent.id.clone(),
        mode: "plan".into(),
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
    };
    let _ = store.save_run(&run);

    let result: Result<(), String> = (|| {
        emit(&app, store, &task.id, run_id, "info", Some("Generando spec y plan…".into()), None);
        // presupuesto de pasos + allowlist de comandos (config del workspace)
        let wcfg = store.load_workspace()?;
        let budget = StepBudget::new(wcfg.max_steps);
        let allowlist = wcfg.command_allowlist.clone();
        if let Some(md) = agents_md {
            emit(&app, store, &task.id, run_id, "info", Some(format!("Contexto de proyecto: {}", md.path.display())), None);
        }
        let plan_prompt = |intent: &str| -> String {
            let base = plan_prompt_for_skill(intent, skill_template.clone());
            match agents_md {
                Some(md) => format!("{}{}", base, crate::agents_md::section(md)),
                None => base,
            }
        };
        if let Some(sid) = &resume_id {
            emit(&app, store, &task.id, run_id, "info", Some(format!("↩ continuando sesión previa del agente ({})", sid)), None);
        }
        let (spec, tickets_json) = if agent.kind == "mock" {
            mock_agent::plan_output(&task.intent)
        } else if agent.kind == "ollama" {
            let cfg = store.load_workspace()?;
            let (text, _) = ollama::run_plan(&cfg.ollama_url, &cfg.ollama_model, &plan_prompt(&task.intent), ws, |line| {
                if cancel.cancelled() {
                    return Err("__cancelled__".into());
                }
                emit(&app, store, &task.id, run_id, "chunk", Some(line.clone()), None);
                Ok(())
            })?;
            let tickets =
                extract_json_block(&text).ok_or("el agente no devolvió el bloque JSON de tickets")?;
            (text.clone(), tickets.to_string())
        } else if agent.kind == "codex" {
            let prompt_file = write_prompt_file(&plan_prompt(&task.intent))?;
            let last_msg = std::env::temp_dir().join(format!("nerve-codex-last-{}.txt", run_id));
            let mut args: Vec<String> = agent.plan_args.clone();
            args.push("--output-last-message".into());
            args.push(last_msg.to_string_lossy().to_string());
            let res = spawn_agent_codex(&agent.bin, &args, ws, Some(&prompt_file)).and_then(|mut child| {
                let pid = child.0.id();
                *cancel.pid.lock().unwrap() = Some(pid);
                read_stream_codex(&mut child, &cancel, &last_msg)
            });
            let _ = fs::remove_file(&prompt_file);
            let _ = fs::remove_file(&last_msg);
            let (text, sid) = res?;
            run.session_id = sid;
            let tickets =
                extract_json_block(&text).ok_or("el agente no devolvió el bloque JSON de tickets")?;
            (text.clone(), tickets.to_string())
        } else {
            let prompt_file = write_prompt_file(&plan_prompt(&task.intent))?;
            let mut args: Vec<String> = agent.plan_args.clone();
            // qwen: -r <id> · claude: --resume <id> (flag con valor separado)
            if let Some(sid) = &resume_id {
                args.push("-r".into());
                args.push(sid.clone());
            }
            let res = spawn_agent(&agent.bin, &args, ws, Some(&prompt_file)).and_then(|mut child| {
                let pid = child.0.id();
                *cancel.pid.lock().unwrap() = Some(pid);
                let app3 = app.clone();
                let store3 = store.clone();
                let tid3 = task.id.clone();
                let rid3 = run_id.to_string();
                read_stream(&mut child, &cancel, &budget, &allowlist, move |desc| {
                    emit(&app3, &store3, &tid3, &rid3, "stream", Some(desc), None);
                })
            });
            let _ = fs::remove_file(&prompt_file);
            let (text, sid) = res?;
            run.session_id = sid;
            let tickets = extract_json_block(&text)
                .ok_or("el agente no devolvió el bloque JSON de tickets")?;
            (text.clone(), tickets.to_string())
        };

        run.summary = Some(spec.lines().take(3).collect::<Vec<_>>().join(" "));
        let _ = store.append_event(
            &task.id,
            run_id,
            &RunEvent {
                ts: now_ms(),
                kind: "plan".into(),
                text: Some(tickets_json.clone()),
                ticket_id: None,
            },
        );
        let _ = app.emit(
            "run-plan",
            json!({"taskId": task.id, "runId": run_id, "spec": spec, "tickets": tickets_json}),
        );
        // la task queda esperando aprobación del plan generado
        let mut fresh = store.load_task(&task.id)?;
        fresh.status = "ready".into();
        fresh.updated_at = now_ms();
        store.save_task(&fresh)?;
        let _ = app.emit("task-updated", &fresh);
        Ok(())
    })();

    if let Err(e) = result {
        finish(&app, store, registry, &mut run, Some(e));
    } else {
        finish(&app, store, registry, &mut run, None);
    }
    let _ = cancel;
    run
}

/// Ejecución de tickets aprobados: cada ticket corre aislado y termina en checkpoint.
#[allow(clippy::too_many_arguments)]
pub fn run_exec(
    app: AppHandle,
    store: &Store,
    registry: &RunRegistry,
    run_id: &str,
    task: &Task,
    agent: &AgentDef,
    mode: &str,
    ws: &Path,
    resume_session: bool,
    agents_md: Option<&crate::agents_md::AgentsMd>,
) -> Run {
    let cancel = register_run(registry, run_id);
    // resume solo en modo workspace: cada worktree nuevo es un contexto distinto
    let resume_id: Option<String> = if resume_session
        && mode == "workspace"
        && (agent.kind == "qwen" || agent.kind == "claude")
    {
        store
            .list_runs(&task.id)
            .ok()
            .and_then(|runs| {
                runs.iter()
                    .find(|r| r.agent == agent.id && r.status == "done" && r.session_id.is_some())
                    .and_then(|r| r.session_id.clone())
            })
    } else {
        None
    };
    let mut run = Run {
        id: run_id.to_string(),
        task_id: task.id.clone(),
        agent: agent.id.clone(),
        mode: mode.to_string(),
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
    };
    let _ = store.save_run(&run);

    let result: Result<(), String> = (|| {
        emit(&app, store, &task.id, run_id, "info", Some("Preparando entorno…".into()), None);
        // presupuesto de pasos + allowlist de comandos (config del workspace)
        let wcfg = store.load_workspace()?;
        let budget = StepBudget::new(wcfg.max_steps);
        let allowlist = wcfg.command_allowlist.clone();
        let paths = if mode == "workspace" {
            if !git::is_repo(ws) {
                return Err("el workspace no es un repositorio git; usa modo worktree".into());
            }
            if git::is_dirty(ws) {
                return Err("el workspace tiene cambios sin confirmar; confírmalos o usa un worktree".into());
            }
            RunPaths {
                base_sha: Some(git::head_sha(ws)?),
                worktree_path: None,
                worktree_id: None,
            }
        } else {
            prepare_paths(task, run_id, ws)?
        };
        run.base_sha = paths.base_sha.clone();
        run.worktree_path = paths
            .worktree_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        let cwd: PathBuf = paths
            .worktree_path
            .clone()
            .unwrap_or_else(|| ws.to_path_buf());
        let exec_prompt_for = |tickets: &[ExecTicket]| -> String {
            let base = exec_prompt(&task.title, tickets, None);
            match agents_md {
                Some(md) => format!("{}{}", base, crate::agents_md::section(md)),
                None => base,
            }
        };

        let approved: Vec<_> = task
            .tickets
            .iter()
            .filter(|t| t.approved && t.status != "done")
            .collect();
        if approved.is_empty() {
            return Err("no hay tickets aprobados pendientes".into());
        }
        // la task entra en construcción mientras corre la ejecución
        let mut fresh = store.load_task(&task.id)?;
        fresh.status = "in_dev".into();
        fresh.updated_at = now_ms();
        store.save_task(&fresh)?;
        let _ = app.emit("task-updated", &fresh);

        for t in &approved {
            if cancel.cancelled() {
                return Err("__cancelled__".into());
            }
            emit(&app, store, &task.id, run_id, "ticket-start", Some(t.title.clone()), Some(t.id.clone()));
            let out = if agent.kind == "mock" {
                let dir = cwd.join("nerve-run");
                fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                mock_agent::exec_apply(&dir, &t.title)?
            } else if agent.kind == "ollama" {
                let cfg = store.load_workspace()?;
                let prompt = exec_prompt_for(&[(
                    t.id.clone(),
                    t.title.clone(),
                    t.description.clone(),
                    t.acceptance.clone(),
                    t.verify_command.clone(),
                )]);
                let (text, complete) = ollama::run_exec(&cfg.ollama_url, &cfg.ollama_model, &prompt, &cwd, |line| {
                    if cancel.cancelled() {
                        return Err("__cancelled__".into());
                    }
                    emit(&app, store, &task.id, run_id, "chunk", Some(line.clone()), Some(t.id.clone()));
                    Ok(())
                })?;
                if !complete {
                    emit(
                        &app,
                        store,
                        &task.id,
                        run_id,
                        "warn",
                        Some("el agente no imprimió NERVE_RUN_COMPLETE; se hace checkpoint igualmente".into()),
                        Some(t.id.clone()),
                    );
                }
                text
            } else if agent.kind == "codex" {
                let prompt_file = write_prompt_file(&exec_prompt_for(&[(
                    t.id.clone(),
                    t.title.clone(),
                    t.description.clone(),
                    t.acceptance.clone(),
                    t.verify_command.clone(),
                )]))?;
                let last_msg = std::env::temp_dir().join(format!("nerve-codex-last-{}.txt", run_id));
                let mut args: Vec<String> = agent.exec_args.clone();
                args.push("--output-last-message".into());
                args.push(last_msg.to_string_lossy().to_string());
                let res = spawn_agent_codex(&agent.bin, &args, &cwd, Some(&prompt_file)).and_then(|mut child| {
                    let pid = child.0.id();
                    *cancel.pid.lock().unwrap() = Some(pid);
                    read_stream_codex(&mut child, &cancel, &last_msg)
                });
                let _ = fs::remove_file(&prompt_file);
                let _ = fs::remove_file(&last_msg);
                let (text, sid) = res?;
                if sid.is_some() {
                    run.session_id = sid;
                }
                text
            } else {
                let prompt_file = write_prompt_file(&exec_prompt_for(&[(
                    t.id.clone(),
                    t.title.clone(),
                    t.description.clone(),
                    t.acceptance.clone(),
                    t.verify_command.clone(),
                )]))?;
                let mut args: Vec<String> = agent.exec_args.clone();
                if let Some(sid) = &resume_id {
                    args.push("-r".into());
                    args.push(sid.clone());
                }
                let res = spawn_agent(&agent.bin, &args, &cwd, Some(&prompt_file)).and_then(|mut child| {
                    let pid = child.0.id();
                    *cancel.pid.lock().unwrap() = Some(pid);
                    let app3 = app.clone();
                    let store3 = store.clone();
                    let tid3 = task.id.clone();
                    let rid3 = run_id.to_string();
                    let tid4 = t.id.clone();
                    read_stream(&mut child, &cancel, &budget, &allowlist, move |desc| {
                        emit(&app3, &store3, &tid3, &rid3, "stream", Some(desc), Some(tid4.clone()));
                    })
                });
                let _ = fs::remove_file(&prompt_file);
                let (text, sid) = res?;
                if sid.is_some() {
                    run.session_id = sid;
                }
                text
            };
            emit(&app, store, &task.id, run_id, "chunk", Some(out), Some(t.id.clone()));
            let sha = git::commit_all(&cwd, &format!("nerve({}): {}", t.id, t.title))?;
            run.checkpoint_sha = Some(sha.clone());
            emit(&app, store, &task.id, run_id, "checkpoint", Some(sha), Some(t.id.clone()));
        }

        // Verificación automática: corre el verify_command de cada ticket
        // ejecutado y marca done (exit 0) o blocked (fallo) persistiendo la task.
        let mut verified = 0usize;
        let mut failed = 0usize;
        for t in &approved {
            let Some(cmd) = &t.verify_command else { continue };
            if cmd.trim().is_empty() {
                continue;
            }
            if cancel.cancelled() {
                return Err("__cancelled__".into());
            }
            emit(&app, store, &task.id, run_id, "info", Some(format!("✔ Verificando {}: {}", t.id, cmd)), Some(t.id.clone()));
            let (code, out) = run_verify(&cwd, cmd);
            let ok = code == 0;
            let verdict = if ok { "done" } else { "blocked" };
            if ok { verified += 1 } else { failed += 1 }
            emit(
                &app,
                store,
                &task.id,
                run_id,
                if ok { "verify" } else { "error" },
                Some(format!(
                    "{} verificación {}: exit={} — {}",
                    t.id,
                    if ok { "OK" } else { "FALLÓ" },
                    code,
                    out.trim().chars().take(400).collect::<String>()
                )),
                Some(t.id.clone()),
            );
            let mut fresh = store.load_task(&task.id)?;
            if let Some(slot) = fresh.tickets.iter_mut().find(|x| x.id == t.id) {
                slot.status = verdict.to_string();
            }
            fresh.updated_at = now_ms();
            store.save_task(&fresh)?;
            let _ = app.emit("task-updated", &fresh);
        }

        run.summary = Some(if verified + failed > 0 {
            format!(
                "{} ticket(s) ejecutados; verificación: {} ok, {} fallido(s)",
                approved.len(),
                verified,
                failed
            )
        } else {
            format!("{} ticket(s) ejecutados", approved.len())
        });
        // tickets sin verify_command quedan done al ejecutarse; si todos
        // terminaron, la task se completa (y si algo falló, blocked)
        let mut fresh = store.load_task(&task.id)?;
        if verified + failed == 0 {
            for t in &approved {
                if let Some(slot) = fresh.tickets.iter_mut().find(|x| x.id == t.id) {
                    slot.status = "done".into();
                }
            }
        }
        let all_done = !fresh.tickets.is_empty() && fresh.tickets.iter().all(|t| t.status == "done");
        fresh.status = if fresh.tickets.iter().any(|t| t.status == "blocked") {
            "blocked".into()
        } else if all_done {
            "done".into()
        } else {
            "in_dev".into()
        };
        fresh.updated_at = now_ms();
        store.save_task(&fresh)?;
        let _ = app.emit("task-updated", &fresh);
        Ok(())
    })();

    if let Err(e) = result {
        finish(&app, store, registry, &mut run, Some(e));
    } else {
        finish(&app, store, registry, &mut run, None);
    }
    let _ = cancel;
    run
}

// new_id se usa en write_prompt_file; mantener el import vivo.
#[allow(dead_code)]
fn _uses_new_id() -> String {
    new_id("x")
}

/// Verificación estilo Traycer: un agente en modo solo-lectura compara el diff
/// de la implementación con el plan de la task y produce comentarios
/// categorizados (critical/major/minor/outdated) persistidos en la task.
/// Nunca modifica archivos: corre sobre el workspace o el worktree del run.
pub fn verify_run(
    app: AppHandle,
    store: &Store,
    registry: &RunRegistry,
    run_id: &str,
    task: &Task,
    target: &Run,
    agent: &AgentDef,
    ws: &Path,
) -> Run {
    let cancel = register_run(registry, run_id);
    let mut run = Run {
        id: run_id.to_string(),
        task_id: task.id.clone(),
        agent: agent.id.clone(),
        mode: "verify".into(),
        worktree: None,
        worktree_path: target.worktree_path.clone(),
        base_sha: target.base_sha.clone(),
        checkpoint_sha: None,
        started_at: now_ms(),
        finished_at: None,
        status: "running".into(),
        session_id: None,
        summary: None,
        events: Vec::new(),
    };
    let _ = store.save_run(&run);

    let result: Result<(), String> = (|| {
        // diff de la implementación a revisar
        let cwd: PathBuf = target
            .worktree_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .unwrap_or_else(|| ws.to_path_buf());
        emit(&app, store, &task.id, run_id, "info", Some("Verificando la implementación contra el plan…".into()), None);
        let diff = git::diff(&cwd, target.base_sha.as_deref())?.diff;
        if diff.trim().is_empty() {
            emit(&app, store, &task.id, run_id, "info", Some("Sin cambios que revisar: el run no produjo diff.".into()), None);
            run.summary = Some("sin cambios que revisar".into());
            return Ok(());
        }
        // plan de la task: spec + tickets
        let mut plan = String::new();
        if let Some(spec) = &task.spec_current {
            plan.push_str(spec);
            plan.push('\n');
        }
        for t in &task.tickets {
            plan.push_str(&format!(
                "- {}: {} [estado: {}]\n",
                t.id, t.title, t.status
            ));
        }
        if plan.trim().is_empty() {
            return Err("la task no tiene plan (spec/tickets); genera el plan antes de verificar".into());
        }
        let prompt = format!(
            "Eres el verificador de Nerve. Compara la IMPLEMENTACIÓN (diff de git) con el PLAN y detecta desviaciones, errores y riesgos. No modifiques ningún archivo.\n\nPLAN:\n{plan}\n\nDIFF (implementación):\n```diff\n{diff}\n```\n\nTu respuesta final debe terminar EXACTAMENTE con este bloque JSON en una línea:\n```json\n{{\"comments\":[{{\"severity\":\"critical\",\"file\":\"ruta o null\",\"title\":\"resumen corto\",\"detail\":\"explicación concreta y sugerencia\"}}]}}\n```\n\nSeveridades: critical (rompe el plan o funcionalidad central), major (problema significativo con workaround), minor (puliendo detalles), outdated (comentario sobre algo ya cambiado). Si no hay problemas, devuelve \"comments\":[] . Sé concreto: cada comentario debe señalar algo verificable del diff."
        );
        let comments_json: String = if agent.kind == "mock" {
            // el agente simulado no analiza código: comentarios desde los
            // resultados de verify_command (tickets blocked)
            let blocked: Vec<&crate::model::Ticket> =
                task.tickets.iter().filter(|t| t.status == "blocked").collect();
            let list: Vec<serde_json::Value> = blocked
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    json!({
                        "id": format!("c{}", i + 1),
                        "severity": "critical",
                        "file": serde_json::Value::Null,
                        "title": format!("verificación fallida en {}", t.id),
                        "detail": format!("el verify_command de '{}' no pasó; revisa la implementación del ticket", t.title),
                    })
                })
                .collect();
            json!({ "comments": list }).to_string()
        } else if agent.kind == "ollama" {
            let cfg = store.load_workspace()?;
            let (text, _) = ollama::run_read_prompt(&cfg.ollama_url, &cfg.ollama_model, &prompt, &cwd, |line| {
                if cancel.cancelled() {
                    return Err("__cancelled__".into());
                }
                emit(&app, store, &task.id, run_id, "chunk", Some(line.clone()), None);
                Ok(())
            })?;
            text
        } else {
            let prompt_file = write_prompt_file(&prompt)?;
            let res = if agent.kind == "codex" {
                let last_msg = std::env::temp_dir().join(format!("nerve-codex-last-{}.txt", run_id));
                let mut args: Vec<String> = agent.plan_args.clone();
                args.push("--output-last-message".into());
                args.push(last_msg.to_string_lossy().to_string());
                let r = spawn_agent_codex(&agent.bin, &args, &cwd, Some(&prompt_file)).and_then(|mut child| {
                    let pid = child.0.id();
                    *cancel.pid.lock().unwrap() = Some(pid);
                    read_stream_codex(&mut child, &cancel, &last_msg)
                });
                let _ = fs::remove_file(&last_msg);
                r
            } else {
                let mut args: Vec<String> = agent.plan_args.clone();
                spawn_agent(&agent.bin, &args, &cwd, Some(&prompt_file)).and_then(|mut child| {
                    let pid = child.0.id();
                    *cancel.pid.lock().unwrap() = Some(pid);
                    let app3 = app.clone();
                    let store3 = store.clone();
                    let tid3 = task.id.clone();
                    let rid3 = run_id.to_string();
                    let budget = StepBudget::new(0);
                    read_stream(&mut child, &cancel, &budget, &[], move |desc| {
                        emit(&app3, &store3, &tid3, &rid3, "stream", Some(desc), None);
                    })
                })
            };
            let _ = fs::remove_file(&prompt_file);
            let (text, sid) = res?;
            run.session_id = sid;
            text
        };
        // parsear comentarios del bloque JSON
        let v = extract_json_block(&comments_json).ok_or("el agente no devolvió el bloque JSON de comentarios")?;
        let list = v
            .get("comments")
            .cloned()
            .ok_or("el bloque JSON no tiene \"comments\"")?;
        let parsed: Vec<crate::model::ReviewComment> =
            serde_json::from_value(list).map_err(|e| format!("comentarios inválidos: {}", e))?;
        let mut comments: Vec<crate::model::ReviewComment> = Vec::new();
        for c in parsed {
            let sev = if ["critical", "major", "minor", "outdated"].contains(&c.severity.as_str()) {
                c.severity.clone()
            } else {
                "minor".to_string()
            };
            comments.push(crate::model::ReviewComment {
                id: new_id("rc"),
                severity: sev,
                file: c.file,
                title: c.title,
                detail: c.detail,
                resolved: false,
                created_at: now_ms(),
            });
        }
        let n = comments.len();
        let criticals = comments.iter().filter(|c| c.severity == "critical").count();
        // verificación nueva reemplaza los comentarios anteriores (fresh)
        let mut fresh = store.load_task(&task.id)?;
        fresh.review_comments = comments;
        fresh.updated_at = now_ms();
        store.save_task(&fresh)?;
        let _ = app.emit("task-updated", &fresh);
        let summary = if n == 0 {
            "sin problemas detectados".to_string()
        } else {
            format!("{} comentario(s): {} critical(s)", n, criticals)
        };
        run.summary = Some(summary.clone());
        emit(
            &app,
            store,
            &task.id,
            run_id,
            "review",
            Some(format!("✔ Verificación: {}", summary)),
            None,
        );
        let _ = app.emit("review-comments", json!({"taskId": task.id, "runId": run_id}));
        Ok(())
    })();

    if let Err(e) = result {
        finish(&app, store, registry, &mut run, Some(e));
    } else {
        finish(&app, store, registry, &mut run, None);
    }
    let _ = cancel;
    run
}

/// Run de corrección ("fix comments"): ejecuta el prompt de corrección sobre
/// el workspace o un worktree nuevo, hace checkpoint y re-verifica los
/// verify_command de los tickets para actualizar su estado.
#[allow(clippy::too_many_arguments)]
pub fn run_fix_run(
    app: AppHandle,
    store: &Store,
    registry: &RunRegistry,
    run_id: &str,
    task: &Task,
    agent: &AgentDef,
    mode: &str,
    ws: &Path,
    fix_prompt: &str,
    target_run: Option<&Run>,
    agents_md: Option<&crate::agents_md::AgentsMd>,
) -> Run {
    let cancel = register_run(registry, run_id);
    let mut run = Run {
        id: run_id.to_string(),
        task_id: task.id.clone(),
        agent: agent.id.clone(),
        mode: "fix".into(),
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
    };
    let _ = store.save_run(&run);

    let result: Result<(), String> = (|| {
        emit(&app, store, &task.id, run_id, "info", Some("Corrigiendo problemas de la verificación…".into()), None);
        // si hay run objetivo con worktree, la corrección trabaja ahí: los
        // cambios de la ejecución aún no fusionados viven en ese directorio
        let target_dir: Option<PathBuf> = target_run
            .and_then(|t| t.worktree_path.as_ref())
            .map(PathBuf::from)
            .filter(|p| p.exists());
        let paths = if let Some(dir) = target_dir {
            RunPaths {
                base_sha: target_run.and_then(|t| t.base_sha.clone()),
                worktree_path: Some(dir),
                worktree_id: None,
            }
        } else if mode == "workspace" {
            if !git::is_repo(ws) {
                return Err("el workspace no es un repositorio git; usa modo worktree".into());
            }
            if git::is_dirty(ws) {
                return Err("el workspace tiene cambios sin confirmar; confírmalos o usa un worktree".into());
            }
            RunPaths {
                base_sha: Some(git::head_sha(ws)?),
                worktree_path: None,
                worktree_id: None,
            }
        } else {
            prepare_paths(task, run_id, ws)?
        };
        run.base_sha = paths.base_sha.clone();
        run.worktree_path = paths
            .worktree_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        let cwd: PathBuf = paths
            .worktree_path
            .clone()
            .unwrap_or_else(|| ws.to_path_buf());

        let prompt = {
            let base = format!(
                "Eres un agente de corrección de Nerve. Task: {title}\n\n{fix}\n\nReglas:\n- Trabaja SOLO sobre el directorio actual.\n- No hagas commits ni nada relacionado con git.\n- No introduzcas cambios fuera de lo pedido.\n- Cuando termines, imprime una línea exacta: NERVE_RUN_COMPLETE",
                title = task.title,
                fix = fix_prompt
            );
            match agents_md {
                Some(md) => format!("{}{}", base, crate::agents_md::section(md)),
                None => base,
            }
        };

        let out = if agent.kind == "mock" {
            let dir = cwd.join("nerve-run");
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            mock_agent::exec_apply(&dir, "corrección de comentarios de verificación")?
        } else if agent.kind == "ollama" {
            let cfg = store.load_workspace()?;
            let (text, _complete) = ollama::run_exec(&cfg.ollama_url, &cfg.ollama_model, &prompt, &cwd, |line| {
                if cancel.cancelled() {
                    return Err("__cancelled__".into());
                }
                emit(&app, store, &task.id, run_id, "chunk", Some(line.clone()), None);
                Ok(())
            })?;
            text
        } else {
            let prompt_file = write_prompt_file(&prompt)?;
            let res = if agent.kind == "codex" {
                let last_msg = std::env::temp_dir().join(format!("nerve-codex-last-{}.txt", run_id));
                let mut args: Vec<String> = agent.exec_args.clone();
                args.push("--output-last-message".into());
                args.push(last_msg.to_string_lossy().to_string());
                let r = spawn_agent_codex(&agent.bin, &args, &cwd, Some(&prompt_file)).and_then(|mut child| {
                    let pid = child.0.id();
                    *cancel.pid.lock().unwrap() = Some(pid);
                    read_stream_codex(&mut child, &cancel, &last_msg)
                });
                let _ = fs::remove_file(&last_msg);
                r
            } else {
                let mut args: Vec<String> = agent.exec_args.clone();
                spawn_agent(&agent.bin, &args, &cwd, Some(&prompt_file)).and_then(|mut child| {
                    let pid = child.0.id();
                    *cancel.pid.lock().unwrap() = Some(pid);
                    let app3 = app.clone();
                    let store3 = store.clone();
                    let tid3 = task.id.clone();
                    let rid3 = run_id.to_string();
                    let wcfg = store.load_workspace()?;
                    let budget = StepBudget::new(wcfg.max_steps);
                    read_stream(&mut child, &cancel, &budget, &wcfg.command_allowlist, move |desc| {
                        emit(&app3, &store3, &tid3, &rid3, "stream", Some(desc), None);
                    })
                })
            };
            let _ = fs::remove_file(&prompt_file);
            let (text, sid) = res?;
            run.session_id = sid;
            text
        };
        emit(&app, store, &task.id, run_id, "chunk", Some(out), None);
        let sha = git::commit_all(&cwd, &format!("nerve(fix): {}", task.title))?;
        run.checkpoint_sha = Some(sha.clone());
        emit(&app, store, &task.id, run_id, "checkpoint", Some(sha), None);

        // re-verificación: los verify_command deciden el estado final de cada ticket
        let mut fresh = store.load_task(&task.id)?;
        let mut ok = 0usize;
        let mut failed = 0usize;
        let snapshot: Vec<Ticket> = fresh.tickets.clone();
        for t in &snapshot {
            let Some(cmd) = &t.verify_command else { continue };
            if cmd.trim().is_empty() {
                continue;
            }
            if cancel.cancelled() {
                return Err("__cancelled__".into());
            }
            emit(&app, store, &task.id, run_id, "info", Some(format!("✔ Re-verificando {}: {}", t.id, cmd)), Some(t.id.clone()));
            let (code, _out2) = run_verify(&cwd, cmd);
            let verdict = if code == 0 { "done" } else { "blocked" };
            if code == 0 { ok += 1 } else { failed += 1 }
            if let Some(slot) = fresh.tickets.iter_mut().find(|x| x.id == t.id) {
                slot.status = verdict.to_string();
            }
            emit(
                &app,
                store,
                &task.id,
                run_id,
                if code == 0 { "verify" } else { "error" },
                Some(format!("{} re-verificación: exit={}", t.id, code)),
                Some(t.id.clone()),
            );
        }
        let any_blocked = fresh.tickets.iter().any(|t| t.status == "blocked");
        let all_done = !fresh.tickets.is_empty() && fresh.tickets.iter().all(|t| t.status == "done");
        fresh.status = if any_blocked {
            "blocked".into()
        } else if all_done {
            "done".into()
        } else {
            "in_dev".into()
        };
        fresh.updated_at = now_ms();
        store.save_task(&fresh)?;
        let _ = app.emit("task-updated", &fresh);
        run.summary = Some(if ok + failed > 0 {
            format!("corrección aplicada; re-verificación: {} ok, {} fallido(s)", ok, failed)
        } else {
            "corrección aplicada".into()
        });
        Ok(())
    })();

    if let Err(e) = result {
        finish(&app, store, registry, &mut run, Some(e));
    } else {
        finish(&app, store, registry, &mut run, None);
    }
    let _ = cancel;
    run
}