// Sync target section of the settings dialog.

export type SftpAuth = "password" | "key" | "agent";

export type SyncTarget =
  | { kind: "none" }
  | { kind: "folder"; path: string }
  | {
      kind: "sftp";
      host: string;
      port: number;
      user: string;
      auth: SftpAuth;
      keyPath: string | null;
      path: string;
      fingerprint: string | null;
    }
  | { kind: "ftp"; host: string; port: number; user: string; secure: boolean; path: string }
  | { kind: "webdav"; url: string; user: string };

const DEFAULT_PORTS: Record<string, number> = { sftp: 22, ftp: 21 };

function field(form: HTMLFormElement, name: string) {
  return form.elements.namedItem(name) as HTMLInputElement & HTMLSelectElement;
}

/** Kind plus auth variant, matched against the `data-kinds` attributes. */
function visibleKinds(form: HTMLFormElement): string[] {
  const kind = field(form, "syncKind").value;
  return kind === "sftp" ? [kind, `sftp-${field(form, "sftpAuth").value}`] : [kind];
}

export function updateSyncVisibility(form: HTMLFormElement) {
  const kinds = visibleKinds(form);
  form.querySelectorAll<HTMLElement>("[data-kinds]").forEach((node) => {
    node.hidden = !node.dataset.kinds!.split(" ").some((k) => kinds.includes(k));
  });
  const kind = field(form, "syncKind").value;
  const auth = field(form, "sftpAuth").value;
  form.querySelector("#secret-label")!.textContent =
    kind === "sftp" && auth === "key" ? "Phrase de passe de la clé (si elle en a une)" : "Mot de passe";
  const port = field(form, "port");
  if (!port.value && DEFAULT_PORTS[kind]) port.value = String(DEFAULT_PORTS[kind]);
}

export function fillSyncForm(form: HTMLFormElement, target: SyncTarget) {
  field(form, "syncKind").value = target.kind;
  for (const name of ["folderPath", "webdavUrl", "host", "port", "user", "keyPath", "remotePath", "secret"]) {
    field(form, name).value = "";
  }
  field(form, "sftpAuth").value = "password";
  field(form, "ftpSecure").checked = false;
  switch (target.kind) {
    case "folder":
      field(form, "folderPath").value = target.path;
      break;
    case "webdav":
      field(form, "webdavUrl").value = target.url;
      field(form, "user").value = target.user;
      break;
    case "sftp":
    case "ftp":
      field(form, "host").value = target.host;
      field(form, "port").value = String(target.port);
      field(form, "user").value = target.user;
      field(form, "remotePath").value = target.path;
      if (target.kind === "sftp") {
        field(form, "sftpAuth").value = target.auth;
        field(form, "keyPath").value = target.keyPath ?? "";
      } else {
        field(form, "ftpSecure").checked = target.secure;
      }
      break;
  }
  updateSyncVisibility(form);
}

/**
 * Builds the target from the form. `fingerprint` is the SFTP host key the user
 * approved, only kept while host and port stay the same.
 */
export function readSyncForm(form: HTMLFormElement, fingerprint: string | null): SyncTarget {
  const value = (name: string) => field(form, name).value.trim();
  const kind = value("syncKind");
  const port = (fallback: number) => Number(value("port")) || fallback;
  switch (kind) {
    case "folder":
      return { kind, path: value("folderPath") };
    case "webdav":
      return { kind, url: value("webdavUrl"), user: value("user") };
    case "sftp":
      return {
        kind,
        host: value("host"),
        port: port(22),
        user: value("user"),
        auth: value("sftpAuth") as SftpAuth,
        keyPath: value("keyPath") || null,
        path: value("remotePath") || "claude-legend",
        fingerprint,
      };
    case "ftp":
      return {
        kind,
        host: value("host"),
        port: port(21),
        user: value("user"),
        secure: field(form, "ftpSecure").checked,
        path: value("remotePath") || "claude-legend",
      };
    default:
      return { kind: "none" };
  }
}

/** Human readable reason why the target can't be used yet, if any. */
export function missingSyncField(target: SyncTarget): string | null {
  switch (target.kind) {
    case "folder":
      return target.path ? null : "Choisis le dossier synchronisé.";
    case "webdav":
      if (!/^https?:\/\//.test(target.url)) return "L'adresse WebDAV doit commencer par https://";
      return target.user ? null : "Indique l'utilisateur.";
    case "sftp":
    case "ftp":
      if (!target.host) return "Indique le serveur.";
      if (!target.user) return "Indique l'utilisateur.";
      if (target.kind === "sftp" && target.auth === "key" && !target.keyPath) return "Choisis la clé privée.";
      return null;
    default:
      return null;
  }
}
