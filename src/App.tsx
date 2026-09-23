import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ReactNode } from "react";
import "./styles.css";

// ---------- tipos (espejo de model.rs) ----------

interface Ticket {
  id: string;
  title: string;
  description: string;
  acceptance: string[];
  verifyCommand: string | null;
  dependsOn: string[];
  status: string;
  approved: boolean;
}

interface SpecVersion {
  version: number;
  content: string;
  createdAt: number;
}

interface PlanArtifact {
  kind: string; // brief | architecture | flows | spec
  content: string;
  createdAt: number;
}

interface Question {
  id: string;
  text: string;
  suggestion: string | null;
  answer: string | null;
}

interface PendingDoc {
  kind: string; // brief | architecture | flows | spec
  questions: Question[];
  createdAt: number;
}

interface ChatMessage {
  role: string; // user | agent
  text: string;
  fromAgent?: string; // agente que respondió (A2A)
  createdAt: number;
}

interface AgentMessage {
  kind: string; // query | reply
  fromAgent: string;
  text: string;
  createdAt: number;
}

interface Task {
  id: string;
  title: string;
  intent: string;
  status: string;
  createdAt: number;
  updatedAt: number;
  specCurrent: string | null;
  specVersions: SpecVersion[];
  planArtifacts?: PlanArtifact[];
  pendingDocs?: PendingDoc[];
  tickets: Ticket[];
  chatMessages?: ChatMessage[];
  agentMessages?: AgentMessage[];
  reviewComments?: ReviewComment[];
}

interface ReviewComment {
  id: string;
  severity: string; // critical | major | minor | outdated
  file: string | null;
  title: string;
  detail: string;
  resolved: boolean;
  createdAt: number;
}

interface RunEvent {
  ts: number;
  kind: string;
  text: string | null;
  ticketId: string | null;
}

interface Run {
  id: string;
  taskId: string;
  agent: string;
  mode: string;
  worktreePath: string | null;
  baseSha: string | null;
  checkpointSha: string | null;
  startedAt: number;
  finishedAt: number | null;
  status: string;
  sessionId: string | null;
  summary: string | null;
  events?: RunEvent[];
}

interface FileChange {
  path: string;
  status: string;
  additions: number;
  deletions: number;
}

interface DiffResult {
  diff: string;
  files: FileChange[];
}

interface AgentDef {
  id: string;
  label: string;
  bin: string;
  kind: string;
  enabled: boolean;
}

interface AgentsConfig {
  agents: AgentDef[];
}

interface OllamaModelInfo {
  name: string;
  supportsTools: boolean;
}

interface EpicStage {
  title: string;
  tasks: string[];
}

interface EpicDef {
  id: string;
  title: string;
  intent: string;
  planTaskId: string;
  agentId: string;
  skillId: string;
  stages: EpicStage[];
  currentStage: number;
  status: string;
  gate: string;
  yolo: boolean;
  lastError: string | null;
  currentRun: string | null;
  createdAt: number;
  updatedAt: number;
}

interface WorkspaceCfg {
  projectPath: string | null;
  ollamaUrl: string;
  ollamaModel: string;
  maxSteps: number;
  commandAllowlist: string[];
  agentsMdEnabled?: boolean;
  execAgent?: string | null;
}

interface SkillDef {
  id: string;
  label: string;
  description: string;
  template: string;
}

type ModalSpec = {
  title: string;
  fields: FieldDef[];
  submitLabel?: string;
  onSubmit: (values: Record<string, string>) => void;
};

type ConfirmSpec = {
  title: string;
  message: string;
  confirmLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
};

const TECH_KEY = "nerve.techMode";

// ---------- helpers ----------

const fmtTime = (ms: number) =>
  new Date(ms).toLocaleString(undefined, {
    day: "2-digit",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });

// estados en lenguaje humano (capa no técnica); el texto crudo queda para el modo técnico
const TASK_STATUS: Record<string, string> = {
  planning: "Creando el plan…",
  ready: "Esperando tu aprobación",
  in_dev: "En construcción",
  done: "Completada",
  blocked: "Necesita tu atención",
  cancelled: "Detenida",
  failed: "Falló",
};

const TICKET_STATUS: Record<string, string> = {
  todo: "Pendiente",
  in_dev: "En construcción",
  done: "Hecho",
  blocked: "Bloqueado",
};

const RUN_STATUS: Record<string, string> = {
  running: "Trabajando…",
  done: "Hecho",
  failed: "Falló",
  cancelled: "Detenido",
};

const EPIC_STATUS: Record<string, string> = {
  draft: "Borrador",
  running: "Trabajando…",
  awaiting_gate: "Esperando tu aprobación",
  done: "Completado",
  failed: "Falló",
  cancelled: "Detenido",
};

const GATE_LABELS: Record<string, string> = {
  master: "Revisa el plan maestro y sus fases",
  plan: "Revisa la especificación de la task",
  fase: "Revisa los cambios construidos",
};

function humanStatus(map: Record<string, string>, s: string) {
  return map[s] ?? s;
}

const AGENT_LABELS: Record<string, string> = {
  mock: "Agente simulado (pruebas)",
  qwen: "Qwen Code",
  claude: "Claude Code",
  codex: "Codex",
  gemini: "Gemini CLI",
  ollama: "Ollama (local)",
  opencode: "OpenCode",
};

function agentLabel(id: string) {
  return AGENT_LABELS[id] ?? id;
}

function runKindLabel(mode: string, techMode: boolean) {
  if (techMode) return mode;
  if (mode === "plan") return "solo planifica";
  if (mode === "doc") return "documento (solo lee)";
  if (mode === "ask") return "preguntas (solo lee)";
  if (mode === "chat") return "conversación (solo lee)";
  if (mode === "a2a") return "consulta al equipo (solo lee)";
  if (mode === "workspace") return "directo en tu proyecto";
  return "en copia aislada";
}

function StatusDot({ status, map }: { status: string; map: Record<string, string> }) {
  return (
    <span className={`status-pill st-${status}`}>
      <span className="dot" />
      {humanStatus(map, status)}
    </span>
  );
}

// ---------- artefactos (specs como documentos) ----------

