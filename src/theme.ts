// Light / dark theme. The choice is a per-machine preference kept in local
// storage; "system" follows the desktop setting.
import type { ITheme } from "@xterm/xterm";

export type ThemeMode = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

const THEME_KEY = "claude-legend:theme";

export const TERMINAL_THEMES: Record<ResolvedTheme, ITheme> = {
  dark: {
    background: "#1a1918",
    foreground: "#ece8e1",
    cursor: "#d97757",
    cursorAccent: "#1a1918",
    selectionBackground: "#d9775755",
    black: "#1a1918",
    brightBlack: "#6b645c",
  },
  // xterm's default ANSI colours are made for dark backgrounds (its yellow is
  // unreadable on white), so the light theme defines all sixteen.
  light: {
    background: "#faf9f5",
    foreground: "#1f1e1d",
    cursor: "#c15f3c",
    cursorAccent: "#faf9f5",
    selectionBackground: "#c15f3c33",
    black: "#1f1e1d",
    red: "#b8322a",
    green: "#3f7d20",
    yellow: "#8a5a00",
    blue: "#1f5fbf",
    magenta: "#8e3fa8",
    cyan: "#16727d",
    white: "#8a857a",
    brightBlack: "#6b665c",
    brightRed: "#d0463c",
    brightGreen: "#4d8a3b",
    brightYellow: "#a86f0a",
    brightBlue: "#2f73d9",
    brightMagenta: "#a557c2",
    brightCyan: "#1f8f9c",
    brightWhite: "#3d3a35",
  },
};

const systemDark = window.matchMedia("(prefers-color-scheme: dark)");

export function loadThemeMode(): ThemeMode {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === "light" || saved === "dark" || saved === "system") return saved;
  } catch {
    // Fall back to the system theme.
  }
  return "system";
}

export function saveThemeMode(mode: ThemeMode) {
  try {
    localStorage.setItem(THEME_KEY, mode);
  } catch {
    // The choice just won't survive a restart.
  }
}

export function resolveTheme(mode: ThemeMode): ResolvedTheme {
  if (mode === "system") return systemDark.matches ? "dark" : "light";
  return mode;
}

/** Applies the page colours and returns the theme actually in use. */
export function applyTheme(mode: ThemeMode): ResolvedTheme {
  const resolved = resolveTheme(mode);
  document.documentElement.dataset.theme = resolved;
  return resolved;
}

export function onSystemThemeChange(callback: () => void) {
  systemDark.addEventListener("change", callback);
}
