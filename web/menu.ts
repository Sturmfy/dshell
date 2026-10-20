// The tab strip's context menu.
//
// It is one long-lived window, created before the first right-click and then
// only shown, moved and hidden: a fresh window per open meant a fresh WebView2
// environment (browser process, renderer, document load) on the interaction
// path, which is what made the menu feel slow. The popup therefore owns no
// per-open state of its own — it pulls the current payload on load and listens
// for later opens, so an open that raced the document still lands.

import { api, events, listen } from './ipc';
import type { TabMenuPayload, TabMenuTheme } from './types';
import './menu.css';

type Entry =
  | { kind: 'item'; action: string; label: string; danger?: boolean }
  | { kind: 'sep' };

const ICONS: Record<string, string> = {
  rename: '<path d="M3 13l1-3 7-7 2 2-7 7-3 1z"/><path d="M10 4l2 2"/>',
  reload: '<path d="M13 8a5 5 0 1 1-1.4-3.4"/><path d="M13 3v4H9"/>',
  restart: '<path d="M8 2v6"/><path d="M5.2 4.2a4.5 4.5 0 1 0 5.6 0"/>',
  'open-browser':
    '<path d="M9 3h4v4"/><path d="M13 3 7.5 8.5"/><path d="M12 10v2.5a.5.5 0 0 1-.5.5h-8a.5.5 0 0 1-.5-.5v-8a.5.5 0 0 1 .5-.5H6"/>',
  'clear-data':
    '<path d="M3 4h10"/><path d="M6 4V3h4v1"/><path d="M5 4l.5 9h5L11 4"/><path d="M7 6.5v4"/><path d="M9 6.5v4"/>',
  'close-others':
    '<rect x="3" y="5" width="8" height="8" rx="1"/><path d="M5 3h6l2 2v6"/>',
  close: '<path d="M4 4l8 8"/><path d="M12 4l-8 8"/>',
};

/** The theme custom properties the popup repaints when an open carries one. */
const THEME_VARS = ['--bg', '--fg', '--muted', '--hover', '--border', '--sep', '--danger'] as const;

const root = document.getElementById('menu');

/** The payload behind the menu currently on screen. */
let current: TabMenuPayload | null = null;
/** The pending blur dismissal, if the popup has just lost focus. */
let hideTimer: number | undefined;
/** When the current open stops being cancellable by a blur it caused itself. */
let armedAt = 0;

function entriesFor(payload: TabMenuPayload): Entry[] {
  if (payload.page) {
    return [{ kind: 'item', action: 'close', label: '关闭', danger: true }];
  }
  return [
    { kind: 'item', action: 'rename', label: '重命名' },
    { kind: 'item', action: 'reload', label: '重新加载页面' },
    { kind: 'item', action: 'restart', label: '重启后端进程' },
    { kind: 'sep' },
    { kind: 'item', action: 'open-browser', label: '在系统浏览器中打开' },
    { kind: 'item', action: 'clear-data', label: '清空该实例数据（登录态）' },
    { kind: 'sep' },
    { kind: 'item', action: 'close-others', label: '关闭其他' },
    { kind: 'item', action: 'close', label: '关闭', danger: true },
  ];
}

function icon(action: string): string {
  return `<span class="item__icon"><svg viewBox="0 0 16 16" aria-hidden="true">${ICONS[action] ?? ''}</svg></span>`;
}

function cancelHide(): void {
  if (hideTimer !== undefined) {
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
  }
}

/**
 * Hide the popup, never close it.
 *
 * `close` destroys the window, which is the cost this whole file exists to
 * avoid: Tauri would build a new WebView2 for the next right-click. Hiding also
 * clears the pending payload on the Rust side, so a popup reloaded later cannot
 * resurrect a stale menu.
 */
function hide(): void {
  cancelHide();
  void api.hideTabMenu().catch(() => undefined);
}

/**
 * Paint the popup in the DSH surface of the tab that opened it.
 *
 * The window is transparent and cannot sample the page behind it, so the menu
 * uses the sidebar colour itself rather than the menu's translucent material;
 * hairlines and hover fills are derived from the reported foreground exactly
 * like the strip derives its active-tab border.
 */
