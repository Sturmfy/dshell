// dshell's chrome UI: the tab strip.
//
// Deliberately *only* the strip — and the strip is the window's title bar. It is
// the sole drag region and holds the only window buttons, which is why it is its
// own webview pinned above the content area rather than something that can be
// covered.
//
// Every page in this window is a tab, including dshell's own (first-run wizard,
// new connection, settings). The strip therefore needs no special cases for
// them beyond cosmetics: a page tab has an icon, cannot be renamed, and may
// refuse to close.
//
// The strip owns no state of its own: everything comes from the shared `Store`,
// which is fed by Rust.

import { api, asFailure, events, listen, win } from './ipc';
import { Store } from './store';
import type { PageKind, TabPhase, TabView } from './types';

const strip = document.getElementById('tabstrip') as HTMLElement;
const controls = document.getElementById('window-controls') as HTMLElement;

const store = new Store();

interface TabTheme {
  tabId: string;
  background: string;
  foreground: string;
  mode: 'dark' | 'light';
}

/** Latest DSH sidebar surface reported by each content tab. */
const tabThemes = new Map<string, TabTheme>();

let renaming: string | null = null;
let renameValue = '';

// ------------------------------------------------------------------ helpers

function h(tag: string, props: Record<string, unknown> = {}, ...children: unknown[]): HTMLElement {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === null || value === undefined || value === false) continue;
    if (key === 'class') el.className = String(value);
    else if (key === 'text') el.textContent = String(value);
    else if (key === 'html') el.innerHTML = String(value);
    else if (key === 'dataset') Object.assign(el.dataset, value as object);
    else if (key.startsWith('on') && typeof value === 'function') {
      el.addEventListener(key.slice(2).toLowerCase(), value as EventListener);
    } else el.setAttribute(key, String(value));
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.appendChild(typeof child === 'string' ? document.createTextNode(child) : (child as Node));
  }
  return el;
}

/** A page tab is a dshell surface, not a DSH session; mark it so the two read
 *  differently at a glance. Icons are distinct because two tabs sharing one is
 *  how a user loses track of which is which. */
const PAGE_ICON: Record<PageKind, string> = {
  onboarding: '✦',
  new: '＋',
  settings: '⚙',
};

function tabIcon(tab: TabView): string | null {
  const kind = tab.pageKind as PageKind | null | undefined;
  return kind ? (PAGE_ICON[kind] ?? null) : null;
}

function dotClass(phase: TabPhase): string {
  switch (phase) {
    case 'ready':
      return 'tab__dot--ready';
    case 'failed':
      return 'tab__dot--failed';
    case 'exited':
      return 'tab__dot--exited';
    case 'preparing':
    case 'starting':
    case 'waitingForReady':
      return 'tab__dot--busy';
    default:
      return 'tab__dot--idle';
  }
}

const PHASE_LABEL: Record<TabPhase, string> = {
  idle: '空闲',
  preparing: '准备中',
  starting: '启动中',
  waitingForReady: '加载界面',
  ready: '就绪',
  failed: '失败',
  exited: '已退出',
  needsToken: '需要启动 URL',
};

function tooltip(tab: TabView): string {
  const lines = [`${tab.title}  ·  ${PHASE_LABEL[tab.phase] ?? tab.phase}`, `类型：${tab.badge}`];
  if (!tab.closable) lines.push('首次配置完成前不能关闭');
  if (tab.origin) lines.push(`地址：${tab.origin}`);
  if (tab.workspace) lines.push(`起始目录：${tab.workspace}`);
  else if (tab.badge === 'local') lines.push('起始目录：用户主目录（项目目录在 DSH 内选择）');
  if (tab.dshHomeEffective) lines.push(`DSH_HOME：${tab.dshHomeEffective}`);
  if (tab.runtime) lines.push(`运行时：${tab.runtime}`);
  if (tab.pid) lines.push(`PID：${tab.pid}`);
  if (tab.uptimeMs !== null && tab.uptimeMs !== undefined) {
    lines.push(`运行时长：${Math.round(tab.uptimeMs / 1000)}s`);
  }
  if (tab.hasSessionCookie) lines.push('会话：已建立');
  if (tab.failure) lines.push(`失败：${tab.failure.message}`);
  return lines.join('\n');
}

