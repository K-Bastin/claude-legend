// Toolbar icons: one 16×16 line style, so every button has the same size and
// weight whatever fonts the system has (text glyphs like ＋ ⟳ ⚙ ☾ differ a lot).

const PATHS = {
  plus: '<path d="M8 3v10M3 8h10"/>',
  rules: '<path d="M4.5 2.5h7a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1z"/><path d="M6 6h4M6 8.5h4M6 11h2.5"/>',
  sync: '<path d="M13 8a5 5 0 1 1-1.46-3.54"/><path d="M13 2.5v2.5h-2.5"/>',
  moon: '<path d="M13 9.5A5.5 5.5 0 1 1 6.5 3a4.5 4.5 0 0 0 6.5 6.5z"/>',
  sun: '<circle cx="8" cy="8" r="2.75"/><path d="M8 1.5v1.5M8 13v1.5M1.5 8H3M13 8h1.5M3.4 3.4l1.06 1.06M11.54 11.54l1.06 1.06M3.4 12.6l1.06-1.06M11.54 4.46l1.06-1.06"/>',
  archive: '<path d="M2.5 3.5h11v3h-11z"/><path d="M3.5 6.5v6h9v-6M6.5 9h3"/>',
  restore: '<path d="M2.5 3.5h11v3h-11z"/><path d="M3.5 6.5v6h9v-6M8 12V8.5M6.5 10 8 8.5 9.5 10"/>',
  settings: '<path d="M2.5 4.5h11M2.5 11.5h11"/><circle cx="10.5" cy="4.5" r="1.75" fill="currentColor"/><circle cx="5.5" cy="11.5" r="1.75" fill="currentColor"/>',
};

export type IconName = keyof typeof PATHS;

export function icon(name: IconName): string {
  return `<svg class="ico" viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${PATHS[name]}</svg>`;
}
