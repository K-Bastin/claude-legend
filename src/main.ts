import "@xterm/xterm/css/xterm.css";
import "./styles.css";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Unicode11Addon } from "@xterm/addon-unicode11";

// ---------- types ----------

interface Settings {
  syncDir: string | null;
  claudePath: string | null;
  extraArgs: string;
  machineName: string;
  fontSize: number;
  syncIntervalSecs: number;
}

interface LockInfo {
  sessionId: string;
  machineId: string;
  machineName: string;
  since: number;
  heartbeat: number;
}

interface SessionEntry {
  id: string;
  title: string;
  firstPrompt: string;
  cwd: string | null;
  projectKey: string;
  projectName: string;
  gitBranch: string | null;
  promptCount: number;
  updatedAt: number;
  location: "local" | "remote";
  synced: boolean;
  lastMachine: string | null;
  lockedBy: LockInfo | null;
  openHere: boolean;
}

interface SyncReport {
  at: number;
  pushed: number;
  pulled: number;
  conflicts: string[];
  errors: string[];
}

interface AppInfo {
  machineId: string;
  claudePath: string | null;
  claudeError: string | null;
  syncEnabled: boolean;
  lastReport: SyncReport | null;
  home: string;
}

type PtyEvent = { kind: "data"; data: string } | { kind: "exit"; code: number | null };

interface Tab {
  key: number;
  ptyId: number | null;
  sessionId: string | null;
  cwd: string;
  title: string;
  term: Terminal;
  fit: FitAddon;
  el: HTMLDivElement;
  tabEl: HTMLDivElement;
  exited: boolean;
  activity: boolean;
}

// ---------- state ----------

const MONO_FONT = '"JetBrains Mono", "Cascadia Mono", "Fira Code", "DejaVu Sans Mono", Consolas, monospace';

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

let settings: Settings;
let info: AppInfo;
let sessions: SessionEntry[] = [];
let lastReport: SyncReport | null = null;
let syncing = false;
const tabs: Tab[] = [];
let activeTab: Tab | null = null;
let tabSeq = 0;

// ---------- helpers ----------

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> & { className?: string } = {},
  ...children: (Node | string | null | false)[]
): HTMLElementTagNameMap[K] {
  const node = Object.assign(document.createElement(tag), props);
  for (const child of children) if (child) node.append(child);
  return node;
}

function toast(message: string, kind: "info" | "warn" | "error" = "info", ms = 6000) {
  const node = el("div", { className: `toast ${kind}`, textContent: message });
  node.onclick = () => node.remove();
  $("#toasts").append(node);
  setTimeout(() => node.remove(), ms);
}

function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  const min = Math.round(diff / 60000);
  if (min < 1) return "à l'instant";
  if (min < 60) return `il y a ${min} min`;
  const h = Math.round(min / 60);
  if (h < 24) return `il y a ${h} h`;
  const d = new Date(ms);
  const yesterday = new Date();
  yesterday.setDate(yesterday.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return "hier";
  return d.toLocaleDateString("fr-FR", { day: "numeric", month: "short", year: diff > 300 * 86400000 ? "numeric" : undefined });
}

function ask(message: string, buttons: { label: string; value: string; primary?: boolean }[]): Promise<string> {
  const dialog = $<HTMLDialogElement>("#confirm-dialog");
  $("#confirm-message").textContent = message;
  const menu = $("#confirm-buttons");
  menu.replaceChildren(
    ...buttons.map((b) => el("button", { value: b.value, textContent: b.label, className: b.primary ? "primary" : "" })),
  );
  dialog.returnValue = "";
  dialog.showModal();
  return new Promise((resolve) => dialog.addEventListener("close", () => resolve(dialog.returnValue), { once: true }));
}

async function pickFolder(title: string): Promise<string | null> {
  const picked = await openDialog({ directory: true, multiple: false, title });
  return typeof picked === "string" ? picked : null;
}

