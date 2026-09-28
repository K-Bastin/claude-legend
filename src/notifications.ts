// Desktop notifications: Claude waiting or done in a conversation that is not
// in view, and plan usage close to its limit.
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";

const ENABLED_KEY = "claude-legend:notifications";
/** Quota windows already alerted, so each one is only notified once. */
const QUOTA_ALERTED_KEY = "claude-legend:quota-alerted";

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

export function notificationsEnabled(): boolean {
  return stored(ENABLED_KEY) !== "off";
}

export async function setNotificationsEnabled(enabled: boolean) {
  store(ENABLED_KEY, enabled ? "on" : "off");
  if (enabled) await permitted();
}

async function permitted(): Promise<boolean> {
  try {
    return (await isPermissionGranted()) || (await requestPermission()) === "granted";
  } catch {
    return false;
  }
}

export async function notify(title: string, body: string) {
  if (!notificationsEnabled() || !(await permitted())) return;
  try {
    sendNotification({ title, body });
  } catch {
    // No notification service on this desktop.
  }
}

/** Notifies once per `key` (a quota window), however often usage is refreshed. */
export function notifyOnce(key: string, title: string, body: string) {
  let seen: string[] = [];
  try {
    seen = JSON.parse(stored(QUOTA_ALERTED_KEY) ?? "[]");
  } catch {
    // Corrupt value: start over.
  }
  if (!Array.isArray(seen) || seen.includes(key)) return;
  store(QUOTA_ALERTED_KEY, JSON.stringify([...seen, key].slice(-20)));
  notify(title, body);
}
