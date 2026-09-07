use std::fs;
use std::path::Path;

use serde_json::json;

use crate::store::now_ms;

/// Plan simulado: spec + tickets JSON (mismo contrato que el adaptador Qwen).
pub fn plan_output(intent: &str) -> (String, String) {
    let title: String = intent.trim().chars().take(60).collect();
    let spec = format!(
        "# Especificación (demo)\n\n**Intención:** {intent}\n\n## Alcance\n- Cambio mínimo y aislado sobre la carpeta del workspace\n- Sin dependencias nuevas ni cambios de configuración\n\n## Verificación\n- El cambio queda visible en el diff de la ejecución\n- El resto del repositorio permanece intacto\n\n> Generado por el agente simulado de Nerve (no usa ningún modelo).\n"
    );
    let tickets = json!({
        "tickets": [
            {
                "id": "T1",
                "title": format!("Implementar: {}", title),
                "description": "Cambio de ejemplo generado por el agente simulado: crea/actualiza nerve-demo.md en la raíz.",
                "acceptance": [
                    "El cambio aparece en el diff de la ejecución",
                    "No se modifica ningún otro archivo"
                ],
                "verify_command": serde_json::Value::Null,
                "depends_on": []
            }
        ]
    });
    (spec, tickets.to_string())
}

/// Ejecución simulada: escribe nerve-demo.md en el run dir y devuelve el resumen.
pub fn exec_apply(run_dir: &Path, ticket_title: &str) -> Result<String, String> {
    let path = run_dir.join("nerve-demo.md");
    let body = format!(
        "# {}\n\nEjecución simulada por Nerve (agente mock) a las {}.\n",
        ticket_title,
        now_ms()
    );
    fs::write(&path, body).map_err(|e| format!("no se pudo escribir nerve-demo.md: {}", e))?;
    Ok(format!("Escrito nerve-demo.md para el ticket \"{}\"", ticket_title))
}