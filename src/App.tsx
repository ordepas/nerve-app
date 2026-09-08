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
  kind: string; // brief | architecture | flows
  content: string;
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
  tickets: Ticket[];
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

function SpecArtifact(props: { task: Task }) {
  const { task } = props;
  const [copied, setCopied] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  if (!task.specCurrent) {
    return <div className="empty-card">Aún no hay spec. Usa “Generar spec y plan”.</div>;
  }
  const versions = task.specVersions;
  const download = () => {
    const blob = new Blob([task.specCurrent!], { type: "text/markdown" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "spec.md";
    a.click();
    URL.revokeObjectURL(url);
  };
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(task.specCurrent!);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      /* noop */
    }
  };
  return (
    <div className="artifact">
      <div className="artifact-head">
        <span className="artifact-icon" aria-hidden="true">📄</span>
        <div className="artifact-title">
          <strong>spec.md</strong>
          <span className="artifact-sub">
            {versions.length > 0 ? `v${versions[versions.length - 1].version}` : "v1"} ·{" "}
            {task.specCurrent!.split("\n").length} líneas · {fmtTime(task.updatedAt)}
          </span>
        </div>
        <div className="artifact-actions">
          {versions.length > 1 && (
            <button className="ghost" onClick={() => setShowHistory(!showHistory)}>
              {showHistory ? "Ocultar historial" : `Historial (${versions.length})`}
            </button>
          )}
          <button className="ghost" onClick={copy}>{copied ? "✓ Copiado" : "Copiar"}</button>
          <button className="ghost" onClick={download}>Descargar</button>
        </div>
      </div>
      {showHistory && versions.length > 1 && (
        <ul className="artifact-history">
          {versions.map((v) => (
            <li key={v.version}>
              <span className="mono">v{v.version}</span> — {fmtTime(v.createdAt)}
              {v.version === versions[versions.length - 1].version && (
                <span className="chip chip-approved">actual</span>
              )}
            </li>
          ))}
        </ul>
      )}
      <div
        className="artifact-body md"
        dangerouslySetInnerHTML={{ __html: renderMarkdown(task.specCurrent!) }}
      />
    </div>
  );
}

// ---------- modal ----------

interface FieldDef {
  key: string;
  label: string;
  multiline?: boolean;
  initial?: string;
  required?: boolean;
}

