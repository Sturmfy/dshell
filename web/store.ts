// The shared data layer for dshell's UI.
//
// Rust is the only authority. The store fetches `app_info` / `list_tabs` and
// subscribes to Rust's events; it never infers state on its own. Rendering is not
// shared — the strip and each page tab render themselves from this store, so the
// views never have to coordinate anything.
//
// Note what is *not* here: there is no notion of an open panel. dshell's own
// pages are tabs, and the only way to show one is to open or activate its tab.

import { api, asFailure, events, listen } from './ipc';
import type { AppInfo, Config, PageKind, RuntimeOption, TabView } from './types';

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
  /** True while `npm install -g pnpm` runs, so every button that offers it can
   * show the same state instead of each page tracking its own. */
  pnpmInstalling: boolean;
  toast: Toast | null;
}

export class Store {
  state: StoreState = {
    info: null,
    tabs: [],
    activeId: null,
    runtimes: [],
    pnpmInstalling: false,
    toast: null,
  };

  private listeners = new Set<() => void>();
  private toastTimer: number | null = null;
  private started = false;

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  emit(): void {
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

    await this.refreshInfo();
    await this.refreshRuntimes();
  }

  // ------------------------------------------------------------------ data

  async refreshInfo(refreshEnvironment = false): Promise<void> {
    try {
      const info = await api.appInfo(refreshEnvironment);
      this.state.info = info;
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

  /**
   * Install pnpm globally with the npm Rust detected.
   *
   * Rust is the authority on both halves: it decides whether an install is
   * possible at all (no npm → it rejects with the reason), runs it, and answers
   * with the **refreshed** report. Adopting that snapshot is what keeps the row
   * from disagreeing with the machine it just changed — and it is why nothing
   * here re-detects.
   */
  async installPnpm(): Promise<void> {
    if (this.state.pnpmInstalling) return;
    this.state.pnpmInstalling = true;
    this.emit();
    try {
      const environment = await api.installPnpm();
      if (this.state.info) this.state.info.environment = environment;
      this.toast(environment.pnpm ? `pnpm ${environment.pnpm} 已可用` : '已安装 pnpm');
    } catch (error) {
      const failure = asFailure(error);
      const detail = failure.detail ? `：${failure.detail}` : '';
      this.toast(`${failure.message}${detail}`, true);
    } finally {
      this.state.pnpmInstalling = false;
      this.emit();
    }
  }

  // -------------------------------------------------------------- pages

  /**
   * Open one of dshell's own pages. Rust owns tab creation and uniqueness, so
   * the UI only ever reports the intent — this is why the strip and a page can
   * never disagree about which tabs exist.
   */
  async openPage(page: PageKind): Promise<void> {
    try {
      await api.openPage(page);
      await this.refreshTabs();
    } catch (error) {
      this.toast(asFailure(error).message, true);
    }
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

  /** Returns false when Rust refused the close (e.g. an unfinished wizard). */
  async close(id: string): Promise<boolean> {
    try {
      await api.closeTab(id);
      await this.refreshTabs();
      return true;
    } catch (error) {
      this.toast(asFailure(error).message, true);
      return false;
    }
  }

  async closeOthers(keep: string): Promise<void> {
    for (const tab of this.state.tabs) {
      if (tab.id !== keep && tab.closable) await api.closeTab(tab.id).catch(() => undefined);
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
}

// -------------------------------------------------------------------- pages

/**
 * The store as seen from inside a dshell page tab.
 *
 * A page needs three things the strip does not: it must know *which* tab it is,
 * observe whether it is the visible one, and close itself. `tabs-changed`
 * already carries the active tab, so all three come from the one subscription
 * the base store already holds.
 */
export class PageStore extends Store {
  /** This page's tab id, resolved from the active tab at load time. */
  pageTabId = '';

  constructor(private readonly kind: PageKind) {
    super();
  }

  async start(): Promise<void> {
    await this.step('start');
    await super.start();
    await this.step('store started');
    this.pageTabId =
      this.state.tabs.find((tab) => tab.pageKind === this.kind)?.id ?? '';
    // A page that is loaded but not the active tab (opened in the background)
    // still needs its id, so fall back to the freshest snapshot.
    if (!this.pageTabId) await this.refreshTabs();
    this.pageTabId ||= this.state.tabs.find((tab) => tab.pageKind === this.kind)?.id ?? '';
    // Tell Rust the page came up. See `page_ready`: this is how a page whose
    // `invoke` was refused gets noticed instead of silently rendering nothing.
    await api.pageReady(this.kind, this.pageTabId, true).catch(() => undefined);
  }

  /** Report that this page never came up, with the reason the browser gave. */
  async reportBootFailure(error: unknown): Promise<void> {
    await api
      .pageReady(this.kind, this.pageTabId, false, String(error))
      .catch(() => undefined);
  }

  /** Report one start-up step to the debug log. Never throws. */
  async step(step: string): Promise<void> {
    await api.clientStep(this.kind, step);
  }

  get isActive(): boolean {
    return this.pageTabId !== '' && this.state.activeId === this.pageTabId;
  }

  /** The tab view for this page, or undefined while it is still being created. */
  get self(): TabView | undefined {
    return this.state.tabs.find((tab) => tab.id === this.pageTabId);
  }

  /** Close this page's own tab. Rust decides whether that is allowed. */
  async closeSelf(): Promise<void> {
    if (!this.pageTabId) return;
    await this.close(this.pageTabId);
  }

  /** Whether Rust would let this page be closed right now. */
  async closable(): Promise<boolean> {
    if (!this.pageTabId) return true;
    return api.tabClosable(this.pageTabId).catch(() => true);
  }
}
