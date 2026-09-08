import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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

interface Task {
  id: string;
  title: string;
  intent: string;
  status: string;
  createdAt: number;
  updatedAt: number;
  specCurrent: string | null;
  specVersions: SpecVersion[];
  tickets: Ticket[];
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

type ModalSpec = {
  title: string;
  fields: FieldDef[];
  submitLabel?: string;
  onSubmit: (values: Record<string, string>) => void;
};

// ---------- helpers ----------

const fmtTime = (ms: number) =>
  new Date(ms).toLocaleString(undefined, {
    day: "2-digit",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });

function statusChip(s: string) {
  return <span className={`chip chip-${s}`}>{s}</span>;
}

// ---------- modal ----------

interface FieldDef {
  key: string;
  label: string;
  multiline?: boolean;
  initial?: string;
  required?: boolean;
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

  const submit = () => {
    const missing = props.fields.find((f) => f.required && !values[f.key]?.trim());
    if (missing) return;
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
                value={values[f.key] ?? ""}
                onChange={(e) => setValues((v) => ({ ...v, [f.key]: e.target.value }))}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
                }}
              />
            ) : (
              <input
                autoFocus={props.fields[0]?.key === f.key}
                value={values[f.key] ?? ""}
                onChange={(e) => setValues((v) => ({ ...v, [f.key]: e.target.value }))}
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
      </div>
    </div>
  );
}

// ---------- app ----------