// tarjeta de documento de planificación (brief / arquitectura / flujos)
function DocArtifact(props: {
  icon: ReactNode;
  title: string;
  content: string;
  createdAt: number;
  busy?: boolean;
  onDelete?: () => void;
  children?: ReactNode;
}) {
  const [copied, setCopied] = useState(false);
  const [open, setOpen] = useState(true);
  const download = () => {
    const blob = new Blob([props.content], { type: "text/markdown" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = props.title;
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
    <div className="artifact">
      <div className="artifact-head">
        <span className="artifact-icon" aria-hidden="true">{props.icon}</span>
        <div className="artifact-title">
          <strong>{props.title}</strong>
          <span className="artifact-sub">
            {props.content.split("\n").length} líneas · {fmtTime(props.createdAt)}
          </span>
        </div>
        <div className="artifact-actions">
          {props.onDelete && (
            <button className="ghost" onClick={props.onDelete} title="Eliminar este documento">🗑</button>
          )}
          <button className="ghost" onClick={() => setOpen(!open)}>{open ? "Contraer" : "Ver"}</button>
          <button className="ghost" onClick={copy}>{copied ? "✓ Copiado" : "Copiar"}</button>
          <button className="ghost" onClick={download}>Descargar</button>
        </div>
      </div>
      {open && (
        <div
          className="artifact-body md"
          dangerouslySetInnerHTML={{ __html: renderMarkdown(props.content) }}
        />
      )}
      {props.children}
    </div>
  );
}

// sección de artefactos de planificación: brief → arquitectura → flujos,
// con confirmación antes de generar cada uno (estilo Traycer)
function PlanArtifactsSection(props: {
  task: Task;
  selectedAgent: string;
  busy: boolean;
  openConfirm: (spec: ConfirmSpec) => void;
  setError: (e: string) => void;
  updateCurrent: (t: Task) => void;
}) {
  const { task } = props;
  const artifacts = task.planArtifacts ?? [];
  const running = props.busy;
  const byKind = (k: string) => artifacts.find((a) => a.kind === k);
  const brief = byKind("brief");
  const arch = byKind("architecture");
  const flows = byKind("flows");

  const askGenerate = (kind: string) => {
    const labels: Record<string, string> = {
      brief: "el brief",
      architecture: "el documento de arquitectura",
      flows: "el documento de flujos",
    };
    props.openConfirm({
      title: "¿Generar este documento?",
      message: `El agente va a leer tu proyecto (solo lectura) para escribir ${labels[kind]}. No modifica ningún archivo.`,
      confirmLabel: "Generar",
      onConfirm: async () => {
        try {
          await invoke("generate_doc", { taskId: task.id, agentId: props.selectedAgent, kind });
          // el estado de la task llega por task-updated al terminar el run
        } catch (e) {
          props.setError(String(e));
        }
      },
    });
  };

  const confirmDelete = (kind: string, label: string) => {
    props.openConfirm({
      title: "Eliminar documento",
      message: `Se eliminará ${label}. Puedes generarlo de nuevo cuando quieras.`,
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

  const nextLabel = !brief
    ? "1 · Brief"
    : !arch
      ? "2 · Arquitectura"
      : !flows
        ? "3 · Flujos"
        : null;
  const nextKind = !brief ? "brief" : !arch ? "architecture" : "flows";

  const stepChip = (kind: string, label: string, a?: PlanArtifact) => {
    if (a) {
      return (
        <span className="chip chip-approved" key={kind}>✓ {label}</span>
      );
    }
    return (
      <button key={kind} className="ghost" disabled={running} onClick={() => askGenerate(kind)}>
        {running ? "Esperando…" : `Generar ${label}`}
      </button>
    );
  };

  return (
    <div className="plan-docs">
      <div className="plan-docs-progress">
        {stepChip("brief", "Brief", brief)}
        {stepChip("architecture", "Arquitectura", arch)}
        {stepChip("flows", "Flujos", flows)}
        {running && <span className="small dim">Generando documento…</span>}
      </div>
      {nextLabel && !running && (
        <button className="primary block" onClick={() => askGenerate(nextKind)}>
          Generar {nextLabel}
        </button>
      )}
      {brief && (
        <DocArtifact
          icon="🎯"
          title="brief.md"
          content={brief.content}
          createdAt={brief.createdAt}
          onDelete={() => confirmDelete("brief", "el brief")}
        />
      )}
      {arch && (
        <DocArtifact
          icon="🏗"
          title="architecture.md"
          content={arch.content}
          createdAt={arch.createdAt}
          onDelete={() => confirmDelete("architecture", "el documento de arquitectura")}
        />
      )}
      {flows && (
        <DocArtifact
          icon="🔀"
          title="flows.md"
          content={flows.content}
          createdAt={flows.createdAt}
          onDelete={() => confirmDelete("flows", "el documento de flujos")}
        />
      )}
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
  const comments = task.reviewComments ?? [];
  const openComments = comments.filter((c) => !c.resolved);
  const resolvedComments = comments.filter((c) => c.resolved);
  const lastExecDone = [...props.runs].reverse().find((r) => r.mode !== "plan" && r.status === "done");
  // resume disponible si el agente elegido ya corrió con éxito en esta task
  const lastRunSameAgent = props.runs.find((r) => r.agent === props.selectedAgent && r.status === "done" && r.sessionId);
  const canResume = (props.selectedAgent === "qwen" || props.selectedAgent === "claude") && !!lastRunSameAgent;
  const wantResume = canResume && resumeSession;

  const runPlan = async () => {
    try {
      await invoke("start_plan_run", { taskId: task.id, agentId: props.selectedAgent, mode: "workspace", resumeSession: wantResume, skillId: selectedSkill });
      props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    }
  };

  const runExec = async () => {
    try {
      await invoke("start_exec_run", { taskId: task.id, agentId: props.selectedAgent, mode: props.runMode, resumeSession: wantResume, yolo });
      props.refreshRuns();
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
      props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    }
  };

  const fixAll = async () => {
    if (!lastExecDone) return;
    try {
      await invoke("fix_comments", { taskId: task.id, mode: props.runMode, agentId: props.selectedAgent, targetRunId: lastExecDone.id });
      props.refreshRuns();
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
        if (s === 1) document.querySelector<HTMLInputElement>(".new-task-inline")?.focus();
        if (s === 2) document.querySelector<HTMLElement>(".spec-anchor")?.scrollIntoView({ behavior: "smooth", block: "start" });
        if (s >= 3) document.querySelector<HTMLElement>(".approval-anchor")?.scrollIntoView({ behavior: "smooth", block: "start" });
      }} />

      <PlanArtifactsSection
        task={task}
        selectedAgent={props.selectedAgent}
        busy={props.runs.some((r) => r.status === "running")}
        openConfirm={props.openConfirm}
        setError={props.setError}
        updateCurrent={props.updateCurrent}
      />

      {stage < 5 && !props.techMode && (
        <div className="approval-anchor" />
      )}
      {!props.techMode && stage < 5 && (stage === 3 || stage === 4) && (
        <ApprovalHero
          count={approvedPending.length}
          total={task.tickets.length}
          tickets={task.tickets}
          onApprove={(tk) => saveTicket({ ...tk, approved: !tk.approved })}
          onRun={runExec}
          busy={props.runs.some((r) => r.status === "running")}
        />
      )}

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

      <div className="cols">
        <section className="col">
          <h2 className="spec-anchor">Spec</h2>
          <SpecArtifact task={task} />

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
                    <strong>{tk.id}</strong> {tk.title}
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

          <h2>Ejecuciones</h2>
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
        </section>
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
              <span className="small dim">{fmtTime(r.startedAt)}</span>
              {r.summary && props.techMode && <span className="small dim flex1">{r.summary}</span>}
              {r.status === "running" && (
                <button
                  className="danger"
                  onClick={async (e) => {
                    e.stopPropagation();
                    await invoke("cancel_run", { runId: r.id });
                    props.refreshRuns();
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