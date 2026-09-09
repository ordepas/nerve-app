use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    // alias snake_case: los agentes devuelven verify_command/depends_on en el
    // bloque JSON (el prompt lo pide así) y sin el alias se descartaban
    #[serde(default, alias = "verify_command")]
    pub verify_command: Option<String>,
    #[serde(default, alias = "depends_on")]
    pub depends_on: Vec<String>,
    #[serde(default = "default_ticket_status")]
    pub status: String, // todo | in_dev | done | blocked
    #[serde(default)]
    pub approved: bool,
}

fn default_ticket_status() -> String {
    "todo".into()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SpecVersion {
    pub version: u32,
    pub content: String,
    pub created_at: u64,
}

/// Comentario de revisión: desviación detectada al comparar la implementación
/// con el plan (estilo Traycer: critical/major/minor/outdated).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    // el id del agente se descarta: Nerve asigna new_id("rc") al normalizar
    #[serde(default)]
    pub id: String,
    pub severity: String, // critical | major | minor | outdated
    #[serde(default)]
    pub file: Option<String>,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub resolved: bool,
    // created_at lo asigna Nerve al normalizar; los agentes no lo devuelven
    #[serde(default)]
    pub created_at: u64,
}

/// Artefacto de planificación previa a la spec (estilo Traycer): brief,
/// arquitectura o flujos. Se genera en fases separadas y con aprobación
/// del usuario entre cada una.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlanArtifact {
    pub kind: String, // brief | architecture | flows
    pub content: String,
    pub created_at: u64,
}

/// Pregunta que el agente formula antes de generar un documento.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub answer: Option<String>,
}

/// Preguntas pendientes de responder para un documento aún no generado.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PendingDoc {
    pub kind: String, // brief | architecture | flows | spec
    pub questions: Vec<Question>,
    pub created_at: u64,
}

/// Respuesta del usuario a una pregunta (entrada del comando answer_doc).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AnswerInput {
    pub id: String,
    pub text: String,
}

