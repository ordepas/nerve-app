use std::fs;

use serde::{Deserialize, Serialize};

use crate::store::Store;

/// Skill = plantilla de prompt para el planificador. `{intent}` se sustituye
/// por la intención de la task. Los integrados viven en código; los del
/// usuario en `store/skills/*.json` (importables desde SKILL.md externos).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SkillDef {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub template: String,
}

pub fn builtin_skills() -> Vec<SkillDef> {
    vec![
        SkillDef {
            id: "plan".into(),
            label: "Plan estándar".into(),
            description: "Especificación + tickets de implementación (por defecto).".into(),
            template: String::new(), // usa el prompt base de runner::plan_prompt
        },
        SkillDef {
            id: "review".into(),
            label: "Revisión de código".into(),
            description: "Analiza el repo y propone tickets de mejora/corrección de calidad.".into(),
            template: "Eres el planificador de Nerve en MODO REVISIÓN. Inspecciona el repositorio (solo lectura) y detecta problemas de calidad: bugs latentes, deuda, seguridad, rendimiento y claridad.\n\nProduce:\n1) Un informe breve en Markdown con los hallazgos más importantes (máx. 8, priorizados).\n2) Tickets accionables para corregir cada hallazgo (uno por hallazgo, pequeños y verificables).\n\n{intent}\n\nTu respuesta final debe terminar EXACTAMENTE con este bloque JSON en una línea:\n```json\n{{\"tickets\":[{{\"id\":\"T1\",\"title\":\"...\",\"description\":\"...\",\"acceptance\":[\"...\"],\"verify_command\":\"comando opcional o null\",\"depends_on\":[]}}]}}\n```".into(),
        },
        SkillDef {
            id: "debug".into(),
            label: "Diagnóstico de bug".into(),
            description: "Diagnostica el problema descrito y produce tickets de corrección.".into(),
            template: "Eres el planificador de Nerve en MODO DIAGNÓSTICO. Inspecciona el repositorio (solo lectura) para encontrar la causa raíz del problema descrito.\n\nProduce:\n1) Un diagnóstico en Markdown: qué pasa, por qué (con archivos/líneas), y cómo confirmarlo.\n2) Tickets de corrección mínimos y verificables, en orden.\n\nProblema reportado: {intent}\n\nTu respuesta final debe terminar EXACTAMENTE con este bloque JSON en una línea:\n```json\n{{\"tickets\":[{{\"id\":\"T1\",\"title\":\"...\",\"description\":\"...\",\"acceptance\":[\"...\"],\"verify_command\":\"comando opcional o null\",\"depends_on\":[]}}]}}\n```".into(),
        },
        SkillDef {
            id: "tickets".into(),
            label: "Descomposición en tickets".into(),
            description: "Descompone la intención en tickets finos, sin informes largos.".into(),
            template: "Eres el planificador de Nerve en MODO DESCOMPOSICIÓN. Convierte la intención del usuario en tickets de implementación muy finos (cada uno ≈ un cambio atómico y verificable, máximo 30 min de trabajo humano equivalente). Inspecciona el repositorio si lo necesitas (solo lectura). Sin especificación larga: ve directo a los tickets.\n\nIntención: {intent}\n\nTu respuesta final debe terminar EXACTAMENTE con este bloque JSON en una línea:\n```json\n{{\"tickets\":[{{\"id\":\"T1\",\"title\":\"...\",\"description\":\"...\",\"acceptance\":[\"...\"],\"verify_command\":\"comando opcional o null\",\"depends_on\":[]}}]}}\n```".into(),
        },
    ]
}

fn skills_dir(store: &Store) -> std::path::PathBuf {
    store.root.join("skills")
}

/// Skills integradas + las guardadas por el usuario (ids con prefijo user:).
pub fn list_skills(store: &Store) -> Result<Vec<SkillDef>, String> {
    let mut out = builtin_skills();
    let dir = skills_dir(store);
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }
            if let Ok(data) = fs::read_to_string(&p) {
                if let Ok(mut s) = serde_json::from_str::<SkillDef>(&data) {
                    if !s.id.starts_with("user:") {
                        s.id = format!("user:{}", s.id);
                    }
                    out.push(s);
                }
            }
        }
    }
    Ok(out)
}

/// Guarda (o actualiza) una skill del usuario. Devuelve el id final.
pub fn save_skill(store: &Store, mut skill: SkillDef) -> Result<String, String> {
    if skill.id.trim().is_empty() {
        skill.id = slug(&skill.label);
    }
    if skill.id.starts_with("user:") {
        skill.id = skill.id.trim_start_matches("user:").to_string();
    }
    if skill.id.trim().is_empty() {
        return Err("la skill necesita un nombre".into());
    }
    let dir = skills_dir(store);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join(format!("{}.json", skill.id));
    fs::write(&p, serde_json::to_string_pretty(&skill).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(format!("user:{}", skill.id))
}

pub fn delete_skill(store: &Store, id: &str) -> Result<(), String> {
    let bare = id.trim_start_matches("user:");
    let p = skills_dir(store).join(format!("{}.json", bare));
    if p.exists() {
        fs::remove_file(&p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Importa un SKILL.md externo (frontmatter YAML con name/description + cuerpo).
/// El cuerpo se convierte en la plantilla; se añade el bloque JSON final si
/// el cuerpo no lo trae.
pub fn import_skill(store: &Store, path: &str) -> Result<String, String> {
    let data = fs::read_to_string(path).map_err(|e| format!("no se pudo leer {}: {}", path, e))?;
    let mut name = String::new();
    let mut desc = String::new();
    let mut body = String::new();
    let trimmed = data.trim_start_matches('\u{feff}');
    if let Some(rest) = trimmed.strip_prefix("---") {
        // frontmatter YAML simple: name:, description:
        if let Some(end) = rest.find("---") {
            let fm = &rest[..end];
            body = rest[end + 3..].trim().to_string();
            for line in fm.lines() {
                if let Some(v) = line.trim().strip_prefix("name:") {
                    name = v.trim().trim_matches('"').to_string();
                } else if let Some(v) = line.trim().strip_prefix("description:") {
                    desc = v.trim().trim_matches('"').to_string();
                }
            }
        } else {
            body = trimmed.to_string();
        }
    } else {
        body = trimmed.to_string();
    }
    if name.is_empty() {
        // primer heading del cuerpo como nombre
        name = body
            .lines()
            .find(|l| l.trim_start().starts_with('#'))
            .map(|l| l.trim_start_matches('#').trim().to_string())
            .unwrap_or_else(|| "skill importada".into());
    }
    if !body.contains("\"tickets\"") {
        body.push_str("\n\nTu respuesta final debe terminar EXACTAMENTE con este bloque JSON en una línea:\n```json\n{\"tickets\":[{\"id\":\"T1\",\"title\":\"...\",\"description\":\"...\",\"acceptance\":[\"...\"],\"verify_command\":\"comando opcional o null\",\"depends_on\":[]}]}\n```");
    }
    save_skill(
        store,
        SkillDef {
            id: String::new(),
            label: name,
            description: desc,
            template: body,
        },
    )
}

fn slug(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        format!("user-{}", crate::store::now_ms())
    } else {
        out
    }
}

/// Resuelve una skill por id; Some(String::new()) = plan estándar.
pub fn resolve_template(store: &Store, skill_id: &str) -> Result<Option<String>, String> {
    if skill_id.is_empty() || skill_id == "plan" {
        return Ok(None);
    }
    for s in list_skills(store)? {
        if s.id == skill_id {
            return Ok(Some(s.template));
        }
    }
    Err(format!("skill {} no encontrada", skill_id))
}