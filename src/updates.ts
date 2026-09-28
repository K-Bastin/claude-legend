// In-app updates from the GitHub releases, through the Tauri updater: the
// update is signed at release time and its signature checked before install.
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { openUrl } from "@tauri-apps/plugin-opener";
import { check, type Update } from "@tauri-apps/plugin-updater";

const RELEASES_URL = "https://github.com/K-Bastin/claude-legend/releases";
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

/** Release notes without the generated PR list, install table and markdown marks. */
function summarizeNotes(body: string | undefined): string {
  if (!body) return "";
  const end = body.search(/^## (Installation|What's Changed)/m);
  return (end === -1 ? body : body.slice(0, end))
    .replace(/^#+\s*/gm, "")
    .replace(/\*\*|`/g, "")
    .trim();
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
  $("#update-notes").textContent = summarizeNotes(update.body);
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
