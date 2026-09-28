import "@xterm/xterm/css/xterm.css";
import "./styles.css";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { icon } from "./icons";
import { applyTheme, loadThemeMode, onSystemThemeChange, saveThemeMode, TERMINAL_THEMES, type ThemeMode } from "./theme";
import { autoCheckEnabled, checkAtStartup, checkNow, setAutoCheck, updateSupport, type InstallHooks } from "./updates";
import { fillSyncForm, missingSyncField, readSyncForm, transportWarning, updateSyncVisibility, type SyncTarget } from "./sync-form";

// ---------- types ----------

interface Settings {
  sync: SyncTarget;
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
  archived: boolean;
}

interface Conflict {
  file: string;
  sessionId: string;
  title: string;
  savedAt: number;
  side: "local" | "distant";
  promptCount: number;
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
  syncLabel: string;
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
  resize: ResizeObserver;
}

// ---------- state ----------

const MONO_FONT = '"JetBrains Mono", "Cascadia Mono", "Fira Code", "DejaVu Sans Mono", Consolas, monospace';

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

let settings: Settings;
let info: AppInfo;
let sessions: SessionEntry[] = [];
/** Plan usage last reported by Claude Code, see refreshQuota. */
interface RateLimit {
  used_percentage: number;
  /** Unix time in seconds. */
  resets_at?: number;
}
let quota: { rateLimits: { five_hour?: RateLimit; seven_day?: RateLimit }; updatedAt: number } | null = null;
/** Projects that have rules, see openRules. */
let rulesKeys = new Set<string>();
let lastReport: SyncReport | null = null;
/** Versions set aside by sync conflicts, see openConflicts. */
let conflicts: Conflict[] = [];
/** Conversations whose messages match the search, with an excerpt, see searchContent. */
let contentHits = new Map<string, string>();
let searchTimer: ReturnType<typeof setTimeout> | undefined;
/** The list shows archived conversations instead of the others. */
let showArchived = false;
let syncing = false;
const tabs: Tab[] = [];
let activeTab: Tab | null = null;
let tabSeq = 0;
// Applied before anything renders to avoid a flash of the wrong theme.
let themeMode: ThemeMode = loadThemeMode();
let theme = applyTheme(themeMode);
let layout: LayoutId = "1";
/** Tab shown in each pane of the current layout. */
let panes: (Tab | null)[] = [null];
let activePane = 0;
/** Set once the window is closing: sessions stopped by the shutdown must still be restored. */
let shuttingDown = false;
/** SFTP host key approved in the settings dialog, see readSyncForm. */
let approvedFingerprint: string | null = null;

interface TestOutcome {
  ok: boolean;
  message: string;
  unknownFingerprint: string | null;
}

const OPEN_TABS_KEY = "claude-legend:open-tabs";

interface SavedTab {
  sessionId: string;
  cwd: string;
  title: string;
  active: boolean;
  /** Pane the tab was shown in, if any. */
  pane?: number | null;
}

type LayoutId = "1" | "2v" | "2h" | "3" | "4";

const LAYOUT_KEY = "claude-legend:layout";

/** Rectangles drawn in a 16×12 box for each layout button. */
const LAYOUTS: { id: LayoutId; panes: number; label: string; rects: [number, number, number, number][] }[] = [
  { id: "1", panes: 1, label: "Un seul panneau", rects: [[1, 1, 14, 10]] },
  { id: "2v", panes: 2, label: "Deux panneaux côte à côte", rects: [[1, 1, 6.5, 10], [8.5, 1, 6.5, 10]] },
  { id: "2h", panes: 2, label: "Deux panneaux l'un sur l'autre", rects: [[1, 1, 14, 4.5], [1, 6.5, 14, 4.5]] },
  { id: "3", panes: 3, label: "Trois panneaux", rects: [[1, 1, 6.5, 10], [8.5, 1, 6.5, 4.5], [8.5, 6.5, 6.5, 4.5]] },
  { id: "4", panes: 4, label: "Quatre panneaux", rects: [[1, 1, 6.5, 4.5], [8.5, 1, 6.5, 4.5], [1, 6.5, 6.5, 4.5], [8.5, 6.5, 6.5, 4.5]] },
];

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
    rulesKeys = new Set(await invoke<string[]>("rules_keys"));
    conflicts = await invoke<Conflict[]>("list_conflicts");
  } catch (e) {
    toast(`Lecture des sessions impossible : ${e}`, "error");
  }
  renderSessions();
  renderStatus();
}

