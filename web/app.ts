// dshell's chrome UI: the tab strip and its panels.
//
// This is the only webview with Tauri IPC access. It renders from a single
// authoritative state object that Rust owns, and every mutation goes back through
// `invoke` — there is no second copy of the truth in the frontend.
//
// Kept intentionally small: a tab strip, a new-connection panel, onboarding and
// settings. The interesting behaviour (process supervision, readiness, failure
// taxonomy) lives in Rust where it can be tested.

import { api, asFailure, events, listen, win } from './ipc';
import type { AppInfo, Config, DshHomeSetting, RuntimeOption, TabPhase, TabView } from './types';

interface NewTabDraft {
  kind: 'launch' | 'attach' | 'remote';
  workspace: string;
  runtimeId: string;
  dshHomeMode: DshHomeSetting['mode'];
  dshHomeCustom: string;
  url: string;
  allowPlaintext: boolean;
  killOnClose: boolean;
  busy: boolean;
  error: string | null;
}

type Panel = 'none' | 'newTab' | 'settings' | 'onboarding';

interface State {
  ready: boolean;
  info: AppInfo | null;
  tabs: TabView[];
  /** The visible tab, as reported by Rust (the frontend never guesses it). */
  activeId: string | null;
  runtimes: RuntimeOption[];
  panel: Panel;
  draft: NewTabDraft;
  renameId: string | null;
  renameValue: string;
  menu: { tabId: string; x: number; y: number } | null;
  toast: { text: string; error: boolean } | null;
  onboardingStep: number;
  onboardingRoot: string;
  onboardingWorkspace: string;
  onboardingWriteEnv: boolean;
  onboardingError: string | null;
}

const state: State = {
  ready: false,
  info: null,
  tabs: [],
  activeId: null,
  runtimes: [],
  panel: 'none',
  draft: {
    kind: 'launch',
    workspace: '',
    runtimeId: 'npm',
    dshHomeMode: 'system',
    dshHomeCustom: '',
    url: '',
    allowPlaintext: false,
    killOnClose: false,
    busy: false,
    error: null,
  },
  renameId: null,
  renameValue: '',
  menu: null,
  toast: null,
  onboardingStep: 0,
  onboardingRoot: '',
  onboardingWorkspace: '',
  onboardingWriteEnv: true,
  onboardingError: null,
};

const root = document.getElementById('chrome') as HTMLElement;

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