/// Mensaje del chat con el agente (persistido en la task).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub role: String, // user | agent
    pub text: String,
    pub created_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    pub intent: String,
    pub status: String, // planning | ready | in_dev | done | archived
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub spec_current: Option<String>,
    #[serde(default)]
    pub spec_versions: Vec<SpecVersion>,
    // documentos de planificación (brief/arquitectura/flujos), opcionales
    #[serde(default)]
    pub plan_artifacts: Vec<PlanArtifact>,
    // preguntas pendientes por documento (fase "pregúntame antes de construir")
    #[serde(default)]
    pub pending_docs: Vec<PendingDoc>,
    #[serde(default)]
    pub tickets: Vec<Ticket>,
    // conversación con el agente sobre esta tarea (tab Chat)
    #[serde(default)]
    pub chat_messages: Vec<ChatMessage>,
    // comentarios de verificación (diff vs plan), persistidos en la task
    #[serde(default)]
    pub review_comments: Vec<ReviewComment>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub ts: u64,
    pub kind: String, // info | chunk | warn | error | ticket-start | ticket-done | done
    pub text: Option<String>,
    #[serde(default)]
    pub ticket_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub task_id: String,
    pub agent: String,
    pub mode: String, // new_worktree | workspace | existing_worktree
    pub worktree: Option<String>,
    pub worktree_path: Option<String>,
    pub base_sha: Option<String>,
    pub checkpoint_sha: Option<String>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub status: String, // running | done | failed | cancelled
    pub session_id: Option<String>,
    pub summary: Option<String>,
    #[serde(default)]
    pub events: Vec<RunEvent>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub status: String,
    pub additions: i64,
    pub deletions: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiffResult {
    pub diff: String,
    pub files: Vec<FileChange>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentDef {
    pub id: String,
    pub label: String,
    pub bin: String,
    pub kind: String, // qwen | mock | disabled
    pub enabled: bool,
    #[serde(default)]
    pub plan_args: Vec<String>,
    #[serde(default)]
    pub exec_args: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentsConfig {
    #[serde(default = "default_agents")]
    pub agents: Vec<AgentDef>,
}

fn default_agents() -> Vec<AgentDef> {
    vec![
        AgentDef {
            id: "qwen".into(),
            label: "Qwen Code".into(),
            bin: "qwen".into(),
            kind: "qwen".into(),
            enabled: true,
            plan_args: vec!["--output-format".into(), "stream-json".into(), "--approval-mode".into(), "plan".into()],
            exec_args: vec!["--output-format".into(), "stream-json".into(), "--approval-mode".into(), "yolo".into()],
        },
        AgentDef {
            id: "ollama".into(),
            label: "Ollama (local)".into(),
            bin: String::new(),
            kind: "ollama".into(),
            enabled: true,
            plan_args: vec![],
            exec_args: vec![],
        },
        AgentDef {
            id: "mock".into(),
            label: "Agente simulado (demo)".into(),
            bin: String::new(),
            kind: "mock".into(),
            enabled: true,
            plan_args: vec![],
            exec_args: vec![],
        },
        AgentDef {
            id: "claude".into(),
            label: "Claude Code".into(),
            bin: "claude".into(),
            kind: "claude".into(),
            enabled: true,
            // -p/--print: modo headless (sin TUI); el prompt llega por stdin.
            // stream-json en print exige --verbose.
            plan_args: vec!["-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into(), "--permission-mode".into(), "plan".into()],
            exec_args: vec!["-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into(), "--permission-mode".into(), "bypassPermissions".into()],
        },
        AgentDef {
            id: "codex".into(),
            label: "Codex".into(),
            bin: "codex".into(),
            kind: "codex".into(),
            enabled: true,
            plan_args: vec!["exec".into(), "--json".into(), "--sandbox".into(), "read-only".into()],
            exec_args: vec!["exec".into(), "--json".into(), "--sandbox".into(), "workspace-write".into(), "--dangerously-bypass-approvals-and-sandbox".into()],
        },
        AgentDef {
            id: "gemini".into(),
            label: "Gemini CLI".into(),
            bin: "gemini".into(),
            kind: "gemini".into(),
            enabled: true,
            plan_args: vec!["--output-format".into(), "stream-json".into(), "--approval-mode".into(), "plan".into()],
            exec_args: vec!["--output-format".into(), "stream-json".into(), "--approval-mode".into(), "yolo".into()],
        },
        AgentDef {
            id: "opencode".into(),
            label: "OpenCode".into(),
            bin: "opencode".into(),
            kind: "disabled".into(),
            enabled: false,
            plan_args: vec![],
            exec_args: vec![],
        },
    ]
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub project_path: Option<String>,
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,
    #[serde(default = "default_ollama_model")]
    pub ollama_model: String,
    // presupuesto máximo de "pasos de agente" por run (tool calls + turns);
    // 0 = sin límite. El run se aborta al agotarlo.
    #[serde(default)]
    pub max_steps: u64,
    // si está vacío, la ejecución de comandos por parte del agente es libre;
    // si tiene entradas, solo se permiten los prefijos listados
    #[serde(default)]
    pub command_allowlist: Vec<String>,
    // inyectar el AGENTS.md más cercano (subiendo hasta la raíz) en plan y exec
    #[serde(default = "default_true")]
    pub agents_md_enabled: bool,
    // perfil por paso: agente para planificar y agente para ejecutar;
    // None = usar el mismo agente elegido en la toolbar
    #[serde(default)]
    pub exec_agent: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_ollama_url() -> String {
    "http://localhost:11434".into()
}

fn default_ollama_model() -> String {
    String::new()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OllamaModelInfo {
    pub name: String,
    pub supports_tools: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMeta {
    #[serde(default = "meta_version")]
    pub version: u32,
}

fn meta_version() -> u32 {
    1
}