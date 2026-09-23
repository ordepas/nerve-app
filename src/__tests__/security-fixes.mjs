/**
 * Tests para validar los fixes de seguridad y correcciones mayores.
 * Ejecutar con: node --test src/__tests__/security-fixes.mjs
 *
 * Simulan la lógica de los fixes aplicados en el backend Rust.
 */

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// ---- Fix 2: sanitización de bin en which_exists (lib.rs) ----
describe("which_exists bin sanitization", () => {
  const isBinSafe = (bin) => {
    if (bin.length === 0) return false;
    return /^[a-zA-Z0-9._/\-]+$/.test(bin);
  };

  it("acepta binarios válidos", () => {
    assert.equal(isBinSafe("qwen"), true);
    assert.equal(isBinSafe("claude"), true);
    assert.equal(isBinSafe("node"), true);
    assert.equal(isBinSafe("my-tool"), true);
    assert.equal(isBinSafe("my_tool"), true);
    assert.equal(isBinSafe("tool.v2"), true);
    assert.equal(isBinSafe("/usr/bin/node"), true);
    assert.equal(isBinSafe("./local-bin"), true);
  });

  it("rechaza inyección de shell", () => {
    assert.equal(isBinSafe(""), false);
    assert.equal(isBinSafe("; rm -rf /"), false);
    assert.equal(isBinSafe("qwen; cat /etc/passwd"), false);
    assert.equal(isBinSafe("tool$(whoami)"), false);
    assert.equal(isBinSafe("tool`id`"), false);
    assert.equal(isBinSafe("tool|nc"), false);
    assert.equal(isBinSafe("tool&&curl"), false);
    assert.equal(isBinSafe("tool\nmalicious"), false);
    assert.equal(isBinSafe("tool\tmalicious"), false);
  });
});

