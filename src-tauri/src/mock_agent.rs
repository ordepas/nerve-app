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
        "flows" => format!(
            "# Flujos (demo)\n\n## Flujo principal\n1. El usuario abre el entregable\n2. Recorre el contenido generado\n3. Confirma que corresponde a la intención: {intent}\n\n## Casos límite\n- Sin conexión: el entregable sigue funcionando (contenido estático)\n\n> Generado por el agente simulado de Nerve.\n"
        ),
        "spec" => format!(
            "# Especificación (demo)\n\n**Intención:** {intent}\n\n## Alcance\n- Entregar lo pedido de forma mínima y verificable\n\n## Decisiones de diseño\n- Un único entregable, sin dependencias nuevas\n\n## Criterios de aceptación\n- El resultado corresponde a la intención: {intent}\n- El cambio queda visible en el diff de la ejecución\n\n> Generado por el agente simulado de Nerve.\n"
        ),
        _ => format!(
            "# Documento (demo)\n\nSección no implementada para kind solicitado.\n\n> Generado por el agente simulado de Nerve.\n"
        ),
    }
}

/// Preguntas simuladas para un documento (mismo contrato que el adaptador real).
pub fn questions_output(kind: &str) -> String {
    let qs: Vec<(&str, Option<&str>)> = match kind {
        "brief" => vec![
            ("¿Quién es el público principal y qué acción clave debe hacer en el sitio?", Some("visitantes móviles; comprar un celular desde la landing")),
            ("¿Qué restricciones de marca o contenido debo respetar?", None),
        ],
        "architecture" => vec![
            ("¿Prefieres un único archivo autocontenido o estructura multi-página?", Some("un único archivo autocontenido")),
            ("¿Hay datos externos que consumir o todo es estático?", Some("todo estático")),
        ],
        "flows" => vec![
            ("¿Qué flujo es el más crítico y debe funcionar sin fallos?", Some("hero → catálogo → compra")),
            ("¿Qué comportamiento esperas en móvil para el menú de navegación?", None),
        ],
        _ => vec![
            ("¿Qué criterios de aceptación son imprescindibles para dar la tarea por terminada?", Some("el sitio abre sin errores y se ve bien en móvil y escritorio")),
            ("¿Hay algo explícitamente fuera de alcance?", None),
        ],
    };
    let list: Vec<serde_json::Value> = qs
        .iter()
        .enumerate()
        .map(|(i, (text, sug))| {
            json!({
                "id": format!("q{}", i + 1),
                "text": text,
                "suggestion": sug,
            })
        })
        .collect();
    json!({ "questions": list }).to_string()
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