function renderSessions() {
  const list = $("#session-list");
  const query = $<HTMLInputElement>("#search").value.trim().toLowerCase();
  const archivedCount = sessions.filter((s) => s.archived).length;
  if (!archivedCount) showArchived = false;
  const visible = sessions.filter(
    (s) =>
      s.archived === showArchived &&
      (!query ||
        `${s.title} ${s.firstPrompt} ${s.projectName} ${s.gitBranch ?? ""}`.toLowerCase().includes(query) ||
        contentHits.has(s.id)),
  );
  const toggle = archivedCount
    ? el("button", {
        className: "archive-toggle",
        textContent: showArchived ? "← Conversations" : `Archives (${archivedCount})`,
        onclick: () => {
          showArchived = !showArchived;
          renderSessions();
        },
      })
    : null;
  if (!visible.length) {
    const empty = query ? "Aucun résultat." : showArchived ? "Aucune conversation archivée." : "Aucune conversation pour l'instant.";
    list.replaceChildren(el("div", { className: "empty-list", textContent: empty }), ...(toggle ? [toggle] : []));
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
    const addBtn = el("button", { className: "icon", innerHTML: icon("plus"), title: cwd ? `Nouvelle session dans ${cwd}` : "Associer à un dossier" });
    addBtn.onclick = (e) => {
      e.stopPropagation();
      if (cwd) startNewSession(cwd);
      else mapProject(first).then((path) => {
        if (path) startNewSession(path);
      });
    };
    const hasRules = rulesKeys.has(first.projectKey);
    const rulesBtn = el("button", {
      className: `icon${hasRules ? " has-rules" : ""}`,
      innerHTML: icon("rules"),
      title: hasRules ? "Règles du projet (définies)" : "Définir des règles pour ce projet",
    });
    rulesBtn.onclick = (e) => {
      e.stopPropagation();
      openRules(first.projectKey, first.projectName);
    };
    const header = el(
      "div",
      { className: "project-header", title: cwd ?? "Projet pas encore présent sur ce PC" },
      el("span", { className: "pname", textContent: first.projectName }),
      rulesBtn,
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
      // A span: the session item is itself a button.
      const archiveBtn = el("span", {
        className: "session-action",
        role: "button",
        title: s.archived ? "Restaurer la conversation" : "Archiver la conversation (masquée sur tous les PC, rien n'est supprimé)",
        innerHTML: icon(s.archived ? "restore" : "archive"),
      });
      archiveBtn.onclick = (e) => {
        e.stopPropagation();
        setArchived(s, !s.archived);
      };
      const item = el(
        "button",
        {
          className: `session ${s.location}${s.id === activeSession ? " active" : ""}`,
          title: s.firstPrompt,
        },
        el("div", { className: "title" }, el("span", { className: "t", textContent: s.title }), ...badges, archiveBtn),
        el("div", { className: "meta", textContent: meta }),
        query && contentHits.has(s.id) ? el("div", { className: "snippet", textContent: contentHits.get(s.id)! }) : null,
      );
      item.onclick = () => resumeSession(s);
      makeDraggable(item, () => s.title, (pane) => {
        const open = tabs.find((t) => t.sessionId === s.id);
        if (open) return showInPane(open, pane);
        activePane = pane;
        resumeSession(s);
      });
      return item;
    });
    nodes.push(el("div", { className: "project" }, header, ...items));
  }
  if (toggle) nodes.push(toggle);
  list.replaceChildren(...nodes);
}

/** Filters titles right away, then searches the messages once typing pauses. */
function onSearchInput() {
  const query = $<HTMLInputElement>("#search").value.trim();
  clearTimeout(searchTimer);
  if (query.length < 3) {
    contentHits = new Map();
    renderSessions();
    return;
  }
  renderSessions();
  searchTimer = setTimeout(() => searchContent(query), 250);
}

async function searchContent(query: string) {
  try {
    const hits = await invoke<{ id: string; snippet: string }[]>("search_sessions", { query });
    // Typing went on while searching.
    if ($<HTMLInputElement>("#search").value.trim() !== query) return;
    contentHits = new Map(hits.map((h) => [h.id, h.snippet]));
    renderSessions();
  } catch (e) {
    toast(`Recherche impossible : ${e}`, "error");
  }
}

async function setArchived(s: SessionEntry, archived: boolean) {
  try {
    await invoke("set_archived", { sessionId: s.id, archived });
    s.archived = archived;
    renderSessions();
    toast(archived ? `« ${s.title} » archivée. Retrouve-la dans les archives en bas de la liste.` : `« ${s.title} » restaurée.`);
  } catch (e) {
    toast(`${e}`, "error");
  }
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
      errs.onclick = openConflicts;
      lines.push(errs);
    }
  }
  if (conflicts.length) {
    const link = el("div", {
      className: "err",
      textContent: `${conflicts.length} version(s) mise(s) de côté — voir`,
      title: "Versions de conversations modifiées sur deux PC à la fois",
    });
    link.onclick = openConflicts;
    lines.push(link);
  }
  lines.push(...quotaRows());
  lines.push(el("div", { textContent: info.syncEnabled ? `${settings.machineName} · ${info.syncLabel}` : settings.machineName }));
  status.replaceChildren(...lines);
}