function phaseDotClass(phase: TabPhase): string {
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

function shortPath(path: string, max = 46): string {
  if (path.length <= max) return path;
  return `…${path.slice(path.length - max + 1)}`;
}

function toast(text: string, error = false) {
  state.toast = { text, error };
  render();
  window.setTimeout(() => {
    if (state.toast?.text === text) {
      state.toast = null;
      render();
    }
  }, 6000);
}

// --------------------------------------------------------------------- data

async function refreshInfo(refreshEnvironment = false) {
  try {
    state.info = await api.appInfo(refreshEnvironment);
    const config = state.info.config;
    if (!state.draft.workspace) {
      state.draft.workspace = config.workspace ?? config.recentWorkspaces[0] ?? '';
    }
    state.draft.runtimeId = config.defaultRuntime || state.draft.runtimeId;
    state.draft.killOnClose = config.killOnClose;
    state.draft.dshHomeMode = config.dshHome?.mode ?? 'system';
    state.draft.dshHomeCustom = config.dshHome?.mode === 'custom' ? config.dshHome.path : '';
    if (!state.info.config.onboardingDone) {
      state.panel = 'onboarding';
      state.onboardingRoot ||= state.info.root.path;
      state.onboardingWorkspace ||= state.draft.workspace || '';
    }
    if (state.info.bootNote) toast(state.info.bootNote);
  } catch (error) {
    toast(asFailure(error).message, true);
  }
  state.ready = true;
  await refreshTabs();
}

async function refreshTabs() {
  try {
    const snapshot = await api.listTabs();
    state.tabs = snapshot.tabs;
    state.activeId = snapshot.activeId;
  } catch {
    /* the app may be shutting down */
  }
  render();
  checkPanelNeeds();
}

async function refreshRuntimes() {
  try {
    state.runtimes = await api.listRuntimes();
  } catch {
    state.runtimes = [];
  }
  render();
}

/** The chrome webview grows when dshell shows a panel; the window keeps the
 *  active tab parked while that is the case. */
function desiredPanelMode(): 'strip' | 'panel' | 'full' {
  if (state.panel === 'onboarding') return 'full';
  if (state.panel === 'settings' || state.panel === 'newTab') return 'panel';
  return 'strip';
}

let lastMode: string | null = null;
function checkPanelNeeds() {
  const mode = desiredPanelMode();
  if (mode !== lastMode) {
    lastMode = mode;
    void api.setChromeMode(mode).catch(() => undefined);
  }
}

// -------------------------------------------------------------------- render

function render() {
  const mode = desiredPanelMode();
  root.className = `chrome chrome--${mode === 'strip' ? 'strip' : mode}`;
  root.innerHTML = '';

  if (mode === 'full') {
    root.appendChild(renderOnboarding());
    return;
  }

  const row = h('div', { class: 'chrome__row' });
  row.appendChild(h('div', { class: 'chrome__drag', 'data-tauri-drag-region': '' }));
  row.appendChild(renderTabStrip());
  row.appendChild(h('div', { class: 'chrome__spacer', 'data-tauri-drag-region': '' }));
  row.appendChild(renderWindowControls());
  root.appendChild(row);

  if (mode === 'panel') {
    if (state.panel === 'newTab') root.appendChild(renderNewTabPanel());
    if (state.panel === 'settings') root.appendChild(renderSettings());
  }

  if (state.menu) root.appendChild(renderMenu());
  if (state.toast) {
    root.appendChild(
      h('div', { class: `toast ${state.toast.error ? 'toast--error' : ''}`, text: state.toast.text }),
    );
  }
}

function renderTabStrip(): HTMLElement {
  const strip = h('div', { class: 'tabstrip', id: 'tabstrip', 'data-tauri-drag-region': '' });

  for (const tab of state.tabs) {
    const active = tab.phase !== 'needsToken' && isActive(tab.id);
    const el = h('div', {
      class: `tab ${active ? 'tab--active' : ''}`,
      title: tooltipText(tab),
      dataset: { tabId: tab.id },
      draggable: 'true',
    });
    el.appendChild(h('span', { class: `tab__dot ${phaseDotClass(tab.phase)}` }));

    if (state.renameId === tab.id) {
      const input = h('input', {
        class: 'tab__rename',
        type: 'text',
        value: state.renameValue,
      }) as HTMLInputElement;
      input.addEventListener('keydown', (event) => {
        const key = (event as KeyboardEvent).key;
        if (key === 'Enter') void commitRename();
        if (key === 'Escape') {
          state.renameId = null;
          render();
        }
        event.stopPropagation();
      });
      input.addEventListener('blur', () => void commitRename());
      input.addEventListener('input', () => {
        state.renameValue = input.value;
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

    const close = h('button', {
      class: 'tab__close',
      title: '关闭选项卡',
      html: '&times;',
      onclick: (event: Event) => {
        event.stopPropagation();
        void closeTab(tab.id);
      },
    });
    el.appendChild(close);

    el.addEventListener('click', (event) => {
      if ((event.target as HTMLElement).closest('.tab__close')) return;
      void api.activateTab(tab.id).then(refreshTabs);
    });
    el.addEventListener('auxclick', (event) => {
      if ((event as MouseEvent).button === 1) {
        event.preventDefault();
        void closeTab(tab.id);
      }
    });
    el.addEventListener('dblclick', (event) => {
      if ((event.target as HTMLElement).closest('.tab__close')) return;
      state.renameId = tab.id;
      state.renameValue = tab.title;
      render();
    });
    el.addEventListener('contextmenu', (event) => {
      event.preventDefault();
      state.menu = { tabId: tab.id, x: (event as MouseEvent).clientX, y: (event as MouseEvent).clientY };
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
    el.addEventListener('dragleave', () => {
      el.classList.remove('tab--drop-before', 'tab--drop-after');
    });
    el.addEventListener('drop', (event) => {
      event.preventDefault();
      const dragged = (event as DragEvent).dataTransfer?.getData('text/plain');
      el.classList.remove('tab--drop-before', 'tab--drop-after');
      if (!dragged || dragged === tab.id) return;
      const rect = el.getBoundingClientRect();
      const after = (event as DragEvent).clientX > rect.left + rect.width / 2;
      void dropTab(dragged, tab.id, after);
    });

    strip.appendChild(el);
  }

  strip.appendChild(
    h('button', {
      class: 'tab__new',
      title: '新建选项卡 (Ctrl+T)',
      text: '+',
      onclick: (event: Event) => {
        event.stopPropagation();
        openPanel('newTab');
      },
    }),
  );

  // Overflow: Windows Terminal lists every tab instead of scrolling sideways,
  // because horizontal scrolling ruins drag-to-reorder.
  if (state.tabs.length > 6) {
    strip.appendChild(
      h('button', {
        class: 'tab__overflow',
        title: '所有选项卡',
        text: '»',
        onclick: (event: Event) => {
          event.stopPropagation();
          const id = state.tabs[state.tabs.length - 1]?.id;
          if (id) {
            state.menu = { tabId: id, x: 12, y: 40 };
            render();
          }
        },
      }),
    );
  }
  return strip;
}

function isActive(id: string): boolean {
  // Rust owns the activation order; the frontend never guesses which content
  // webview is the visible one.
  return state.activeId === id;
}

function tooltipText(tab: TabView): string {
  const lines = [
    `${tab.title}  ·  ${PHASE_LABEL[tab.phase] ?? tab.phase}`,
    `类型：${tab.badge}`,
  ];
  if (tab.origin) lines.push(`地址：${tab.origin}`);
  if (tab.workspace) lines.push(`工作目录：${tab.workspace}`);
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

function renderWindowControls(): HTMLElement {
  const wrap = h('div', { class: 'window-controls' });
  wrap.appendChild(
    h('button', {
      class: 'wc',
      title: '设置',
      html: '⚙',
      onclick: () => openPanel(state.panel === 'settings' ? 'none' : 'settings'),
    }),
  );
  wrap.appendChild(
    h('button', { class: 'wc', title: '最小化', html: '&#8212;', onclick: () => void win.minimize() }),
  );
  wrap.appendChild(
    h('button', { class: 'wc', title: '最大化', html: '&#9723;', onclick: () => void win.toggleMaximize() }),
  );
  wrap.appendChild(
    h('button', {
      class: 'wc wc--close',
      title: '关闭窗口',
      html: '&#10005;',
      onclick: () => void win.close(),
    }),
  );
  return wrap;
}

function renderMenu(): HTMLElement {
  const menu = state.menu!;
  const tab = state.tabs.find((candidate) => candidate.id === menu.tabId);
  const items: [string, () => void, boolean?][] = [
    [
      '重命名',
      () => {
        state.renameId = menu.tabId;
        state.renameValue = tab?.title ?? '';
        closeMenu();
        render();
      },
    ],
    ['重新加载页面', () => void api.reloadTab(menu.tabId).then(refreshTabs)],
    ['重启后端进程', () => void api.restartTab(menu.tabId).then(refreshTabs)],
    [
      '在系统浏览器中打开',
      () => void api.openInSystemBrowser(menu.tabId).catch((error) => toast(asFailure(error).message, true)),
    ],
    [
      '清空该实例数据（登录态）',
      () => void api.clearTabData(menu.tabId).then(() => toast('已清空该实例的站点数据')),
    ],
    ['关闭其他', () => void closeOthers(menu.tabId)],
    ['关闭', () => void closeTab(menu.tabId), true],
  ];
  const el = h('div', {
    class: 'menu',
    style: `left:${Math.min(menu.x, window.innerWidth - 240)}px; top:${Math.min(menu.y, window.innerHeight - 260)}px`,
  });
  for (const [label, action, danger] of items) {
    el.appendChild(
      h('button', {
        class: `menu__item ${danger ? 'menu__item--danger' : ''}`,
        text: label,
        onclick: () => {
          closeMenu();
          action();
        },
      }),
    );
    if (label === '清空该实例数据（登录态）') el.appendChild(h('div', { class: 'menu__sep' }));
  }
  return el;
}

function closeMenu() {
  state.menu = null;
  render();
}

// -------------------------------------------------------------------- actions

async function commitRename() {
  const id = state.renameId;
  const value = state.renameValue.trim();
  state.renameId = null;
  render();
  if (!id || !value) return;
  try {
    await api.renameTab(id, value);
    await refreshTabs();
  } catch (error) {
    toast(asFailure(error).message, true);
  }
}

async function closeTab(id: string) {
  try {
    await api.closeTab(id);
    await refreshTabs();
  } catch (error) {
    toast(asFailure(error).message, true);
  }
}

async function closeOthers(keep: string) {
  for (const tab of state.tabs) {
    if (tab.id !== keep) await api.closeTab(tab.id).catch(() => undefined);
  }
  await refreshTabs();
}

async function dropTab(dragged: string, target: string, after: boolean) {
  const ids = state.tabs.map((tab) => tab.id).filter((id) => id !== dragged);
  const index = ids.indexOf(target);
  if (index < 0) return;
  ids.splice(after ? index + 1 : index, 0, dragged);
  try {
    await api.reorderTabs(ids);
    await refreshTabs();
  } catch (error) {
    toast(asFailure(error).message, true);
  }
}

function openPanel(panel: Panel) {
  state.panel = panel;
  state.draft.error = null;
  if (panel === 'newTab') void refreshRuntimes();
  render();
  checkPanelNeeds();
}

async function launchLocal() {
  const draft = state.draft;
  draft.busy = true;
  draft.error = null;
  render();
  try {
    const dshHome: DshHomeSetting =
      draft.dshHomeMode === 'custom'
        ? { mode: 'custom', path: draft.dshHomeCustom.trim() }
        : draft.dshHomeMode === 'instance'
          ? { mode: 'instance' }
          : { mode: 'system' };
    await api.newTab({
      kind: 'launch',
      workspace: draft.workspace.trim(),
      runtimeId: draft.runtimeId,
      dshHome,
      killOnClose: draft.killOnClose,
    });
    draft.busy = false;
    await api.setConfig({ ...state.info!.config, defaultRuntime: draft.runtimeId });
    openPanel('none');
    await refreshTabs();
  } catch (error) {
    const failure = asFailure(error);
    draft.busy = false;
    draft.error = failure.detail ? `${failure.message}\n${failure.detail}` : failure.message;
    render();
  }
}

async function attach(remote: boolean) {
  const draft = state.draft;
  draft.busy = true;
  draft.error = null;
  render();
  try {
    await api.newTab({
      kind: 'attach',
      url: draft.url.trim(),
      remote,
      allowPlaintext: draft.allowPlaintext,
    });
    draft.busy = false;
    openPanel('none');
    await refreshTabs();
  } catch (error) {
    const failure = asFailure(error);
    draft.busy = false;
    draft.error = failure.detail ? `${failure.message}\n${failure.detail}` : failure.message;
    render();
  }
}

// ----------------------------------------------------------------- panels

function renderNewTabPanel(): HTMLElement {
  const draft = state.draft;
  const panel = h('div', { class: 'panel' });
  const head = h('div', { class: 'panel__head' });
  head.appendChild(h('h2', { class: 'panel__title', text: '新建连接' }));
  head.appendChild(
    h('p', {
      class: 'panel__sub',
      text: '连接已有实例不需要任何本地依赖；启动本地需要 Node 或一个独立的后端二进制。',
    }),
  );
  panel.appendChild(head);

  const tabsRow = h('div', { class: 'choices' });
  const kindChoices: [NewTabDraft['kind'], string, string][] = [
    ['launch', '启动本地', '由 dshell 启动并监管一个 dsh 后端进程'],
    ['attach', '连接已有本地实例', '粘贴 `dsh web` 打印的那一行启动 URL'],
    ['remote', '连接远程实例', '粘贴远端 `dsh web` 的启动 URL'],
  ];
  for (const [kind, label, desc] of kindChoices) {
    tabsRow.appendChild(
      h(
        'button',
        {
          class: `choice ${draft.kind === kind ? 'choice--active' : ''}`,
          onclick: () => {
            draft.kind = kind;
            draft.error = null;
            render();
          },
        },
        h('span', { class: 'choice__body' }, h('span', { class: 'choice__label', text: label }), h('span', { class: 'choice__desc', text: desc })),
      ),
    );
  }
  panel.appendChild(tabsRow);

  const grid = h('div', { class: 'panel__grid', style: 'margin-top:16px' });
  if (draft.kind === 'launch') {
    grid.appendChild(renderLaunchForm());
  } else {
    grid.appendChild(renderAttachForm(draft.kind === 'remote'));
  }
  panel.appendChild(grid);

  if (draft.error) {
    panel.appendChild(h('div', { class: 'notice notice--danger', text: draft.error }));
  }

  const actions = h('div', { class: 'actions' });
  actions.appendChild(
    h('button', {
      class: 'btn btn--primary',
      text: draft.busy ? '正在创建…' : draft.kind === 'launch' ? '启动' : '连接并测试',
      disabled: draft.busy ? 'disabled' : undefined,
      onclick: () => (draft.kind === 'launch' ? void launchLocal() : void attach(draft.kind === 'remote')),
    }),
  );
  actions.appendChild(
    h('button', { class: 'btn', text: '取消', onclick: () => openPanel('none') }),
  );
  panel.appendChild(actions);
  return panel;
}

function renderLaunchForm(): HTMLElement {
  const draft = state.draft;
  const column = h('div');

  // --- workspace
  const workspaceField = h('div', { class: 'field' });
  workspaceField.appendChild(h('span', { class: 'field__label', text: '工作目录（DSH 会话的工作根目录）' }));
  const workspaceRow = h('div', { class: 'field__row' });
  const workspaceInput = h('input', {
    type: 'text',
    value: draft.workspace,
    placeholder: '选择一个已存在的目录',
  }) as HTMLInputElement;
  workspaceInput.addEventListener('input', () => {
    draft.workspace = workspaceInput.value;
  });
  workspaceRow.appendChild(workspaceInput);
  workspaceRow.appendChild(
    h('button', {
      class: 'btn',
      text: '浏览',
      onclick: async () => {
        const picked = await api.pickDirectory(draft.workspace || null);
        if (picked) {
          draft.workspace = picked;
          render();
        }
      },
    }),
  );
  workspaceField.appendChild(workspaceRow);
  const recents = state.info?.config.recentWorkspaces ?? [];
  if (recents.length > 0) {
    const recentRow = h('div', { class: 'field__hint' });
    recentRow.appendChild(document.createTextNode('最近：'));
    recents.slice(0, 4).forEach((path, index) => {
      recentRow.appendChild(
        h('button', {
          class: 'btn',
          style: 'padding:2px 8px;margin:0 4px',
          text: shortPath(path, 30),
          title: path,
          onclick: () => {
            draft.workspace = path;
            render();
          },
        }),
      );
      if (index === 3) recentRow.appendChild(document.createTextNode(' '));
    });
    workspaceField.appendChild(recentRow);
  }
  column.appendChild(workspaceField);

  // --- runtime
  const runtimeField = h('div', { class: 'field' });
  runtimeField.appendChild(h('span', { class: 'field__label', text: '运行时' }));
  const runtimeChoices = h('div', { class: 'choices' });
  for (const runtime of state.runtimes) {
    runtimeChoices.appendChild(
      h(
        'button',
        {
          class: `choice ${draft.runtimeId === runtime.id ? 'choice--active' : ''} ${runtime.available ? '' : 'choice--disabled'}`,
          title: runtime.reason ?? '',
          onclick: () => {
            if (!runtime.available) {
              toast(runtime.reason ?? '该运行时当前不可用', true);
              return;
            }
            draft.runtimeId = runtime.id;
            render();
          },
        },
        h(
          'span',
          { class: 'choice__body' },
          h('span', { class: 'choice__label', text: runtime.label + (runtime.available ? '' : '（不可用）') }),
          h('span', { class: 'choice__desc', text: runtime.reason ?? runtime.description }),
        ),
      ),
    );
  }
  if (state.runtimes.length === 0) {
    runtimeChoices.appendChild(h('div', { class: 'field__hint', text: '正在检测运行时…' }));
  }
  runtimeField.appendChild(runtimeChoices);
  if (!state.info?.environment.node) {
    runtimeField.appendChild(
      h('div', {
        class: 'notice notice--warn',
        text:
          '未检测到 Node.js。可以下载安装 Node、用便携 Node，或改选「其他后端二进制」——' +
          '连接模式完全不需要 Node。',
      }),
    );
    runtimeField.appendChild(
      h('button', {
        class: 'btn',
        text: '重新检测',
        onclick: () => void refreshInfo(true).then(refreshRuntimes),
      }),
    );
  }
  if (config().extraBinary === null || config().extraBinary === undefined) {
    runtimeField.appendChild(
      h('button', {
        class: 'btn',
        style: 'margin-top:6px',
        text: '选择后端二进制…',
        onclick: async () => {
          const picked = await api.pickFile('选择能运行 `dsh web` 的可执行文件');
          if (!picked) return;
          try {
            await api.setExtraBinary(picked);
            await refreshInfo();
            await refreshRuntimes();
            draft.runtimeId = 'external';
            toast('已通过验证并采用该二进制');
          } catch (error) {
            toast(asFailure(error).message, true);
          }
          render();
        },
      }),
    );
  }
  column.appendChild(runtimeField);

  // --- DSH_HOME
  const homeField = h('div', { class: 'field' });
  homeField.appendChild(h('span', { class: 'field__label', text: 'DSH_HOME（DSH 的状态目录）' }));
  const group = h('div', { class: 'radio-group' });
  const defaultHome = state.info?.root.defaultDshHome ?? '~/.dsh';
  const instanceHome = state.info
    ? `${state.info.root.path}\\dsh-home\\${slugOf(draft.workspace)}`
    : '<数据根>\\dsh-home\\…';
  const options: [DshHomeSetting['mode'], string, string][] = [
    ['system', '使用系统默认', `DSH 将使用：${defaultHome}`],
    ['instance', '为这个工作目录建独立状态', `DSH 将使用：${instanceHome}（不含现有会话历史）`],
    ['custom', '自定义路径', '必须是不含空白的绝对路径'],
  ];
  for (const [value, label, preview] of options) {
    const radio = h('label', { class: 'radio' });
    const input = h('input', {
      type: 'radio',
      name: 'dsh-home',
      checked: draft.dshHomeMode === value ? 'checked' : undefined,
    }) as HTMLInputElement;
    input.addEventListener('change', () => {
      draft.dshHomeMode = value;
      render();
    });
    radio.appendChild(input);
    const body = h('span', { class: 'radio__body' });
    body.appendChild(h('span', { text: label }));
    body.appendChild(h('span', { class: 'radio__preview', text: preview }));
    radio.appendChild(body);
    group.appendChild(radio);
  }
  homeField.appendChild(group);
  if (draft.dshHomeMode === 'custom') {
    const row = h('div', { class: 'field__row' });
    const input = h('input', {
      type: 'text',
      value: draft.dshHomeCustom,
      placeholder: 'E:\\my-dsh-home（必须绝对路径）',
    }) as HTMLInputElement;
    input.addEventListener('input', () => {
      draft.dshHomeCustom = input.value;
    });
    row.appendChild(input);
    row.appendChild(
      h('button', {
        class: 'btn',
        text: '浏览',
        onclick: async () => {
          const picked = await api.pickDirectory(draft.dshHomeCustom || null);
          if (picked) {
            draft.dshHomeCustom = picked;
            render();
          }
        },
      }),
    );
    homeField.appendChild(row);
    const trimmed = draft.dshHomeCustom.trim();
    if (trimmed && !isAbsolute(trimmed)) {
      homeField.appendChild(h('span', { class: 'field__error', text: '必须是绝对路径（相对路径会随工作目录漂移）' }));
    }
  }
  homeField.appendChild(
    h('span', {
      class: 'field__hint',
      text: '留空即不设置 DSH_HOME，DSH 使用它自己的默认目录，因此你已有的会话、设置与 API Key 会被沿用。',
    }),
  );
  column.appendChild(homeField);

  // --- misc
  const kill = h('label', { class: 'radio' });
  const killInput = h('input', {
    type: 'checkbox',
    checked: draft.killOnClose ? 'checked' : undefined,
  }) as HTMLInputElement;
  killInput.addEventListener('change', () => {
    draft.killOnClose = killInput.checked;
  });
  kill.appendChild(killInput);
  kill.appendChild(h('span', { class: 'radio__body' }, h('span', { text: '关闭选项卡时结束后台进程' })));
  column.appendChild(kill);

  return column;
}

function renderAttachForm(remote: boolean): HTMLElement {
  const draft = state.draft;
  const column = h('div', { class: 'field' });
  column.appendChild(h('span', { class: 'field__label', text: '启动 URL（含 token）' }));
  const input = h('input', {
    type: 'text',
    value: draft.url,
    placeholder: 'http://127.0.0.1:9808/?token=…',
  }) as HTMLInputElement;
  input.addEventListener('input', () => {
    draft.url = input.value;
  });
  column.appendChild(input);
  column.appendChild(
    h('span', {
      class: 'field__hint',
      text: remote
        ? '在远端终端运行 `dsh web --host <网卡IP> --trusted-host <authority>`，复制它打印的整行输出。'
        : '在运行 dsh web 的终端里复制整行 `dsh web: …`（包含 token）。',
    }),
  );

  const plaintext = remote && draft.url.trim().startsWith('http://') && !isLoopback(draft.url);
  if (plaintext) {
    column.appendChild(
      h('div', {
        class: 'notice notice--warn',
        text: '明文 HTTP：token 与会话内容不经加密传输。建议改用 SSH 端口转发或内网/VPN。',
      }),
    );
    const confirm = h('label', { class: 'radio' });
    const box = h('input', {
      type: 'checkbox',
      checked: draft.allowPlaintext ? 'checked' : undefined,
    }) as HTMLInputElement;
    box.addEventListener('change', () => {
      draft.allowPlaintext = box.checked;
    });
    confirm.appendChild(box);
    confirm.appendChild(h('span', { class: 'radio__body' }, h('span', { text: '我已了解风险，仍要继续' })));
    column.appendChild(confirm);
  }
  return column;
}

function renderSettings(): HTMLElement {
  const info = state.info!;
  const config = info.config;
  const panel = h('div', { class: 'panel' });
  const head = h('div', { class: 'panel__head' });
  head.appendChild(h('h2', { class: 'panel__title', text: '设置' }));
  head.appendChild(h('p', { class: 'panel__sub', text: `dshell ${info.version} · 固定 DSH ${info.pinnedDshVersion}` }));
  panel.appendChild(head);

  const grid = h('div', { class: 'panel__grid' });

  // data root
  const rootCard = h('div');
  rootCard.appendChild(h('h3', { class: 'panel__title', text: '数据根（DSHELL_HOME）' }));
  rootCard.appendChild(
    h(
      'dl',
      { class: 'kv' },
      h('dt', { text: '路径' }),
      h('dd', { text: info.root.path }),
      h('dt', { text: '生效层' }),
      h('dd', { text: info.root.sourceLabel }),
      h('dt', { text: `$${info.root.envVar}` }),
      h('dd', { text: info.root.envValue ?? '（未设置）' }),
      h('dt', { text: 'runtimes' }),
      h('dd', { text: info.root.runtimesDir }),
      h('dt', { text: 'webview 存储' }),
      h('dd', { text: info.root.webviewDataDir }),
    ),
  );
  if (info.root.note) {
    rootCard.appendChild(h('div', { class: 'notice notice--warn', text: info.root.note }));
  }
  const rootActions = h('div', { class: 'actions' });
  rootActions.appendChild(
    h('button', {
      class: 'btn',
      text: '打开数据根',
      onclick: () => void api.openPath(info.root.path).catch((error) => toast(asFailure(error).message, true)),
    }),
  );
  rootActions.appendChild(
    h('button', {
      class: 'btn btn--danger',
      text: `移除 $${info.root.envVar} 环境变量`,
      onclick: async () => {
        try {
          await api.removeEnvVar();
          toast('已移除环境变量；dshell 之后按回落链定位数据根');
          await refreshInfo();
        } catch (error) {
          toast(asFailure(error).message, true);
        }
      },
    }),
  );
  rootCard.appendChild(rootActions);
  grid.appendChild(rootCard);

  // environment
  const envCard = h('div');
  envCard.appendChild(h('h3', { class: 'panel__title', text: '运行环境' }));
  const checks = h('ul', { class: 'checks' });
  const env = info.environment;
  const node = env.node;
  checks.appendChild(
    checkRow(
      node && !node.versionIssue ? 'ok' : node ? 'warn' : 'bad',
      'Node.js',
      node ? `${node.version}（${node.node}）` : '未检测到 —— 启动本地不可用，连接不受影响',
    ),
  );
  checks.appendChild(checkRow(env.webview2Version ? 'ok' : 'bad', 'WebView2', env.webview2Version ?? '未检测到'));
  checks.appendChild(checkRow(env.pnpm ? 'ok' : 'warn', 'pnpm', env.pnpm ?? '未找到（源码模式不可用）'));
  envCard.appendChild(checks);
  envCard.appendChild(
    h('div', { class: 'actions' }, h('button', {
      class: 'btn',
      text: '重新检测',
      onclick: () => void refreshInfo(true),
    })),
  );

  // preferences
  envCard.appendChild(h('h3', { class: 'panel__title', text: '偏好' }));
  const pref = h('div', { class: 'radio-group' });

  pref.appendChild(
    labeledSelect('DSH 版本通道', config.dshVersionChannel, [
      ['pinned', `固定 ${info.pinnedDshVersion}（推荐）`],
      ['latest', 'latest（可能引入破坏性变更）'],
      ['alpha', 'alpha'],
    ], async (value) => {
      await api.setConfig({ ...config, dshVersionChannel: value });
      await refreshInfo();
      await refreshRuntimes();
    }),
  );

  pref.appendChild(
    labeledSelect('非活动选项卡', config.backgroundMode, [
      ['move-offscreen', '保持活跃（推荐，长会话不断流）'],
      ['hide', '隐藏以省内存（可能中断流）'],
    ], async (value) => {
      await api.setConfig({ ...config, backgroundMode: value });
      await refreshInfo();
    }),
  );

  const fixedPort = h('label', { class: 'radio' });
  const fixedPortBox = h('input', {
    type: 'checkbox',
    checked: config.fixedPort ? 'checked' : undefined,
  }) as HTMLInputElement;
  fixedPortBox.addEventListener('change', async () => {
    await api.setConfig({ ...config, fixedPort: fixedPortBox.checked });
    await refreshInfo();
  });
  fixedPort.appendChild(fixedPortBox);
  fixedPort.appendChild(
    h(
      'span',
      { class: 'radio__body' },
      h('span', { text: '固定端口以保持登录态' }),
      h('span', {
        class: 'radio__preview',
        text: '默认用随机端口。cookie 绑定端口，端口不变才能跨重启保持登录；代价是可能撞上 EADDRINUSE。',
      }),
    ),
  );
  pref.appendChild(fixedPort);

  const killOnClose = h('label', { class: 'radio' });
  const killBox = h('input', {
    type: 'checkbox',
    checked: config.killOnClose ? 'checked' : undefined,
  }) as HTMLInputElement;
  killBox.addEventListener('change', async () => {
    await api.setConfig({ ...config, killOnClose: killBox.checked });
    await refreshInfo();
  });
  killOnClose.appendChild(killBox);
  killOnClose.appendChild(h('span', { class: 'radio__body' }, h('span', { text: '新建选项卡默认在关闭时结束后台进程' })));
  pref.appendChild(killOnClose);
  envCard.appendChild(pref);

  if (config.managedInstalls.length > 0) {
    envCard.appendChild(h('h3', { class: 'panel__title', text: '本地维护的运行时' }));
    for (const install of config.managedInstalls) {
      const row = h('div', { class: 'field__row', style: 'margin-bottom:6px' });
      row.appendChild(h('span', { class: 'field__hint', text: `${install.label} — ${install.root}（${install.mode}）`, style: 'flex:1' }));
      row.appendChild(
        h('button', {
          class: 'btn btn--danger',
          text: '移除',
          onclick: async () => {
            await api.removeManagedInstall(install.id);
            await refreshInfo();
            await refreshRuntimes();
          },
        }),
      );
      envCard.appendChild(row);
    }
  }
  envCard.appendChild(
    h('div', { class: 'actions' }, h('button', {
      class: 'btn',
      text: '添加本地维护目录…',
      onclick: async () => {
        const picked = await api.pickDirectory(null);
        if (!picked) return;
        try {
          await api.addManagedInstall(picked);
          await refreshInfo();
          await refreshRuntimes();
          toast('已添加');
        } catch (error) {
          toast(asFailure(error).message, true);
        }
      },
    })),
  );

  grid.appendChild(envCard);
  panel.appendChild(grid);

  panel.appendChild(
    h('div', { class: 'actions' }, h('button', { class: 'btn', text: '关闭', onclick: () => openPanel('none') })),
  );
  return panel;
}

function checkRow(kind: 'ok' | 'warn' | 'bad', label: string, value: string): HTMLElement {
  const mark = kind === 'ok' ? '✓' : kind === 'warn' ? '⚠' : '✗';
  return h(
    'li',
    { class: 'check' },
    h('span', { class: `check__mark check__mark--${kind}`, text: mark }),
    h('span', { text: label }),
    h('span', { class: 'check__value', text: value }),
  );
}

function labeledSelect(
  label: string,
  value: string,
  options: [string, string][],
  onChange: (value: string) => void | Promise<void>,
): HTMLElement {
  const field = h('div', { class: 'field' });
  field.appendChild(h('span', { class: 'field__label', text: label }));
  const select = h('select') as HTMLSelectElement;
  for (const [id, text] of options) {
    const option = h('option', { value: id, text }) as HTMLOptionElement;
    if (id === value) option.selected = true;
    select.appendChild(option);
  }
  select.addEventListener('change', () => void onChange(select.value));
  field.appendChild(select);
  return field;
}

function renderOnboarding(): HTMLElement {
  const info = state.info!;
  const panel = h('div', { class: 'panel', 'data-tauri-drag-region': '' });
  const steps = h('div', { class: 'steps' });
  ['选择数据位置', '选择工作目录', '环境检查'].forEach((label, index) => {
    if (index > 0) steps.appendChild(h('span', { class: 'step__sep', text: '›' }));
    const step = h('span', { class: `step ${state.onboardingStep === index ? 'step--active' : ''}` });
    step.appendChild(h('span', { class: 'step__num', text: String(index + 1) }));
    step.appendChild(h('span', { text: label }));
    steps.appendChild(step);
  });
  panel.appendChild(steps);

  if (state.onboardingStep === 0) {
    panel.appendChild(h('h2', { class: 'panel__title', text: '选择一个位置存放 dshell 的数据' }));
    panel.appendChild(
      h('p', {
        class: 'panel__sub',
        text: '配置、下载的运行时、以及每个实例的 webview 存储都会放在这里。DSH 自己的会话与凭据默认不动。',
      }),
    );
    const row = h('div', { class: 'field__row', style: 'margin:16px 0' });
    const input = h('input', { type: 'text', value: state.onboardingRoot }) as HTMLInputElement;
    input.addEventListener('input', () => {
      state.onboardingRoot = input.value;
    });
    row.appendChild(input);
    row.appendChild(
      h('button', {
        class: 'btn',
        text: '浏览',
        onclick: async () => {
          const picked = await api.pickDirectory(state.onboardingRoot || null);
          if (picked) {
            state.onboardingRoot = picked;
            render();
          }
        },
      }),
    );
    panel.appendChild(row);
    panel.appendChild(
      h('div', {
        class: 'notice',
        text: `最终生效层：${info.root.sourceLabel}\n当前进程实际使用：${info.root.path}`,
      }),
    );
    const writeEnv = h('label', { class: 'radio' });
    const box = h('input', {
      type: 'checkbox',
      checked: state.onboardingWriteEnv ? 'checked' : undefined,
    }) as HTMLInputElement;
    box.addEventListener('change', () => {
      state.onboardingWriteEnv = box.checked;
    });
    writeEnv.appendChild(box);
    writeEnv.appendChild(
      h(
        'span',
        { class: 'radio__body' },
        h('span', { text: `把该位置写入用户环境变量 ${info.root.envVar}` }),
        h('span', {
          class: 'radio__preview',
          text: '这样把 dshell.exe 挪到别处也能找到同一份数据。写入失败时回落链仍然有效。',
        }),
      ),
    );
    panel.appendChild(writeEnv);
    panel.appendChild(
      h('div', {
        class: 'notice notice--warn',
        text: '⚠ 请不要选网络盘或同步盘（OneDrive 等）：会话历史频繁写入，且其中包含凭据。',
      }),
    );
    if (state.onboardingError) {
      panel.appendChild(h('div', { class: 'notice notice--danger', text: state.onboardingError }));
    }
    panel.appendChild(
      h(
        'div',
        { class: 'actions' },
        h('button', {
          class: 'btn btn--primary',
          text: '继续',
          onclick: async () => {
            const writable = await api.probeWritable(state.onboardingRoot).catch(() => false);
            if (!writable) {
              state.onboardingError = `该位置不可写：${state.onboardingRoot}`;
              render();
              return;
            }
            state.onboardingError = null;
            state.onboardingStep = 1;
            render();
          },
        }),
        h('button', {
          class: 'btn',
          text: '使用默认位置',
          onclick: () => {
            state.onboardingRoot = info.root.path;
            render();
          },
        }),
      ),
    );
    return panel;
  }

  if (state.onboardingStep === 1) {
    panel.appendChild(h('h2', { class: 'panel__title', text: '选择一个主要工作目录' }));
    panel.appendChild(
      h('p', {
        class: 'panel__sub',
        text: '本地会话以它作为工作根目录，这也是 DSH 读写你项目文件的地方。',
      }),
    );
    const row = h('div', { class: 'field__row', style: 'margin:16px 0' });
    const input = h('input', { type: 'text', value: state.onboardingWorkspace }) as HTMLInputElement;
    input.addEventListener('input', () => {
      state.onboardingWorkspace = input.value;
    });
    row.appendChild(input);
    row.appendChild(
      h('button', {
        class: 'btn',
        text: '浏览',
        onclick: async () => {
          const picked = await api.pickDirectory(state.onboardingWorkspace || null);
          if (picked) {
            state.onboardingWorkspace = picked;
            render();
          }
        },
      }),
    );
    panel.appendChild(row);
    panel.appendChild(
      h('div', { class: 'actions' }, h('button', {
        class: 'btn',
        text: '返回',
        onclick: () => {
          state.onboardingStep = 0;
          render();
        },
      }), h('button', {
        class: 'btn btn--primary',
        text: '继续',
        onclick: () => {
          state.onboardingStep = 2;
          render();
        },
      })),
    );
    return panel;
  }

  panel.appendChild(h('h2', { class: 'panel__title', text: '运行环境检查' }));
  panel.appendChild(h('p', { class: 'panel__sub', text: '检查结果不影响「连接」功能。' }));
  const node = info.environment.node;
  const checks = h('ul', { class: 'checks', style: 'margin:16px 0' });
  checks.appendChild(
    checkRow(
      info.environment.webview2Version ? 'ok' : 'bad',
      'WebView2 运行时',
      info.environment.webview2Version ?? '未检测到（应用无法运行）',
    ),
  );
  checks.appendChild(
    checkRow(
      node && !node.versionIssue ? 'ok' : node ? 'warn' : 'warn',
      'Node.js',
      node ? `${node.version}${node.versionIssue ? ` — ${node.versionIssue}` : '（满足 ^22.19 || >=24）'}` : '未找到（启动本地不可用）',
    ),
  );
  checks.appendChild(checkRow(info.environment.pnpm ? 'ok' : 'warn', 'pnpm', info.environment.pnpm ?? '未找到（源码模式不可用）'));
  panel.appendChild(checks);
  panel.appendChild(
    h(
      'dl',
      { class: 'kv' },
      h('dt', { text: '数据位置' }),
      h('dd', { text: state.onboardingRoot }),
      h('dt', { text: '工作目录' }),
      h('dd', { text: state.onboardingWorkspace || '（未设置）' }),
      h('dt', { text: 'DSH_HOME' }),
      h('dd', { text: `默认：${info.root.defaultDshHome ?? '~/.dsh'}（不设置环境变量，复用你已有的会话与凭据）` }),
    ),
  );
  panel.appendChild(
    h(
      'div',
      { class: 'actions' },
      h('button', {
        class: 'btn',
        text: '返回',
        onclick: () => {
          state.onboardingStep = 1;
          render();
        },
      }),
      h('button', {
        class: 'btn',
        text: '重新检测',
        onclick: () => void refreshInfo(true),
      }),
      h('button', {
        class: 'btn btn--primary',
        text: '完成',
        onclick: async () => {
          try {
            await api.completeOnboarding({
              dataRoot: state.onboardingRoot,
              workspace: state.onboardingWorkspace || null,
              writeEnv: state.onboardingWriteEnv,
            });
            state.panel = 'none';
            state.onboardingStep = 0;
            await refreshInfo();
            await refreshRuntimes();
          } catch (error) {
            state.onboardingError = asFailure(error).message;
            toast(state.onboardingError, true);
          }
        },
      }),
    ),
  );
  return panel;
}

function config(): Config {
  return state.info!.config;
}

function slugOf(workspace: string): string {
  const parts = workspace.split(/[\\/]/).filter(Boolean);
  const name = parts[parts.length - 1] ?? 'workspace';
  return name.replace(/[^a-zA-Z0-9\u4e00-\u9fa5_-]/g, '-').slice(0, 48).toLowerCase();
}

function isAbsolute(path: string): boolean {
  return /^[a-zA-Z]:[\\/]/.test(path) || path.startsWith('\\\\') || path.startsWith('/');
}

function isLoopback(url: string): boolean {
  const match = /^https?:\/\/([^/:]+)/i.exec(url);
  if (!match) return false;
  const host = match[1].toLowerCase();
  return host === 'localhost' || host === '127.0.0.1' || host === '[::1]' || host === '::1';
}

// ----------------------------------------------------------------- shortcuts

function installShortcuts() {
  window.addEventListener('keydown', (event) => {
    const ctrl = event.ctrlKey || event.metaKey;
    if (!ctrl) {
      if (event.key === 'Escape') {
        if (state.menu) closeMenu();
        else if (state.panel !== 'none' && state.panel !== 'onboarding') openPanel('none');
      }
      return;
    }
    const index = Number.parseInt(event.key, 10);
    switch (event.key.toLowerCase()) {
      case 't':
        event.preventDefault();
        openPanel('newTab');
        break;
      case 'w':
        event.preventDefault();
        if (state.tabs.length > 0) void closeTab(activeTabId() ?? state.tabs[state.tabs.length - 1].id);
        break;
      case 'r':
        event.preventDefault();
        if (activeTabId()) void api.reloadTab(activeTabId()!).then(refreshTabs);
        break;
      case 'tab':
        event.preventDefault();
        cycleTab(event.shiftKey ? -1 : 1);
        break;
      case ',':
        event.preventDefault();
        openPanel('settings');
        break;
      default:
        if (!Number.isNaN(index) && index >= 1 && index <= 9) {
          event.preventDefault();
          const target =
            index === 9 ? state.tabs[state.tabs.length - 1] : state.tabs[index - 1];
          if (target) void api.activateTab(target.id).then(refreshTabs);
        }
        break;
    }
  });

  // The window event menu (Rust side) forwards accelerators that arrive while a
  // DSH content webview has focus.
  void listen<{ action: string }>(events.shortcut, ({ action }) => {
    switch (action) {
      case 'new-tab':
        openPanel('newTab');
        break;
      case 'close-tab': {
        const id = activeTabId();
        if (id) void closeTab(id);
        break;
      }
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

function cycleTab(direction: number) {
  if (state.tabs.length === 0) return;
  const current = state.tabs.findIndex((tab) => tab.id === activeTabId());
  const next = (current + direction + state.tabs.length) % state.tabs.length;
  void api.activateTab(state.tabs[next].id).then(refreshTabs);
}

function activeTabId(): string | null {
  return state.activeId;
}

// ---------------------------------------------------------------------- boot

void (async () => {
  installShortcuts();
  await listen<{ tabs: TabView[]; activeId: string | null }>(events.tabsChanged, (payload) => {
    state.tabs = payload.tabs;
    state.activeId = payload.activeId;
    render();
  });
  await listen<{ tab: TabView; reset: boolean }>(events.tabChanged, ({ tab }) => {
    const index = state.tabs.findIndex((existing) => existing.id === tab.id);
    if (index >= 0) state.tabs[index] = tab;
    else state.tabs.push(tab);
    render();
  });
  await listen<{ tabId: string; stage: string; message: string }>(events.runtimeProgress, (payload) => {
    const tab = state.tabs.find((candidate) => candidate.id === payload.tabId);
    if (tab) tab.notice = payload.message;
    render();
  });
  await refreshInfo();
  await refreshRuntimes();
  await api.notifyTabsChanged().catch(() => undefined);
  window.setInterval(() => void refreshTabs(), 4000);
})();
