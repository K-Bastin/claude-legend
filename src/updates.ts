// In-app updates from the GitHub releases, through the Tauri updater: the
// update is signed at release time and its signature checked before install.
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { openUrl } from "@tauri-apps/plugin-opener";
import { check, type Update } from "@tauri-apps/plugin-updater";

const RELEASES_URL = "https://github.com/K-Bastin/claude-legend/releases";
const RELEASE_API_URL = "https://api.github.com/repos/K-Bastin/claude-legend/releases/tags";
const AUTO_CHECK_KEY = "claude-legend:auto-update-check";

export interface UpdateSupport {
  version: string;
  /** False for .deb/.rpm installs, which the package manager must update. */
  selfUpdate: boolean;
}

/** Called around installing, so open sessions are restored after the restart. */
export interface InstallHooks {
  before: () => void;
  failed: () => void;
}

let support: UpdateSupport | null = null;

export async function updateSupport(): Promise<UpdateSupport> {
  support ??= await invoke<UpdateSupport>("update_support");
  return support;
}

function stored(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Preference just won't be remembered.
  }
}

export function autoCheckEnabled(): boolean {
  return stored(AUTO_CHECK_KEY) !== "off";
}

export function setAutoCheck(enabled: boolean) {
  store(AUTO_CHECK_KEY, enabled ? "on" : "off");
}

/**
 * Notes of the release as currently shown on GitHub. latest.json carries the
 * notes as they were when the release was built, before they were written, so
 * they are only a fallback.
 */
async function releaseNotes(version: string, fallback: string | undefined): Promise<string> {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 5000);
  try {
    const response = await fetch(`${RELEASE_API_URL}/v${version}`, {
      headers: { Accept: "application/vnd.github+json" },
      signal: abort.signal,
    });
    if (response.ok) {
      const release = (await response.json()) as { body?: string };
      if (release.body?.trim()) return release.body;
    }
  } catch {
    // Offline or rate-limited: fall back to latest.json.
  } finally {
    clearTimeout(timer);
  }
  return fallback ?? "";
}

/** Inline **bold** and `code`, built as text nodes so release notes can't inject markup. */
function inline(text: string): Node[] {
  return text.split(/(\*\*[^*]+\*\*|`[^`]+`)/).filter(Boolean).map((part) => {
    if (part.startsWith("**") && part.endsWith("**")) return Object.assign(document.createElement("strong"), { textContent: part.slice(2, -2) });
    if (part.startsWith("`") && part.endsWith("`")) return Object.assign(document.createElement("code"), { textContent: part.slice(1, -1) });
    return document.createTextNode(part);
  });
}

/**
 * The user-facing part of the notes (from the first heading, without the
 * install table and the generated PR list), as headings, lists and paragraphs.
 */
function renderNotes(markdown: string): Node[] {
  let text = markdown.replace(/\r/g, "");
  const firstHeading = text.search(/^#{1,6} /m);
  if (firstHeading > 0) text = text.slice(firstHeading);
  const end = text.search(/^#{1,6} (Installation|What's Changed)/m);
  if (end !== -1) text = text.slice(0, end);

  const nodes: Node[] = [];
  let list: HTMLUListElement | null = null;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    const item = /^[-*] (.*)/.exec(line);
    if (item) {
      list ??= nodes[nodes.push(document.createElement("ul")) - 1] as HTMLUListElement;
      const li = document.createElement("li");
      li.append(...inline(item[1]));
      list.append(li);
      continue;
    }
    list = null;
    if (!line || line.startsWith("|") || line.startsWith(">")) continue;
    const heading = /^#{1,6} (.*)/.exec(line);
    const node = document.createElement(heading ? "h3" : "p");
    node.append(...inline(heading ? heading[1] : line));
    nodes.push(node);
  }
  return nodes;
}

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

export async function showUpdate(update: Update, hooks: InstallHooks) {
  const { selfUpdate } = await updateSupport();
  const dialog = $<HTMLDialogElement>("#update-dialog");
  const install = $<HTMLButtonElement>("#update-install");
  const buttons = [install, $<HTMLButtonElement>("#update-later")];
  const progress = $<HTMLProgressElement>("#update-progress");
  const error = $("#update-error");

  $("#update-title").textContent = `Claude Legend ${update.version} est disponible`;
  $("#update-versions").textContent = `Version installée : ${update.currentVersion}`;
  const notes = $("#update-notes");
  notes.replaceChildren(...renderNotes(update.body ?? ""));
  releaseNotes(update.version, update.body).then((body) => notes.replaceChildren(...renderNotes(body)));
  $("#update-hint").textContent = selfUpdate
    ? "Les conversations ouvertes seront fermées puis rouvertes automatiquement après le redémarrage."
    : "Application installée par paquet (.rpm / .deb) : télécharge le nouveau paquet et installe-le comme le précédent.";
  install.textContent = selfUpdate ? "Installer et redémarrer" : "Télécharger";
  progress.hidden = true;
  error.textContent = "";
  buttons.forEach((b) => (b.disabled = false));

  $("#update-later").onclick = () => dialog.close();
  install.onclick = async () => {
    if (!selfUpdate) {
      await openUrl(`${RELEASES_URL}/tag/v${update.version}`);
      dialog.close();
      return;
    }
    buttons.forEach((b) => (b.disabled = true));
    progress.hidden = false;
    progress.removeAttribute("value");
    let total = 0;
    let received = 0;
    hooks.before();
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") total = event.data.contentLength ?? 0;
        if (event.event === "Progress" && total) {
          received += event.data.chunkLength;
          progress.value = Math.round((received / total) * 100);
        }
      });
      await relaunch();
    } catch (e) {
      hooks.failed();
      progress.hidden = true;
      error.textContent = `La mise à jour a échoué : ${e}`;
      buttons.forEach((b) => (b.disabled = false));
    }
  };
  // Escape must not close the dialog in the middle of an install.
  dialog.oncancel = (e) => install.disabled && e.preventDefault();
  if (!dialog.open) dialog.showModal();
}

/** Silent check at launch. */
export async function checkAtStartup(hooks: InstallHooks) {
  if (!autoCheckEnabled()) return;
  try {
    const update = await check();
    if (update) await showUpdate(update, hooks);
  } catch {
    // Offline, or no published release with update information yet.
  }
}

/** Manual check from the settings; returns a status message. */
export async function checkNow(hooks: InstallHooks): Promise<string> {
  try {
    const update = await check();
    if (!update) return "Tu as la dernière version.";
    await showUpdate(update, hooks);
    return `Version ${update.version} disponible.`;
  } catch (e) {
    // GitHub answers 404 until a release built with update information is published.
    if (/valid release JSON/i.test(String(e))) return "Aucune mise à jour n'est publiée pour le moment.";
    return `Impossible de vérifier les mises à jour : ${e}`;
  }
}