// ---------- sync alerts ----------

function openConflicts() {
  renderConflicts();
  const dialog = $<HTMLDialogElement>("#conflicts-dialog");
  if (!dialog.open) dialog.showModal();
}

function renderConflicts() {
  const messages = lastReport ? [...lastReport.errors, ...lastReport.conflicts] : [];
  $("#sync-errors").hidden = !messages.length;
  $("#sync-error-list").replaceChildren(...messages.map((m) => el("li", { textContent: m })));
  $("#conflict-section").hidden = !conflicts.length && !!messages.length;
  $("#conflict-list").replaceChildren(
    ...(conflicts.length
      ? conflicts.map((c) => {
          const restore = el("button", { type: "button", textContent: "Restaurer comme nouvelle conversation" });
          restore.onclick = () => restoreConflict(c);
          const remove = el("button", { type: "button", textContent: "Supprimer" });
          remove.onclick = () => deleteConflict(c);
          const when = new Date(c.savedAt).toLocaleString("fr-FR", { dateStyle: "medium", timeStyle: "short" });
          return el(
            "li",
            { className: "conflict" },
            el("div", { className: "conflict-title", textContent: c.title }),
            el("div", {
              className: "meta",
              textContent: `Version ${c.side === "local" ? "de ce PC" : "d'un autre PC"}, mise de côté le ${when} · ${c.promptCount} message(s)`,
            }),
            el("div", { className: "row" }, restore, remove),
          );
        })
      : [el("li", { className: "meta", textContent: "Aucune version mise de côté." })]),
  );
}

async function restoreConflict(c: Conflict) {
  try {
    await invoke<string>("restore_conflict", { file: c.file });
    toast(`Version de « ${c.title} » restaurée comme nouvelle conversation.`);
    await refreshSessions();
    renderConflicts();
  } catch (e) {
    toast(`Restauration impossible : ${e}`, "error", 10000);
  }
}

async function deleteConflict(c: Conflict) {
  const choice = await ask(`Supprimer définitivement cette version de « ${c.title} » ?`, [
    { label: "Annuler", value: "cancel" },
    { label: "Supprimer", value: "delete", primary: true },
  ]);
  if (choice === "delete") {
    try {
      await invoke("delete_conflict", { file: c.file });
      await refreshSessions();
    } catch (e) {
      toast(`${e}`, "error");
    }
  }
  openConflicts();
}

// ---------- quota ----------

async function refreshQuota() {
  quota = await invoke<typeof quota>("get_quota").catch(() => null);
  renderStatus();
}

function formatReset(seconds: number): string {
  const date = new Date(seconds * 1000);
  const time = date.toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
  if (date.toDateString() === new Date().toDateString()) return time;
  if (seconds * 1000 - Date.now() < 6 * 86_400_000) return `${date.toLocaleDateString("fr-FR", { weekday: "short" })} ${time}`;
  return date.toLocaleDateString("fr-FR", { day: "numeric", month: "short" });
}

function quotaRow(label: string, limit: RateLimit): HTMLElement {
  const reset = limit.resets_at;
  // Past its reset time, the window has started over.
  const expired = reset !== undefined && reset * 1000 <= Date.now();
  const used = expired ? 0 : Math.max(0, Math.min(100, Math.round(limit.used_percentage)));
  const level = used >= 90 ? "danger" : used >= 70 ? "warn" : "ok";
  const fill = el("span", { className: `quota-fill ${level}` });
  fill.style.width = `${used}%`;
  const resetText = expired ? "réinitialisé" : reset ? `↻ ${formatReset(reset)}` : "";
  const updated = quota ? relativeTime(quota.updatedAt) : "";
  return el(
    "div",
    {
      className: "quota-row",
      title: `${label} : ${used} % utilisé${reset && !expired ? `, réinitialisation ${new Date(reset * 1000).toLocaleString("fr-FR")}` : ""}\nDernière mesure ${updated}, à la dernière réponse de Claude.`,
    },
    el("span", { className: "quota-label", textContent: label }),
    el("span", { className: "quota-bar" }, fill),
    el("span", { className: "quota-pct", textContent: `${used} %` }),
    el("span", { className: "quota-reset", textContent: resetText }),
  );
}

