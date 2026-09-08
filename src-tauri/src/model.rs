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
    #[serde(default)]
    pub verify_command: Option<String>,
    #[serde(default)]
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
    #[serde(default)]
    pub tickets: Vec<Ticket>,
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
            kind: "disabled".into(),
            enabled: false,
            plan_args: vec![],
            exec_args: vec![],
        },
        AgentDef {
            id: "codex".into(),
            label: "Codex".into(),
            bin: "codex".into(),
            kind: "disabled".into(),
            enabled: false,
            plan_args: vec![],
            exec_args: vec![],
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