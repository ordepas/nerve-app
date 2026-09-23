use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::model::OllamaModelInfo;
use crate::store::now_ms;

const DEFAULT_URL: &str = "http://localhost:11434";

fn base_url(url: &str) -> String {
    let u = if url.trim().is_empty() { DEFAULT_URL } else { url.trim() };
    u.trim_end_matches('/').to_string()
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|e| format!("cliente HTTP: {}", e))
}

pub fn list_models(url: &str) -> Result<Vec<OllamaModelInfo>, String> {
    let v: Value = client()?
        .get(format!("{}/api/tags", base_url(url)))
        .send()
        .map_err(|e| format!("no se pudo conectar con Ollama en {}: {}", base_url(url), e))?
        .json()
        .map_err(|e| format!("respuesta inválida de Ollama: {}", e))?;
    let mut out = Vec::new();
    for m in v.get("models").and_then(Value::as_array).cloned().unwrap_or_default() {
        let name = m.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let caps: Vec<String> = m
            .pointer("/details/capabilities")
            .or_else(|| m.get("capabilities"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        out.push(OllamaModelInfo {
            name,
            supports_tools: caps.iter().any(|c| c == "tools"),
        });
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ChatMsg {
    role: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<Value>,
}

#[derive(Deserialize)]
struct ChatResp {
    message: ChatMsg,
    #[serde(default)]
    done: bool,
}

/// System prompt común (plan y ejecución).
fn nerve_system() -> String {
    "Eres el agente de Nerve, un workspace spec-first que implementa tickets sobre un repositorio real. \
Eres preciso, escribes código completo y funcional, y no haces nada fuera de lo pedido."
        .into()
}

fn chat(url: &str, model: &str, messages: &[Value], tools: Option<&Value>) -> Result<ChatResp, String> {
    let mut body = json!({ "model": model, "messages": messages, "stream": false });
    if let Some(t) = tools {
        body["tools"] = t.clone();
    }
    let resp = client()?
        .post(format!("{}/api/chat", base_url(url)))
        .json(&body)
        .send()
        .map_err(|e| format!("error llamando a Ollama /api/chat: {}", e))?;
    let status = resp.status();
    let text = resp.text().map_err(|e| format!("leyendo respuesta: {}", e))?;
    if !status.is_success() {
        return Err(format!("Ollama {} : {}", status, text.chars().take(400).collect::<String>()));
    }
    serde_json::from_str(&text).map_err(|e| format!("respuesta de chat inválida: {} — {}", e, text.chars().take(200).collect::<String>()))
}

/// Plan: pide spec + tickets (JSON) con una pasada de herramientas de lectura.
/// El callback puede devolver Err para abortar (p. ej. cancelación).
pub fn run_plan(
    url: &str,
    model: &str,
    intent: &str,
    cwd: &Path,
    mut on_event: impl FnMut(String) -> Result<(), String>,
) -> Result<(String, String), String> {
    if model.trim().is_empty() {
        return Err("no hay modelo de Ollama seleccionado (Ajustes)".into());
    }
    let tools = json!([
        {"type":"function","function":{
            "name":"list_files","description":"Lista archivos y carpetas de una ruta relativa del repositorio.",
            "parameters":{"type":"object","properties":{"path":{"type":"string","description":"ruta relativa, '.' para la raíz"}},"required":["path"]}}},
        {"type":"function","function":{
            "name":"read_file","description":"Lee un archivo de texto del repositorio (máx 400 líneas).",
            "parameters":{"type":"object","properties":{"path":{"type":"string","description":"ruta relativa del archivo"}},"required":["path"]}}},
        {"type":"function","function":{
            "name":"grep","description":"Busca un texto literal en archivos del repositorio y devuelve archivo:línea:contenido.",
            "parameters":{"type":"object","properties":{"query":{"type":"string"},"glob":{"type":"string","description":"opcional, ej. *.rs"}},"required":["query"]}}},
    ]);

    let mut messages = vec![
        json!({"role":"system","content": nerve_system()}),
        json!({"role":"user","content": crate::runner::plan_prompt(intent)}),
    ];

    for _round in 0..12 {
        let resp = chat(url, model, &messages, Some(&tools))?;
        let calls = resp.message.tool_calls.clone();
        if calls.is_empty() {
            let text = resp.message.content.trim().to_string();
            return Ok((text.clone(), text));
        }
        messages.push(json!({"role":"assistant","content": resp.message.content, "tool_calls": calls}));
        for call in &calls {
            let name = call.pointer("/function/name").and_then(Value::as_str).unwrap_or("");
            let raw = call.pointer("/function/arguments").cloned().unwrap_or(json!({}));
            let out = dispatch_read_tool(cwd, name, &raw);
            on_event(format!("{}({}) → {}", name, brief_args(&raw), out.chars().take(120).collect::<String>()))?;
            let tcid = call.get("id").and_then(Value::as_str).unwrap_or("");
            messages.push(json!({"role":"tool","tool_call_id": tcid, "content": out}));
        }
    }
    Err("el agente Ollama excedió el máximo de rondas de herramientas".into())
}

/// Ejecución: bucle agente con herramientas de lectura Y escritura; devuelve
/// texto final y true si imprimió NERVE_RUN_COMPLETE. El callback puede
/// devolver Err para abortar (p. ej. cancelación).
pub fn run_exec(
    url: &str,
    model: &str,
    prompt: &str,
    cwd: &Path,
    allowlist: &[String],
    mut on_event: impl FnMut(String) -> Result<(), String>,
) -> Result<(String, bool), String> {
    if model.trim().is_empty() {
        return Err("no hay modelo de Ollama seleccionado (Ajustes)".into());
    }
    let tools = json!([
        {"type":"function","function":{
            "name":"list_files","description":"Lista archivos y carpetas de una ruta relativa del directorio de trabajo.",
            "parameters":{"type":"object","properties":{"path":{"type":"string","description":"ruta relativa, '.' para la raíz"}},"required":["path"]}}},
        {"type":"function","function":{
            "name":"read_file","description":"Lee un archivo de texto (máx 400 líneas).",
            "parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},
        {"type":"function","function":{
            "name":"grep","description":"Busca un texto literal en archivos y devuelve archivo:línea:contenido.",
            "parameters":{"type":"object","properties":{"query":{"type":"string"},"glob":{"type":"string","description":"opcional, ej. *.rs"}},"required":["query"]}}},
        {"type":"function","function":{
            "name":"write_file","description":"Crea o sobrescribe un archivo con el contenido completo dado.",
            "parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}}},
        {"type":"function","function":{
            "name":"edit_file","description":"Reemplaza la primera ocurrencia exacta de un texto por otra en un archivo existente.",
            "parameters":{"type":"object","properties":{"path":{"type":"string"},"find":{"type":"string"},"replace":{"type":"string"}},"required":["path","find","replace"]}}},
        {"type":"function","function":{
            "name":"delete_file","description":"Borra un archivo existente (no carpetas).",
            "parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},
        {"type":"function","function":{
            "name":"run_command","description":"Ejecuta un comando de verificación (p. ej. tests) con timeout de 120s y devuelve stdout+stderr.",
            "parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}},
    ]);

    let mut messages = vec![
        json!({"role":"system","content": nerve_system()}),
        json!({"role":"user","content": prompt}),
    ];

    for _round in 0..30 {
        let resp = chat(url, model, &messages, Some(&tools))?;
        let calls = resp.message.tool_calls.clone();
        if calls.is_empty() {
            let text = resp.message.content.trim().to_string();
            let complete = text.contains("NERVE_RUN_COMPLETE");
            return Ok((text.clone(), complete));
        }
        messages.push(json!({"role":"assistant","content": resp.message.content, "tool_calls": calls}));
        for call in &calls {
            let name = call.pointer("/function/name").and_then(Value::as_str).unwrap_or("");
            let raw = call.pointer("/function/arguments").cloned().unwrap_or(json!({}));
            let out = dispatch_exec_tool(cwd, name, &raw, allowlist);
            on_event(format!("{}({}) → {}", name, brief_args(&raw), out.chars().take(120).collect::<String>()))?;
            let tcid = call.get("id").and_then(Value::as_str).unwrap_or("");
            messages.push(json!({"role":"tool","tool_call_id": tcid, "content": out}));
        }
    }
    Err("el agente Ollama excedió el máximo de rondas de herramientas".into())
}

/// Prompt de solo lectura (p. ej. verificación) con herramientas de lectura;
/// devuelve el texto final. El callback puede devolver Err para abortar.
pub fn run_read_prompt(
    url: &str,
    model: &str,
    prompt: &str,
    cwd: &Path,
    mut on_event: impl FnMut(String) -> Result<(), String>,
) -> Result<(String, bool), String> {
    if model.trim().is_empty() {
        return Err("no hay modelo de Ollama seleccionado (Ajustes)".into());
    }
    let tools = json!([
        {"type":"function","function":{
            "name":"list_files","description":"Lista archivos y carpetas de una ruta relativa del directorio de trabajo.",
            "parameters":{"type":"object","properties":{"path":{"type":"string","description":"ruta relativa, '.' para la raíz"}},"required":["path"]}}},
        {"type":"function","function":{
            "name":"read_file","description":"Lee un archivo de texto (máx 400 líneas).",
            "parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}},
        {"type":"function","function":{
            "name":"grep","description":"Busca un texto literal en archivos y devuelve archivo:línea:contenido.",
            "parameters":{"type":"object","properties":{"query":{"type":"string"},"glob":{"type":"string","description":"opcional, ej. *.rs"}},"required":["query"]}}},
    ]);
    let mut messages = vec![
        json!({"role":"system","content": nerve_system()}),
        json!({"role":"user","content": prompt}),
    ];
    for _round in 0..12 {
        let resp = chat(url, model, &messages, Some(&tools))?;
        let calls = resp.message.tool_calls.clone();
        if calls.is_empty() {
            let text = resp.message.content.trim().to_string();
            return Ok((text.clone(), true));
        }
        messages.push(json!({"role":"assistant","content": resp.message.content, "tool_calls": calls}));
        for call in &calls {
            let name = call.pointer("/function/name").and_then(Value::as_str).unwrap_or("");
            let raw = call.pointer("/function/arguments").cloned().unwrap_or(json!({}));
            let out = dispatch_read_tool(cwd, name, &raw);
            on_event(format!("{}({}) → {}", name, brief_args(&raw), out.chars().take(120).collect::<String>()))?;
            let tcid = call.get("id").and_then(Value::as_str).unwrap_or("");
            messages.push(json!({"role":"tool","tool_call_id": tcid, "content": out}));
        }
    }
    Err("el agente Ollama excedió el máximo de rondas de herramientas".into())
}

fn brief_args(v: &Value) -> String {
    let keys = ["path", "query", "command", "glob", "find"];
    for k in keys {
        if let Some(s) = v.get(k).and_then(Value::as_str) {
            return s.chars().take(60).collect();
        }
    }
    "{}".into()
}

fn dispatch_read_tool(cwd: &Path, name: &str, args: &Value) -> String {
    match name {
        "list_files" => tool_list_files(cwd, args),
        "read_file" => tool_read_file(cwd, args),
        "grep" => tool_grep(cwd, args),
        _ => format!("herramienta desconocida: {}", name),
    }
}

fn dispatch_exec_tool(cwd: &Path, name: &str, args: &Value, allowlist: &[String]) -> String {
    match name {
        "write_file" | "edit_file" | "delete_file" | "run_command" => {
            match check_path(cwd, args.get("path").and_then(Value::as_str).unwrap_or("")) {
                Err(e) => return format!("ERROR: {}", e),
                Ok(()) => {}
            }
            match name {
                "write_file" => tool_write_file(cwd, args),
                "edit_file" => tool_edit_file(cwd, args),
                "delete_file" => tool_delete_file(cwd, args),
                "run_command" => tool_run_command(cwd, args, allowlist),
                _ => unreachable!(),
            }
        }
        _ => dispatch_read_tool(cwd, name, args),
    }
}

/// Rechaza rutas que se salgan del directorio de trabajo.
fn check_path(_cwd: &Path, rel: &str) -> Result<(), String> {
    let p = PathBuf::from(rel);
    if p.is_absolute() || p.components().any(|c| c == std::path::Component::ParentDir) {
        return Err("ruta fuera del directorio de trabajo no permitida".into());
    }
    Ok(())
}

fn resolve(cwd: &Path, rel: &str) -> PathBuf {
    cwd.join(rel)
}

fn tool_list_files(cwd: &Path, args: &Value) -> String {
    let rel = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let dir = resolve(cwd, rel);
    let mut out = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name == ".git" || name == "node_modules" {
                    continue;
                }
                let tag = if e.path().is_dir() { "/" } else { "" };
                out.push(format!("{}{}", name, tag));
            }
        }
        Err(e) => return format!("ERROR: {} ({})", rel, e),
    }
    if out.is_empty() {
        format!("{} (vacío)", rel)
    } else {
        out.join("\n")
    }
}

fn tool_read_file(cwd: &Path, args: &Value) -> String {
    let rel = args.get("path").and_then(Value::as_str).unwrap_or("");
    if let Err(e) = check_path(cwd, rel) {
        return format!("ERROR: {}", e);
    }
    let p = resolve(cwd, rel);
    match std::fs::read_to_string(&p) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().take(400).collect();
            if content.lines().count() > 400 {
                format!("{}\n…(truncado a 400 líneas)", lines.join("\n"))
            } else {
                lines.join("\n")
            }
        }
        Err(e) => format!("ERROR: {} ({})", rel, e),
    }
}

fn tool_grep(cwd: &Path, args: &Value) -> String {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let glob = args.get("glob").and_then(Value::as_str).unwrap_or("*");
    if query.is_empty() {
        return "ERROR: query vacía".into();
    }
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    walk(cwd, glob, &mut |p| {
        if hits.len() >= 80 {
            return;
        }
        if let Ok(content) = std::fs::read_to_string(p) {
            for (i, line) in content.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    hits.push(format!("{}:{}: {}", p.display(), i + 1, line.trim().chars().take(160).collect::<String>()));
                    if hits.len() >= 80 {
                        break;
                    }
                }
            }
        }
    });
    if hits.is_empty() {
        format!("sin resultados para \"{}\"", query)
    } else {
        hits.join("\n")
    }
}

fn walk(dir: &Path, glob: &str, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" || name == "node_modules" || name == ".nerve-worktrees" || name == "target" {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            walk(&p, glob, f);
        } else if glob == "*" || name.ends_with(glob.trim_start_matches('*')) {
            f(&p);
        }
    }
}

fn tool_write_file(cwd: &Path, args: &Value) -> String {
    let rel = args.get("path").and_then(Value::as_str).unwrap_or("");
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");
    if rel.is_empty() {
        return "ERROR: falta path".into();
    }
    let p = resolve(cwd, rel);
    if let Some(parent) = p.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!("ERROR creando carpetas: {}", e);
        }
    }
    match std::fs::write(&p, content) {
        Ok(()) => format!("escrito {} ({} bytes)", rel, content.len()),
        Err(e) => format!("ERROR: {} ({})", rel, e),
    }
}

fn tool_edit_file(cwd: &Path, args: &Value) -> String {
    let rel = args.get("path").and_then(Value::as_str).unwrap_or("");
    let find = args.get("find").and_then(Value::as_str).unwrap_or("");
    let replace = args.get("replace").and_then(Value::as_str).unwrap_or("");
    if rel.is_empty() || find.is_empty() {
        return "ERROR: faltan path/find".into();
    }
    let p = resolve(cwd, rel);
    match std::fs::read_to_string(&p) {
        Ok(content) => {
            if !content.contains(find) {
                return format!("ERROR: el texto a buscar no está en {}", rel);
            }
            match std::fs::write(&p, content.replacen(find, replace, 1)) {
                Ok(()) => format!("editado {}", rel),
                Err(e) => format!("ERROR escribiendo {}: {}", rel, e),
            }
        }
        Err(e) => format!("ERROR: {} ({})", rel, e),
    }
}

fn tool_delete_file(cwd: &Path, args: &Value) -> String {
    let rel = args.get("path").and_then(Value::as_str).unwrap_or("");
    let p = resolve(cwd, rel);
    match std::fs::remove_file(&p) {
        Ok(()) => format!("borrado {}", rel),
        Err(e) => format!("ERROR: {} ({})", rel, e),
    }
}

#[cfg(windows)]
fn run_command_raw(cwd: &Path, command: &str) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let bat = std::env::temp_dir().join(format!(
        "nerve-cmd-{}-{}.bat",
        now_ms(),
        std::process::id()
    ));
    if fs::write(&bat, format!("@echo off\r\n{}\r\n", command)).is_err() {
        return std::process::Command::new("cmd")
            .args(["/d", "/s", "/c", command])
            .current_dir(cwd)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("no se pudo lanzar el comando: {}", e));
    }
    let out = std::process::Command::new("cmd")
        .arg("/d")
        .arg("/c")
        .arg(bat.to_string_lossy().to_string())
        .current_dir(cwd)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("no se pudo lanzar el comando: {}", e))?;
    let _ = fs::remove_file(&bat);
    Ok(out)
}

#[cfg(not(windows))]
fn run_command_raw(cwd: &Path, command: &str) -> Result<std::process::Output, String> {
    std::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("no se pudo lanzar el comando: {}", e))
}

fn tool_run_command(cwd: &Path, args: &Value, allowlist: &[String]) -> String {
    let command = args.get("command").and_then(Value::as_str).unwrap_or("");
    if command.trim().is_empty() {
        return "ERROR: comando vacío".into();
    }
    if !crate::runner::command_allowed(command, allowlist) {
        return "ERROR: comando bloqueado — no está en la allowlist del workspace".into();
    }
    match run_command_raw(cwd, command) {
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            format!(
                "exit={} \n{}{}",
                code,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        }
        Err(e) => format!("ERROR: {}", e),
    }
}

// mantener `Write` en uso (write! sobre archivos temporales futuros)
#[allow(dead_code)]
fn _uses_write() {
    let _ = std::io::sink().write_all(b"x");
}