function quotaRows(): HTMLElement[] {
  const limits = quota?.rateLimits;
  if (!limits || (!limits.five_hour && !limits.seven_day)) {
    return [el("div", { className: "quota-empty", textContent: "Quota : affiché après une réponse de Claude" })];
  }
  const rows: HTMLElement[] = [];
  if (limits.five_hour) rows.push(quotaRow("Session 5 h", limits.five_hour));
  if (limits.seven_day) rows.push(quotaRow("Semaine", limits.seven_day));
  return [el("div", { className: "quota" }, ...rows)];
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
    theme: TERMINAL_THEMES[theme],
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.loadAddon(new WebLinksAddon((_e, uri) => openUrl(uri)));
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";

  const container = el("div", { className: "term" });
  $("#term-holder").append(container);
  term.open(container);

  const tabEl = el("div", { className: "tab" });
  const resize = new ResizeObserver(() => {
    // Terminals parked in the hidden holder have no size to fit to.
    if (container.offsetParent !== null) fit.fit();
  });
  resize.observe(container);
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
    resize,
  };
  tabEl.onclick = () => activateTab(tab);
  tabEl.onauxclick = (e) => e.button === 1 && closeTab(tab);
  makeDraggable(tabEl, () => tab.title, (pane) => showInPane(tab, pane));
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
  saveOpenTabs();
  tab.tabEl.title = `${tab.title}\n${tab.cwd}`;
  const pane = panes.indexOf(tab);
  const badge = panes.length > 1 && pane !== -1 ? el("span", { className: "pane-badge", textContent: String(pane + 1) }) : null;
  tab.tabEl.replaceChildren(
    ...[badge, tab.activity ? el("span", { className: "activity" }) : null, el("span", { className: "t", textContent: tab.title }), close].filter(
      (n): n is HTMLElement => n !== null,
    ),
  );
  const header = document.querySelectorAll<HTMLElement>("#terminals > .pane .pane-header .t")[pane];
  if (header) header.textContent = tab.title;
}

/** Focuses the tab's pane, or shows the tab in the focused pane. */
function activateTab(tab: Tab) {
  const pane = panes.indexOf(tab);
  if (pane !== -1) focusPane(pane);
  else showInPane(tab, activePane);
}

function closeTab(tab: Tab) {
  if (tab.ptyId !== null && !tab.exited) invoke("pty_kill", { id: tab.ptyId });
  tab.resize.disconnect();
  tab.term.dispose();
  tab.el.remove();
  tab.tabEl.remove();
  tabs.splice(tabs.indexOf(tab), 1);
  const pane = panes.indexOf(tab);
  if (pane !== -1) panes[pane] = nextHiddenTab();
  renderPanes();
  focusPane(activePane);
}

// ---------- panes ----------

function nextHiddenTab(): Tab | null {
  return [...tabs].reverse().find((t) => !panes.includes(t)) ?? null;
}

function layoutPaneCount(id: LayoutId): number {
  return LAYOUTS.find((l) => l.id === id)?.panes ?? 1;
}

/** Rebuilds the pane grid and puts every terminal in its pane or in the hidden holder. */
function renderPanes() {
  const split = panes.length > 1;
  const nodes = panes.map((tab, i) => {
    const body = el("div", { className: "pane-body" });
    if (tab) {
      body.append(tab.el);
    } else {
      const button = el("button", { textContent: "Nouvelle session…" });
      button.onclick = () => {
        focusPane(i, false);
        startNewSession();
      };
      body.append(
        el(
          "div",
          { className: "pane-empty" },
          el("p", { textContent: "Choisis une conversation dans la liste, clique sur un onglet, ou glisse-le ici." }),
          button,
        ),
      );
    }
    const header = split
      ? el(
          "div",
          { className: "pane-header" },
          el("span", { className: "pane-num", textContent: String(i + 1) }),
          el("span", { className: "t", textContent: tab?.title ?? "Panneau vide" }),
        )
      : null;
    const pane = el("div", { className: "pane" }, header, body);
    pane.addEventListener("mousedown", () => activePane !== i && focusPane(i, false));
    pane.addEventListener("focusin", () => activePane !== i && focusPane(i, false));
    return pane;
  });
  const holder = $("#term-holder");
  for (const t of tabs) if (!panes.includes(t)) holder.append(t.el);
  const grid = $("#terminals");
  grid.className = `layout-${layout}`;
  grid.replaceChildren(...nodes);
  // With no conversation open only the welcome screen shows, not an empty pane under it.
  grid.hidden = tabs.length === 0;
  $("#welcome").classList.toggle("hidden", tabs.length > 0);
  $("#tabbar").classList.toggle("hidden", tabs.length === 0);
  requestAnimationFrame(() => panes.forEach((t) => t?.fit.fit()));
}

function focusPane(index: number, focusTerminal = true) {
  activePane = Math.min(index, panes.length - 1);
  activeTab = panes[activePane] ?? null;
  if (activeTab) activeTab.activity = false;
  document
    .querySelectorAll("#terminals > .pane")
    .forEach((p, i) => p.classList.toggle("focused", panes.length > 1 && i === activePane));
  tabs.forEach(renderTab);
  const tab = activeTab;
  if (focusTerminal && tab) requestAnimationFrame(() => tab.term.focus());
  renderSessions();
}

