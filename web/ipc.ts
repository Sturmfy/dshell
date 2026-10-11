// Typed wrappers over the Rust `invoke` surface and events.
//
// Everything the UI does goes through this file, so the Rust contract is declared
// exactly once. Note that *only* the chrome webview and dshell's own shell pages
// can call these — content webviews have no capability at all.

import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { emit as tauriEmit, listen as tauriListen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type {
  AppInfo,
  Config,
  LogEntry,
  RuntimeOption,
  TabFailureDetail,
  TabView,
} from './types';

export const events = {
  tabsChanged: 'dshell://tabs-changed',
  tabChanged: 'dshell://tab-changed',
  runtimeProgress: 'dshell://runtime-progress',
  shortcut: 'dshell://shortcut',
  /** Rust tells both of dshell's webviews which panel is open, so the strip and
   *  the panel can never disagree about it. */
  panel: 'dshell://panel',
} as const;

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return tauriInvoke<T>(command, args);
}

/** Rust failures arrive as structured objects, so the UI can render them. */
export function asFailure(error: unknown): {
  code: string;
  message: string;
  detail?: string | null;
  actions?: { id: string; label: string; kind: 'primary' | 'secondary' }[];
} {
  if (error && typeof error === 'object' && 'code' in error && 'message' in error) {
    return error as never;
  }
  return { code: 'internal', message: String(error) };
}

/** Tabs plus the visible one, as Rust sees them. */
export interface TabsSnapshot {
  tabs: TabView[];
  activeId: string | null;
}

export const api = {
  appInfo: (refreshEnvironment = false) =>
    invoke<AppInfo>('app_info', { refreshEnvironment }),

  listTabs: () => invoke<TabsSnapshot>('list_tabs'),
  activeTab: () => invoke<string | null>('active_tab'),
  /** Opens/closes the panel webview. Rust owns the geometry. */
  setPanel: (panel: 'newTab' | 'settings' | 'onboarding' | null) =>
    invoke<void>('set_panel', { panel }),

  newTab: (request: unknown) =>
    invoke<{ id: string; instanceKey: string }>('new_tab', { request }),

  closeTab: (tabId: string) => invoke<void>('close_tab', { tabId }),
  activateTab: (tabId: string) => invoke<void>('activate_tab', { tabId }),
  renameTab: (tabId: string, title: string) => invoke<void>('rename_tab', { tabId, title }),
  reorderTabs: (tabIds: string[]) => invoke<void>('reorder_tabs', { tabIds }),
  restartTab: (tabId: string) => invoke<void>('restart_tab', { tabId }),
  reloadTab: (tabId: string) => invoke<void>('reload_tab', { tabId }),
  clearTabData: (tabId: string) => invoke<void>('clear_tab_data', { tabId }),
  focusChrome: () => invoke<void>('focus_chrome'),
  openInSystemBrowser: (tabId: string) => invoke<void>('open_in_system_browser', { tabId }),
  shortcut: (action: string) => invoke<void>('shortcut', { action }),
  sessionState: (tabId: string) => invoke<boolean>('session_state', { tabId }),

  tabDiagnostics: (tabId: string) =>
    invoke<{
      view: TabView;
      logTail: LogEntry[];
      commandLine?: string | null;
      node?: { version: string; node: string } | null;
      dataRoot: string;
    }>('tab_diagnostics', { tabId }),

  tabFailureDetail: (tabId: string) => invoke<TabFailureDetail>('tab_failure_detail', { tabId }),

  listRuntimes: () => invoke<RuntimeOption[]>('list_runtimes'),
  setExtraBinary: (path: string) => invoke<void>('set_extra_binary', { path }),
  addManagedInstall: (root: string, label?: string) =>
    invoke<{ id: string; label: string; root: string; mode: string }>('managed_install_add', {
      root,
      label,
    }),
  removeManagedInstall: (id: string) => invoke<void>('managed_install_remove', { id }),

  getConfig: () => invoke<Config>('get_config'),
  setConfig: (config: Config) => invoke<Config>('set_config', { config }),

  // No `workspace` here on purpose: DSH picks the project folder, so onboarding
  // only decides where dshell keeps its own data.
  completeOnboarding: (args: { dataRoot?: string | null; writeEnv?: boolean }) =>
    invoke<AppInfo['root']>('complete_onboarding', args),
  removeEnvVar: () => invoke<void>('remove_env_var'),
  probeWritable: (path: string) => invoke<boolean>('probe_writable_path', { path }),

  pickDirectory: (initial?: string | null) =>
    invoke<string | null>('pick_directory', { initial: initial ?? null }),
  pickFile: (title?: string | null) => invoke<string | null>('pick_file', { title: title ?? null }),
  openPath: (path: string) => invoke<void>('open_path', { path }),

  setWindowTitle: (title: string) => invoke<void>('set_window_title', { title }),
  persistNow: () => invoke<void>('persist_now'),
  notifyTabsChanged: () => invoke<void>('notify_tabs_changed'),
};

/**
 * Window operations, i.e. `plugin:window|…`.
 *
 * These need explicit `core:window:allow-*` permissions — `core:default` grants
 * only the getters. A missing permission fails *silently* from the user's point
 * of view (an unhandled promise rejection), which is exactly how "the window can
 * not be dragged and the close button does nothing" happens. So each call
 * reports its failure instead of swallowing it.
 */
function windowCommand(name: string, action: () => Promise<void>): Promise<void> {
  return action().catch((error: unknown) => {
    console.error(`[dshell] window.${name} failed:`, error);
    throw new Error(`window.${name} 失败：${String(error)}`);
  });
}

export const win = {
  minimize: () => windowCommand('minimize', () => getCurrentWindow().minimize()),
  toggleMaximize: () => windowCommand('toggleMaximize', () => getCurrentWindow().toggleMaximize()),
  close: () => windowCommand('close', () => getCurrentWindow().close()),
  startDragging: () => windowCommand('startDragging', () => getCurrentWindow().startDragging()),
};

export function listen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return tauriListen<T>(event, (message) => handler(message.payload));
}

export function emit(event: string, payload?: unknown): Promise<void> {
  return tauriEmit(event, payload);
}

export function currentWebviewLabel(): string {
  return (window as unknown as { __TAURI_INTERNALS__?: { metadata?: { currentWebview?: { label?: string } } } })
    .__TAURI_INTERNALS__?.metadata?.currentWebview?.label ?? '';
}