// -------------------------------------------------------------------- render

function render() {
  const { tabs, activeId } = store.state;
  strip.innerHTML = '';

  for (const tab of tabs) {
    const active = tab.id === activeId;
    const isPage = Boolean(tab.pageKind);
    const el = h('div', {
      class: `tab ${active ? 'tab--active' : ''} ${isPage ? 'tab--page' : ''}`,
      title: tooltip(tab),
      dataset: { tabId: tab.id },
      draggable: 'true',
    });

    const icon = tabIcon(tab);
    if (icon) el.appendChild(h('span', { class: 'tab__icon', text: icon }));
    else el.appendChild(h('span', { class: `tab__dot ${dotClass(tab.phase)}` }));

    if (renaming === tab.id) {
      const input = h('input', { class: 'tab__rename', type: 'text', value: renameValue }) as HTMLInputElement;
      input.addEventListener('keydown', (event) => {
        const key = (event as KeyboardEvent).key;
        if (key === 'Enter') void commitRename();
        if (key === 'Escape') {
          renaming = null;
          render();
        }
        event.stopPropagation();
      });
      input.addEventListener('blur', () => void commitRename());
      input.addEventListener('input', () => {
        renameValue = input.value;
      });
      el.appendChild(input);
      window.setTimeout(() => {
        input.focus();
        input.select();
      }, 0);
    } else {
      el.appendChild(
        h('span', {
          class: 'tab__title',
          text: (tab.phase === 'failed' ? '⚠ ' : '') + (tab.title || '未命名'),
        }),
      );
    }

    if (tab.plaintext) {
      el.appendChild(h('span', { class: 'tab__warn', text: '⚠', title: '此连接未加密（明文 HTTP）' }));
    }
    el.appendChild(h('span', { class: 'tab__badge', text: tab.badge }));

    // The close affordance is absent, not merely disabled, for a tab Rust would
    // refuse to close: a button that exists and does nothing is worse than no
    // button, and the reason is in the tooltip.
    if (tab.closable) {
      el.appendChild(
        h('button', {
          class: 'tab__close',
          title: '关闭选项卡 (Ctrl+W)',
          html: '&times;',
          onclick: (event: Event) => {
            event.stopPropagation();
            void store.close(tab.id);
          },
        }),
      );
    } else {
      el.appendChild(
        h('span', {
          class: 'tab__pinned',
          title: '首次配置完成前不能关闭',
          text: '•',
        }),
      );
    }

    el.addEventListener('click', (event) => {
      if ((event.target as HTMLElement).closest('.tab__close')) return;
      void store.activate(tab.id);
    });
    el.addEventListener('auxclick', (event) => {
      const middle = (event as MouseEvent).button === 1;
      if (!middle) return;
      event.preventDefault();
      if (tab.closable) void store.close(tab.id);
    });
    el.addEventListener('dblclick', (event) => {
      if ((event.target as HTMLElement).closest('.tab__close')) return;
      // Page titles are fixed — Rust rejects the rename anyway, so do not offer
      // the editing affordance for them.
      if (isPage) return;
      renaming = tab.id;
      renameValue = tab.title;
      render();
    });
    el.addEventListener('contextmenu', (event) => {
      event.preventDefault();
      const { clientX, clientY } = event as MouseEvent;
      void api
        .showTabMenu(tab.id, clientX, clientY)
        .catch((error) => store.toast(asFailure(error).message, true));
    });
    el.addEventListener('dragstart', (event) => {
      (event as DragEvent).dataTransfer?.setData('text/plain', tab.id);
      el.classList.add('tab--dragging');
    });
    el.addEventListener('dragend', () => el.classList.remove('tab--dragging'));
    el.addEventListener('dragover', (event) => {
      event.preventDefault();
      const rect = el.getBoundingClientRect();
      const after = (event as DragEvent).clientX > rect.left + rect.width / 2;
      el.classList.toggle('tab__drop-after', after);
      el.classList.toggle('tab__drop-before', !after);
    });
    el.addEventListener('dragleave', () => el.classList.remove('tab__drop-before', 'tab__drop-after'));
    el.addEventListener('drop', (event) => {
      event.preventDefault();
      const dragged = (event as DragEvent).dataTransfer?.getData('text/plain');
      el.classList.remove('tab__drop-before', 'tab__drop-after');
      if (!dragged || dragged === tab.id) return;
      const rect = el.getBoundingClientRect();
      const after = (event as DragEvent).clientX > rect.left + rect.width / 2;
      const ids = tabs.map((candidate) => candidate.id).filter((id) => id !== dragged);
      const index = ids.indexOf(tab.id);
      if (index < 0) return;
      ids.splice(after ? index + 1 : index, 0, dragged);
      void store.reorder(ids);
    });

    strip.appendChild(el);
  }

  // "+" always opens the new-connection page. It is a tab, so pressing it twice
  // focuses the same tab rather than stacking a second one.
  strip.appendChild(
    h('button', {
      class: `tab__new ${isPageActive('new') ? 'tab__new--active' : ''}`,
      title: '新建连接 (Ctrl+T)',
      text: '+',
      onclick: (event: Event) => {
        event.stopPropagation();
        void store.openPage('new');
      },
    }),
  );

  // Overflow lists every tab instead of scrolling sideways: horizontal scrolling
  // ruins drag-to-reorder.
  if (tabs.length > 6) {
    strip.appendChild(
      h('button', {
        class: 'tab__overflow',
        title: '所有选项卡',
        text: '»',
        onclick: (event: Event) => {
          event.stopPropagation();
          const last = tabs[tabs.length - 1];
          if (last) {
            void api
              .showTabMenu(last.id, 12, 44)
              .catch((error) => store.toast(asFailure(error).message, true));
          }
        },
      }),
    );
  }

  controls.innerHTML = '';
  controls.appendChild(
    h('button', {
      class: `wc ${isPageActive('settings') ? 'wc--active' : ''}`,
      title: '设置',
      html: '&#9881;',
      onclick: () => void store.openPage('settings'),
    }),
  );
  controls.appendChild(
    h('button', {
      class: 'wc',
      title: '最小化',
      html: '&#8212;',
      onclick: () => void win.minimize().catch((error) => store.toast(String(error), true)),
    }),
  );
  controls.appendChild(
    h('button', {
      class: 'wc',
      title: '最大化',
      html: '&#9723;',
      onclick: () => void win.toggleMaximize().catch((error) => store.toast(String(error), true)),
    }),
  );
  controls.appendChild(
    h('button', {
      class: 'wc wc--close',
      title: '关闭窗口',
      html: '&#10005;',
      onclick: () => void win.close().catch((error) => store.toast(String(error), true)),
    }),
  );

  pruneTabThemes();
  applyActiveTheme();
}

