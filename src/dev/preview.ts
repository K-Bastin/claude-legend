// Browser preview: lets the UI run in a regular browser (`npm run dev`, then
// http://localhost:1420) with fake data and fake terminals, so layout changes can
// be checked without the Tauri backend. Never loaded inside the app.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";

interface FakePty {
  channel: { onmessage: (event: unknown) => void };
  cols: number;
  rows: number;
  title: string;
  input: string;
}

const HOUR = 3_600_000;
const now = Date.now();

const settings = {
  sync: { kind: "folder", path: "/home/preview/Sync" } as Record<string, unknown>,
  claudePath: null,
  extraArgs: "",
  machineName: "pc-preview",
  fontSize: 14,
  syncIntervalSecs: 60,
};

const sessions = [
  session("s1", "Refonte de la synchronisation", "claude-legend", "/home/preview/dev/claude-legend", now - 0.1 * HOUR, "develop"),
  session("s2", "Écran partagé en 4 panneaux", "claude-legend", "/home/preview/dev/claude-legend", now - 3 * HOUR, "feature/split"),
  session("s3", "Corriger le calcul des adresses", "cohabsys", "/home/preview/dev/cohabsys", now - 26 * HOUR, "main"),
  session("s4", "Migration de la base de données", "cohabsys", "/home/preview/dev/cohabsys", now - 50 * HOUR, "main", { lockedBy: "pc-bureau" }),
  session("s5", "Tableau de bord des ventes", "tric-house", null, now - 80 * HOUR, null, { remote: true }),
];

function session(
  id: string,
  title: string,
  projectName: string,
  cwd: string | null,
  updatedAt: number,
  gitBranch: string | null,
  opts: { lockedBy?: string; remote?: boolean } = {},
) {
  return {
    id,
    title,
    firstPrompt: `${title} — premier message de la conversation.`,
    cwd,
    projectKey: `local-${projectName}`,
    projectName,
    gitBranch,
    promptCount: 12,
    updatedAt,
    location: opts.remote ? "remote" : "local",
    synced: !opts.remote,
    lastMachine: opts.remote ? "pc-bureau" : null,
    lockedBy: opts.lockedBy
      ? { sessionId: id, machineId: "x", machineName: opts.lockedBy, since: now - HOUR, heartbeat: now }
      : null,
    openHere: false,
  };
}

const rules: Record<string, string> = { "local-claude-legend": "Réponds en français.\nLes tests sont en anglais." };

const ptys = new Map<number, FakePty>();
let nextPty = 1;

/** Draws a screen shaped like Claude Code's, with its status line on the very last row. */
function draw(pty: FakePty) {
  const { cols, rows } = pty;
  const rule = "─".repeat(Math.max(cols - 2, 10));
  const lines = [
    "\x1b[2J\x1b[H",
    `\x1b[38;2;217;119;87m✻\x1b[0m \x1b[1mClaude Code\x1b[0m \x1b[2m(aperçu navigateur)\x1b[0m\r\n\r\n`,
    `\x1b[2m> ${pty.title}\x1b[0m\r\n\r\n`,
    "● Ceci est un faux terminal : il sert à vérifier la mise en page.\r\n",
    `  Taille actuelle : ${cols} colonnes × ${rows} lignes.\r\n`,
    `\x1b[${rows - 3};1H\x1b[38;2;90;120;200m${rule}\x1b[0m`,
    `\x1b[${rows - 2};1H❯ ${pty.input}`,
    `\x1b[${rows - 1};1H\x1b[38;2;90;120;200m${rule}\x1b[0m`,
    `\x1b[${rows};1H  \x1b[33m⏵⏵ auto mode on\x1b[0m \x1b[2m(shift+tab to cycle) · ← for agents\x1b[0m`,
    `\x1b[${rows - 2};${3 + pty.input.length}H`,
  ];
  pty.channel.onmessage({ kind: "data", data: lines.join("") });
}

