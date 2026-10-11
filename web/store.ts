// The shared data layer for dshell's two UIs.
//
// dshell's chrome lives in *two* webviews — the tab strip on top, and the panel
// below it — because the strip must stay visible (and therefore draggable) while
// a panel is open. Both need the same picture of the world, so that picture lives
// here rather than in either view:
//
//   * Rust is the only authority. The store fetches `list_tabs` / `app_info` and
//     subscribes to Rust's events; it never infers state on its own.
//   * Rendering is not shared. Each view renders itself from the store, so the
//     two webviews never have to coordinate anything.

import { api, asFailure, events, listen } from './ipc';
import type { AppInfo, Config, DshHomeSetting, RuntimeOption, TabView } from './types';

export type PanelKind = 'none' | 'newTab' | 'settings' | 'onboarding';

export interface NewTabDraft {
  kind: 'launch' | 'attach' | 'remote';
  /** Destination for `attach`/`remote`. */
  url: string;
  runtimeId: string;
  /** Optional starting directory; empty means "let DSH decide". */
  workspace: string;
  dshHomeMode: DshHomeSetting['mode'];
  dshHomeCustom: string;
  allowPlaintext: boolean;
  killOnClose: boolean;
  busy: boolean;
  error: string | null;
}

export interface Toast {
  text: string;
  error: boolean;
}

export interface StoreState {
  info: AppInfo | null;
  tabs: TabView[];
  /** The visible tab, as reported by Rust. Never inferred locally. */
  activeId: string | null;
  runtimes: RuntimeOption[];
  panel: PanelKind;
  draft: NewTabDraft;
  toast: Toast | null;
  onboardingStep: number;
  onboardingRoot: string;
  onboardingWriteEnv: boolean;
  onboardingError: string | null;
}

export class Store {
  state: StoreState = {
    info: null,
    tabs: [],
    activeId: null,
    runtimes: [],
    panel: 'none',
    draft: {
      kind: 'launch',
      url: '',
      runtimeId: 'npm',
      workspace: '',
      dshHomeMode: 'system',
      dshHomeCustom: '',
      allowPlaintext: false,
      killOnClose: false,
      busy: false,
      error: null,
    },
    toast: null,
    onboardingStep: 0,
    onboardingRoot: '',
    onboardingWriteEnv: true,
    onboardingError: null,
  };

  private listeners = new Set<() => void>();
  private toastTimer: number | null = null;
  private started = false;

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private emit(): void {
    for (const listener of this.listeners) listener();
  }

  // -------------------------------------------------------------- lifecycle

  /** Wire up Rust's events and load the initial snapshot. Safe to call once. */
  async start(): Promise<void> {
    if (this.started) return;
    this.started = true;

    await listen<{ tabs: TabView[]; activeId: string | null }>(events.tabsChanged, (payload) => {
      this.state.tabs = payload.tabs;
      this.state.activeId = payload.activeId;
      this.emit();
    });
    await listen<{ tab: TabView; reset: boolean }>(events.tabChanged, ({ tab }) => {
      const index = this.state.tabs.findIndex((existing) => existing.id === tab.id);
      if (index >= 0) this.state.tabs[index] = tab;
      else this.state.tabs.push(tab);
      this.emit();
    });
    await listen<{ tabId: string; stage: string; message: string }>(
      events.runtimeProgress,
      (payload) => {
        const tab = this.state.tabs.find((candidate) => candidate.id === payload.tabId);
        if (tab) tab.notice = payload.message;
        this.emit();
      },
    );
    await listen<{ panel: PanelKind }>(events.panel, (payload) => {
      this.state.panel = payload.panel;
      this.emit();
    });

    await this.refreshInfo();
    await this.refreshRuntimes();
  }

  // ------------------------------------------------------------------ data

  async refreshInfo(refreshEnvironment = false): Promise<void> {
    try {
      const info = await api.appInfo(refreshEnvironment);
      this.state.info = info;
      const config = info.config;
      const draft = this.state.draft;
      if (!draft.workspace) draft.workspace = config.recentWorkspaces[0] ?? '';
      draft.runtimeId = config.defaultRuntime || draft.runtimeId;
      draft.killOnClose = config.killOnClose;
      draft.dshHomeMode = config.dshHome?.mode ?? 'system';
      draft.dshHomeCustom = config.dshHome?.mode === 'custom' ? config.dshHome.path : '';
      if (!config.onboardingDone) {
        this.state.onboardingRoot ||= info.root.path;
      }
      if (info.bootNote) this.toast(info.bootNote);
    } catch (error) {
      this.toast(asFailure(error).message, true);
    }
    await this.refreshTabs();
  }