function isPageActive(kind: PageKind): boolean {
  const active = store.state.tabs.find((tab) => tab.id === store.state.activeId);
  return active?.pageKind === kind;
}

/**
 * Paint the active tab with the DSH surface it sits above. Unselected tabs stay
 * transparent and keep the strip's own muted text; only the active card takes
 * the page's sidebar colour and foreground.
 */
function applyActiveTheme(): void {
  const chrome = document.getElementById('chrome');
  if (!chrome) return;
  const active = store.state.tabs.find((tab) => tab.id === store.state.activeId);
  const theme = active ? tabThemes.get(active.id) : undefined;
  if (!theme) {
    chrome.style.removeProperty('--tab-active-bg');
    chrome.style.removeProperty('--tab-active-fg');
    chrome.style.removeProperty('--tab-active-border');
    return;
  }
  chrome.style.setProperty('--tab-active-bg', theme.background);
  chrome.style.setProperty('--tab-active-fg', theme.foreground);
  chrome.style.setProperty(
    '--tab-active-border',
    `color-mix(in srgb, ${theme.foreground} 18%, transparent)`,
  );
}

function pruneTabThemes(): void {
  for (const id of tabThemes.keys()) {
    if (!store.state.tabs.some((tab) => tab.id === id)) tabThemes.delete(id);
  }
}

