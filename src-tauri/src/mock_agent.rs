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

/// Documento de planificación simulado (brief / architecture / flows):
/// mismo contrato que el adaptador de agentes reales.
pub fn doc_output(kind: &str, task: &crate::model::Task, context: &str) -> String {
    let _ = context;
    let intent = task.intent.trim();
    match kind {
        "brief" => format!(
            "# Brief (demo)\n\n**Intención:** {intent}\n\n## Objetivo\n- Entregar lo pedido de forma mínima y verificable\n\n## Alcance\n- En alcance: cambio único sobre el workspace\n- Fuera de alcance: integraciones, datos reales\n\n## Restricciones\n- Sin dependencias nuevas\n- Todo se valida con el diff de la ejecución\n\n> Generado por el agente simulado de Nerve.\n"
        ),
        "architecture" => format!(
            "# Arquitectura (demo)\n\n## Estructura\n- Cambio autocontenido en la raíz del workspace\n\n## Tecnologías\n- Las ya presentes en el repositorio (sin dependencias nuevas)\n\n## Datos\n- Estáticos, embebidos en el propio entregable\n\n> Generado por el agente simulado de Nerve.\n"
        ),
        _ => format!(
            "# Flujos (demo)\n\n## Flujo principal\n1. El usuario abre el entregable\n2. Recorre el contenido generado\n3. Confirma que corresponde a la intención: {intent}\n\n## Casos límite\n- Sin conexión: el entregable sigue funcionando (contenido estático)\n\n> Generado por el agente simulado de Nerve.\n"
        ),
    }
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