function applyTheme(theme: TabMenuTheme | undefined): void {
  const style = document.documentElement.style;
  if (theme === undefined) {
    for (const name of THEME_VARS) style.removeProperty(name);
    style.removeProperty('color-scheme');
    return;
  }
  const dark = theme.mode !== 'light';
  const foreground = theme.foreground;
  style.setProperty('color-scheme', dark ? 'dark' : 'light');
  // Dark surfaces lift the menu off the page; a light sidebar is already the
  // menu's own colour, and darkening it would read as a pressed row.
  style.setProperty(
    '--bg',
    dark ? `color-mix(in srgb, ${theme.background} 88%, ${foreground} 12%)` : theme.background,
  );
  style.setProperty('--fg', foreground);
  style.setProperty('--muted', `color-mix(in srgb, ${foreground} 62%, transparent)`);
  style.setProperty('--hover', `color-mix(in srgb, ${foreground} 10%, transparent)`);
  style.setProperty('--border', `color-mix(in srgb, ${foreground} 16%, transparent)`);
  style.setProperty('--sep', `color-mix(in srgb, ${foreground} 14%, transparent)`);
  style.setProperty('--danger', dark ? '#ff7a7a' : '#c62828');
}

/** Build one open's row set. The popup is reused, so every open starts clean. */
function render(payload: TabMenuPayload): void {
  if (!root) {
    hide();
    return;
  }
  current = payload;
  cancelHide();
  armedAt = performance.now() + 400;
  applyTheme(payload.theme);
  root.replaceChildren();

  const buttons: HTMLButtonElement[] = [];
  for (const entry of entriesFor(payload)) {
    if (entry.kind === 'sep') {
      const separator = document.createElement('div');
      separator.className = 'sep';
      separator.setAttribute('role', 'separator');
      root.appendChild(separator);
      continue;
    }
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `item${entry.danger ? ' item--danger' : ''}`;
    button.setAttribute('role', 'menuitem');
    button.innerHTML = `${icon(entry.action)}<span class="item__label"></span>`;
    const label = button.querySelector('.item__label');
    if (label) label.textContent = entry.label;
    button.addEventListener('click', () => choose(entry.action));
    root.appendChild(button);
    buttons.push(button);
  }

  buttons.forEach((button, index) => {
    button.addEventListener('keydown', (event) => {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        const step = event.key === 'ArrowDown' ? 1 : -1;
        buttons[(index + step + buttons.length) % buttons.length]?.focus();
      }
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        button.click();
      }
    });
  });

  // The window is sized around the rows, not around a guess: report what the
  // panel actually rendered. Rust keeps the popup hidden until this lands, so
  // the first painted menu is already the right size.
  const bounds = root.getBoundingClientRect();
  if (bounds.width >= 1 && bounds.height >= 1) {
    void api
      .tabMenuMeasured(Math.ceil(bounds.width), Math.ceil(bounds.height))
      .catch(() => undefined);
  }

  window.focus();
  buttons[0]?.focus();
}

async function choose(action: string): Promise<void> {
  const payload = current;
  hide();
  if (!payload) return;
  // The strip owns the action (and its toast); a failure there is reported by
  // the strip, not by this closing popup.
  await api.tabMenuChoose(payload.tabId, action).catch(() => undefined);
}

// Once-only wiring: the popup survives across opens, so these must not be
// re-registered per render.
window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') {
    event.preventDefault();
    hide();
  }
});
window.addEventListener('blur', () => {
  // Showing the popup and handing it focus can blur it for a moment; the
  // arming window keeps an open from closing itself before it is usable. A new
  // open cancels a pending timer, so clicking the next tab's menu is not
  // swallowed by the previous one's dismissal.
  cancelHide();
  hideTimer = window.setTimeout(() => {
    hideTimer = undefined;
    if (performance.now() >= armedAt) hide();
  }, 100);
});
window.addEventListener('contextmenu', (event) => event.preventDefault());
document.addEventListener('mousedown', (event) => {
  if (event.target === document.body) hide();
});

void listen<TabMenuPayload>(events.tabMenuOpen, (payload) => render(payload));

// A payload may already be waiting: the menu can be opened before this document
// has finished loading. Pulling it after the listener is live leaves no gap.
void (async () => {
  const pending = await api.tabMenuState().catch(() => null);
  if (pending) render(pending);
})();
