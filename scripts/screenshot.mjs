// Screenshot of the real app (WebKitGTK), rendered on a virtual X display.
//
//   npm run screenshot -- out.png [--wait 6000] [--js "<expression>"]…
//
// Runs the debug build (`cargo build` in src-tauri) against the Vite dev server,
// in isolated settings, data and Claude folders filled with fake sessions and
// quota, with a fake `claude` so no real session is shown, started or modified.
// The virtual screen has the window size of tauri.conf.json (without a window
// manager, the window cannot be resized). Each --js expression runs in the page,
// in order, through the WebKit remote inspector, before the capture. Linux only;
// needs Xvfb and ImageMagick (`dnf install xorg-x11-server-Xvfb ImageMagick`).
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const ROOT = resolve(import.meta.dirname, "..");
const APP = join(ROOT, "src-tauri/target/debug/claude-legend");
const DISPLAY = ":97";
const INSPECTOR_PORT = 9397;
const tauriConf = JSON.parse(readFileSync(join(ROOT, "src-tauri/tauri.conf.json"), "utf8"));
const { width, height } = tauriConf.app.windows[0];

const args = process.argv.slice(2);
const out = resolve(args.find((a, i) => !a.startsWith("--") && !args[i - 1]?.startsWith("--")) ?? "screenshot.png");
const option = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i === -1 ? fallback : args[i + 1];
};
const scripts = args.flatMap((a, i) => (a === "--js" ? [args[i + 1]] : []));
const wait = Number(option("wait", "6000"));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

if (!existsSync(APP)) {
  console.error(`Build the app first: (cd src-tauri && cargo build)`);
  process.exit(1);
}

/** Evaluates an expression in the page through the WebKit remote inspector. */
function evaluate(expression) {
  return new Promise((done, fail) => {
    const ws = new WebSocket(`ws://127.0.0.1:${INSPECTOR_PORT}/socket/1/1/WebPage`);
    let targetId = null;
    const timer = setTimeout(() => (ws.close(), fail(new Error("inspector timeout"))), 10000);
    const send = () => {
      const inner = { id: 1000, method: "Runtime.evaluate", params: { expression, returnByValue: true } };
      ws.send(JSON.stringify(targetId ? { id: 1, method: "Target.sendMessageToTarget", params: { targetId, message: JSON.stringify(inner) } } : inner));
    };
    ws.onopen = () => setTimeout(() => targetId || send(), 800);
    ws.onerror = () => (clearTimeout(timer), fail(new Error("inspector unreachable")));
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      if (m.method === "Target.targetCreated" && !targetId) return ((targetId = m.params.targetInfo.targetId), send());
      const r = m.method === "Target.dispatchMessageFromTarget" ? JSON.parse(m.params.message) : m;
      if (r.id === 1000) (clearTimeout(timer), ws.close(), done(r.result?.result?.value));
    };
  });
}

/** Fake Claude Code history: a few sessions, and plan usage as the status line relay records it. */
function seedFakeData(claudeHome, dataDir) {
  const HOUR = 3_600_000;
  const now = Date.now();
  const sessions = [
    ["Refonte de la synchronisation", "/home/preview/dev/claude-legend", "develop", 0.1],
    ["Écran partagé en 4 panneaux", "/home/preview/dev/claude-legend", "feature/split", 3],
    ["Corriger le calcul des adresses", "/home/preview/dev/cohabsys", "main", 26],
    ["Migration de la base de données", "/home/preview/dev/cohabsys", "main", 50],
    ["Tableau de bord des ventes", "/home/preview/dev/tric-house", "develop", 80],
  ];
  sessions.forEach(([title, cwd, gitBranch, hoursAgo], i) => {
    const dir = join(claudeHome, "projects", cwd.replaceAll("/", "-"));
    mkdirSync(dir, { recursive: true });
    const file = join(dir, `00000000-0000-4000-8000-00000000000${i}.jsonl`);
    const lines = [
      { type: "user", cwd, gitBranch, message: { role: "user", content: title } },
      { type: "ai-title", aiTitle: title },
    ];
    writeFileSync(file, lines.map((l) => JSON.stringify(l)).join("\n") + "\n");
    const mtime = new Date(now - hoursAgo * HOUR);
    utimesSync(file, mtime, mtime);
  });

  const inSeconds = (ms) => Math.round((now + ms) / 1000);
  mkdirSync(dataDir, { recursive: true });
  writeFileSync(
    join(dataDir, "quota.json"),
    JSON.stringify({
      rateLimits: {
        five_hour: { used_percentage: 66, resets_at: inSeconds(2 * HOUR) },
        seven_day: { used_percentage: 9, resets_at: inSeconds(4 * 24 * HOUR) },
      },
      updatedAt: now,
    }),
  );
}