export function installPreview() {
  mockWindows("main");
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, any>;
      switch (cmd) {
        case "get_settings":
          return settings;
        case "save_settings":
          Object.assign(settings, a.settings);
          return null;
        case "app_info":
          return {
            machineId: "preview",
            claudePath: "/usr/local/bin/claude",
            claudeError: null,
            syncEnabled: settings.sync.kind !== "none",
            syncLabel: `${settings.sync.kind} (aperçu)`,
            lastReport: { at: now - 60_000, pushed: 2, pulled: 1, conflicts: [], errors: [] },
            home: "/home/preview",
          };
        case "list_sessions":
          return sessions;
        case "lock_status":
          return sessions.find((s) => s.id === a.sessionId)?.lockedBy ?? null;
        case "sync_now":
          return { at: Date.now(), pushed: 0, pulled: 0, conflicts: [], errors: [] };
        case "map_project":
          return null;
        case "has_sync_secret":
          return false;
        case "test_sync":
          if (a.target.kind === "sftp" && !a.target.fingerprint) {
            return { ok: false, message: "", unknownFingerprint: "SHA256:aperçuAPERÇUaperçu0123456789abcdefABCDEF" };
          }
          return { ok: true, message: "Connexion réussie (aperçu).", unknownFingerprint: null };
        case "open_session": {
          const id = nextPty++;
          const sessionId = a.request.sessionId ?? `new-${id}`;
          const title = sessions.find((s) => s.id === sessionId)?.title ?? "Nouvelle session";
          const pty: FakePty = { channel: a.channel, cols: a.request.cols, rows: a.request.rows, title, input: "" };
          ptys.set(id, pty);
          setTimeout(() => draw(pty), 50);
          return { ptyId: id, sessionId, warnings: [] };
        }
        case "pty_write": {
          const pty = ptys.get(a.id);
          if (!pty) return null;
          const data: string = a.data;
          if (data === "\x7f") pty.input = pty.input.slice(0, -1);
          else if (data === "\r") pty.input = "";
          else if (!data.startsWith("\x1b")) pty.input += data;
          draw(pty);
          return null;
        }
        case "pty_resize": {
          const pty = ptys.get(a.id);
          if (pty) {
            pty.cols = a.cols;
            pty.rows = a.rows;
            draw(pty);
          }
          return null;
        }
        case "pty_kill": {
          const pty = ptys.get(a.id);
          ptys.delete(a.id);
          pty?.channel.onmessage({ kind: "exit", code: 0 });
          return null;
        }
        case "rules_keys":
          return Object.keys(rules).filter((k) => rules[k].trim());
        case "get_project_rules":
          return rules[a.projectKey] ?? "";
        case "save_project_rules":
          rules[a.projectKey] = a.rules;
          return null;
        case "update_support":
          return { version: "0.2.0", selfUpdate: true };
        case "plugin:updater|check":
          return {
            rid: 1,
            currentVersion: "0.2.0",
            version: "0.3.0",
            date: new Date().toISOString(),
            body: "Claude Code dans une application desktop.\n\n## Nouveautés\n\n- **Mises à jour** intégrées\n- Aperçu des notes\n\n## What's Changed\n* liste générée",
            rawJson: {},
          };
        case "plugin:updater|download_and_install": {
          const channel = a.onEvent as { onmessage: (e: unknown) => void };
          const total = 5_000_000;
          channel.onmessage({ event: "Started", data: { contentLength: total } });
          return new Promise((resolve) => {
            let sent = 0;
            const timer = setInterval(() => {
              sent += 500_000;
              channel.onmessage({ event: "Progress", data: { chunkLength: 500_000 } });
              if (sent >= total) {
                clearInterval(timer);
                channel.onmessage({ event: "Finished" });
                resolve(null);
              }
            }, 150);
          });
        }
        case "plugin:process|restart":
          location.reload();
          return null;
        case "plugin:clipboard-manager|read_text":
          return "";
        case "plugin:dialog|open":
          return "/home/preview/dev/nouveau-projet";
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}