/** Shows a tab in a pane; if it was already visible elsewhere the two panes swap. */
function showInPane(tab: Tab, index: number) {
  const from = panes.indexOf(tab);
  if (from !== -1 && from !== index) panes[from] = panes[index];
  panes[index] = tab;
  renderPanes();
  focusPane(index);
}

function setLayout(id: LayoutId) {
  const count = layoutPaneCount(id);
  const keep = activeTab;
  layout = id;
  panes = Array.from({ length: count }, (_, i) => panes[i] ?? null);
  activePane = Math.min(activePane, count - 1);
  if (keep && !panes.includes(keep)) panes[activePane] = keep;
  for (let i = 0; i < count; i++) panes[i] ??= nextHiddenTab();
  try {
    localStorage.setItem(LAYOUT_KEY, id);
  } catch {
    // Layout just won't be remembered.
  }
  renderLayoutPicker();
  renderPanes();
  focusPane(activePane);
}

function renderLayoutPicker() {
  $("#layouts").replaceChildren(
    ...LAYOUTS.map((l) => {
      const button = el("button", { className: l.id === layout ? "on" : "", title: l.label });
      button.innerHTML = `<svg width="16" height="12" viewBox="0 0 16 12" aria-hidden="true">${l.rects
        .map(([x, y, w, h]) => `<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="1"/>`)
        .join("")}</svg>`;
      button.onclick = () => setLayout(l.id);
      return button;
    }),
  );
}

function paneAt(x: number, y: number): number {
  const pane = document.elementFromPoint(x, y)?.closest("#terminals > .pane");
  return pane ? [...$("#terminals").children].indexOf(pane) : -1;
}

/**
 * Mouse-driven drag towards a pane. HTML5 drag and drop is not used because
 * Tauri intercepts it on Windows for file drops.
 */
function makeDraggable(handle: HTMLElement, label: () => string, onDrop: (pane: number) => void) {
  handle.addEventListener("mousedown", (down) => {
    if (down.button !== 0) return;
    let ghost: HTMLElement | null = null;
    const highlight = (index: number) =>
      document.querySelectorAll("#terminals > .pane").forEach((p, i) => p.classList.toggle("drop-target", i === index));
    const move = (e: MouseEvent) => {
      if (!ghost) {
        if (Math.hypot(e.clientX - down.clientX, e.clientY - down.clientY) < 6) return;
        ghost = el("div", { className: "drag-ghost", textContent: label() });
        document.body.append(ghost);
        document.body.classList.add("dragging");
      }
      ghost.style.left = `${e.clientX + 12}px`;
      ghost.style.top = `${e.clientY + 12}px`;
      highlight(paneAt(e.clientX, e.clientY));
    };
    const up = (e: MouseEvent) => {
      removeEventListener("mousemove", move);
      removeEventListener("mouseup", up, true);
      if (!ghost) return;
      ghost.remove();
      document.body.classList.remove("dragging");
      highlight(-1);
      const pane = paneAt(e.clientX, e.clientY);
      if (pane !== -1) onDrop(pane);
    };
    addEventListener("mousemove", move);
    addEventListener("mouseup", up, true);
  });
}