export default function App() {
  const [view, setView] = useState<"home" | "task" | "settings">("home");
  const [projectPath, setProjectPath] = useState<string | null>(null);
  const [tasks, setTasks] = useState<Task[]>([]);
  const [current, setCurrent] = useState<Task | null>(null);
  const [runs, setRuns] = useState<Run[]>([]);
  const [currentRun, setCurrentRun] = useState<Run | null>(null);
  const [events, setEvents] = useState<RunEvent[]>([]);
  const [agents, setAgents] = useState<AgentDef[]>([]);
  const [selectedAgent, setSelectedAgent] = useState("mock");
  const [runMode, setRunMode] = useState("worktree");
  const [error, setError] = useState<string | null>(null);
  const [modal, setModal] = useState<ModalSpec | null>(null);

  const openModal = useCallback((spec: ModalSpec) => setModal(spec), []);

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
      setAgents(cfg.agents.filter((a) => a.enabled && a.kind !== "disabled"));
    } catch {
      setAgents([]);
    }
  }, []);

  const openTask = useCallback(async (id: string) => {
    setError(null);
    try {
      const t = await invoke<Task>("get_task", { id });
      setCurrent(t);
      const rs = await invoke<Run[]>("list_runs", { taskId: id });
      setRuns(rs);
      setEvents([]);
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
      });
      if (disposed) {
        [l1, l2, l3].forEach((l) => l());
        return;
      }
      unsubs.push(l1, l2, l3);
    })();
    return () => {
      disposed = true;
      unsubs.forEach((u) => u());
    };
  }, [openTask, refreshAgents, refreshTasks, refreshWorkspace]);

  useEffect(() => {
    void refreshWorkspace();
    void refreshTasks();
    void refreshAgents();
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
        onOpenTask={openTask}
        onNewTask={async () => {
          openModal({
            title: "Nueva task",
            fields: [
              { key: "title", label: "Título de la tarea", required: true },
              { key: "intent", label: "Intención (¿qué quieres lograr?)", multiline: true, required: true },
            ],
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
            updateCurrent={updateCurrent}
            setError={setError}
            openModal={openModal}
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
    </div>
  );
}

// ---------- onboarding ----------

function Onboarding({ onSaved }: { onSaved: (p: string) => void }) {
  const [path, setPath] = useState("");
  return (
    <div className="onboard">
      <h1>⚡ Nerve</h1>
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
              alert(String(e));
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

// ---------- sidebar ----------

function Sidebar(props: {
  view: string;
  setView: (v: "home" | "task" | "settings") => void;
  projectPath: string | null;
  tasks: Task[];
  onOpenTask: (id: string) => void;
  onNewTask: () => Promise<void> | void;
  onOpenSettings: () => void;
}) {
  return (
    <aside className="sidebar">
      <div className="brand">
        ⚡ Nerve <span className="sub">spec-first workspace</span>
      </div>
      <button className="primary block" onClick={props.onNewTask}>
        + Nueva task
      </button>
      <div className="list">
        {props.tasks.length === 0 && <div className="empty">Aún no hay tasks</div>}
        {props.tasks.map((t) => (
          <div
            key={t.id}
            className="item"
            onClick={() => props.onOpenTask(t.id)}
          >
            <div className="item-title">{t.title}</div>
            <div className="item-sub">
              {t.tickets.length} ticket(s) · {t.status}
            </div>
          </div>
        ))}
      </div>
      <div className="foot">
        <div className="mono small">{props.projectPath}</div>
        <button onClick={props.onOpenSettings}>⚙ Ajustes</button>
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
      <h1>Tus tasks</h1>
      <p className="hint">
        Cada task convierte una intención en spec + tickets, y se ejecuta aislada
        con checkpoints de git.
      </p>
      {props.tasks.length === 0 ? (
        <div className="empty-card">
          Crea tu primera task con “+ Nueva task”. Prueba con el
          <b> agente simulado</b> para ver el flujo completo sin consumir tokens.
        </div>
      ) : (
        <div className="grid">
          {props.tasks.map((t) => (
            <div key={t.id} className="card clickable" onClick={() => props.onOpenTask(t.id)}>
              <div className="card-title">{t.title}</div>
              <div className="card-sub">{t.intent.slice(0, 120)}</div>
              <div className="card-meta">
                {t.status} · {t.tickets.length} tickets · {fmtTime(t.updatedAt)}
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
  updateCurrent: (t: Task) => void;
  setError: (e: string) => void;
  openModal: (spec: ModalSpec) => void;
  refreshRuns: () => Promise<void>;
}) {
  const { task } = props;
  const approvedPending = task.tickets.filter((t) => t.approved && t.status !== "done");

  const runPlan = async () => {
    try {
      await invoke("start_plan_run", { taskId: task.id, agentId: props.selectedAgent, mode: "workspace" });
      props.refreshRuns();
    } catch (e) {
      props.setError(String(e));
    }
  };

  const runExec = async () => {
    try {
      await invoke("start_exec_run", { taskId: task.id, agentId: props.selectedAgent, mode: props.runMode });
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

  return (
    <div className="page taskview">
      <div className="taskhead">
        <h1>{task.title}</h1>
        {statusChip(task.status)}
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
      <p className="hint mono">{task.intent}</p>

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
        <button className="primary" onClick={runPlan}>🧠 Generar spec y plan</button>
        <button
          className="primary"
          onClick={runExec}
          disabled={approvedPending.length === 0}
          title={approvedPending.length === 0 ? "Aprueba al menos un ticket" : ""}
        >
          ▶ Ejecutar tickets aprobados ({approvedPending.length})
        </button>
      </div>

      <div className="cols">
        <section className="col">
          <h2>Spec</h2>
          {task.specCurrent ? (
            <>
              <pre className="spec">{task.specCurrent}</pre>
              <details>
                <summary>Historial ({task.specVersions.length} versiones)</summary>
                <ul>
                  {task.specVersions.map((v) => (
                    <li key={v.version}>v{v.version} — {fmtTime(v.createdAt)}</li>
                  ))}
                </ul>
              </details>
            </>
          ) : (
            <div className="empty-card">Aún no hay spec. Usa “Generar spec y plan”.</div>
          )}

          <h2>Tickets ({task.tickets.length})</h2>
          <div className="tickets">
            {task.tickets.map((tk) => (
              <div key={tk.id} className={`ticket ${tk.status}`}>
                <div className="ticket-head">
                  <strong>{tk.id}</strong> {tk.title}
                  <span className="chip">{tk.status}</span>
                  {tk.approved && <span className="chip chip-approved">aprobado</span>}
                </div>
                {tk.description && <p>{tk.description}</p>}
                {tk.acceptance.length > 0 && (
                  <ul>{tk.acceptance.map((a, i) => <li key={i}>{a}</li>)}</ul>
                )}
                {tk.verifyCommand && <div className="mono small">✔ {tk.verifyCommand}</div>}
                <div className="ticket-actions">
                  <button onClick={() => saveTicket({ ...tk, approved: !tk.approved })}>
                    {tk.approved ? "Desaprobar" : "Aprobar"}
                  </button>
                  <button onClick={() => saveTicket({ ...tk, status: "done" })} disabled={tk.status === "done"}>
                    Marcar done
                  </button>
                  <button className="danger" onClick={() => deleteTicket(tk.id)}>Eliminar</button>
                </div>
              </div>
            ))}
          </div>
          <button className="block" onClick={addTicket}>+ Añadir ticket</button>
        </section>

        <section className="col">
          <h2>Ejecuciones</h2>
          <RunsPanel
            task={task}
            runs={props.runs}
            currentRun={props.currentRun}
            events={props.events}
            refreshRuns={props.refreshRuns}
          />
        </section>
      </div>
    </div>
  );
}

// ---------- runs ----------

function RunsPanel(props: {
  task: Task;
  runs: Run[];
  currentRun: Run | null;
  events: RunEvent[];
  refreshRuns: () => Promise<void>;
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

  return (
    <div>
      {props.runs.length === 0 && <div className="empty-card">Sin ejecuciones todavía.</div>}
      <div className="runs">
        {props.runs.map((r) => (
          <div key={r.id} className="run-row">
            <span className={`chip chip-${r.status}`}>{r.status}</span>
            <span className="mono small">{r.agent}</span>
            <span className="mono small">{r.mode}</span>
            <span className="small">{fmtTime(r.startedAt)}</span>
            {r.status === "running" && (
              <button
                className="danger"
                onClick={async () => {
                  await invoke("cancel_run", { runId: r.id });
                  props.refreshRuns();
                }}
              >
                Cancelar
              </button>
            )}
            {r.status === "done" && r.mode !== "plan" && (
              <button onClick={() => viewDiff(r)}>Ver diff</button>
            )}
            {r.worktreePath && r.status === "done" && (
              <button
                className="danger"
                onClick={async () => {
                  if (window.confirm("¿Descartar el worktree (los cambios se pierden)?")) {
                    await invoke("discard_worktree", { id: worktreeId(r) });
                    props.refreshRuns();
                  }
                }}
              >
                Descartar
              </button>
            )}
            {r.worktreePath && r.status === "done" && (
              <button
                onClick={async () => {
                  if (window.confirm("¿Fusionar el worktree en tu rama actual?")) {
                    try {
                      await invoke("merge_worktree", { id: worktreeId(r) });
                      props.refreshRuns();
                    } catch (e) {
                      alert(String(e));
                    }
                  }
                }}
              >
                Fusionar
              </button>
            )}
          </div>
        ))}
      </div>

      {lastRun && lastRun.status === "running" && (
        <div className="log" ref={logRef}>
          {liveEvents.length === 0 && <div className="empty">Esperando eventos…</div>}
          {liveEvents.map((ev, i) => (
            <div key={i} className={`log-line log-${ev.kind}`}>
              <span className="mono">{ev.kind}</span> {ev.text}
            </div>
          ))}
        </div>
      )}

      {diff && (
        <div className="diff-wrap">
          <h3>Diff <button className="ghost" onClick={() => setDiff(null)}>✕</button></h3>
          {diff.files.length > 0 && (
            <table className="files">
              <tbody>
                {diff.files.map((f) => (
                  <tr key={f.path}>
                    <td className="mono">{f.path}</td>
                    <td>{f.status}</td>
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

// ---------- settings ----------

function Settings(props: {
  projectPath: string | null;
  setProjectPath: (p: string | null) => void;
  refreshAgents: () => Promise<void>;
}) {
  const [path, setPath] = useState(props.projectPath ?? "");
  const [agents, setAgents] = useState<AgentsConfig>({ agents: [] });

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
            } catch (e) {
              alert(String(e));
            }
          }}
        >
          Guardar
        </button>
      </div>

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