function quotePath(p: string): string {
  if (/^[A-Za-z]:\\/.test(p)) return p.includes(" ") ? `"${p}"` : p;
  return /[\s'"()&;$`\\]/.test(p) ? `'${p.replace(/'/g, `'\\''`)}'` : p;
}

// ---------- session list ----------

async function refreshSessions() {
  try {
    sessions = await invoke<SessionEntry[]>("list_sessions");
  } catch (e) {
    toast(`Lecture des sessions impossible : ${e}`, "error");
  }
  renderSessions();
}

function renderSessions() {
  const list = $("#session-list");
  const query = $<HTMLInputElement>("#search").value.trim().toLowerCase();
  const visible = sessions.filter(
    (s) => !query || `${s.title} ${s.firstPrompt} ${s.projectName} ${s.gitBranch ?? ""}`.toLowerCase().includes(query),
  );
  if (!visible.length) {
    list.replaceChildren(
      el("div", { className: "empty-list", textContent: query ? "Aucun résultat." : "Aucune conversation pour l'instant." }),
    );
    return;
  }

  const groups = new Map<string, SessionEntry[]>();
  for (const s of visible) {
    const group = groups.get(s.projectKey) ?? [];
    group.push(s);
    groups.set(s.projectKey, group);
  }

  const activeSession = activeTab?.sessionId;
  const nodes: HTMLElement[] = [];
  for (const [, group] of groups) {
    const first = group[0];
    const cwd = group.find((s) => s.cwd)?.cwd ?? null;
    const addBtn = el("button", { className: "icon", textContent: "＋", title: cwd ? `Nouvelle session dans ${cwd}` : "Associer à un dossier" });
    addBtn.onclick = (e) => {
      e.stopPropagation();
      if (cwd) startNewSession(cwd);
      else mapProject(first).then((path) => {
        if (path) startNewSession(path);
      });
    };
    const header = el(
      "div",
      { className: "project-header", title: cwd ?? "Projet pas encore présent sur ce PC" },
      el("span", { className: "pname", textContent: first.projectName }),
      addBtn,
    );
    const items = group.map((s) => {
      const badges: HTMLElement[] = [];
      if (s.openHere || tabs.some((t) => t.sessionId === s.id)) badges.push(el("span", { className: "badge open", textContent: "ouverte" }));
      if (s.lockedBy) badges.push(el("span", { className: "badge lock", textContent: `🔒 ${s.lockedBy.machineName}` }));
      if (s.location === "remote") badges.push(el("span", { className: "badge cloud", textContent: "☁", title: "Pas encore sur ce PC" }));
      else if (s.synced) badges.push(el("span", { className: "badge cloud", textContent: "✓", title: "Synchronisée" }));
      const meta = [relativeTime(s.updatedAt), s.gitBranch, s.lastMachine && s.location === "remote" ? s.lastMachine : null]
        .filter(Boolean)
        .join(" · ");
      const item = el(
        "button",
        {
          className: `session ${s.location}${s.id === activeSession ? " active" : ""}`,
          title: s.firstPrompt,
        },
        el("div", { className: "title" }, el("span", { className: "t", textContent: s.title }), ...badges),
        el("div", { className: "meta", textContent: meta }),
      );
      item.onclick = () => resumeSession(s);
      return item;
    });
    nodes.push(el("div", { className: "project" }, header, ...items));
  }
  list.replaceChildren(...nodes);
}

function renderStatus() {
  const status = $("#status");
  const lines: HTMLElement[] = [];
  if (!info.syncEnabled) {
    const link = el("span", { className: "err", textContent: "Synchronisation désactivée — configurer" });
    link.style.color = "var(--warn)";
    link.onclick = openSettings;
    lines.push(el("div", {}, el("span", { className: "dot warn" }), link));
  } else if (syncing) {
    lines.push(el("div", {}, el("span", { className: "dot" }), "Synchronisation…"));
  } else if (lastReport) {
    const time = new Date(lastReport.at).toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
    const ok = !lastReport.errors.length && !lastReport.conflicts.length;
    lines.push(el("div", {}, el("span", { className: `dot ${ok ? "ok" : "warn"}` }), `Synchronisé à ${time}`));
    if (!ok) {
      const errs = el("div", {
        className: "err",
        textContent: `${lastReport.errors.length + lastReport.conflicts.length} alerte(s) — détails`,
      });
      errs.onclick = () => toast([...lastReport!.conflicts, ...lastReport!.errors].join("\n"), "warn", 15000);
      lines.push(errs);
    }
  }
  lines.push(el("div", { textContent: settings.machineName }));
  status.replaceChildren(...lines);
}

// ---------- sync ----------

async function syncNow() {
  if (!info.syncEnabled) return openSettings();
  syncing = true;
  $("#btn-sync").classList.add("spinning");
  renderStatus();
  try {
    lastReport = (await invoke<SyncReport | null>("sync_now")) ?? lastReport;
  } catch (e) {
    toast(`Synchronisation échouée : ${e}`, "error");
  }
  syncing = false;
  $("#btn-sync").classList.remove("spinning");
  renderStatus();
  await refreshSessions();
}

async function mapProject(s: SessionEntry): Promise<string | null> {
  const choice = await ask(
    `Le projet « ${s.projectName} » n'est pas encore associé à un dossier sur ce PC.\n\n` +
      `Choisis le dossier où il se trouve (par ex. ton clone git). Les conversations y seront importées.`,
    [
      { label: "Annuler", value: "cancel" },
      { label: "Choisir le dossier…", value: "pick", primary: true },
    ],
  );
  if (choice !== "pick") return null;
  const path = await pickFolder(`Dossier du projet ${s.projectName}`);
  if (!path) return null;
  try {
    await invoke("map_project", { projectKey: s.projectKey, path });
    await refreshSessions();
    return path;
  } catch (e) {
    toast(`${e}`, "error");
    return null;
  }
}

// ---------- terminals ----------

function createTab(title: string, cwd: string, sessionId: string | null): Tab {
  const term = new Terminal({
    allowProposedApi: true,
    cursorBlink: true,
    fontFamily: MONO_FONT,
    fontSize: settings.fontSize,
    scrollback: 20000,
    macOptionIsMeta: true,
    theme: {
      background: "#1a1918",
      foreground: "#ece8e1",
      cursor: "#d97757",
      selectionBackground: "#d9775755",
      black: "#1a1918",
      brightBlack: "#6b645c",
    },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.loadAddon(new WebLinksAddon((_e, uri) => openUrl(uri)));
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";

  const container = el("div", { className: "term" });
  $("#terminals").append(container);
  term.open(container);

  const tabEl = el("div", { className: "tab" });
  const tab: Tab = {
    key: ++tabSeq,
    ptyId: null,
    sessionId,
    cwd,
    title,
    term,
    fit,
    el: container,
    tabEl,
    exited: false,
    activity: false,
  };
  tabEl.onclick = () => activateTab(tab);
  tabEl.onauxclick = (e) => e.button === 1 && closeTab(tab);
  $("#tabs").append(tabEl);
  renderTab(tab);

  term.attachCustomKeyEventHandler((e) => handleKey(tab, e));
  term.onData((data) => {
    if (tab.exited) {
      if (data === "\r") restartTab(tab);
      return;
    }
    if (tab.ptyId !== null) invoke("pty_write", { id: tab.ptyId, data }).catch(() => {});
  });
  term.onResize(({ cols, rows }) => {
    if (tab.ptyId !== null && !tab.exited) invoke("pty_resize", { id: tab.ptyId, cols, rows }).catch(() => {});
  });
  term.onTitleChange((t) => {
    const clean = t.replace(/^[^\p{L}\p{N}]+/u, "").trim();
    if (clean && !/^claude( code)?$/i.test(clean)) {
      tab.title = clean;
      renderTab(tab);
    }
  });
  new ResizeObserver(() => {
    if (container.classList.contains("active")) fit.fit();
  }).observe(container);

  tabs.push(tab);
  activateTab(tab);
  return tab;
}

function renderTab(tab: Tab) {
  const close = el("button", { className: "close", textContent: "✕", title: "Fermer" });
  close.onclick = (e) => {
    e.stopPropagation();
    closeTab(tab);
  };
  tab.tabEl.className = `tab${tab === activeTab ? " active" : ""}${tab.exited ? " exited" : ""}`;
  tab.tabEl.title = `${tab.title}\n${tab.cwd}`;
  tab.tabEl.replaceChildren(
    ...[tab.activity ? el("span", { className: "activity" }) : null, el("span", { className: "t", textContent: tab.title }), close].filter(
      (n): n is HTMLElement => n !== null,
    ),
  );
}

function activateTab(tab: Tab) {
  activeTab = tab;
  tab.activity = false;
  for (const t of tabs) {
    t.el.classList.toggle("active", t === tab);
    renderTab(t);
  }
  $("#welcome").classList.add("hidden");
  requestAnimationFrame(() => {
    tab.fit.fit();
    tab.term.focus();
  });
  renderSessions();
}

function closeTab(tab: Tab) {
  if (tab.ptyId !== null && !tab.exited) invoke("pty_kill", { id: tab.ptyId });
  tab.term.dispose();
  tab.el.remove();
  tab.tabEl.remove();
  tabs.splice(tabs.indexOf(tab), 1);
  if (activeTab === tab) {
    activeTab = null;
    const next = tabs[tabs.length - 1];
    if (next) activateTab(next);
    else $("#welcome").classList.remove("hidden");
  }
  renderSessions();
}

async function launch(tab: Tab, sessionId: string | null): Promise<boolean> {
  const channel = new Channel<PtyEvent>();
  channel.onmessage = (event) => {
    if (event.kind === "data") {
      tab.term.write(event.data);
      if (tab !== activeTab && !tab.activity) {
        tab.activity = true;
        renderTab(tab);
      }
    } else {
      tab.exited = true;
      tab.term.write(`\r\n\x1b[2m[Claude s'est arrêté${event.code ? ` (code ${event.code})` : ""} — Entrée pour relancer la session]\x1b[0m\r\n`);
      renderTab(tab);
      refreshSessions();
    }
  };
  tab.fit.fit();
  try {
    const result = await invoke<{ ptyId: number; sessionId: string; warnings: string[] }>("open_session", {
      request: { sessionId, cwd: tab.cwd, cols: tab.term.cols, rows: tab.term.rows },
      channel,
    });
    tab.ptyId = result.ptyId;
    tab.sessionId = result.sessionId;
    tab.exited = false;
    if (result.warnings.length) toast(result.warnings.join("\n"), "warn", 12000);
    renderTab(tab);
    renderSessions();
    return true;
  } catch (e) {
    tab.exited = true;
    tab.term.write(`\x1b[31m${e}\x1b[0m\r\n`);
    renderTab(tab);
    return false;
  }
}

async function restartTab(tab: Tab) {
  tab.exited = false;
  tab.term.reset();
  const known = sessions.some((s) => s.id === tab.sessionId && s.location === "local");
  await launch(tab, known ? tab.sessionId : null);
}

async function resumeSession(s: SessionEntry) {
  const existing = tabs.find((t) => t.sessionId === s.id);
  if (existing) return activateTab(existing);

  let cwd = s.cwd;
  if (!cwd) {
    cwd = await mapProject(s);
    if (!cwd) return;
  }
  const lock = info.syncEnabled ? await invoke<LockInfo | null>("lock_status", { sessionId: s.id }) : null;
  if (lock) {
    const choice = await ask(
      `Cette conversation est actuellement ouverte sur « ${lock.machineName} » (depuis ${relativeTime(lock.since)}).\n\n` +
        `Ferme-la là-bas d'abord pour éviter que les deux PC écrivent en même temps.`,
      [
        { label: "Annuler", value: "cancel" },
        { label: "Ouvrir quand même", value: "open" },
      ],
    );
    if (choice !== "open") return;
  }
  const tab = createTab(s.title, cwd, s.id);
  await launch(tab, s.id);
}

async function startNewSession(cwd?: string) {
  const dir = cwd ?? (await pickFolder("Dossier de travail de la nouvelle session"));
  if (!dir) return;
  const name = dir.split(/[\\/]/).filter(Boolean).pop() ?? dir;
  const tab = createTab(`Nouvelle session · ${name}`, dir, null);
  await launch(tab, null);
  setTimeout(refreshSessions, 3000);
}

/** Returns false to stop xterm from handling the key. */
function handleKey(tab: Tab, e: KeyboardEvent): boolean {
  if (e.type !== "keydown") return true;
  const ctrl = e.ctrlKey || e.metaKey;
  const key = e.key.toLowerCase();

  if (ctrl && e.shiftKey && key === "c") {
    const sel = tab.term.getSelection();
    if (sel) writeText(sel);
    return false;
  }
  if (ctrl && e.shiftKey && key === "v") {
    e.preventDefault();
    readText().then((t) => t && tab.term.paste(t)).catch(() => {});
    return false;
  }
  // Ctrl+C copies when text is selected, interrupts otherwise.
  if (ctrl && !e.shiftKey && key === "c" && tab.term.hasSelection()) {
    writeText(tab.term.getSelection());
    tab.term.clearSelection();
    return false;
  }
  // Ctrl+V pastes text; with no text in the clipboard Claude gets the key and
  // attaches the image from the clipboard itself.
  if (ctrl && !e.shiftKey && key === "v") {
    e.preventDefault();
    readText()
      .then((t) => (t ? tab.term.paste(t) : sendRaw(tab, "\x16")))
      .catch(() => sendRaw(tab, "\x16"));
    return false;
  }
  // Newline in the prompt, like /terminal-setup configures.
  if (e.key === "Enter" && e.shiftKey && !ctrl) {
    sendRaw(tab, "\x1b\r");
    return false;
  }
  if (ctrl && e.shiftKey && key === "t") {
    startNewSession(tab.cwd);
    return false;
  }
  if (ctrl && e.shiftKey && key === "w") {
    closeTab(tab);
    return false;
  }
  if (ctrl && e.key === "Tab") {
    const idx = tabs.indexOf(tab);
    const next = tabs[(idx + (e.shiftKey ? tabs.length - 1 : 1)) % tabs.length];
    if (next) activateTab(next);
    return false;
  }
  if (ctrl && (key === "=" || key === "+" || key === "-" || key === "0")) {
    const size = key === "0" ? settings.fontSize : Math.max(8, Math.min(32, tab.term.options.fontSize! + (key === "-" ? -1 : 1)));
    for (const t of tabs) {
      t.term.options.fontSize = size;
      t.fit.fit();
    }
    return false;
  }
  return true;
}

function sendRaw(tab: Tab, data: string) {
  if (tab.ptyId !== null && !tab.exited) invoke("pty_write", { id: tab.ptyId, data }).catch(() => {});
}

// ---------- settings ----------

function openSettings() {
  const form = $<HTMLFormElement>("#settings-form");
  const field = (name: string) => form.elements.namedItem(name) as HTMLInputElement;
  field("syncDir").value = settings.syncDir ?? "";
  field("machineName").value = settings.machineName;
  field("claudePath").value = settings.claudePath ?? "";
  field("extraArgs").value = settings.extraArgs;
  field("fontSize").value = String(settings.fontSize);
  field("syncIntervalSecs").value = String(settings.syncIntervalSecs);
  $("#claude-detected").textContent = info.claudePath ? `Détecté : ${info.claudePath}` : info.claudeError ?? "";
  const dialog = $<HTMLDialogElement>("#settings-dialog");
  dialog.returnValue = "";
  dialog.showModal();
}

async function saveSettingsFromForm() {
  const form = $<HTMLFormElement>("#settings-form");
  const field = (name: string) => (form.elements.namedItem(name) as HTMLInputElement).value.trim();
  const next: Settings = {
    syncDir: field("syncDir") || null,
    machineName: field("machineName") || settings.machineName,
    claudePath: field("claudePath") || null,
    extraArgs: field("extraArgs"),
    fontSize: Number(field("fontSize")) || 14,
    syncIntervalSecs: Number(field("syncIntervalSecs")) || 60,
  };
  try {
    await invoke("save_settings", { settings: next });
    settings = next;
    info = await invoke<AppInfo>("app_info");
    for (const t of tabs) {
      t.term.options.fontSize = settings.fontSize;
      t.fit.fit();
    }
    renderStatus();
    renderWelcomeWarning();
    if (settings.syncDir) {
      syncing = true;
      renderStatus();
    }
  } catch (e) {
    toast(`Réglages non enregistrés : ${e}`, "error");
  }
}

function renderWelcomeWarning() {
  $("#welcome-warning").textContent = info.claudeError ?? "";
}

// ---------- boot ----------

async function boot() {
  settings = await invoke<Settings>("get_settings");
  info = await invoke<AppInfo>("app_info");
  lastReport = info.lastReport;
  renderStatus();
  renderWelcomeWarning();

  $("#btn-new").onclick = () => startNewSession();
  $("#btn-new-welcome").onclick = () => startNewSession();
  $("#btn-sync").onclick = syncNow;
  $("#btn-settings").onclick = openSettings;
  $("#search").oninput = renderSessions;
  $("#pick-sync-dir").onclick = async () => {
    const dir = await pickFolder("Dossier synchronisé entre tes PC");
    if (dir) ((($("#settings-form") as HTMLFormElement).elements.namedItem("syncDir")) as HTMLInputElement).value = dir;
  };
  $<HTMLDialogElement>("#settings-dialog").addEventListener("close", (e) => {
    if ((e.target as HTMLDialogElement).returnValue === "save") saveSettingsFromForm();
  });

  document.addEventListener("keydown", (e) => {
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && e.shiftKey && e.key.toLowerCase() === "t") {
      e.preventDefault();
      startNewSession(activeTab?.cwd);
    }
    if (ctrl && e.key.toLowerCase() === "f" && !activeTab) {
      e.preventDefault();
      $("#search").focus();
    }
  });

  await getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "drop" && activeTab && event.payload.paths.length) {
      sendRaw(activeTab, event.payload.paths.map(quotePath).join(" ") + " ");
      activeTab.term.focus();
    }
  });

  await listen<SyncReport>("sync-report", (e) => {
    lastReport = e.payload;
    syncing = false;
    renderStatus();
    if (e.payload.conflicts.length) toast(e.payload.conflicts.join("\n"), "warn", 15000);
  });
  await listen("sessions-changed", () => refreshSessions());
  window.addEventListener("focus", () => refreshSessions());
  setInterval(refreshSessions, 20000);
  setInterval(() => renderSessions(), 60000);

  await refreshSessions();
  if (!info.syncEnabled && !settings.syncDir) {
    toast("Choisis un dossier de synchronisation dans les réglages pour retrouver tes conversations sur tes autres PC.", "info", 10000);
  }
}

boot().catch((e) => toast(`Erreur au démarrage : ${e}`, "error", 30000));
