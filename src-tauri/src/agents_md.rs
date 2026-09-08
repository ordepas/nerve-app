use std::fs;
use std::path::{Path, PathBuf};

use crate::store::Store;

/// Contexto de proyecto estilo Traycer: detecta AGENTS.md desde el directorio
/// de trabajo hacia la raíz y lo inyecta en los prompts de plan y ejecución.
/// Se puede desactivar en Ajustes (workspace.agents_md_enabled).
const FILENAME: &str = "AGENTS.md";
const MAX_CHARS: usize = 8000;

pub struct AgentsMd {
    pub path: PathBuf,
    pub content: String,
}

/// Busca el AGENTS.md más cercano subiendo hasta la raíz del volumen.
pub fn find(cwd: &Path) -> Option<PathBuf> {
    let mut dir = Some(cwd.to_path_buf());
    while let Some(d) = dir {
        let p = d.join(FILENAME);
        if p.is_file() {
            return Some(p);
        }
        dir = d.parent().map(|x| x.to_path_buf());
    }
    None
}

/// Devuelve el contexto activo (None = desactivado o sin archivo).
pub fn load(ws: &Path, store: &Store) -> Option<AgentsMd> {
    let cfg = store.load_workspace().ok()?;
    if !cfg.agents_md_enabled {
        return None;
    }
    let path = find(ws)?;
    let content = fs::read_to_string(&path).ok()?;
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    let content = if trimmed.chars().count() > MAX_CHARS {
        format!(
            "{}\n\n…(AGENTS.md truncado a {} caracteres)",
            trimmed.chars().take(MAX_CHARS).collect::<String>(),
            MAX_CHARS
        )
    } else {
        trimmed
    };
    Some(AgentsMd { path, content })
}

/// Sección lista para incrustar en un prompt.
pub fn section(a: &AgentsMd) -> String {
    format!(
        "\n\nContexto del proyecto ({} — instrucciones del equipo; respétalas en el plan y el código):\n---\n{}\n---\n",
        a.path.display(),
        a.content
    )
}