// ---- Fix 3: command_allowed allowlist (runner.rs) ----
describe("command_allowed allowlist", () => {
  const commandAllowed = (command, allowlist) => {
    if (allowlist.length === 0) return true;
    const cmd = command.trimStart();
    let candidate = cmd.split(/\s+/)[0] || "";
    const lower = candidate.toLowerCase();
    if (lower === "cd") {
      const pos = cmd.toLowerCase().indexOf("&&");
      if (pos !== -1) {
        candidate = cmd.slice(pos + 2).trimStart().split(/\s+/)[0] || "";
      }
    }
    candidate = candidate.replace(/^["']|["']$/g, "");
    return allowlist.some((a) => {
      const cleanA = a.trim().replace(/^["']|["']$/g, "");
      return (
        cleanA.length > 0 &&
        (candidate.toLowerCase() === cleanA.toLowerCase() ||
          candidate.toLowerCase().startsWith(cleanA.toLowerCase()))
      );
    });
  };

  it("allowlist vacía permite todo", () => {
    assert.equal(commandAllowed("rm -rf /", []), true);
    assert.equal(commandAllowed("npm test", []), true);
  });

  it("comandos permitidos pasan", () => {
    assert.equal(commandAllowed("npm test", ["npm", "node"]), true);
    assert.equal(commandAllowed("node script.js", ["npm", "node"]), true);
    assert.equal(commandAllowed("npm run build --prod", ["npm"]), true);
  });

  it("comandos no permitidos se bloquean", () => {
    assert.equal(commandAllowed("rm -rf /", ["npm", "node"]), false);
    assert.equal(commandAllowed("curl http://evil.com", ["npm"]), false);
    assert.equal(commandAllowed("cat /etc/passwd", ["npm", "node"]), false);
  });

  it("soporta prefijo cd &&", () => {
    assert.equal(commandAllowed("cd src && npm test", ["npm"]), true);
    assert.equal(commandAllowed("cd src && rm -rf /", ["npm"]), false);
  });
});

// ---- Fix 8: new_id no colisiona (store.rs) ----
describe("new_id uniqueness", () => {
  const newId = (prefix) => {
    const n = BigInt(Date.now()) * 1000000n;
    const r = Math.floor(Math.random() * 0xffffffffffffffff);
    return `${prefix}_${n}_${r.toString(16)}`;
  };

  it("genera IDs únicos en rápida sucesión", () => {
    const ids = new Set();
    for (let i = 0; i < 1000; i++) {
      ids.add(newId("run"));
    }
    assert.equal(ids.size, 1000);
  });

  it("el formato incluye los 3 componentes", () => {
    const id = newId("task");
    const parts = id.split("_");
    assert.equal(parts.length, 3);
    assert.equal(parts[0], "task");
    assert.match(parts[1], /^\d+$/);
    assert.match(parts[2], /^[0-9a-f]+$/);
  });
});

// ---- Fix 5: @keyframes pulse separados (styles.css) ----
describe("CSS keyframes pulse separation", () => {
  const css = fs.readFileSync(path.resolve(__dirname, "../styles.css"), "utf-8");

  it("pulse-dot existe como keyframe separado", () => {
    assert.ok(css.includes("@keyframes pulse-dot"));
  });

  it("pulse existe como keyframe separado (no pulse-dot)", () => {
    const pulseMatch = css.match(/@keyframes pulse\b(?!-)/);
    assert.ok(pulseMatch !== null);
  });

  it("los status dots usan pulse-dot, no pulse genérico", () => {
    assert.match(css, /\.st-in_dev\s+\.dot\s*\{[^}]*animation:\s*pulse-dot/);
    assert.match(css, /\.st-running\s+\.dot\s*\{[^}]*animation:\s*pulse-dot/);
  });

  it("los elementos animados usan pulse genérico", () => {
    assert.match(css, /\.tab-badge\.live\s*\{[^}]*animation:\s*pulse\b/);
    assert.match(css, /\.chat-bubble\.typing\s+span\s*\{[^}]*animation:\s*pulse\b/);
  });
});

// ---- Fix 1: CSP en tauri.conf.json ----
describe("CSP configuration", () => {
  const conf = JSON.parse(
    fs.readFileSync(path.resolve(__dirname, "../../src-tauri/tauri.conf.json"), "utf-8")
  );

  it("CSP no es null", () => {
    assert.notEqual(conf.app.security.csp, null);
    assert.equal(typeof conf.app.security.csp, "string");
  });

  it("CSP restringe script-src a self", () => {
    assert.ok(conf.app.security.csp.includes("script-src 'self'"));
  });

  it("CSP no permite unsafe-eval", () => {
    assert.ok(!conf.app.security.csp.includes("unsafe-eval"));
  });

  it("CSP permite connect-src para Tauri IPC y Ollama", () => {
    assert.ok(conf.app.security.csp.includes("ipc:"));
    assert.ok(conf.app.security.csp.includes("http://localhost:11434"));
  });
});

// ---- Fix 7: refreshRuns siempre con await (App.tsx) ----
describe("refreshRuns always awaited", () => {
  const app = fs.readFileSync(path.resolve(__dirname, "../App.tsx"), "utf-8");

  it("no hay refreshRuns() sin await", () => {
    const lines = app.split("\n");
    const violations = [];
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i].trim();
      if (line.includes("refreshRuns()") && !line.includes("await") && !line.includes("//") && !line.includes("Promise<void>")) {
        violations.push(`  Línea ${i + 1}: ${line}`);
      }
    }
    assert.equal(violations.length, 0,
      `refreshRuns() sin await encontrado:\n${violations.join("\n")}`);
  });
});

// ---- Fix 4: run_command_raw no usa .expect() (ollama.rs) ----
describe("run_command_raw no panics", () => {
  const ollama = fs.readFileSync(
    path.resolve(__dirname, "../../src-tauri/src/ollama.rs"), "utf-8"
  );

  it("run_command_raw devuelve Result, no usa expect()", () => {
    // Busca la función y verifica que no tiene .expect() en ella
    const fnMatch = ollama.match(/fn run_command_raw[\s\S]*?(?=\nfn |\n#\[cfg)/);
    assert.ok(fnMatch !== null, "run_command_raw no encontrada");
    assert.ok(!fnMatch[0].includes(".expect("), "run_command_raw no debe usar .expect()");
    assert.ok(fnMatch[0].includes("Result"), "run_command_raw debe devolver Result");
  });

  it("tool_run_command maneja el error de run_command_raw", () => {
    const fnMatch = ollama.match(/fn tool_run_command[\s\S]*?(?=\nfn |\n#\[cfg|\n\/\/ )/);
    assert.ok(fnMatch !== null, "tool_run_command no encontrada");
    assert.ok(fnMatch[0].includes("match run_command_raw"), "tool_run_command debe usar match para manejar errores");
  });
});

// ---- Fix 6: mutex unwrap → expect (store.rs) ----
describe("mutex uses expect, not unwrap", () => {
  const store = fs.readFileSync(
    path.resolve(__dirname, "../../src-tauri/src/store.rs"), "utf-8"
  );

  it("store.rs no tiene .lock().unwrap()", () => {
    const matches = store.match(/\.lock\(\)\.unwrap\(\)/g);
    assert.equal(matches, null, `Encontrado .lock().unwrap() ${matches?.length} veces`);
  });

  it("store.rs usa .lock().expect()", () => {
    const matches = store.match(/\.lock\(\)\.expect\(/g);
    assert.ok(matches !== null && matches.length > 0);
  });
});

// ---- Fix 9: fail_stale_running_runs usa transacción ----
describe("fail_stale_running_runs uses transaction", () => {
  const store = fs.readFileSync(
    path.resolve(__dirname, "../../src-tauri/src/store.rs"), "utf-8"
  );

  it("usa BEGIN IMMEDIATE para transacción", () => {
    assert.ok(store.includes('BEGIN IMMEDIATE'));
  });

  it("hace COMMIT en caso de éxito", () => {
    assert.ok(store.includes('COMMIT'));
  });

  it("hace ROLLBACK en caso de error", () => {
    assert.ok(store.includes('ROLLBACK'));
  });
});