// renderer de markdown mínimo (títulos, listas, código, citas, énfasis) — sin dependencias
function renderMarkdown(src: string): string {
  const esc = (s: string) =>
    s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const inline = (s: string) =>
    esc(s)
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/(^|[^*])\*([^*\n]+)\*/g, "$1<em>$2</em>");
  const lines = src.split("\n");
  const out: string[] = [];
  let inCode = false;
  let codeBuf: string[] = [];
  let listOpen: "ul" | "ol" | null = null;
  const closeList = () => {
    if (listOpen) {
      out.push(`</${listOpen}>`);
      listOpen = null;
    }
  };
  for (const raw of lines) {
    const line = raw.trimEnd();
    const fence = line.match(/^```(\w*)/);
    if (fence) {
      if (inCode) {
        out.push(`<pre class="md-code"><code>${esc(codeBuf.join("\n"))}</code></pre>`);
        codeBuf = [];
        inCode = false;
      } else {
        closeList();
        inCode = true;
      }
      continue;
    }
    if (inCode) {
      codeBuf.push(raw);
      continue;
    }
    const h = line.match(/^(#{1,4})\s+(.*)$/);
    if (h) {
      closeList();
      const lvl = h[1].length;
      out.push(`<h${lvl}>${inline(h[2])}</h${lvl}>`);
      continue;
    }
    const ul = line.match(/^[-*]\s+(.*)$/);
    const ol = line.match(/^\d+[.)]\s+(.*)$/);
    if (ul || ol) {
      const want: "ul" | "ol" = ul ? "ul" : "ol";
      if (listOpen !== want) {
        closeList();
        out.push(`<${want}>`);
        listOpen = want;
      }
      out.push(`<li>${inline((ul ?? ol)![1])}</li>`);
      continue;
    }
    const quote = line.match(/^>\s?(.*)$/);
    if (quote) {
      closeList();
      out.push(`<blockquote>${inline(quote[1])}</blockquote>`);
      continue;
    }
    if (/^(---|\*\*\*|___)\s*$/.test(line)) {
      closeList();
      out.push("<hr />");
      continue;
    }
    if (line.trim() === "") {
      closeList();
      continue;
    }
    closeList();
    out.push(`<p>${inline(line)}</p>`);
  }
  if (inCode && codeBuf.length > 0) {
    out.push(`<pre class="md-code"><code>${esc(codeBuf.join("\n"))}</code></pre>`);
  }
  closeList();
  return out.join("\n");
}

// ---------- modal ----------

interface FieldDef {
  key: string;
  label: string;
  multiline?: boolean;
  initial?: string;
  required?: boolean;
}

// fila compacta de la lista de documentos: doble clic (o Ver) abre el lector
function ArtifactRow(props: {
  icon: string;
  title: string;
  sub: ReactNode;
  onOpen: () => void;
  onDelete?: () => void;
}) {
  return (
    <div
      className="artifact-row"
      onDoubleClick={props.onOpen}
      title="Doble clic para ver el documento"
    >
      <span className="artifact-icon" aria-hidden="true">{props.icon}</span>
      <div className="artifact-title">
        <strong>{props.title}</strong>
        <span className="artifact-sub">{props.sub}</span>
      </div>
      <div className="artifact-actions">
        <button className="ghost" onClick={props.onOpen}>Ver</button>
        {props.onDelete && (
          <button className="ghost" onClick={props.onDelete} title="Eliminar este documento">🗑</button>
        )}
      </div>
    </div>
  );
}

// lector modal de un documento (markdown renderizado, copiar/descargar)
function DocViewerModal(props: {
  icon: string;
  title: string;
  sub: ReactNode;
  content: string;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const download = () => {
    const blob = new Blob([props.content], { type: "text/markdown" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = props.title.endsWith(".md") ? props.title : `${props.title}.md`;
    a.click();
    URL.revokeObjectURL(url);
  };
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.content);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      /* noop */
    }
  };
  return (
    <div className="modal-backdrop" onClick={props.onClose}>
      <div className="modal doc-viewer" onClick={(e) => e.stopPropagation()}>
        <div className="doc-viewer-head">
          <span className="artifact-icon" aria-hidden="true">{props.icon}</span>
          <div className="artifact-title">
            <strong>{props.title}</strong>
            <span className="artifact-sub">{props.sub}</span>
          </div>
          <div className="artifact-actions">
            <button className="ghost" onClick={copy}>{copied ? "✓ Copiado" : "Copiar"}</button>
            <button className="ghost" onClick={download}>Descargar</button>
            <button className="ghost" onClick={props.onClose}>✕ Cerrar</button>
          </div>
        </div>
        <div
          className="doc-viewer-body artifact-body md"
          dangerouslySetInnerHTML={{ __html: renderMarkdown(props.content) }}
        />
      </div>
    </div>
  );
}

// sección de artefactos del plan: brief → arquitectura → flujos → spec,
// listada como navegación (doble clic abre el lector), con fase de preguntas
// del agente antes de cada documento y confirmación para generarlo
function PlanArtifactsSection(props: {
  task: Task;
  selectedAgent: string;
  busy: boolean;
  techMode: boolean;
  openConfirm: (spec: ConfirmSpec) => void;
  setError: (e: string) => void;
  updateCurrent: (t: Task) => void;
}) {
  const { task } = props;
  const artifacts = task.planArtifacts ?? [];
  const pendings = task.pendingDocs ?? [];
  const running = props.busy;
  const [viewer, setViewer] = useState<null | {
    icon: string; title: string; sub: ReactNode; content: string;
  }>(null);
  const byKind = (k: string) => artifacts.find((a) => a.kind === k);
  const pendingByKind = (k: string) => pendings.find((p) => p.kind === k);

  const DOC_META: Record<string, { label: string; file: string; icon: string; the: string }> = {
    brief: { label: "Brief", file: "brief.md", icon: "🎯", the: "el brief" },
    architecture: { label: "Arquitectura", file: "architecture.md", icon: "🏗", the: "el documento de arquitectura" },
    flows: { label: "Flujos", file: "flows.md", icon: "🔀", the: "el documento de flujos" },
    spec: { label: "Spec", file: "spec.md", icon: "📄", the: "la especificación" },
  };
  const order = ["brief", "architecture", "flows", "spec"];
  const nextKind = order.find((k) => !byKind(k));
  const nextMeta = nextKind ? DOC_META[nextKind] : null;

  const openViewer = (k: string) => {
    const meta = DOC_META[k];
    const a = byKind(k);
    if (!a) return;
    const lines = a.content.split("\n").length;
    const sub = props.techMode
      ? <>{k} · {lines} líneas · {fmtTime(a.createdAt)}</>
      : <>{lines} líneas · {fmtTime(a.createdAt)}</>;
    setViewer({ icon: meta.icon, title: props.techMode ? meta.file : meta.label, sub, content: a.content });
  };

  const askGenerate = (kind: string) => {
    const meta = DOC_META[kind];
    props.openConfirm({
      title: "¿Generar este documento?",
      message: `El agente va a leer tu proyecto (solo lectura) para escribir ${meta.the}. No modifica ningún archivo.`,
      confirmLabel: "Generar",
      onConfirm: async () => {
        try {
          await invoke("generate_doc", { taskId: task.id, agentId: props.selectedAgent, kind });
        } catch (e) {
          props.setError(String(e));
        }
      },
    });
  };

  const askQuestions = (kind: string) => {
    const meta = DOC_META[kind];
    props.openConfirm({
      title: `¿Preguntar antes de generar ${meta.label}?`,
      message: `El agente leerá tu proyecto (solo lectura) y te formulará sus preguntas para escribir ${meta.the}. Tú las respondes y luego se genera.`,
      confirmLabel: "Preguntar",
      onConfirm: async () => {
        try {
          await invoke("ask_doc", { taskId: task.id, agentId: props.selectedAgent, kind });
        } catch (e) {
          props.setError(String(e));
        }
      },
    });
  };

  const confirmDelete = (kind: string) => {
    const meta = DOC_META[kind];
    props.openConfirm({
      title: "Eliminar documento",
      message: `Se eliminará ${meta.the} y sus preguntas pendientes. Puedes generarlo de nuevo cuando quieras.`,
      confirmLabel: "Eliminar",
      danger: true,
      onConfirm: async () => {
        try {
          const t = await invoke<Task>("delete_doc", { taskId: task.id, kind });
          props.updateCurrent(t);
        } catch (e) {
          props.setError(String(e));
        }
      },
    });
  };

  return (
    <div className="plan-docs">
      <div className="doc-list" role="list">
        {order.map((k) => {
          const meta = DOC_META[k];
          const a = byKind(k);
          const p = pendingByKind(k);
          if (a) {
            const lines = a.content.split("\n").length;
            const sub = props.techMode
              ? <>{k} · {lines} líneas · {fmtTime(a.createdAt)}</>
              : <>{lines} líneas · {fmtTime(a.createdAt)}</>;
            return (
              <ArtifactRow
                key={k}
                icon={meta.icon}
                title={props.techMode ? meta.file : meta.label}
                sub={sub}
                onOpen={() => openViewer(k)}
                onDelete={() => confirmDelete(k)}
              />
            );
          }
          if (p) {
            return (
              <QuestionsCard
                key={k}
                pending={p}
                meta={meta}
                running={running}
                openConfirm={props.openConfirm}
                setError={props.setError}
                updateCurrent={props.updateCurrent}
                taskId={task.id}
                selectedAgent={props.selectedAgent}
              />
            );
          }
          const isNext = k === nextKind;
          const pendingLabel = props.techMode ? `${meta.file} · pendiente` : `${meta.label} — aún no generado`;
          return (
            <div key={k} className="doc-step" role="listitem">
              <div className="doc-step-info">
                <span className="artifact-icon" aria-hidden="true">{meta.icon}</span>
                <div>
                  <strong>{props.techMode ? meta.file : meta.label}</strong>
                  <span className="artifact-sub">{pendingLabel}</span>
                </div>
              </div>
              <div className="doc-step-actions">
                {isNext && (
                  <button className="ghost" disabled={running} onClick={() => askQuestions(k)}>
                    💬 Preguntar
                  </button>
                )}
                {isNext && (
                  <button className="ghost" disabled={running} onClick={() => askGenerate(k)}>
                    Generar
                  </button>
                )}
                {!isNext && (
                  <span className="doc-step-wait">
                    Sigue después de {nextMeta ? nextMeta.label.toLowerCase() : "el documento anterior"}
                  </span>
                )}
              </div>
            </div>
          );
        })}
      </div>
      {running && <span className="small dim">El agente está trabajando…</span>}
      {viewer && (
        <DocViewerModal
          icon={viewer.icon}
          title={viewer.title}
          sub={viewer.sub}
          content={viewer.content}
          onClose={() => setViewer(null)}
        />
      )}
    </div>
  );
}

// tarjeta de preguntas pendientes para un documento (respuestas del usuario)
function QuestionsCard(props: {
  pending: PendingDoc;
  meta: { label: string; file: string; icon: string; the: string };
  running: boolean;
  openConfirm: (spec: ConfirmSpec) => void;
  setError: (e: string) => void;
  updateCurrent: (t: Task) => void;
  taskId: string;
  selectedAgent: string;
}) {
  const { pending, meta } = props;
  const [vals, setVals] = useState<Record<string, string>>(() =>
    Object.fromEntries(pending.questions.map((q) => [q.id, q.answer ?? q.suggestion ?? ""])),
  );
  const save = async () => {
    try {
      const t = await invoke<Task>("answer_doc", {
        taskId: props.taskId,
        kind: pending.kind,
        answers: pending.questions.map((q) => ({ id: q.id, text: vals[q.id] ?? "" })),
      });
      props.updateCurrent(t);
    } catch (e) {
      props.setError(String(e));
    }
  };
  const allFilled = pending.questions.every((q) => (vals[q.id] ?? "").trim() !== "");
  return (
    <div className="questions-card">
      <div className="questions-head">
        <span className="artifact-icon" aria-hidden="true">💬</span>
        <strong>El agente tiene preguntas sobre {meta.the}</strong>
      </div>
      {pending.questions.map((q, i) => (
        <label key={q.id} className="question-row">
          <span className="q-text">{i + 1}. {q.text}</span>
          {q.suggestion && (
            <span className="q-suggestion">Sugerencia del agente: {q.suggestion}</span>
          )}
          <textarea
            value={vals[q.id] ?? ""}
            placeholder="Tu respuesta (opcional)"
            onChange={(e) => setVals((v) => ({ ...v, [q.id]: e.target.value }))}
          />
        </label>
      ))}
      <div className="questions-actions">
        <button onClick={save}>Guardar respuestas</button>
        <button
          className="primary"
          disabled={!allFilled || props.running}
          title={allFilled ? "" : "Responde todas las preguntas para generar el documento"}
          onClick={() =>
            props.openConfirm({
              title: "¿Generar con tus respuestas?",
              message: `Se escribirá ${meta.the} incorporando tus respuestas. Las preguntas quedarán respondidas.`,
              confirmLabel: "Generar",
              onConfirm: async () => {
                await save();
                try {
                  await invoke("generate_doc", { taskId: props.taskId, agentId: props.selectedAgent, kind: pending.kind });
                } catch (e) {
                  props.setError(String(e));
                }
              },
            })
          }
        >
          Generar {meta.label}
        </button>
      </div>
    </div>
  );
}

function FormModal(props: {
  title: string;
  fields: FieldDef[];
  submitLabel?: string;
  onSubmit: (values: Record<string, string>) => void;
  onClose: () => void;
}) {
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(props.fields.map((f) => [f.key, f.initial ?? ""])),
  );
  const [missing, setMissing] = useState<Set<string>>(new Set());

  const submit = () => {
    const empty = props.fields.filter((f) => f.required && !values[f.key]?.trim());
    if (empty.length > 0) {
      setMissing(new Set(empty.map((f) => f.key)));
      return;
    }
    props.onSubmit(values);
  };

  return (
    <div className="modal-backdrop" onClick={props.onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3>{props.title}</h3>
        {props.fields.map((f) => (
          <label key={f.key}>
            <span>{f.label}{f.required && " *"}</span>
            {f.multiline ? (
              <textarea
                autoFocus={props.fields[0]?.key === f.key}
                className={missing.has(f.key) ? "field-error" : undefined}
                value={values[f.key] ?? ""}
                onChange={(e) => {
                  setValues((v) => ({ ...v, [f.key]: e.target.value }));
                  if (missing.has(f.key) && e.target.value.trim()) {
                    setMissing((prev) => {
                      const next = new Set(prev);
                      next.delete(f.key);
                      return next;
                    });
                  }
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
                }}
              />
            ) : (
              <input
                autoFocus={props.fields[0]?.key === f.key}
                className={missing.has(f.key) ? "field-error" : undefined}
                value={values[f.key] ?? ""}
                onChange={(e) => {
                  setValues((v) => ({ ...v, [f.key]: e.target.value }));
                  if (missing.has(f.key) && e.target.value.trim()) {
                    setMissing((prev) => {
                      const next = new Set(prev);
                      next.delete(f.key);
                      return next;
                    });
                  }
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit();
                  if (e.key === "Escape") props.onClose();
                }}
              />
            )}
          </label>
        ))}
        <div className="modal-actions">
          <button onClick={props.onClose}>Cancelar</button>
          <button className="primary" onClick={submit}>
            {props.submitLabel ?? "Guardar"}
          </button>
        </div>
        {missing.size > 0 && (
          <p className="modal-warn">Rellena los campos marcados con * para continuar.</p>
        )}
      </div>
    </div>
  );
}

function ConfirmModal(props: {
  title: string;
  message: string;
  confirmLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <div className="modal-backdrop" onClick={props.onClose}>
      <div className="modal modal-confirm" onClick={(e) => e.stopPropagation()}>
        <h3>{props.title}</h3>
        <p className="modal-message">{props.message}</p>
        <div className="modal-actions">
          <button onClick={props.onClose}>Cancelar</button>
          <button
            className={props.danger ? "danger solid" : "primary"}
            onClick={() => {
              props.onConfirm();
              props.onClose();
            }}
            autoFocus
          >
            {props.confirmLabel ?? "Confirmar"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------- app ----------

export default function App() {
  const [view, setView] = useState<"home" | "task" | "settings" | "epics">("home");
  const [projectPath, setProjectPath] = useState<string | null>(null);
  const [tasks, setTasks] = useState<Task[]>([]);
  const [current, setCurrent] = useState<Task | null>(null);
  const [runs, setRuns] = useState<Run[]>([]);
  const [currentRun, setCurrentRun] = useState<Run | null>(null);
  const [events, setEvents] = useState<RunEvent[]>([]);
  // runId del log activo: distingue historial del run anterior vs stream nuevo
  const logRunId = useRef<string>("");
  // task actualmente abierta: filtra eventos de runs de otras tasks
  const currentTaskIdRef = useRef<string>("");
  const [agents, setAgents] = useState<AgentDef[]>([]);
  const [epics, setEpics] = useState<EpicDef[]>([]);
  const [selectedAgent, setSelectedAgent] = useState("mock");
  const [runMode, setRunMode] = useState("worktree");
  const [error, setError] = useState<string | null>(null);
  const [modal, setModal] = useState<ModalSpec | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [techMode, setTechMode] = useState(() => {
    try {
      return localStorage.getItem(TECH_KEY) === "1";
    } catch {
      return false;
    }
  });

  const toggleTechMode = useCallback((v: boolean) => {
    setTechMode(v);
    try {
      localStorage.setItem(TECH_KEY, v ? "1" : "0");
    } catch {
      /* noop */
    }
  }, []);

  const openModal = useCallback((spec: ModalSpec) => setModal(spec), []);
  const openConfirm = useCallback((spec: ConfirmSpec) => setConfirm(spec), []);

  const refreshWorkspace = useCallback(async () => {
    try {
      const cfg = await invoke<{ projectPath: string | null }>("get_workspace");
      setProjectPath(cfg.projectPath);
    } catch {
      setProjectPath(null);
    }
  }, []);

  const refreshTasks = useCallback(async () => {
    try {
      setTasks(await invoke<Task[]>("list_tasks"));
    } catch {
      setTasks([]);
    }
  }, []);

  const refreshAgents = useCallback(async () => {
    try {
      const cfg = await invoke<AgentsConfig>("list_agents");
      const enabled = cfg.agents.filter((a) => a.enabled && a.kind !== "disabled");
      setAgents(enabled);
      // con cada recarga, si el agente actual dejó de existir, elegir el primero real
      setSelectedAgent((cur) => (enabled.some((a) => a.id === cur) ? cur : enabled[0]?.id ?? "mock"));
    } catch {
      setAgents([]);
    }
  }, []);

  const refreshEpics = useCallback(async () => {
    try {
      setEpics(await invoke<EpicDef[]>("list_epics"));
    } catch {
      setEpics([]);
    }
  }, []);

  const epicAction = useCallback(async (cmd: "start_epic" | "continue_epic" | "cancel_epic", id: string) => {
    try {
      const updated = await invoke<EpicDef>(cmd, { id });
      // el motor puede haber emitido epic-updated más nuevo que este invoke
      setEpics((prev) => {
        const i = prev.findIndex((x) => x.id === id);
        if (i < 0) return [updated, ...prev];
        if (prev[i].updatedAt >= updated.updatedAt) return prev;
        const copy = [...prev];
        copy[i] = updated;
        return copy;
      });
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const openTask = useCallback(async (id: string) => {
    setError(null);
    try {
      const t = await invoke<Task>("get_task", { id });
      currentTaskIdRef.current = id;
      setCurrent(t);
      const rs = await invoke<Run[]>("list_runs", { taskId: id });
      setRuns(rs);
      setEvents([]);
      // historial del run más reciente (el stream en vivo se añade encima)
      const latest = rs[0];
      logRunId.current = latest ? latest.id : "";
      if (latest && latest.events && latest.events.length > 0) {
        setEvents(latest.events.map((ev) => ({ ...ev, text: ev.text })));
      }
      setCurrentRun(null);
      setView("task");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    const unsubs: Array<() => void> = [];
    void (async () => {
      const l1 = await listen<{ taskId: string; runId: string; event: RunEvent }>("run-event", (e) => {
        if (e.payload.taskId !== currentTaskIdRef.current) return;
        if (logRunId.current !== e.payload.runId) {
          // run nuevo: el log empieza de cero
          logRunId.current = e.payload.runId;
          setEvents([]);
        }
        setEvents((prev) => [...prev, e.payload.event]);
      });
      const l2 = await listen<{ taskId: string; runId: string; spec: string; tickets: string }>("run-plan", (e) => {
        void (async () => {
          try {
            const t = await invoke<Task>("get_task", { id: e.payload.taskId });
            const parsed = JSON.parse(e.payload.tickets) as { tickets: Ticket[] };
            await invoke("set_tickets", { taskId: t.id, tickets: parsed.tickets });
            await invoke("set_spec", { taskId: t.id, content: e.payload.spec });
            setCurrent(await invoke<Task>("get_task", { id: e.payload.taskId }));
          } catch (err) {
            setError(String(err));
          }
        })();
      });
      const l3 = await listen<Run>("run-finished", (e) => {
        const run = e.payload;
        setRuns((prev) => [run, ...prev.filter((r) => r.id !== run.id)]);
        setCurrentRun(run);
        // carreras de escritura del store: recargar runs al terminar
        void (async () => {
          try {
            const rs = await invoke<Run[]>("list_runs", { taskId: run.taskId });
            setRuns(rs);
          } catch {
            /* noop */
          }
        })();
      });
      const l4 = await listen<Task>("task-updated", (e) => {
        const t = e.payload;
        setCurrent((prev) => (prev?.id === t.id ? t : prev));
        setTasks((prev) => {
          const i = prev.findIndex((x) => x.id === t.id);
          if (i < 0) return prev;
          const copy = [...prev];
          copy[i] = t;
          return copy;
        });
      });
      const l5 = await listen<EpicDef>("epic-updated", (e) => {
        const epic = e.payload;
        setEpics((prev) => {
          const i = prev.findIndex((x) => x.id === epic.id);
          if (i < 0) return [epic, ...prev];
          const copy = [...prev];
          copy[i] = epic;
          return copy;
        });
        void refreshTasks();
      });
      if (disposed) {
        [l1, l2, l3, l4, l5].forEach((l) => l());
        return;
      }
      unsubs.push(l1, l2, l3, l4, l5);
    })();
    return () => {
      disposed = true;
      unsubs.forEach((u) => u());
    };
  }, [openTask, refreshAgents, refreshTasks, refreshWorkspace, refreshEpics]);

  useEffect(() => {
    void refreshWorkspace();
    void refreshTasks();
    void refreshAgents();
    void refreshEpics();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const updateCurrent = useCallback((t: Task) => {
    setCurrent(t);
    setTasks((prev) => {
      const i = prev.findIndex((x) => x.id === t.id);
      if (i < 0) return [t, ...prev];
      const copy = [...prev];
      copy[i] = t;
      return copy;
    });
  }, []);

  if (!projectPath && view !== "settings") {
    return (
      <div className="app">
        <Onboarding onSaved={(p) => { setProjectPath(p); setView("home"); }} />
      </div>
    );
  }

  return (
    <div className="app">
      <Sidebar
        view={view}
        setView={setView}
        projectPath={projectPath}
        tasks={tasks}
        currentTaskId={current?.id ?? null}
        epics={epics}
        onOpenTask={openTask}
        onOpenEpics={() => setView("epics")}
        onNewTask={async () => {
          openModal({
            title: "¿Qué quieres construir?",
            fields: [
              { key: "title", label: "Ponle un nombre a esta idea", required: true },
              { key: "intent", label: "Descríbela con tus palabras (qué quieres lograr)", multiline: true, required: true },
            ],
            submitLabel: "Empezar",
            onSubmit: async (values) => {
              setModal(null);
              try {
                const t = await invoke<Task>("create_task", {
                  title: values.title.trim(),
                  intent: values.intent.trim(),
                });
                await refreshTasks();
                void openTask(t.id);
              } catch (e) {
                setError(String(e));
              }
            },
          });
        }}
        onOpenSettings={() => setView("settings")}
      />
      <main className="main">
        {error && (
          <div className="error-bar" onClick={() => setError(null)}>
            {error} — click para cerrar
          </div>
        )}
        {view === "home" && (
          <Home tasks={tasks} onOpenTask={openTask} projectPath={projectPath} />
        )}
        {view === "epics" && (
          <EpicsView
            epics={epics}
            tasks={tasks}
            projectPath={projectPath}
            onNewEpic={() => {
              openModal({
                title: "Nuevo epic",
                fields: [
                  { key: "title", label: "Ponle un nombre a este epic", required: true },
                  { key: "intent", label: "Describe el objetivo grande (qué quieres lograr al final)", multiline: true, required: true },
                ],
                submitLabel: "Crear epic",
                onSubmit: async (values) => {
                  setModal(null);
                  try {
                    await invoke<EpicDef>("create_epic", {
                      title: values.title.trim(),
                      intent: values.intent.trim(),
                      agentId: selectedAgent,
                      skillId: "plan",
                    });
                    await refreshEpics();
                  } catch (e) {
                    setError(String(e));
                  }
                },
              });
            }}
            onStart={(id) => void epicAction("start_epic", id)}
            onContinue={(id) => void epicAction("continue_epic", id)}
            onToggleYolo={async (id, yolo) => {
              try {
                await invoke<EpicDef>("set_epic_yolo", { id, yolo });
                setEpics((prev) => prev.map((x) => (x.id === id ? { ...x, yolo } : x)));
              } catch (e) {
                setError(String(e));
              }
            }}
            onCancel={(id) => {
              openConfirm({
                title: "Detener el epic",
                message: "Se detendrá el trabajo del epic. Las tasks ya construidas no se pierden.",
                confirmLabel: "Detener",
                onConfirm: async () => {
                  setConfirm(null);
                  await epicAction("cancel_epic", id);
                },
              });
            }}
            onDelete={(id) => {
              openConfirm({
                title: "Eliminar el epic",
                message: "Se elimina el epic (el plan y las tasks creadas se conservan como tasks normales).",
                confirmLabel: "Eliminar",
                danger: true,
                onConfirm: async () => {
                  setConfirm(null);
                  try {
                    await invoke("delete_epic", { id });
                    setEpics((prev) => prev.filter((x) => x.id !== id));
                  } catch (e) {
                    setError(String(e));
                  }
                },
              });
            }}
            onOpenTask={openTask}
          />
        )}
        {view === "task" && current && (
          <TaskView
            task={current}
            runs={runs}
            currentRun={currentRun}
            events={events}
            agents={agents}
            selectedAgent={selectedAgent}
            setSelectedAgent={setSelectedAgent}
            runMode={runMode}
            setRunMode={setRunMode}
            techMode={techMode}
            onTechMode={toggleTechMode}
            updateCurrent={updateCurrent}
            setError={setError}
            openModal={openModal}
            openConfirm={openConfirm}
            refreshRuns={async () => {
              setRuns(await invoke<Run[]>("list_runs", { taskId: current.id }));
            }}
          />
        )}
        {view === "settings" && (
          <Settings
            projectPath={projectPath}
            setProjectPath={setProjectPath}
            refreshAgents={refreshAgents}
          />
        )}
      </main>
      {modal && (
        <FormModal
          title={modal.title}
          fields={modal.fields}
          submitLabel={modal.submitLabel}
          onSubmit={(values) => {
            setModal(null);
            void modal.onSubmit(values);
          }}
          onClose={() => setModal(null)}
        />
      )}
      {confirm && (
        <ConfirmModal
          title={confirm.title}
          message={confirm.message}
          confirmLabel={confirm.confirmLabel}
          danger={confirm.danger}
          onConfirm={confirm.onConfirm}
          onClose={() => setConfirm(null)}
        />
      )}
    </div>
  );
}

// ---------- onboarding ----------

function Onboarding({ onSaved }: { onSaved: (p: string) => void }) {
  const [path, setPath] = useState("");
  const [err, setErr] = useState<string | null>(null);
  return (
    <div className="onboard">
      <h1>⚡ Nerve</h1>
      {err && <div className="error-bar">{err} — click para cerrar</div>}
      <p>
        Workspace spec-first para agentes de código. Primero indica la carpeta
        de tu proyecto (idealmente un repositorio git):
      </p>
      <div className="row">
        <input
          value={path}
          onChange={(e) => setPath(e.target.value)}
          placeholder="C:\\ruta\\a\\tu\\proyecto"
        />
        <button
          className="primary"
          onClick={async () => {
            try {
              const cfg = await invoke<{ projectPath: string | null }>("set_workspace", { path });
              onSaved(cfg.projectPath ?? path);
            } catch (e) {
              setErr(String(e));
            }
          }}
        >
          Conectar
        </button>
      </div>
      <p className="hint">
        Los agentes leerán y ejecutarán sobre esta carpeta. Puedes cambiarla
        luego en Ajustes.
      </p>
    </div>
  );
}

// ---------- epic mode ----------

function EpicsView(props: {
  epics: EpicDef[];
  tasks: Task[];
  projectPath: string | null;
  onNewEpic: () => void;
  onStart: (id: string) => void;
  onContinue: (id: string) => void;
  onCancel: (id: string) => void;
  onDelete: (id: string) => void;
  onOpenTask: (id: string) => void;
  onToggleYolo: (id: string, yolo: boolean) => void;
}) {
  return (
    <div className="page">
      <div className="hero">
        <h1>Epic Mode</h1>
        <p className="hero-sub">
          Grandes ideas divididas en fases. Nerve planifica el conjunto, tú apruebas
          fase a fase (plan, especificación y cambios) y los agentes construyen con
          todo el control de siempre.
        </p>
      </div>
      <h2>Tus epics</h2>
      {props.epics.length === 0 ? (
        <div className="empty-card">
          Aún no hay epics. Un epic toma una intención grande, genera un plan maestro
          con fases (respetando las dependencias entre tickets) y avanza con tu
          aprobación en cada gate.
        </div>
      ) : (
        <div className="grid">
          {props.epics.map((epic) => {
            const stage = epic.stages[epic.currentStage];
            const taskById = (id: string) => props.tasks.find((t) => t.id === id);
            const stageTasks = (stage?.tasks ?? []).map(taskById).filter(Boolean) as Task[];
            return (
              <div key={epic.id} className="card">
                <div className="card-title">{epic.title}</div>
                <div className="card-sub">{epic.intent.slice(0, 120)}</div>
                <div className="card-meta">
                  <StatusDot status={epic.status} map={EPIC_STATUS} />
                  {epic.stages.length > 0 && (
                    <span className="dim">
                      {epic.status === "done"
                        ? `${epic.stages.length} fase(s)`
                        : `Fase ${Math.min(epic.currentStage + 1, epic.stages.length)}/${epic.stages.length}`}
                    </span>
                  )}
                  {epic.yolo && (
                    <span className="chip chip-planning">⚡ YOLO</span>
                  )}
                </div>
                {epic.stages.length > 0 && (
                  <div className="epic-stages">
                    {epic.stages.map((st, i) => {
                      const ts = st.tasks.map(taskById).filter(Boolean) as Task[];
                      const done = ts.length > 0 && ts.every((t) => t.status === "done");
                      const isCurrent = i === epic.currentStage && epic.status !== "done";
                      return (
                        <div
                          key={st.title + i}
                          className={`epic-stage ${done ? "stage-done" : ""} ${isCurrent ? "stage-current" : ""}`}
                        >
                          {done ? "✓" : isCurrent ? "▶" : "○"} {st.title} ({ts.length})
                        </div>
                      );
                    })}
                  </div>
                )}
                {epic.lastError && (
                  <div className="error-bar">{epic.lastError}</div>
                )}
                <div className="card-actions">
                  {epic.status !== "running" && (
                    <button
                      onClick={() => props.onToggleYolo(epic.id, !epic.yolo)}
                      title="YOLO: el epic planifica, aprueba y construye sin detenerse en ningún gate"
                    >
                      ⚡ YOLO: {epic.yolo ? "on" : "off"}
                    </button>
                  )}
                  {(epic.status === "draft" || epic.status === "failed") && (
                    <button className="primary" onClick={() => props.onStart(epic.id)}>
                      {epic.status === "draft" ? "Empezar epic" : "Reintentar epic"}
                      </button>
                  )}
                  {epic.status === "awaiting_gate" && (
                    <>
                      <span className="hint">
                        {GATE_LABELS[epic.gate] ?? "Revisa y continúa"}
                        {stageTasks.length > 0 && (
                          <span>
                            {" · "}
                            {stageTasks.map((t, i) => (
                              <span key={t.id}>
                                {i > 0 && ", "}
                                <a className="link" onClick={() => props.onOpenTask(t.id)}>{t.title}</a>
                              </span>
                            ))}
                          </span>
                        )}
                      </span>
                      <button className="primary" onClick={() => props.onContinue(epic.id)}>
                        Aprobar y continuar
                      </button>
                    </>
                  )}
                  {epic.status === "running" && (
                    <button onClick={() => props.onCancel(epic.id)}>Detener</button>
                  )}
                  {(epic.status === "done" || epic.status === "cancelled") && (
                    <button className="danger" onClick={() => props.onDelete(epic.id)}>
                      Eliminar
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
      <div className="row">
        <button className="primary" onClick={props.onNewEpic}>+ Nuevo epic</button>
      </div>
      <p className="hint">
        Cada gate es una decisión tuya: aprobar planes, revisar especificaciones y
        aplicar los cambios construidos desde la task correspondiente.
      </p>
    </div>
  );
}

// ---------- sidebar ----------

function Sidebar(props: {
  view: string;
  setView: (v: "home" | "task" | "settings" | "epics") => void;
  projectPath: string | null;
  tasks: Task[];
  currentTaskId: string | null;
  epics: EpicDef[];
  onOpenTask: (id: string) => void;
  onOpenEpics: () => void;
  onNewTask: () => Promise<void> | void;
  onOpenSettings: () => void;
}) {
  return (
    <aside className="sidebar">
      <div className="brand">
        ⚡ Nerve <span className="sub">de la idea al código, con control</span>
      </div>
      <button className="primary block" onClick={props.onNewTask}>
        + Empezar algo nuevo
      </button>
      <button className={`block ${props.view === "epics" ? "active-btn" : ""}`} onClick={props.onOpenEpics}>
        🗺 Epic Mode{props.epics.length > 0 ? ` (${props.epics.length})` : ""}
      </button>
      <div className="list">
        {props.tasks.length === 0 && <div className="empty">Aún no hay nada aquí</div>}
        {props.tasks.map((t) => (
          <div
            key={t.id}
            className={`item ${props.view === "task" && props.currentTaskId === t.id ? "active" : ""}`}
            onClick={() => props.onOpenTask(t.id)}
          >
            <div className="item-title">{t.title}</div>
            <div className="item-sub">
              {humanStatus(TASK_STATUS, t.status)}
              {t.tickets.length > 0 && ` · ${t.tickets.filter((x) => x.status === "done").length}/${t.tickets.length}`}
            </div>
          </div>
        ))}
      </div>
      <div className="foot">
        <div className="mono small">{props.projectPath}</div>
        <button className={props.view === "settings" ? "active-btn" : ""} onClick={props.onOpenSettings}>⚙ Ajustes</button>
      </div>
    </aside>
  );
}

// ---------- home ----------

function Home(props: {
  tasks: Task[];
  onOpenTask: (id: string) => void;
  projectPath: string | null;
}) {
  return (
    <div className="page">
      <div className="hero">
        <h1>¿Qué quieres construir hoy?</h1>
        <p className="hero-sub">
          Descríbelo en tus palabras. Nerve crea un plan, tú lo apruebas y los
          agentes lo construyen — con una copia de seguridad de tu proyecto en cada paso.
        </p>
      </div>
      <h2>Tus proyectos</h2>
      {props.tasks.length === 0 ? (
        <div className="empty-card">
          Aún no hay nada aquí. Pulsa <b>“+ Empezar algo nuevo”</b> para crear tu
          primera tarea. Puedes probarla con el <b>agente simulado</b> para ver el
          flujo completo sin gastar tokens.
        </div>
      ) : (
        <div className="grid">
          {props.tasks.map((t) => (
            <div key={t.id} className="card clickable" onClick={() => props.onOpenTask(t.id)}>
              <div className="card-title">{t.title}</div>
              <div className="card-sub">{t.intent.slice(0, 120)}</div>
              <div className="card-meta">
                <StatusDot status={t.status} map={TASK_STATUS} />
                <span className="dim">
                  {t.tickets.length === 0
                    ? "sin plan todavía"
                    : `${t.tickets.filter((x) => x.status === "done").length} de ${t.tickets.length} cambios listos`}
                  {" · "}
                  {fmtTime(t.updatedAt)}
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ---------- task view ----------

function TaskView(props: {
  task: Task;
  runs: Run[];
  currentRun: Run | null;
  events: RunEvent[];
  agents: AgentDef[];
  selectedAgent: string;
  setSelectedAgent: (a: string) => void;
  runMode: string;
  setRunMode: (m: string) => void;
  techMode: boolean;
  onTechMode: (v: boolean) => void;
  updateCurrent: (t: Task) => void;
  setError: (e: string) => void;
  openModal: (spec: ModalSpec) => void;
  openConfirm: (spec: ConfirmSpec) => void;
  refreshRuns: () => Promise<void>;
}) {
  const { task } = props;
  const [resumeSession, setResumeSession] = useState(false);
  const [yolo, setYolo] = useState(false);
  const [skills, setSkills] = useState<SkillDef[]>([]);
  const [selectedSkill, setSelectedSkill] = useState("plan");
  const [detail, setDetail] = useState<{ type: "ticket" | "run"; id: string } | null>(null);
  useEffect(() => {
    void (async () => {
      try {
        setSkills(await invoke<SkillDef[]>("list_skills"));
      } catch {
        /* noop */
      }
    })();
  }, []);
  const approvedPending = task.tickets.filter((t) => t.approved && t.status !== "done");
  const doneTickets = task.tickets.filter((t) => t.status === "done");
  const planned = task.tickets.length > 0 && !!task.specCurrent;
  const allDone = task.tickets.length > 0 && doneTickets.length === task.tickets.length;
  const stage = allDone ? 5 : approvedPending.length > 0 || task.status === "in_dev" ? 4 : planned ? 3 : 1;
  const [tab, setTab] = useState<"docs" | "chat" | "changes" | "runs" | "review">("chat");
  const tabRef = useRef(tab);
  tabRef.current = tab;
  // la vista sigue al usuario sin sacarlo del chat: run activo → Ejecuciones; comentarios abiertos → Revisión
  useEffect(() => {
    if (props.runs.some((r) => r.status === "running") && tabRef.current !== "runs" && tabRef.current !== "chat") setTab("runs");
  }, [props.runs]);
  useEffect(() => {
    const open = (task.reviewComments ?? []).filter((c) => !c.resolved);
    if (open.length > 0 && tabRef.current !== "review" && tabRef.current !== "chat" && props.runs.every((r) => r.status !== "running")) {
      setTab("review");
    }
  }, [task.reviewComments, props.runs]);
  const docsDone = (task.planArtifacts ?? []).length;
  const awaitingApproval = task.tickets.filter((t) => !t.approved && t.status !== "done").length;
  const runningRuns = props.runs.filter((r) => r.status === "running").length;
  const pendingDecision = props.runs.filter((r) => r.worktreePath && r.status === "done").length;
  const comments = task.reviewComments ?? [];
  const openComments = comments.filter((c) => !c.resolved);

  const nextStep: { text: string; label: string; tab: "docs" | "changes" | "runs" | "review" | "chat" } | null = (() => {
    if (runningRuns > 0) {
      return { text: "El agente está construyendo los cambios aprobados…", label: "Ver progreso", tab: "runs" };
    }
    if (task.tickets.length === 0 && docsDone < 4) {
      return {
        text: docsDone === 0
          ? "Empieza generando los documentos del plan (el agente puede preguntarte antes de escribir cada uno)."
          : `Van ${docsDone} de 4 documentos. Genera el siguiente para avanzar.`,
        label: "Continuar con los documentos",
        tab: "docs",
      };
    }
    if (task.tickets.length > 0 && awaitingApproval > 0) {
      return {
        text: awaitingApproval === task.tickets.length
          ? "El plan está listo: revisa los cambios propuestos y aprueba los que quieras construir."
          : `Quedan ${awaitingApproval} cambios por aprobar.`,
        label: "Ir a Cambios",
        tab: "changes",
      };
    }
    if (pendingDecision > 0) {
      return {
        text: "Hay ejecuciones terminadas con cambios aún sin decidir (aplicar o descartar).",
        label: "Ir a Ejecuciones",
        tab: "runs",
      };
    }
    if (openComments.length > 0) {
      return { text: "La verificación encontró puntos a mejorar.", label: "Ir a Revisión", tab: "review" };
    }
    return null;
  })();
  const resolvedComments = comments.filter((c) => c.resolved);
  const lastExecDone = [...props.runs].reverse().find((r) => r.mode !== "plan" && r.status === "done");
  // resume disponible si el agente elegido ya corrió con éxito en esta task
  const lastRunSameAgent = props.runs.find((r) => r.agent === props.selectedAgent && r.status === "done" && r.sessionId);
  const canResume = (props.selectedAgent === "qwen" || props.selectedAgent === "claude") && !!lastRunSameAgent;
  const wantResume = canResume && resumeSession;

  const runPlan = async () => {
    try {
      await invoke("start_plan_run", { taskId: task.id, agentId: props.selectedAgent, mode: "workspace", resumeSession: wantResume, skillId: selectedSkill });
      await props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    }
  };

  const runExec = async () => {
    try {
      await invoke("start_exec_run", { taskId: task.id, agentId: props.selectedAgent, mode: props.runMode, resumeSession: wantResume, yolo });
      await props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    }
  };

  const saveTicket = async (tk: Ticket) => {
    const t = await invoke<Task>("save_ticket", { taskId: task.id, ticket: tk });
    props.updateCurrent(t);
  };

  const deleteTicket = async (id: string) => {
    const t = await invoke<Task>("delete_ticket", { taskId: task.id, ticketId: id });
    props.updateCurrent(t);
  };

  const addTicket = async () => {
    props.openModal({
      title: "Nuevo ticket",
      fields: [
        { key: "id", label: "Id del ticket (ej. T3)", required: true },
        { key: "title", label: "Título", required: true },
        { key: "description", label: "Descripción", multiline: true },
      ],
      submitLabel: "Añadir",
      onSubmit: async (values) => {
        try {
          await saveTicket({
            id: values.id.trim(),
            title: values.title.trim(),
            description: values.description ?? "",
            acceptance: [],
            verifyCommand: null,
            dependsOn: [],
            status: "todo",
            approved: false,
          });
        } catch (e) {
          props.setError(String(e));
        }
      },
    });
  };

  const verifyRun = async () => {
    if (!lastExecDone) return;
    try {
      await invoke("start_verify_run", { taskId: task.id, runId: lastExecDone.id, agentId: props.selectedAgent });
      await props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    }
  };

  const fixAll = async () => {
    if (!lastExecDone) return;
    try {
      await invoke("fix_comments", { taskId: task.id, mode: props.runMode, agentId: props.selectedAgent, targetRunId: lastExecDone.id });
      await props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    }
  };

  const resolveOne = async (commentId: string) => {
    try {
      const t = await invoke<Task>("resolve_comment", { taskId: task.id, commentId });
      props.updateCurrent(t);
    } catch (e) {
      props.setError(String(e));
    }
  };

  return (
    <div className="page taskview">
      <div className="taskhead">
        <h1>{task.title}</h1>
        <StatusDot status={task.status} map={TASK_STATUS} />
        {props.techMode && <span className="mono small">{task.status}</span>}
        <span style={{ flex: 1 }} />
        <label className="tech-toggle" title="Muestra detalles para desarrolladores: ids, sesiones, rutas, comandos">
          <input
            type="checkbox"
            checked={props.techMode}
            onChange={(e) => props.onTechMode(e.target.checked)}
          />
          Modo técnico
        </label>
        <button className="ghost" onClick={() => {
          props.openModal({
            title: "Editar task",
            fields: [
              { key: "title", label: "Título", required: true, initial: task.title },
              { key: "intent", label: "Intención", multiline: true, initial: task.intent },
            ],
            onSubmit: async (values) => {
              try {
                const t = await invoke<Task>("update_task", {
                  task: { ...task, title: values.title.trim(), intent: values.intent },
                });
                props.updateCurrent(t);
              } catch (e) {
                props.setError(String(e));
              }
            },
          });
        }}>✎</button>
      </div>
      {props.techMode ? (
        <p className="hint mono">{task.intent}</p>
      ) : (
        <p className="task-intent">{task.intent}</p>
      )}

      <Stepper stage={stage} onGo={(s) => {
        setTab(s <= 2 ? "docs" : s === 3 ? "changes" : "runs");
        if (s === 1) document.querySelector<HTMLInputElement>(".new-task-inline")?.focus();
        if (s >= 3) document.querySelector<HTMLElement>(".approval-anchor")?.scrollIntoView({ behavior: "smooth", block: "start" });
      }} />

      {props.techMode && (
        <div className="toolbar">
          <select value={props.selectedAgent} onChange={(e) => props.setSelectedAgent(e.target.value)}>
            {props.agents.map((a) => (
              <option key={a.id} value={a.id}>{a.label}</option>
            ))}
          </select>
          <select value={props.runMode} onChange={(e) => props.setRunMode(e.target.value)}>
            <option value="worktree">Ejecutar en worktree nuevo (aislado)</option>
            <option value="workspace">Ejecutar en workspace (directo)</option>
          </select>
          {skills.length > 0 && (
            <select
              value={selectedSkill}
              onChange={(e) => setSelectedSkill(e.target.value)}
              title="Skill para el planificador"
            >
              {skills.map((s) => (
                <option key={s.id} value={s.id}>{s.label}</option>
              ))}
            </select>
          )}
          <button className="primary" onClick={runPlan}>🧠 Generar spec y plan</button>
          {canResume && (
            <label className="mono small" style={{ display: "flex", alignItems: "center", gap: 6, cursor: "pointer" }}>
              <input
                type="checkbox"
                checked={resumeSession}
                onChange={(e) => setResumeSession(e.target.checked)}
              />
              ↩ continuar sesión ({lastRunSameAgent!.sessionId!.slice(0, 8)})
            </label>
          )}
          <button
            className="primary"
            onClick={runExec}
            disabled={approvedPending.length === 0}
            title={approvedPending.length === 0 ? "Aprueba al menos un ticket" : ""}
          >
            ▶ Ejecutar tickets aprobados ({approvedPending.length})
          </button>
          <label
            className="tech-toggle"
            title="YOLO: lanza la ejecución aprobando automáticamente todos los tickets pendientes, sin revisarlos uno a uno"
            style={{ display: "flex", alignItems: "center", gap: 6, cursor: "pointer" }}
          >
            <input
              type="checkbox"
              checked={yolo}
              onChange={(e) => setYolo(e.target.checked)}
            />
            ⚡ YOLO
          </label>
        </div>
      )}

      <div className="task-body">
        <main className="task-chat">
          <ChatPanel
            task={task}
            selectedAgent={props.selectedAgent}
            agents={props.agents}
            busy={props.runs.some((r) => r.status === "running")}
            techMode={props.techMode}
            runs={props.runs}
            onGoTab={setTab}
            setSelectedAgent={props.setSelectedAgent}
            setError={props.setError}
            updateCurrent={props.updateCurrent}
            refreshRuns={props.refreshRuns}
            openConfirm={props.openConfirm}
          />
        </main>
        <aside className="task-side">
      {nextStep && (
        <div className="next-strip">
          <span className="ns-arrow" aria-hidden="true">→</span>
          <span className="ns-text">{nextStep.text}</span>
          <button className="ghost" onClick={() => setTab(nextStep.tab)}>{nextStep.label}</button>
        </div>
      )}

      <div className="tabs sidebar-tabs" role="tablist">
        <button role="tab" aria-selected={tab === "chat"} className={tab === "chat" ? "active" : ""} onClick={() => setTab("chat")}>
          💬 Chat
          {runningRuns > 0 && <span className="tab-badge live">{runningRuns} activa{runningRuns === 1 ? "" : "s"}</span>}
        </button>
        <button role="tab" aria-selected={tab === "docs"} className={tab === "docs" ? "active" : ""} onClick={() => setTab("docs")}>
          📄 Documentos
          {docsDone > 0 && <span className="tab-badge">{docsDone}/4</span>}
        </button>
        <button role="tab" aria-selected={tab === "changes"} className={tab === "changes" ? "active" : ""} onClick={() => setTab("changes")}>
          ✅ Cambios
          {task.tickets.length > 0 && (
            <span className="tab-badge">{awaitingApproval > 0 ? `${awaitingApproval} por aprobar` : `${doneTickets.length}/${task.tickets.length}`}</span>
          )}
        </button>
        <button role="tab" aria-selected={tab === "runs"} className={tab === "runs" ? "active" : ""} onClick={() => setTab("runs")}>
          ⚙️ Ejecuciones
          {runningRuns > 0 ? (
            <span className="tab-badge live">{runningRuns} activa{runningRuns === 1 ? "" : "s"}</span>
          ) : pendingDecision > 0 ? (
            <span className="tab-badge warn">{pendingDecision} por decidir</span>
          ) : null}
        </button>
        <button role="tab" aria-selected={tab === "review"} className={tab === "review" ? "active" : ""} onClick={() => setTab("review")}>
          🔍 Revisión
          {openComments.length > 0 && <span className="tab-badge warn">{openComments.length}</span>}
        </button>
      </div>

      {tab === "chat" && (
        <section className="col chat-activity">
          <h2>Actividad</h2>
          {runningRuns > 0 && props.runs.filter((r) => r.status === "running").map((r) => (
            <div key={r.id} className="chat-act-card running" onClick={() => setTab("runs")}>
              <StatusDot status="running" map={RUN_STATUS} />
              <span className="small flex1">{agentLabel(r.agent)} · {runKindLabel(r.mode, props.techMode)}</span>
              <span className="chev">▸</span>
            </div>
          ))}
          {pendingDecision > 0 && (
            <div className="chat-act-card pending" onClick={() => setTab("runs")}>
              <span className="chat-act-count">{pendingDecision}</span>
              <span className="small flex1">cambios listos en copia aislada: revísalos y decide si aplicarlos</span>
              <span className="chev">▸</span>
            </div>
          )}
          {runningRuns === 0 && pendingDecision === 0 && (
            <div className="empty-card">Todo tranquilo: sin ejecuciones activas ni cambios por decidir.</div>
          )}
        </section>
      )}

      {tab === "docs" && (
        <PlanArtifactsSection
          task={task}
          selectedAgent={props.selectedAgent}
          busy={props.runs.some((r) => r.status === "running")}
          techMode={props.techMode}
          openConfirm={props.openConfirm}
          setError={props.setError}
          updateCurrent={props.updateCurrent}
        />
      )}

      {tab === "changes" && <div className="approval-anchor" />}
      {tab === "changes" && !props.techMode && stage < 5 && (stage === 3 || stage === 4) && (
        <ApprovalHero
          count={approvedPending.length}
          total={task.tickets.length}
          tickets={task.tickets}
          onApprove={(tk) => saveTicket({ ...tk, approved: !tk.approved })}
          onRun={runExec}
          busy={props.runs.some((r) => r.status === "running")}
        />
      )}

      {tab === "changes" && (
        <section className="col">
          <h2>Tickets ({task.tickets.length})</h2>
          <div className="tickets">
            {task.tickets.map((tk) => {
              const open = detail?.type === "ticket" && detail.id === tk.id;
              return (
                <div
                  key={tk.id}
                  className={`ticket ${tk.status} ${open ? "open" : ""}`}
                  onClick={() => setDetail(open ? null : { type: "ticket", id: tk.id })}
                >
                  <div className="ticket-head">
                    <span className={`tstate st-${tk.status}`} />
                    {props.techMode && <strong>{tk.id}</strong>} {tk.title}
                    <span className="chip">{humanStatus(TICKET_STATUS, tk.status)}</span>
                    {tk.approved && <span className="chip chip-approved">aprobado</span>}
                    <span className="chev">{open ? "▾" : "▸"}</span>
                  </div>
                  {open && (
                    <div className="ticket-body" onClick={(e) => e.stopPropagation()}>
                      {tk.description && <p>{tk.description}</p>}
                      {tk.acceptance.length > 0 && (
                        <ul>{tk.acceptance.map((a, i) => <li key={i}>{a}</li>)}</ul>
                      )}
                      <div className="ticket-actions">
                        <button onClick={() => saveTicket({ ...tk, approved: !tk.approved })}>
                          {tk.approved ? "Desaprobar" : "Aprobar"}
                        </button>
                        <button onClick={() => saveTicket({ ...tk, status: tk.status === "done" ? "todo" : "done" })}>
                          {tk.status === "done" ? "Reabrir" : "Marcar hecho"}
                        </button>
                        <button
                          onClick={() => {
                            props.openModal({
                              title: `Editar ticket ${tk.id}`,
                              fields: [
                                { key: "title", label: "Título", required: true, initial: tk.title },
                                { key: "description", label: "Descripción", multiline: true, initial: tk.description },
                                { key: "verifyCommand", label: "Comando de verificación (opcional)", initial: tk.verifyCommand ?? "" },
                              ],
                              submitLabel: "Guardar",
                              onSubmit: async (values) => {
                                try {
                                  await saveTicket({
                                    ...tk,
                                    title: values.title.trim(),
                                    description: values.description ?? "",
                                    verifyCommand: (values.verifyCommand ?? "").trim() || null,
                                  });
                                } catch (e) {
                                  props.setError(String(e));
                                }
                              },
                            });
                          }}
                        >
                          Editar
                        </button>
                        <button className="danger" onClick={() => deleteTicket(tk.id)}>Eliminar</button>
                      </div>
                      {props.techMode && (
                        <div className="tech-box mono">
                          <div>estado: {tk.status}</div>
                          {tk.verifyCommand && <div>verify: {tk.verifyCommand}</div>}
                          {tk.dependsOn.length > 0 && <div>dependsOn: {tk.dependsOn.join(", ")}</div>}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
          <button className="block" onClick={addTicket}>+ Añadir ticket</button>
        </section>
      )}

      {tab === "review" && (
        <section className="col">
          <h2>Revisión {comments.length > 0 && `(${openComments.length} abiertos)`}</h2>
          {comments.length === 0 ? (
            <div className="empty-card">
              Sin comentarios de verificación. Tras una ejecución, pulsa “Verificar” para comparar el diff con el plan.
            </div>
          ) : (
            <div className="review-panel">
              {openComments.map((c) => (
                <div key={c.id} className={`review-item sev-${c.severity}`}>
                  <div className="review-head">
                    <span className={`chip chip-sev-${c.severity}`}>{c.severity}</span>
                    <strong>{c.title}</strong>
                    <span className="chev-spacer" />
                    <button
                      onClick={() => resolveOne(c.id)}
                      title="Marcar como resuelto"
                    >✓</button>
                  </div>
                  {c.file && <div className="mono small review-file">{c.file}</div>}
                  {c.detail && <p className="review-detail">{c.detail}</p>}
                </div>
              ))}
              {openComments.length === 0 && <div className="empty-card">Todos los comentarios están resueltos ✓</div>}
              {resolvedComments.length > 0 && (
                <details>
                  <summary>Resueltos ({resolvedComments.length})</summary>
                  <ul className="review-resolved">
                    {resolvedComments.map((c) => (
                      <li key={c.id}>
                        <span className={`chip chip-sev-${c.severity}`}>{c.severity}</span> {c.title}
                      </li>
                    ))}
                  </ul>
                </details>
              )}
            </div>
          )}
          <div className="review-actions">
            <button className="primary" onClick={verifyRun} disabled={!lastExecDone || props.runs.some((r) => r.status === "running")}>
              🔍 Verificar
            </button>
            {openComments.length > 0 && (
              <button className="primary" onClick={fixAll} disabled={props.runs.some((r) => r.status === "running")}>
                🛠 Corregir todos ({openComments.length})
              </button>
            )}
          </div>
        </section>
      )}

      {tab === "runs" && (
        <RunsPanel
          task={task}
          runs={props.runs}
          currentRun={props.currentRun}
          events={props.events}
          techMode={props.techMode}
          openDetail={(id) => setDetail({ type: "run", id })}
          openDetailRunId={detail?.type === "run" ? detail.id : null}
          refreshRuns={props.refreshRuns}
          openConfirm={props.openConfirm}
          setError={props.setError}
        />
      )}
        </aside>
      </div>
    </div>
  );
}

// ---------- chat con el agente ----------

function ChatPanel(props: {
  task: Task;
  selectedAgent: string;
  agents: { id: string; label: string }[];
  busy: boolean;
  techMode: boolean;
  runs: Run[];
  onGoTab: (t: "docs" | "chat" | "changes" | "runs" | "review") => void;
  setSelectedAgent: (id: string) => void;
  setError: (e: string) => void;
  updateCurrent: (t: Task) => void;
  refreshRuns: () => Promise<void>;
  openConfirm: (spec: ConfirmSpec) => void;
}) {
  const msgs = props.task.chatMessages ?? [];
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);
  const chatTab = props.onGoTab;
  useEffect(() => {
    endRef.current?.scrollTo(0, endRef.current.scrollHeight);
  }, [msgs.length, sending]);

  const send = async () => {
    const m = draft.trim();
    if (!m || sending || props.busy) return;
    setSending(true);
    setDraft("");
    try {
      await invoke("send_chat", { taskId: props.task.id, agentId: props.selectedAgent, message: m });
      await props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    } finally {
      setSending(false);
    }
  };

  // A2A: consulta a los agentes pares del agente seleccionado (solo lectura)
  const askTeam = async () => {
    const m = draft.trim();
    if (!m || sending || props.busy) return;
    setSending(true);
    setDraft("");
    try {
      await invoke("send_a2a", { taskId: props.task.id, agentId: props.selectedAgent, question: m });
      await props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    } finally {
      setSending(false);
    }
  };

  const clear = () => {
    invoke<Task>("clear_chat", { taskId: props.task.id })
      .then((t) => props.updateCurrent(t))
      .catch((e) => props.setError(String(e)));
  };

  const running = props.runs.filter((r) => r.status === "running");
  const decisions = props.runs.filter((r) => r.worktreePath && r.status === "done");
  const openComments = (props.task.reviewComments ?? []).filter((c) => !c.resolved);
  const worktreeId = (r: Run) => r.worktreePath?.split(/[\\/]/).pop() ?? "";
  const lastDoc = (props.task.planArtifacts ?? []).slice(-1)[0] ?? null;
  const lastDocIdx = msgs.length; // documentos y plan aparecen como tarjetas al final del hilo
  const planned = props.task.tickets.length > 0 && !!props.task.specCurrent;
  void lastDocIdx;

  const applyRun = (r: Run) => {
    props.openConfirm({
      title: "Aplicar los cambios",
      message: "Se aplicará lo construido sobre tu proyecto actual y se limpiará la copia aislada.",
      confirmLabel: "Aplicar cambios",
      onConfirm: async () => {
        try {
          await invoke("merge_worktree", { id: worktreeId(r) });
          await props.refreshRuns();
        } catch (e) {
          props.setError(String(e));
        }
      },
    });
  };
  const discardRun = (r: Run) => {
    props.openConfirm({
      title: "Descartar los cambios",
      message: "Se borrará la copia aislada donde se trabajó. Los cambios no aplicados se pierden.",
      confirmLabel: "Descartar",
      danger: true,
      onConfirm: async () => {
        try {
          await invoke("discard_worktree", { id: worktreeId(r) });
          await props.refreshRuns();
        } catch (e) {
          props.setError(String(e));
        }
      },
    });
  };

  // timeline: mensajes del chat + tarjetas de artefactos/plan/verificación/cambios intercaladas
  const timeline: { key: string; el: ReactNode }[] = msgs.map((m, i) => ({
    key: `m${i}`,
    el: (
      <div className={`chat-msg ${m.role}`}>
        <div className="chat-bubble">
          {m.role === "agent" && m.fromAgent && (
            <div className="chat-agent-name">{agentLabel(m.fromAgent)}</div>
          )}
          <div className="chat-text">{m.text}</div>
          <div className="chat-time">{fmtTime(m.createdAt)}</div>
        </div>
      </div>
    ),
  }));
  if (lastDoc) {
    const meta = ({ brief: { label: "Brief", icon: "🎯" }, architecture: { label: "Arquitectura", icon: "🏗" }, flows: { label: "Flujos", icon: "🔀" }, spec: { label: "Spec", icon: "📄" } } as Record<string, { label: string; icon: string }>)[lastDoc.kind] ?? { label: lastDoc.kind, icon: "📄" };
    timeline.push({
      key: `doc-${lastDoc.kind}`,
      el: (
        <div className="chat-card doc" onClick={() => chatTab("docs")}>
          <span className="chat-card-icon" aria-hidden="true">{meta.icon}</span>
          <span className="chat-card-body">
            <strong>{meta.label}</strong>
            <span className="chat-card-sub">{lastDoc.content.split("\n").length} líneas · {fmtTime(lastDoc.createdAt)}</span>
          </span>
          <span className="chat-card-action"><button className="ghost">📄 Leer</button></span>
        </div>
      ),
    });
  }
  if (planned) {
    timeline.push({
      key: "plan",
      el: (
        <div className="chat-card plan" onClick={() => chatTab("changes")}>
          <span className="chat-card-icon" aria-hidden="true">🧠</span>
          <span className="chat-card-body">
            <strong>Plan generado · {props.task.tickets.length} tickets</strong>
            <span className="chat-card-sub">revísalos y aprueba los que quieras construir</span>
          </span>
          <span className="chat-card-action"><button className="ghost">Ver tickets</button></span>
        </div>
      ),
    });
  }
  if (openComments.length > 0) {
    timeline.push({
      key: "review",
      el: (
        <div className="chat-card review" onClick={() => chatTab("review")}>
          <span className="chat-card-icon" aria-hidden="true">🔍</span>
          <span className="chat-card-body">
            <strong>Verificación</strong>
            <span className="chat-card-sub">{openComments.length} punto{openComments.length === 1 ? "" : "s"} a mejorar</span>
          </span>
          <span className="chat-card-action"><button className="ghost">Ver</button></span>
        </div>
      ),
    });
  }
  for (const r of decisions) {
    timeline.push({
      key: `run-${r.id}`,
      el: (
        <div className="chat-card decision">
          <span className="chat-card-icon" aria-hidden="true">📦</span>
          <span className="chat-card-body">
            <strong>Cambios listos{r.summary ? ` · ${r.summary.slice(0, 44)}` : ""}</strong>
            <span className="chat-card-sub mono">{worktreeId(r)}</span>
          </span>
          <span className="chat-card-actions">
            <button className="ghost" onClick={(e) => { e.stopPropagation(); discardRun(r); }}>Descartar</button>
            <button className="primary" onClick={(e) => { e.stopPropagation(); applyRun(r); }}>Aplicar</button>
          </span>
        </div>
      ),
    });
  }
  for (const r of running) {
    timeline.push({
      key: `run-${r.id}`,
      el: (
        <div className="chat-card running" onClick={() => chatTab("runs")}>
          <span className="chat-card-icon spin" aria-hidden="true">⏳</span>
          <span className="chat-card-body">
            <strong>Construyendo…</strong>
            <span className="chat-card-sub">{agentLabel(r.agent)} · {runKindLabel(r.mode, props.techMode)}</span>
          </span>
          <span className="chat-card-action"><button className="ghost">Ver</button></span>
        </div>
      ),
    });
  }

  return (
    <div className="chat-panel">
      <div className="chat-head">
        <span className="chat-title">💬 Conversación con el agente</span>
        <span className="small dim chat-hint">Solo lee tu proyecto; para construir, aprueba los cambios.</span>
        {msgs.length > 0 && (
          <button className="ghost" onClick={clear} title="Borrar la conversación">🗑</button>
        )}
      </div>
      <div className="chat-log" ref={endRef}>
        {timeline.length === 0 && (
          <div className="chat-empty">
            Conversa con el agente: pregúntale por el plan, pide alternativas o aclara qué quieres construir. Nada se modifica hasta que apruebes cambios.
          </div>
        )}
        {timeline.map((item) => <div key={item.key}>{item.el}</div>)}
        {sending && (
          <div className="chat-msg agent">
            <div className="chat-bubble typing">
              <span /><span /><span />
            </div>
          </div>
        )}
      </div>
      <div className="chat-compose">
        <textarea
          value={draft}
          placeholder="Pídele lo que quieras construir o pregúntale por el plan… (Enter para enviar)"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send();
            }
          }}
          rows={2}
        />
        <div className="chat-compose-row">
          <select
            value={props.selectedAgent}
            onChange={(e) => props.setSelectedAgent(e.target.value)}
            className="chat-agent"
            title="Agente que responde en el chat"
          >
            {props.agents.map((a) => (
              <option key={a.id} value={a.id}>{a.label}</option>
            ))}
          </select>
          <button
            className="primary"
            onClick={send}
            disabled={!draft.trim() || sending || props.busy}
            title={props.busy ? "Espera a que termine la ejecución en curso" : ""}
          >
            Enviar
          </button>
          <button
            className="ghost"
            onClick={askTeam}
            disabled={!draft.trim() || sending || props.busy}
            title="Consulta a los demás agentes del workspace y agrega su criterio al chat"
          >
            🤝 A2A
          </button>
        </div>
      </div>
      <div className="chat-status">
        <span className="cs-pill">{agentLabel(props.selectedAgent)}</span>
        <span className="cs-pill">{props.techMode ? "Modo técnico" : "Modo simple"}</span>
        <span className="cs-pill">{props.task.tickets.filter((t) => t.status === "done").length}/{props.task.tickets.length} hechos</span>
        <span className="cs-pill cs-right">{running.length > 0 ? `${running.length} ejecución${running.length === 1 ? "" : "es"} activa${running.length === 1 ? "" : "s"}` : props.busy ? "ocupado" : "listo"}</span>
      </div>
    </div>
  );
}

// ---------- stepper / aprobación ----------

const STEPS = [
  { n: 1, label: "Cuéntanos qué quieres" },
  { n: 2, label: "Revisa el plan" },
  { n: 3, label: "Aprueba" },
  { n: 4, label: "Se construye" },
  { n: 5, label: "Listo" },
];

function Stepper(props: { stage: number; onGo: (s: number) => void }) {
  return (
    <div className="stepper">
      {STEPS.map((s) => {
        const cls = props.stage > s.n ? "done" : props.stage === s.n ? "current" : "";
        return (
          <button key={s.n} className={`step ${cls}`} onClick={() => props.onGo(s.n)}>
            <span className="step-num">{props.stage > s.n ? "✓" : s.n}</span>
            {s.label}
          </button>
        );
      })}
    </div>
  );
}

function ApprovalHero(props: {
  count: number;
  total: number;
  tickets: Ticket[];
  onApprove: (tk: Ticket) => void;
  onRun: () => void;
  busy: boolean;
}) {
  const message =
    props.total === 0
      ? "Cuando el plan esté listo, verás aquí los cambios propuestos para aprobar."
      : props.count === 0
        ? "Marca los cambios que quieras construir:"
        : undefined;
  return (
    <div className="approval-hero">
      <div className="approval-head">
        <h3>¿Apruebas este plan?</h3>
        {props.count > 0 && (
          <button className="primary" onClick={props.onRun} disabled={props.busy}>
            Sí, construir {props.count} cambio{props.count === 1 ? "" : "s"}
          </button>
        )}
      </div>
      {message && <p className="modal-message">{message}</p>}
      {props.total > 0 && (
        <ul className="approval-list">
          {props.tickets.map((tk) => (
            <li key={tk.id} className={tk.approved ? "ok" : ""}>
              <button
                className="approve-box"
                title={tk.approved ? "Quitar aprobación" : "Aprobar este cambio"}
                onClick={() => props.onApprove(tk)}
              >
                {tk.approved ? "✓" : ""}
              </button>
              <span className="ap-title">{tk.title}</span>
              {tk.description && <span className="ap-desc">{tk.description}</span>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ---------- runs ----------

function RunsPanel(props: {
  task: Task;
  runs: Run[];
  currentRun: Run | null;
  events: RunEvent[];
  techMode: boolean;
  openDetail: (runId: string) => void;
  openDetailRunId: string | null;
  refreshRuns: () => Promise<void>;
  openConfirm: (spec: ConfirmSpec) => void;
  setError: (e: string) => void;
}) {
  const [diff, setDiff] = useState<DiffResult | null>(null);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    logRef.current?.scrollTo(0, logRef.current.scrollHeight);
  }, [props.events.length]);

  const viewDiff = async (run: Run) => {
    try {
      setDiff(await invoke<DiffResult>("get_diff", { taskId: run.taskId, runId: run.id }));
    } catch (e) {
      setDiff({ diff: String(e), files: [] });
    }
  };

  const worktreeId = (r: Run) => r.worktreePath?.split(/[\\/]/).pop() ?? "";
  const lastRun = props.currentRun ?? props.runs[0] ?? null;
  const liveEvents = lastRun ? props.events : [];
  const detailRun = props.openDetailRunId;

  return (
    <div>
      {props.runs.length === 0 && <div className="empty-card">Sin ejecuciones todavía.</div>}
      <div className="runs">
        {props.runs.map((r) => {
          const open = detailRun === r.id;
          return (
            <div
              key={r.id}
              className={`run-row ${open ? "open" : ""}`}
              onClick={() => props.openDetail(r.id)}
            >
              <StatusDot status={r.status} map={RUN_STATUS} />
              <span className="small">{agentLabel(r.agent)}</span>
              <span className="small dim">{runKindLabel(r.mode, props.techMode)}</span>
              {props.techMode && <span className="mono small dim">{r.id.slice(-4)}</span>}
              <span className="small dim">{fmtTime(r.startedAt)}</span>
              {r.summary && props.techMode && <span className="small dim flex1">{r.summary}</span>}
              {r.status === "running" && (
                <button
                  className="danger"
                  onClick={async (e) => {
                    e.stopPropagation();
                    await invoke("cancel_run", { runId: r.id });
                    await props.refreshRuns();
                  }}
                >
                  Detener
                </button>
              )}
              {(r.status === "failed" || r.status === "cancelled") && (
                <button
                  onClick={async (e) => {
                    e.stopPropagation();
                    try {
                      await invoke(r.mode === "plan" ? "start_plan_run" : "start_exec_run", {
                        taskId: r.taskId,
                        agentId: r.agent,
                        mode: r.mode === "plan" ? "worktree" : r.mode,
                      });
                      await props.refreshRuns();
                    } catch (err) {
                      props.setError(String(err));
                    }
                  }}
                >
                  Reintentar
                </button>
              )}
              {r.status === "done" && r.mode !== "plan" && (
                <button onClick={(e) => { e.stopPropagation(); viewDiff(r); }}>Ver cambios</button>
              )}
              {r.worktreePath && r.status === "done" && (
                <button
                  className="danger"
                  onClick={(e) => {
                    e.stopPropagation();
                    props.openConfirm({
                      title: "Descartar los cambios",
                      message: "Se borrará la copia aislada donde se trabajó. Los cambios no aplicados se pierden.",
                      confirmLabel: "Descartar",
                      danger: true,
                      onConfirm: async () => {
                        try {
                          await invoke("discard_worktree", { id: worktreeId(r) });
                          await props.refreshRuns();
                        } catch (e) {
                          props.setError(String(e));
                        }
                      },
                    });
                  }}
                >
                  Descartar
                </button>
              )}
              {r.worktreePath && r.status === "done" && (
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    props.openConfirm({
                      title: "Aplicar los cambios",
                      message: "Se aplicará lo construido sobre tu proyecto actual y se limpiará la copia aislada.",
                      confirmLabel: "Aplicar cambios",
                      onConfirm: async () => {
                        try {
                          await invoke("merge_worktree", { id: worktreeId(r) });
                          await props.refreshRuns();
                        } catch (e) {
                          props.setError(String(e));
                        }
                      },
                    });
                  }}
                >
                  Aplicar cambios
                </button>
              )}
              <span className="chev">{open ? "▾" : "▸"}</span>
            </div>
          );
        })}
      </div>

      {detailRun && (
        <RunDetail
          run={props.runs.find((r) => r.id === detailRun) ?? null}
          techMode={props.techMode}
          onClose={() => props.openDetail("")}
        />
      )}

      {liveEvents.length > 0 && (
        <div className="log" ref={logRef}>
          {liveEvents.map((ev, i) => (
            <div key={i} className={`log-line log-${ev.kind}`}>
              {props.techMode && <span className="mono">{ev.kind}</span>} {ev.text}
            </div>
          ))}
        </div>
      )}

      {diff && (
        <div className="diff-wrap">
          <h3>{props.techMode ? "Diff" : "Cambios realizados"} <button className="ghost" onClick={() => setDiff(null)}>✕</button></h3>
          {diff.files.length > 0 && (
            <table className="files">
              <tbody>
                {diff.files.map((f) => (
                  <tr key={f.path}>
                    <td className="mono">{f.path}</td>
                    <td>{props.techMode ? f.status : { A: "nuevo", M: "modificado", D: "eliminado" }[f.status] ?? f.status}</td>
                    <td className="add">+{f.additions}</td>
                    <td className="del">-{f.deletions}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <pre className="diff">{diff.diff || "(sin cambios)"}</pre>
        </div>
      )}
    </div>
  );
}

function RunDetail(props: { run: Run | null; techMode: boolean; onClose: () => void }) {
  if (!props.run) return null;
  const r = props.run;
  return (
    <div className="detail-panel">
      <div className="detail-head">
        <h3>{r.agent === "mock" ? "Ejecución" : `Ejecución con ${agentLabel(r.agent)}`}</h3>
        <button className="ghost" onClick={props.onClose}>✕</button>
      </div>
      <div className="detail-grid">
        <div><span className="k">Estado</span><span>{humanStatus(RUN_STATUS, r.status)}</span></div>
        <div><span className="k">Tipo</span><span>{r.mode === "plan" ? "Plan (solo lee tu proyecto)" : "Construcción (aplica los cambios)"}</span></div>
        <div><span className="k">Empezó</span><span>{fmtTime(r.startedAt)}</span></div>
        {r.finishedAt && <div><span className="k">Terminó</span><span>{fmtTime(r.finishedAt)}</span></div>}
        {r.summary && <div className="wide"><span className="k">Resumen</span><span>{r.summary}</span></div>}
        {props.techMode && (
          <>
            <div><span className="k">Run id</span><span className="mono">{r.id}</span></div>
            <div><span className="k">Modo</span><span className="mono">{r.mode}</span></div>
            {r.worktreePath && <div className="wide"><span className="k">Worktree</span><span className="mono">{r.worktreePath}</span></div>}
            {r.baseSha && <div><span className="k">SHA base</span><span className="mono">{r.baseSha.slice(0, 10)}</span></div>}
            {r.checkpointSha && <div><span className="k">Checkpoint</span><span className="mono">{r.checkpointSha.slice(0, 10)}</span></div>}
            {r.sessionId && <div><span className="k">Sesión</span><span className="mono">{r.sessionId}</span></div>}
          </>
        )}
      </div>
    </div>
  );
}

// ---------- ollama ----------

function OllamaSection() {
  const [url, setUrl] = useState("");
  const [model, setModel] = useState("");
  const [models, setModels] = useState<OllamaModelInfo[] | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    void (async () => {
      try {
        const cfg = await invoke<WorkspaceCfg>("get_workspace");
        setUrl(cfg.ollamaUrl || "http://localhost:11434");
        setModel(cfg.ollamaModel || "");
      } catch {
        /* noop */
      }
    })();
  }, []);

  const connect = async () => {
    setStatus("Conectando…");
    try {
      await invoke("set_ollama", { url, model });
      const list = await invoke<OllamaModelInfo[]>("list_ollama_models");
      setModels(list);
      setStatus(`Conectado — ${list.length} modelo(s) disponibles`);
    } catch (e) {
      setModels(null);
      setStatus(String(e));
    }
  };

  return (
    <div>
      <div className="row">
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="http://localhost:11434"
        />
        <button className="primary" onClick={connect}>Conectar</button>
      </div>
      {models && models.length > 0 && (
        <div className="row">
          <select value={model} onChange={(e) => setModel(e.target.value)}>
            <option value="">— elige un modelo —</option>
            {models.map((m) => (
              <option key={m.name} value={m.name}>
                {m.name}{m.supportsTools ? " ✔ tools" : " (sin tools)"}
              </option>
            ))}
          </select>
          <button
            className="primary"
            onClick={async () => {
              try {
                await invoke("set_ollama", { url, model });
                setStatus(`Guardado: ${model}`);
              } catch (e) {
                setStatus(String(e));
              }
            }}
          >
            Guardar modelo
          </button>
        </div>
      )}
      {models && models.length === 0 && (
        <div className="empty">No hay modelos instalados (usa `ollama pull &lt;modelo&gt;`).</div>
      )}
      {status && <div className="hint">{status}</div>}
    </div>
  );
}

// ---------- settings ----------

function SecuritySection() {
  const [maxSteps, setMaxSteps] = useState("");
  const [allowlist, setAllowlist] = useState("");
  const [agentsMd, setAgentsMd] = useState(true);
  const [execAgent, setExecAgent] = useState("");
  const [agentOpts, setAgentOpts] = useState<AgentDef[]>([]);
  const [msg, setMsg] = useState<string | null>(null);

  useEffect(() => {
    void (async () => {
      try {
        const cfg = await invoke<WorkspaceCfg>("get_workspace");
        setMaxSteps(cfg.maxSteps ? String(cfg.maxSteps) : "");
        setAllowlist((cfg.commandAllowlist ?? []).join("\n"));
        setAgentsMd(cfg.agentsMdEnabled !== false);
        setExecAgent(cfg.execAgent ?? "");
      } catch {
        /* noop */
      }
      try {
        setAgentOpts((await invoke<AgentsConfig>("list_agents")).agents.filter((a) => a.enabled && a.kind !== "disabled"));
      } catch {
        /* noop */
      }
    })();
  }, []);

  return (
    <div>
      <div className="row">
        <input
          value={maxSteps}
          onChange={(e) => setMaxSteps(e.target.value.replace(/[^0-9]/g, ""))}
          placeholder="Pasos máximos por run (vacío = sin límite)"
          style={{ width: 320 }}
        />
        <input
          value={allowlist}
          onChange={(e) => setAllowlist(e.target.value)}
          placeholder="Allowlist de comandos (uno por línea, vacío = libre)"
          style={{ width: 320 }}
        />
      </div>
      <div className="row" style={{ marginTop: 8, display: "flex", gap: 14, alignItems: "center", flexWrap: "wrap" }}>
        <label className="tech-toggle" title="Inyecta el AGENTS.md más cercano (subiendo hasta la raíz) en los prompts de plan y ejecución">
          <input
            type="checkbox"
            checked={agentsMd}
            onChange={(e) => setAgentsMd(e.target.checked)}
          />
          Usar AGENTS.md del proyecto
        </label>
        <label className="tech-toggle" title="Perfil por paso: el agente que construye puede ser distinto del que planifica">
          <span>Agente de ejecución (perfil):</span>
          <select
            value={execAgent}
            onChange={(e) => setExecAgent(e.target.value)}
            style={{ width: "auto", minWidth: 200 }}
          >
            <option value="">— el mismo que planifica —</option>
            {agentOpts.map((a) => (
              <option key={a.id} value={a.id}>{a.label}</option>
            ))}
          </select>
        </label>
      </div>
      <div className="row" style={{ marginTop: 8 }}>
        <button
          className="primary"
          onClick={async () => {
            try {
              await invoke("set_security", {
                maxSteps: Number(maxSteps) || 0,
                commandAllowlist: allowlist.split("\n").map((s) => s.trim()).filter(Boolean),
                agentsMdEnabled: agentsMd,
                execAgent: execAgent || null,
              });
              setMsg("Guardado");
            } catch (e) {
              setMsg(String(e));
            }
          }}
        >
          Guardar
        </button>
      </div>
      <p className="hint">
        El presupuesto aborta el run cuando el agente supera N pasos (uso de herramientas).
        La allowlist restringe los comandos que el agente puede ejecutar: cada línea es un
        prefijo permitido (p. ej. <code>node</code>, <code>npm</code>). Vacío = sin restricción.
        AGENTS.md da al agente el contexto del proyecto en plan y ejecución. El perfil de
        ejecución permite planificar con un agente y construir con otro.
      </p>
      {msg && <div className="hint">{msg}</div>}
    </div>
  );
}

// ---------- skills ----------

function SkillsSection() {
  const [skills, setSkills] = useState<SkillDef[]>([]);
  const [importPath, setImportPath] = useState("");
  const [msg, setMsg] = useState<string | null>(null);

  const reload = async () => {
    try {
      setSkills(await invoke<SkillDef[]>("list_skills"));
    } catch {
      /* noop */
    }
  };

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div>
      <table className="files">
        <thead>
          <tr><th>Skill</th><th>Descripción</th><th></th></tr>
        </thead>
        <tbody>
          {skills.map((s) => (
            <tr key={s.id}>
              <td>{s.label}</td>
              <td className="small dim">{s.description || "—"}</td>
              <td>
                {s.id.startsWith("user:") && (
                  <button
                    className="danger"
                    onClick={async () => {
                      try {
                        await invoke("delete_skill", { id: s.id });
                        await reload();
                      } catch (e) {
                        setMsg(String(e));
                      }
                    }}
                  >
                    Eliminar
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="row" style={{ marginTop: 8 }}>
        <input
          value={importPath}
          onChange={(e) => setImportPath(e.target.value)}
          placeholder="Ruta de un SKILL.md para importar (C:\\ruta\skill.md)"
          style={{ flex: 1 }}
        />
        <button
          className="primary"
          onClick={async () => {
            try {
              const id = await invoke<string>("import_skill", { path: importPath.trim() });
              setMsg(`Importada: ${id}`);
              setImportPath("");
              await reload();
            } catch (e) {
              setMsg(String(e));
            }
          }}
        >
          Importar SKILL.md
        </button>
      </div>
      <p className="hint">
        Las skills definen cómo el planificador interpreta tu intención. Elige una en la
        toolbar (modo técnico) al generar el plan. Importa SKILL.md de Claude/Cursor con su
        frontmatter (name, description) — el cuerpo se usa como plantilla del prompt.
      </p>
      {msg && <div className="hint">{msg}</div>}
    </div>
  );
}

function Settings(props: {
  projectPath: string | null;
  setProjectPath: (p: string | null) => void;
  refreshAgents: () => Promise<void>;
}) {
  const [path, setPath] = useState(props.projectPath ?? "");
  const [agents, setAgents] = useState<AgentsConfig>({ agents: [] });
  const [saveMsg, setSaveMsg] = useState<string | null>(null);

  useEffect(() => {
    void (async () => {
      try {
        setAgents(await invoke<AgentsConfig>("list_agents"));
      } catch {
        /* noop */
      }
    })();
  }, []);

  return (
    <div className="page">
      <h1>Ajustes</h1>
      <h2>Carpeta del proyecto</h2>
      <div className="row">
        <input value={path} onChange={(e) => setPath(e.target.value)} />
        <button
          className="primary"
          onClick={async () => {
            try {
              const cfg = await invoke<{ projectPath: string | null }>("set_workspace", { path });
              props.setProjectPath(cfg.projectPath);
              setSaveMsg(`Guardado: ${cfg.projectPath ?? path}`);
            } catch (e) {
              setSaveMsg(String(e));
            }
          }}
        >
          Guardar
        </button>
      </div>
      {saveMsg && <div className="hint">{saveMsg}</div>}

      <h2>Ollama (modelos locales)</h2>
      <p className="hint">
        Conéctate a tu servidor Ollama para usar sus modelos como agentes.
        Para ejecutar tickets necesitan soporte de <i>tools</i>.
      </p>
      <OllamaSection />

      <h2>Seguridad</h2>
      <SecuritySection />

      <h2>Skills</h2>
      <SkillsSection />

      <h2>Agentes</h2>
      <p className="hint">
        Qwen Code usa tu CLI local (`qwen`) en modo headless. El agente simulado
        no consume tokens. Los demás se activarán en fases posteriores.
      </p>
      <table className="files">
        <thead>
          <tr><th>Agente</th><th>Binario</th><th>Tipo</th><th>Habilitado</th></tr>
        </thead>
        <tbody>
          {agents.agents.map((a, i) => (
            <tr key={a.id}>
              <td>{a.label}</td>
              <td className="mono">{a.bin || "—"}</td>
              <td className="mono">{a.kind}</td>
              <td>
                <input
                  type="checkbox"
                  checked={a.enabled}
                  disabled={a.kind === "disabled"}
                  onChange={async (e) => {
                    const copy = { ...agents, agents: [...agents.agents] };
                    copy.agents[i] = { ...a, enabled: e.target.checked };
                    setAgents(copy);
                    await invoke("save_agents", { cfg: copy });
                    props.refreshAgents();
                  }}
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}