// dshell chrome UI and shell-page logic.
//
// Deliberately dependency-free (no framework, no bundler beyond esbuild): the
// whole UI is a tab strip plus a few page tabs, and the interesting part of this
// app lives in Rust. Keeping the chrome thin is what makes it easy to keep
// iterating on the parts that matter.

// ---------------------------------------------------------------------- types

export type TabPhase =
  | 'idle'
  | 'preparing'
  | 'starting'
  | 'waitingForReady'
  | 'ready'
  | 'failed'
  | 'exited'
  | 'needsToken';

export interface FailureAction {
  id: string;
  label: string;
  kind: 'primary' | 'secondary';
}

export interface Failure {
  code: string;
  message: string;
  detail?: string | null;
  logPath?: string | null;
  actions?: FailureAction[];
  stage?: string | null;
}

/**
 * dshell's own pages. Every one of them is an ordinary tab.
 *
 * These are the slugs Rust knows them by (`PageKind::slug`), which is also what
 * appears in `TabView.pageKind`.
 */
export type PageKind = 'onboarding' | 'new' | 'settings';

export interface TabView {
  id: string;
  title: string;
  badge: string;
  /** Set when this tab renders one of dshell's own pages. */
  pageKind?: PageKind | null;
  /** `false` only for the first-run wizard while it is still unfinished. */
  closable: boolean;
  phase: TabPhase;
  origin?: string | null;
  workspace?: string | null;
  /** Where DSH will actually keep this instance's state. */
  dshHome?: string | null;
  dshHomeEffective?: string | null;
  runtime?: string | null;
  pid?: number | null;
  uptimeMs?: number | null;
  failure?: Failure | null;
  notice?: string | null;
  plaintext: boolean;
  hasSessionCookie: boolean;
  logPath?: string | null;
  exitCode?: number | null;
  ready: boolean;
}

export interface NodeInfo {
  node: string;
  npx?: string | null;
  npm?: string | null;
  version: string;
  source: 'portable' | 'installed' | 'path';
  versionIssue?: string | null;
}

export interface EnvironmentSnapshot {
  node?: NodeInfo | null;
  webview2Version?: string | null;
  pnpm?: string | null;
  notes: string[];
}

export type DshHomeSetting =
  | { mode: 'system' }
  | { mode: 'instance' }
  | { mode: 'custom'; path: string };

export interface WindowConfig {
  width: number;
  height: number;
  x?: number | null;
  y?: number | null;
  maximized: boolean;
}

export interface Config {
  schemaVersion: number;
  onboardingDone: boolean;
  dshHome: DshHomeSetting;
  dshAgentsHome: DshHomeSetting;
  workspace?: string | null;
  recentWorkspaces: string[];
  dshVersionChannel: string;
  defaultRuntime: string;
  managedInstalls: { id: string; label: string; root: string; mode: string }[];
  extraBinary?: string | null;
  language: string;
  backgroundMode: string;
  fixedPort: boolean;
  killOnClose: boolean;
  window: WindowConfig;
}

export interface RootView {
  path: string;
  source: string;
  sourceLabel: string;
  note?: string | null;
  configFile: string;
  runtimesDir: string;
  webviewDataDir: string;
  envVar: string;
  envValue?: string | null;
  defaultDshHome?: string | null;
  defaultAgentsHome?: string | null;
}

export interface AppInfo {
  version: string;
  pinnedDshVersion: string;
  root: RootView;
  tabs: TabView[];
  config: Config;
  environment: EnvironmentSnapshot;
  bootNote?: string | null;
}

export interface RuntimeOption {
  id: string;
  label: string;
  description: string;
  available: boolean;
  reason?: string | null;
}

export interface LogEntry {
  text: string;
  stream: string;
  at: number;
}

export interface TabFailureDetail {
  failure?: Failure | null;
  logTail: LogEntry[];
  commandLine?: string | null;
  workspace?: string | null;
  dshHome?: string | null;
  runtime?: string | null;
  dataDir?: string | null;
  tailText: string;
}
