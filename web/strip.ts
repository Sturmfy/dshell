// dshell's chrome UI: the tab strip.
//
// Deliberately *only* the strip. dshell's panels (new tab, settings, onboarding)
// live in a second webview below it, because the strip has to stay on screen
// while a panel is open — it is the window's only drag region, and a frameless
// window with no drag region cannot be moved.
//
// The strip owns no state of its own: everything comes from the shared `Store`,
// which is fed by Rust.

import { api, asFailure, events, listen, win } from './ipc';
import { Store } from './store';
import type { TabPhase, TabView } from './types';

const strip = document.getElementById('tabstrip') as HTMLElement;
const controls = document.getElementById('window-controls') as HTMLElement;

const store = new Store();

let renaming: string | null = null;
let renameValue = '';
let menu: { tabId: string; x: number; y: number } | null = null;

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
    const el = h('div', {
      class: `tab ${active ? 'tab--active' : ''}`,
      title: tooltip(tab),
      dataset: { tabId: tab.id },
      draggable: 'true',
    });
    el.appendChild(h('span', { class: `tab__dot ${dotClass(tab.phase)}` }));

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

    el.addEventListener('click', (event) => {
      if ((event.target as HTMLElement).closest('.tab__close')) return;
      void store.activate(tab.id);
    });
    el.addEventListener('auxclick', (event) => {
      if ((event as MouseEvent).button === 1) {
        event.preventDefault();
        void store.close(tab.id);
      }
    });
    el.addEventListener('dblclick', (event) => {
      if ((event.target as HTMLElement).closest('.tab__close')) return;
      renaming = tab.id;
      renameValue = tab.title;
      render();
    });
    el.addEventListener('contextmenu', (event) => {
      event.preventDefault();
      menu = { tabId: tab.id, x: (event as MouseEvent).clientX, y: (event as MouseEvent).clientY };
      render();
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
      el.classList.toggle('tab--drop-after', after);
      el.classList.toggle('tab--drop-before', !after);
    });
    el.addEventListener('dragleave', () => el.classList.remove('tab--drop-before', 'tab--drop-after'));
    el.addEventListener('drop', (event) => {
      event.preventDefault();
      const dragged = (event as DragEvent).dataTransfer?.getData('text/plain');
      el.classList.remove('tab--drop-before', 'tab--drop-after');
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

  strip.appendChild(
    h('button', {
      class: `tab__new ${store.state.panel === 'newTab' ? 'tab__new--active' : ''}`,
      title: '新建选项卡 (Ctrl+T)',
      text: '+',
      onclick: (event: Event) => {
        event.stopPropagation();
        void store.openPanel(store.state.panel === 'newTab' ? 'none' : 'newTab');
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
            menu = { tabId: last.id, x: 12, y: 44 };
            render();
          }
        },
      }),
    );
  }

  controls.innerHTML = '';
  controls.appendChild(
    h('button', {
      class: `wc ${store.state.panel === 'settings' ? 'wc--active' : ''}`,
      title: '设置',
      html: '&#9881;',
      onclick: () => void store.openPanel(store.state.panel === 'settings' ? 'none' : 'settings'),
    }),
  );  controls.appendChild(
    h('button', { class: 'wc', title: '最小化', html: '&#8212;', onclick: () => void win.minimize() }),
  );
  controls.appendChild(
    h('button', { class: 'wc', title: '最大化', html: '&#9723;', onclick: () => void win.toggleMaximize() }),
  );
  controls.appendChild(
    h('button', {
      class: 'wc wc--close',
      title: '关闭窗口',
      html: '&#10005;',
      onclick: () => void win.close(),
    }),
  );

  renderMenu();
}

function renderMenu() {
  document.querySelector('.menu')?.remove();
  if (!menu) return;
  const tab = store.state.tabs.find((candidate) => candidate.id === menu!.tabId);
  const items: [string, () => void, boolean?][] = [
    [
      '重命名',
      () => {
        renaming = menu!.tabId;
        renameValue = tab?.title ?? '';
        menu = null;
        render();
      },
    ],
    ['重新加载页面', () => void store.reload(menu!.tabId)],
    ['重启后端进程', () => void store.restart(menu!.tabId)],
    [
      '在系统浏览器中打开',
      () =>
        void api.openInSystemBrowser(menu!.tabId).catch((error) => store.toast(asFailure(error).message, true)),
    ],
    [
      '清空该实例数据（登录态）',
      () => void api.clearTabData(menu!.tabId).then(() => store.toast('已清空该实例的站点数据')),
    ],
    ['关闭其他', () => void store.closeOthers(menu!.tabId)],
    ['关闭', () => void store.close(menu!.tabId), true],
  ];
  const el = h('div', {
    class: 'menu',
    style: `left:${Math.min(menu.x, window.innerWidth - 250)}px; top:${Math.min(menu.y, window.innerHeight - 270)}px`,
  });
  for (const [label, action, danger] of items) {
    el.appendChild(
      h('button', {
        class: `menu__item ${danger ? 'menu__item--danger' : ''}`,
        text: label,
        onclick: () => {
          menu = null;
          action();
          render();
        },
      }),
    );
    if (label === '清空该实例数据（登录态）') el.appendChild(h('div', { class: 'menu__sep' }));
  }
  document.body.appendChild(el);
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
    if (!ctrl) {
      if (event.key === 'Escape') {
        if (menu) {
          menu = null;
          render();
        } else if (store.state.panel !== 'none') {
          void store.openPanel('none');
        }
      }
      return;
    }
    const index = Number.parseInt(event.key, 10);
    switch (event.key.toLowerCase()) {
      case 't':
        event.preventDefault();
        void store.openPanel(store.state.panel === 'newTab' ? 'none' : 'newTab');
        break;
      case 'w':
        event.preventDefault();
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
        void store.openPanel('settings');
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
        void store.openPanel(store.state.panel === 'newTab' ? 'none' : 'newTab');
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

document.addEventListener('click', (event) => {
  if (menu && !(event.target as HTMLElement).closest('.menu')) {
    menu = null;
    render();
  }
});

// ---------------------------------------------------------------------- boot

void (async () => {
  installShortcuts();
  store.subscribe(render);
  await store.start();
  render();
  window.setInterval(() => void store.refreshTabs(), 4000);
})();
