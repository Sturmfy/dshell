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

/**
 * The strongest unacknowledged session state in a tab.
 *
 * `completed` is green, `needsInput` amber, `error` red. A dead backend is
 * reported as `error`, and a missing launch token as `needsInput`.
 */
export type TabAttention = 'error' | 'needsInput' | 'completed';

/** What the DSH page last reported about its session rows. */
export interface SessionAttentionCounts {
  error: number;
  needsInput: number;
  completed: number;
  running: number;
}

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
  /** Unacknowledged session state, or null when the tab is quiet. */
  attention?: TabAttention | null;
  attentionCounts?: SessionAttentionCounts | null;
  hasSessionCookie: boolean;
  logPath?: string | null;
  exitCode?: number | null;
  ready: boolean;
}

/** The DSH sidebar surface a content tab reports, as the strip paints with it. */
export interface TabTheme {
  tabId: string;
  background: string;
  foreground: string;
  mode: 'dark' | 'light';
}

/** The part of a {@link TabTheme} the tab-menu popup wears. */
export type TabMenuTheme = Omit<TabTheme, 'tabId'>;

/** What the tab-menu popup renders for one open. */
export interface TabMenuPayload {
  tabId: string;
  page: boolean;
  theme?: TabMenuTheme;
}

export interface NodeInfo {
  node: string;
  npx?: string | null;
  npm?: string | null;
  version: string;
  source: 'portable' | 'installed' | 'path';
  versionIssue?: string | null;
}

/**
 * How dshell could install `pnpm` itself.
 *
 * pnpm is one of the two things dshell reports but never runs: a source checkout
 * needs it for the user's own `pnpm install` / `pnpm run build`. When it is
 * missing and `npm` is present, `command` is the exact line the button runs.
 */
export interface PnpmInstall {
  npm?: string | null;
  npmVersion?: string | null;
  command?: string | null;
  /** Why the button is not offered, when it is not. */
  blocked?: string | null;
}

export interface EnvironmentSnapshot {
  node?: NodeInfo | null;
  webview2Version?: string | null;
  pnpm?: string | null;
  pnpmInstall?: PnpmInstall | null;
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
  /** The directory the source checkout is kept *in*; `null` = the default. */
  sourceContainer?: string | null;
  /** Remembered attach/remote targets, newest first. */
  recentConnections?: RecentConnection[];
  /** Origins pinned above the ordinary remote lists, newest pin first. */
  pinnedConnections?: string[];
  /** User-chosen tab names, keyed by the connection's stable instance id. */
  tabNames?: Record<string, string>;
  language: string;
  backgroundMode: string;
  fixedPort: boolean;
  killOnClose: boolean;
  /**
   * Keep DSH's working indicators animated even when Windows asks for reduced
   * motion. Applies to DSH content tabs only. WebView2 browser arguments are
   * fixed at environment creation, so the value affects tabs created
   * afterwards; restart dshell to update tabs that are already open.
   */
  forceDshMotion: boolean;
  window: WindowConfig;
}

/**
 * One remembered connection target.
 *
 * `url` keeps its `token=` parameter: a remembered entry that cannot reconnect
 * would be a list of failures. See `src-tauri/src/config.rs` for why that is a
 * deliberate trade rather than an oversight.
 */
export interface RecentConnection {
  id: string;
  url: string;
  label: string;
  remote: boolean;
  lastUsedAt: number;
}

/** What `probe_connection` says before a tab is opened. */
export interface ConnectionProbe {
  /** The token (when supplied) reached DSH's frontend. */
  ok: boolean;
  /** The server answered like DSH, but no token was supplied yet. */
  requiresToken: boolean;
  origin: string;
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
  /** `<data root>/runtimes` — the default *parent* of a first checkout. */
  defaultSourceContainer: string;
  /** The container in use (the default, or the user's directory). */
  sourceContainer: string;
}

/**
 * The two ways to start a local backend, and what each would do right now.
 *
 * Everything here is Rust's answer, including the numbers in the sentences: the
 * page never guesses whether a download is needed.
 */
export interface CachedNpxPackage {
  version: string;
  path: string;
  managed: boolean;
}

export interface NpxRuntime {
  packageSpec: string;
  targetVersion?: string | null;
  cached: CachedNpxPackage[];
  hit?: string | null;
  state: 'ready' | 'install' | 'update' | 'blocked';
  cacheDir: string;
  /** Only filled when the (optional) network check ran. */
  latestVersion?: string | null;
  nodeVersion?: string | null;
  blocked?: string | null;
}

export interface SourceCheckout {
  root: string;
  checkout: boolean;
  installed: boolean;
  frontendBuilt: boolean;
  version?: string | null;
  commit?: string | null;
  branch?: string | null;
  remote?: string | null;
  dirty: boolean;
  blocked?: string | null;
}

export interface SourceRuntime {
  /** The checkout itself: `<container>/deepseek-harness`. */
  status: SourceCheckout;
  /** The directory the user picked, which *contains* the checkout. */
  container: string;
  /** The fixed subdirectory the clone lands in (`deepseek-harness`). */
  checkoutName: string;
  state: 'ready' | 'install' | 'blocked';
  repo: string;
  git?: string | null;
  pnpm?: string | null;
  nodeVersion?: string | null;
  blocked?: string | null;
}

/** A DSH server already listening on this machine. */
export interface RunningInstance {
  origin: string;
  port: number;
  tabId?: string | null;
  title?: string | null;
  /** dshell holds a token for this origin (its own tab, or a saved entry). */
  connectable: boolean;
  notice?: string | null;
}

/** A backend dshell itself is supervising right now. */
export interface RunningLocal {
  tabId: string;
  title: string;
  phase: string;
  pid?: number | null;
  origin?: string | null;
}

export interface LocalRuntimeState {
  npx: NpxRuntime;
  source: SourceRuntime;
  instances: RunningInstance[];
  runningLocal: RunningLocal[];
}

/** One `dshell://prepare-progress` payload. */
export interface PrepareProgress {
  runtimeId: string;
  stage: string;
  message: string;
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