  async refreshTabs(): Promise<void> {
    try {
      const snapshot = await api.listTabs();
      this.state.tabs = snapshot.tabs;
      this.state.activeId = snapshot.activeId;
    } catch {
      /* the app may be shutting down */
    }
    this.emit();
  }

  async refreshRuntimes(): Promise<void> {
    try {
      this.state.runtimes = await api.listRuntimes();
    } catch {
      this.state.runtimes = [];
    }
    this.emit();
  }

  // -------------------------------------------------------------- panel

  /**
   * Open or close dshell's panel. Rust does the webview work; the strip only
   * reports the intent, so the two views cannot disagree about what is open.
   */
  async openPanel(panel: PanelKind): Promise<void> {
    this.state.draft.error = null;
    this.state.panel = panel;
    this.emit();
    if (panel === 'newTab') void this.refreshRuntimes();
    await api.setPanel(panel === 'none' ? null : panel).catch((error) => {
      this.toast(asFailure(error).message, true);
    });
  }

  // -------------------------------------------------------------- toasts

  toast(text: string, error = false): void {
    this.state.toast = { text, error };
    this.emit();
    if (this.toastTimer !== null) window.clearTimeout(this.toastTimer);
    this.toastTimer = window.setTimeout(() => {
      this.state.toast = null;
      this.emit();
    }, 6000);
  }

  // ------------------------------------------------------------- config

  get config(): Config | null {
    return this.state.info?.config ?? null;
  }

  async updateConfig(patch: Partial<Config>): Promise<void> {
    const config = this.config;
    if (!config) return;
    try {
      await api.setConfig({ ...config, ...patch });
      await this.refreshInfo();
    } catch (error) {
      this.toast(asFailure(error).message, true);
    }
  }

  // --------------------------------------------------------- tab actions

  async activate(id: string): Promise<void> {
    await api.activateTab(id).then(() => this.refreshTabs());
  }

  async close(id: string): Promise<void> {
    try {
      await api.closeTab(id);
      await this.refreshTabs();
    } catch (error) {
      this.toast(asFailure(error).message, true);
    }
  }

  async closeOthers(keep: string): Promise<void> {
    for (const tab of this.state.tabs) {
      if (tab.id !== keep) await api.closeTab(tab.id).catch(() => undefined);
    }
    await this.refreshTabs();
  }

  async rename(id: string, title: string): Promise<void> {
    try {
      await api.renameTab(id, title);
      await this.refreshTabs();
    } catch (error) {
      this.toast(asFailure(error).message, true);
    }
  }

  async reorder(ids: string[]): Promise<void> {
    try {
      await api.reorderTabs(ids);
      await this.refreshTabs();
    } catch (error) {
      this.toast(asFailure(error).message, true);
    }
  }

  async reload(id: string): Promise<void> {
    await api.reloadTab(id).then(() => this.refreshTabs()).catch(() => undefined);
  }

  async restart(id: string): Promise<void> {
    await api.restartTab(id).then(() => this.refreshTabs()).catch(() => undefined);
  }

  // --------------------------------------------------------- new tab

  /** Send the draft to Rust. Returns true when the tab was created. */
  async submitDraft(): Promise<boolean> {
    const draft = this.state.draft;
    draft.busy = true;
    draft.error = null;
    this.emit();
    try {
      if (draft.kind === 'launch') {
        const dshHome: DshHomeSetting =
          draft.dshHomeMode === 'custom'
            ? { mode: 'custom', path: draft.dshHomeCustom.trim() }
            : draft.dshHomeMode === 'instance'
              ? { mode: 'instance' }
              : { mode: 'system' };
        const startingDir = draft.workspace.trim();
        await api.newTab({
          kind: 'launch',
          // `null`, not "": it tells Rust the user did not pick a directory, so
          // DSH keeps ownership of the project folder.
          workspace: startingDir.length > 0 ? startingDir : null,
          runtimeId: draft.runtimeId,
          dshHome,
          killOnClose: draft.killOnClose,
        });
      } else {
        await api.newTab({
          kind: 'attach',
          url: draft.url.trim(),
          remote: draft.kind === 'remote',
          allowPlaintext: draft.allowPlaintext,
        });
      }
      draft.busy = false;
      await this.openPanel('none');
      await this.refreshTabs();
      return true;
    } catch (error) {
      const failure = asFailure(error);
      draft.busy = false;
      draft.error = failure.detail ? `${failure.message}\n${failure.detail}` : failure.message;
      this.emit();
      return false;
    }
  }
}

/** Where a panel should be shown, given an active tab and the tab count. */
export function panelFor(state: StoreState): PanelKind {
  return state.panel;
}