async function launch(tab: Tab, sessionId: string | null): Promise<boolean> {
  const channel = new Channel<PtyEvent>();
  channel.onmessage = (event) => {
    // Output still in flight when the tab was closed.
    if (!tabs.includes(tab)) return;
    if (event.kind === "data") {
      tab.term.write(event.data);
      if (!panes.includes(tab) && !tab.activity) {
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

// ---------- tab persistence ----------

/** Remembers running sessions so they come back if the app restarts or is killed. */
function saveOpenTabs() {
  if (shuttingDown) return;
  const saved: SavedTab[] = tabs
    .filter((t) => t.sessionId && !t.exited)
    .map((t) => ({
      sessionId: t.sessionId!,
      cwd: t.cwd,
      title: t.title,
      active: t === activeTab,
      pane: panes.includes(t) ? panes.indexOf(t) : null,
    }));
  try {
    localStorage.setItem(OPEN_TABS_KEY, JSON.stringify(saved));
  } catch {
    // Storage unavailable: tabs simply won't be restored.
  }
}

function loadSavedTabs(): SavedTab[] {
  try {
    const saved = JSON.parse(localStorage.getItem(OPEN_TABS_KEY) ?? "[]");
    return Array.isArray(saved) ? saved : [];
  } catch {
    return [];
  }
}

async function restoreTabs() {
  // Sessions with no prompt yet have no file to resume from.
  const resumable = new Set(sessions.filter((s) => s.location === "local").map((s) => s.id));
  let active: Tab | null = null;
  const placed: [Tab, number | null | undefined][] = [];
  for (const saved of loadSavedTabs()) {
    if (!resumable.has(saved.sessionId) || tabs.some((t) => t.sessionId === saved.sessionId)) continue;
    const tab = createTab(saved.title, saved.cwd, saved.sessionId);
    tab.term.write("\x1b[2m[Session rouverte après le redémarrage de Claude Legend]\x1b[0m\r\n");
    placed.push([tab, saved.pane]);
    await launch(tab, saved.sessionId);
    if (saved.active) active = tab;
  }
  if (placed.length) {
    panes = panes.map(() => null);
    for (const [tab, pane] of placed) if (pane != null && pane < panes.length && !panes[pane]) panes[pane] = tab;
    for (let i = 0; i < panes.length; i++) panes[i] ??= nextHiddenTab();
    renderPanes();
    const activeIndex = active ? panes.indexOf(active) : -1;
    if (active && activeIndex === -1) showInPane(active, activePane);
    else focusPane(Math.max(activeIndex, 0));
  }
  saveOpenTabs();
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
  let lock: LockInfo | null = null;
  if (info.syncEnabled) {
    try {
      lock = await invoke<LockInfo | null>("lock_status", { sessionId: s.id });
    } catch (e) {
      // Offline or target unreachable: the conversation must still open.
      toast(`Impossible de vérifier si la conversation est ouverte sur un autre PC : ${e}`, "warn", 8000);
    }
  }
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

/** Ctrl+Alt+1..4 focuses a pane. Uses the physical key so it works on AZERTY too. */
function handlePaneShortcut(e: KeyboardEvent): boolean {
  const match = /^Digit([1-4])$/.exec(e.code);
  if (!match || !e.ctrlKey || !e.altKey) return false;
  const index = Number(match[1]) - 1;
  if (index < panes.length) focusPane(index);
  return true;
}

/** Returns false to stop xterm from handling the key. */
function handleKey(tab: Tab, e: KeyboardEvent): boolean {
  if (e.type !== "keydown") return true;
  if (handlePaneShortcut(e)) {
    e.preventDefault();
    return false;
  }
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

// ---------- project rules ----------

let rulesProject: { key: string; name: string } | null = null;

async function openRules(key: string, name: string) {
  rulesProject = { key, name };
  const form = $<HTMLFormElement>("#rules-form");
  $("#rules-title").textContent = `Règles du projet « ${name} »`;
  const textarea = form.elements.namedItem("rules") as HTMLTextAreaElement;
  textarea.value = await invoke<string>("get_project_rules", { projectKey: key }).catch(() => "");
  const dialog = $<HTMLDialogElement>("#rules-dialog");
  dialog.returnValue = "";
  dialog.showModal();
  textarea.focus();
}

async function saveRules() {
  if (!rulesProject) return;
  const { key, name } = rulesProject;
  const rules = ($<HTMLFormElement>("#rules-form").elements.namedItem("rules") as HTMLTextAreaElement).value;
  try {
    await invoke("save_project_rules", { projectKey: key, rules });
    const open = tabs.some((t) => !t.exited && sessions.find((s) => s.id === t.sessionId)?.projectKey === key);
    toast(
      `Règles de « ${name} » enregistrées.` + (open ? "\nRelance les conversations ouvertes de ce projet pour les appliquer." : ""),
      "info",
      open ? 9000 : 4000,
    );
    await refreshSessions();
  } catch (e) {
    toast(`Règles non enregistrées : ${e}`, "error");
  }
}

// ---------- updates ----------

/** The install restarts the app: remember open tabs and keep them through the shutdown. */
const installHooks: InstallHooks = {
  before: () => {
    saveOpenTabs();
    shuttingDown = true;
  },
  failed: () => {
    shuttingDown = false;
  },
};

// ---------- theme ----------

function setThemeMode(mode: ThemeMode) {
  themeMode = mode;
  saveThemeMode(mode);
  refreshTheme();
}

function refreshTheme() {
  theme = applyTheme(themeMode);
  for (const t of tabs) t.term.options.theme = TERMINAL_THEMES[theme];
  const button = $("#btn-theme");
  button.innerHTML = icon(theme === "dark" ? "moon" : "sun");
  button.title = theme === "dark" ? "Passer en thème clair" : "Passer en thème sombre";
}

// ---------- settings ----------

function settingsField(name: string) {
  return $<HTMLFormElement>("#settings-form").elements.namedItem(name) as HTMLInputElement;
}

function renderFingerprint() {
  const target = readSyncForm($<HTMLFormElement>("#settings-form"), approvedFingerprint);
  $("#fingerprint").textContent =
    target.kind === "sftp" && approvedFingerprint ? `Serveur approuvé — empreinte ${approvedFingerprint}` : "";
  $("#transport-warning").textContent = transportWarning(target) ?? "";
}

function showTestResult(text: string, kind: "ok" | "fail" | "" = "") {
  const result = $("#test-result");
  result.textContent = text;
  result.className = kind;
}

async function openSettings() {
  const form = $<HTMLFormElement>("#settings-form");
  fillSyncForm(form, settings.sync);
  approvedFingerprint = settings.sync.kind === "sftp" ? settings.sync.fingerprint : null;
  renderFingerprint();
  showTestResult("");
  settingsField("machineName").value = settings.machineName;
  settingsField("theme").value = themeMode;
  settingsField("autoUpdateCheck").checked = autoCheckEnabled();
  $("#update-status").textContent = "";
  updateSupport()
    .then((s) => ($("#app-version").textContent = `Version installée : ${s.version}`))
    .catch(() => {});
  settingsField("claudePath").value = settings.claudePath ?? "";
  settingsField("extraArgs").value = settings.extraArgs;
  settingsField("fontSize").value = String(settings.fontSize);
  settingsField("syncIntervalSecs").value = String(settings.syncIntervalSecs);
  $("#claude-detected").textContent = info.claudePath ? `Détecté : ${info.claudePath}` : info.claudeError ?? "";
  const hasSecret = settings.sync.kind !== "none" && (await invoke<boolean>("has_sync_secret", { target: settings.sync }).catch(() => false));
  settingsField("secret").placeholder = hasSecret ? "enregistré — laisser vide pour le conserver" : "";
  const dialog = $<HTMLDialogElement>("#settings-dialog");
  dialog.returnValue = "";
  dialog.showModal();
}

/** Tests the target in the form, asking to approve an unknown SFTP host key. */
async function testSyncTarget(): Promise<{ ok: boolean; message: string }> {
  const target = readSyncForm($<HTMLFormElement>("#settings-form"), approvedFingerprint);
  const missing = missingSyncField(target);
  if (missing) return { ok: false, message: missing };
  if (target.kind === "none") return { ok: true, message: "" };
  const secret = settingsField("secret").value || null;
  showTestResult("Connexion…");
  try {
    let outcome = await invoke<TestOutcome>("test_sync", { target, secret });
    if (outcome.unknownFingerprint && target.kind === "sftp") {
      const choice = await ask(
        `Première connexion à ${target.host}. Empreinte de la clé du serveur :\n\n${outcome.unknownFingerprint}\n\n` +
          `Vérifie qu'elle correspond à celle du serveur (sur le serveur : ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub). ` +
          `Faire confiance à ce serveur ?`,
        [
          { label: "Annuler", value: "cancel" },
          { label: "Faire confiance", value: "trust", primary: true },
        ],
      );
      if (choice !== "trust") return { ok: false, message: "Serveur non approuvé." };
      approvedFingerprint = outcome.unknownFingerprint;
      renderFingerprint();
      outcome = await invoke<TestOutcome>("test_sync", { target: { ...target, fingerprint: approvedFingerprint }, secret });
    }
    return { ok: outcome.ok, message: outcome.message };
  } catch (e) {
    return { ok: false, message: `${e}` };
  }
}

async function runSyncTest(): Promise<boolean> {
  const { ok, message } = await testSyncTarget();
  showTestResult(ok ? message || "Connexion réussie." : message, ok ? "ok" : "fail");
  return ok;
}

/** Validates the sync target before the dialog closes; false keeps it open. */
async function confirmSave(): Promise<boolean> {
  const target = readSyncForm($<HTMLFormElement>("#settings-form"), approvedFingerprint);
  const changed = JSON.stringify(target) !== JSON.stringify(settings.sync) || settingsField("secret").value !== "";
  if (!changed || target.kind === "none") return true;
  if (await runSyncTest()) return true;
  const missing = missingSyncField(target);
  if (missing) return false;
  const choice = await ask(`La connexion a échoué :\n${$("#test-result").textContent}\n\nEnregistrer quand même ?`, [
    { label: "Corriger", value: "fix", primary: true },
    { label: "Enregistrer quand même", value: "save" },
  ]);
  return choice === "save";
}

async function saveSettingsFromForm() {
  const field = (name: string) => settingsField(name).value.trim();
  const next: Settings = {
    sync: readSyncForm($<HTMLFormElement>("#settings-form"), approvedFingerprint),
    machineName: field("machineName") || settings.machineName,
    claudePath: field("claudePath") || null,
    extraArgs: field("extraArgs"),
    fontSize: Number(field("fontSize")) || 14,
    syncIntervalSecs: Number(field("syncIntervalSecs")) || 60,
  };
  const secret = settingsField("secret").value || null;
  settingsField("secret").value = "";
  setThemeMode(settingsField("theme").value as ThemeMode);
  setAutoCheck(settingsField("autoUpdateCheck").checked);
  try {
    await invoke("save_settings", { settings: next, secret });
    settings = next;
    info = await invoke<AppInfo>("app_info");
    for (const t of tabs) {
      t.term.options.fontSize = settings.fontSize;
      t.fit.fit();
    }
    syncing = info.syncEnabled;
    renderStatus();
    renderWelcomeWarning();
  } catch (e) {
    toast(`Réglages non enregistrés : ${e}`, "error");
  }
}

function renderWelcomeWarning() {
  $("#welcome-warning").textContent = info.claudeError ?? "";
}

// ---------- boot ----------

async function boot() {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    (await import("./dev/preview")).installPreview();
  }
  settings = await invoke<Settings>("get_settings");
  info = await invoke<AppInfo>("app_info");
  lastReport = info.lastReport;
  renderStatus();
  renderWelcomeWarning();

  $("#btn-new").onclick = () => startNewSession();
  $("#btn-new-welcome").onclick = () => startNewSession();
  $("#btn-sync").onclick = syncNow;
  $("#btn-new").innerHTML = icon("plus");
  $("#btn-sync").innerHTML = icon("sync");
  $("#btn-settings").innerHTML = icon("settings");
  $("#btn-settings").onclick = openSettings;
  $("#btn-theme").onclick = () => setThemeMode(theme === "dark" ? "light" : "dark");
  onSystemThemeChange(() => themeMode === "system" && refreshTheme());
  refreshTheme();
  $("#search").oninput = onSearchInput;
  const settingsForm = $<HTMLFormElement>("#settings-form");
  $("#pick-sync-dir").onclick = async () => {
    const dir = await pickFolder("Dossier synchronisé entre tes PC");
    if (dir) settingsField("folderPath").value = dir;
  };
  $("#pick-key").onclick = async () => {
    const key = await openDialog({ multiple: false, directory: false, title: "Clé privée SSH", defaultPath: `${info.home}/.ssh` });
    if (typeof key === "string") settingsField("keyPath").value = key;
  };
  $("#test-sync").onclick = () => runSyncTest();
  $<HTMLDialogElement>("#rules-dialog").addEventListener("close", (e) => {
    if ((e.target as HTMLDialogElement).returnValue === "save") saveRules();
  });
  $("#check-updates").onclick = async () => {
    $("#update-status").textContent = "Recherche…";
    $("#update-status").textContent = await checkNow(installHooks);
  };
  settingsForm.addEventListener("change", (e) => {
    const name = (e.target as HTMLInputElement).name;
    if (name === "syncKind" || name === "sftpAuth") updateSyncVisibility(settingsForm);
    renderFingerprint();
    showTestResult("");
  });
  settingsForm.addEventListener("input", (e) => {
    // A different server needs its own host key approval.
    const name = (e.target as HTMLInputElement).name;
    if (name === "host" || name === "port") approvedFingerprint = null;
    renderFingerprint();
  });
  settingsForm.addEventListener("submit", async (e) => {
    if ((e.submitter as HTMLButtonElement | null)?.value !== "save") return;
    e.preventDefault();
    if (await confirmSave()) $<HTMLDialogElement>("#settings-dialog").close("save");
  });
  $<HTMLDialogElement>("#settings-dialog").addEventListener("close", (e) => {
    if ((e.target as HTMLDialogElement).returnValue === "save") saveSettingsFromForm();
  });

  document.addEventListener("keydown", (e) => {
    // Keys typed in a terminal were already handled by handleKey.
    if ((e.target as Element | null)?.closest?.(".xterm")) return;
    if (handlePaneShortcut(e)) return e.preventDefault();
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
  await getCurrentWindow().onCloseRequested(() => {
    saveOpenTabs();
    shuttingDown = true;
  });
  await listen("sessions-changed", () => refreshSessions());
  window.addEventListener("focus", () => refreshSessions());
  setInterval(refreshSessions, 20000);
  setInterval(() => renderSessions(), 60000);

  let savedLayout: string | null = null;
  try {
    savedLayout = localStorage.getItem(LAYOUT_KEY);
  } catch {
    // Default layout.
  }
  layout = LAYOUTS.some((l) => l.id === savedLayout) ? (savedLayout as LayoutId) : "1";
  panes = Array.from({ length: layoutPaneCount(layout) }, () => null);
  renderLayoutPicker();
  renderPanes();

  await refreshSessions();
  await refreshQuota();
  setInterval(refreshQuota, 15_000);
  await restoreTabs();
  setTimeout(() => checkAtStartup(installHooks), 3000);
  if (!info.syncEnabled) {
    toast("Choisis un dossier de synchronisation dans les réglages pour retrouver tes conversations sur tes autres PC.", "info", 10000);
  }
}

boot().catch((e) => toast(`Erreur au démarrage : ${e}`, "error", 30000));