function beginTabRename(tabId: string): void {
  const tab = store.state.tabs.find((candidate) => candidate.id === tabId);
  if (!tab || tab.pageKind) return;
  renaming = tabId;
  renameValue = tab.title;
  render();
}

/**
 * Run one native-menu selection. The menu itself lives in Rust; all actions
 * stay here so their toasts, tab refresh, and rename editor keep one owner.
 * @param tabId - the tab whose menu was opened.
 * @param action - the menu item suffix.
 */
function performTabMenuAction(tabId: string, action: string): void {
  switch (action) {
    case 'rename':
      beginTabRename(tabId);
      break;
    case 'reload':
      void store.reload(tabId);
      break;
    case 'restart':
      void store.restart(tabId);
      break;
    case 'open-browser':
      void api
        .openInSystemBrowser(tabId)
        .catch((error) => store.toast(asFailure(error).message, true));
      break;
    case 'clear-data':
      void api
        .clearTabData(tabId)
        .then(() => store.toast('已清空该实例的站点数据'))
        .catch((error) => store.toast(asFailure(error).message, true));
      break;
    case 'close-others':
      void store.closeOthers(tabId);
      break;
    case 'close':
      void store.close(tabId);
      break;
    default:
      break;
  }
}

async function commitRename() {
  const id = renaming;
  const value = renameValue.trim();
  renaming = null;
  render();
  if (id && value) await store.rename(id, value);
}

// ----------------------------------------------------------------- shortcuts

function activeId(): string | null {
  return store.state.activeId;
}

function cycleTab(direction: number) {
  const { tabs } = store.state;
  if (tabs.length === 0) return;
  const current = tabs.findIndex((tab) => tab.id === activeId());
  const next = (current + direction + tabs.length) % tabs.length;
  void store.activate(tabs[next].id);
}

function installShortcuts() {
  window.addEventListener('keydown', (event) => {
    const ctrl = event.ctrlKey || event.metaKey;
    if (!ctrl) return;
    const index = Number.parseInt(event.key, 10);
    switch (event.key.toLowerCase()) {
      case 't':
        event.preventDefault();
        void store.openPage('new');
        break;
      case 'w':
        event.preventDefault();
        // Rust refuses the one close that must not happen; asking anyway keeps
        // the rule in a single place.
        if (activeId()) void store.close(activeId()!);
        break;
      case 'r':
        event.preventDefault();
        if (activeId()) void store.reload(activeId()!);
        break;
      case 'tab':
        event.preventDefault();
        cycleTab(event.shiftKey ? -1 : 1);
        break;
      case ',':
        event.preventDefault();
        void store.openPage('settings');
        break;
      default:
        if (!Number.isNaN(index) && index >= 1 && index <= 9) {
          event.preventDefault();
          const tabs = store.state.tabs;
          const target = index === 9 ? tabs[tabs.length - 1] : tabs[index - 1];
          if (target) void store.activate(target.id);
        }
        break;
    }
  });

  // Rust re-emits accelerators that arrive while a DSH content webview has focus,
  // so the strip is not the only path to them.
  void listen<{ action: string }>(events.shortcut, ({ action }) => {
    switch (action) {
      case 'new-tab':
        void store.openPage('new');
        break;
      case 'close-tab':
        if (activeId()) void store.close(activeId()!);
        break;
      case 'next-tab':
        cycleTab(1);
        break;
      case 'prev-tab':
        cycleTab(-1);
        break;
      default:
        break;
    }
  });
}

// ---------------------------------------------------------------------- boot

void (async () => {
  installShortcuts();
  void listen<{ tabId: string; action: string }>(events.tabMenu, ({ tabId, action }) => {
    performTabMenuAction(tabId, action);
  });
  void listen<TabTheme>(events.tabTheme, (theme) => {
    if (!theme || typeof theme.tabId !== 'string') return;
    tabThemes.set(theme.tabId, theme);
    applyActiveTheme();
  });
  store.subscribe(render);
  await store.start();
  render();
  window.setInterval(() => void store.refreshTabs(), 4000);
})();