async function devServerUp() {
  return fetch("http://localhost:1420/").then((r) => r.ok, () => false);
}

const children = [];
const home = mkdtempSync(join(tmpdir(), "claude-legend-shot-"));
const cleanup = () => {
  for (const child of children.reverse()) child.kill();
  rmSync(home, { recursive: true, force: true });
};
process.on("SIGINT", () => (cleanup(), process.exit(130)));

try {
  if (!(await devServerUp())) {
    children.push(spawn("npx", ["vite"], { cwd: ROOT, stdio: "ignore" }));
    for (let i = 0; i < 30 && !(await devServerUp()); i++) await sleep(500);
  }

  children.push(spawn("Xvfb", [DISPLAY, "-screen", "0", `${width}x${height}x24`, "-nolisten", "tcp"], { stdio: "ignore" }));
  await sleep(1500);

  // Isolated settings, with a fake claude that only draws a screen, and a local
  // sync folder so the "sync disabled" toast stays away.
  const config = join(home, "config", tauriConf.identifier);
  mkdirSync(config, { recursive: true });
  const claudeHome = join(home, "claude");
  seedFakeData(claudeHome, join(home, "data", tauriConf.identifier));
  const syncDir = join(home, "sync");
  mkdirSync(syncDir);
  const fakeClaude = join(home, "fake-claude.sh");
  writeFileSync(
    fakeClaude,
    `#!/bin/bash\nprintf '\\033[1mFake Claude\\033[0m (capture)\\n'\nwhile true; do read -r -t 1 _; done\n`,
  );
  chmodSync(fakeClaude, 0o755);
  writeFileSync(
    join(config, "settings.json"),
    JSON.stringify({ sync: { kind: "folder", path: syncDir }, claudePath: fakeClaude, machineName: "capture", fontSize: 14, syncIntervalSecs: 3600, extraArgs: "" }),
  );

  // A dead proxy keeps the updater offline, so no update dialog covers the window.
  const env = {
    ...process.env,
    DISPLAY,
    GDK_BACKEND: "x11",
    WEBKIT_DISABLE_COMPOSITING_MODE: "1",
    XDG_CONFIG_HOME: join(home, "config"),
    XDG_DATA_HOME: join(home, "data"),
    CLAUDE_CONFIG_DIR: claudeHome,
    WEBKIT_INSPECTOR_HTTP_SERVER: `127.0.0.1:${INSPECTOR_PORT}`,
    HTTPS_PROXY: "http://127.0.0.1:9",
    HTTP_PROXY: "http://127.0.0.1:9",
    NO_PROXY: "localhost,127.0.0.1",
  };
  delete env.WAYLAND_DISPLAY;
  children.push(spawn(APP, [], { env, stdio: "ignore" }));
  await sleep(wait);

  for (const expression of scripts) {
    const value = await evaluate(expression);
    if (value !== undefined) console.log(JSON.stringify(value));
    await sleep(700);
  }

  const shot = spawnSync("import", ["-display", DISPLAY, "-window", "root", out]);
  if (shot.status !== 0) throw new Error(`capture failed: ${shot.stderr}`);
  console.log(`Capture : ${out}`);
} catch (e) {
  console.error(e.message);
  process.exitCode = 1;
} finally {
  cleanup();
}
