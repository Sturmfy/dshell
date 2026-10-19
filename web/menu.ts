import { invoke } from '@tauri-apps/api/core';
import './menu.css';

interface TabMenuPayload {
  tabId: string;
  page: boolean;
}

declare global {
  interface Window {
    __DSHELL_TAB_MENU__?: TabMenuPayload;
  }
}

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

function mount(payload: TabMenuPayload): void {
  const root = document.getElementById('menu');
  if (!root) {
    window.close();
    return;
  }

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
    button.addEventListener('click', () => void choose(entry.action));
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

  const armedAt = performance.now() + 400;
  window.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      window.close();
    }
  });
  window.addEventListener('blur', () => {
    window.setTimeout(() => {
      if (performance.now() >= armedAt) window.close();
    }, 100);
  });
  window.addEventListener('contextmenu', (event) => event.preventDefault());
  document.addEventListener('mousedown', (event) => {
    if (event.target === document.body) window.close();
  });

  window.focus();
  buttons[0]?.focus();
}

async function choose(action: string): Promise<void> {
  const payload = window.__DSHELL_TAB_MENU__;
  if (!payload) {
    window.close();
    return;
  }
  try {
    await invoke('tab_menu_choose', { tabId: payload.tabId, action });
  } finally {
    window.close();
  }
}

const payload = window.__DSHELL_TAB_MENU__;
if (payload) {
  mount(payload);
} else {
  window.close();
}
