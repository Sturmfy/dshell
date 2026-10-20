// The new-connection page, shown as a tab.
//
// A two-step flow with a persistent step rail:
//
//   1. 方式   local (dshell starts a backend) or remote (a `dsh web` is already
//             running somewhere, localhost included)
//   2. 设置   local: exactly two backends — npx and source — each showing what it
//             would do *right now* (download? upgrade? clone? rebuild?)
//             remote: a URL box plus the two lists that make reconnecting quick:
//             the DSH servers already listening on this machine, and the
//             remembered connections
//
// The rail is rendered on *both* steps: the first page is the start of the flow,
// not a separate screen that happens to precede it. The first page also owns a
// real footer action ("继续") rather than ending in a line of prose.
//
// Details are deliberately demoted. Anything that is not needed to make the
// decision on screen — package specs, cache directories, Node/git/pnpm versions,
// the token mechanics, the shared-DSH_HOME risk — lives in the "高级选项" panel.
// The page keeps: the two choices, the one status line that changes the action
// ("下载并启动" vs "启动"), warnings that change what the next click does, and
// the primary button.
//
// Two rules survive from the design:
//
//   * connecting needs no local dependencies — the remote step never looks at
//     Node, npx or git;
//   * dshell does not choose the project folder — DSH does. Nothing on this page
//     asks where the project is; the backend simply starts in the user's home.
//
// Slow work (an npx download, a `pnpm run build`) happens **on this page**, with
// every control disabled, because the alternative is a new tab that sits on a
// spinner with no way to tell "building" from "hung". Rust emits
// `dshell://prepare-progress` for it, and `prepare_runtime` hands back the
// refreshed state so nothing is detected twice.

import { api, asFailure, events, listen } from '../ipc';
import { PageStore } from '../store';
import type {
  AppInfo,
  DshHomeSetting,
  LocalRuntimeState,
  PrepareProgress,
  RecentConnection,
  RunningInstance,
} from '../types';
import {
  bootPage,
  checkboxRow,
  h,
  isAbsolute,
  isLoopback,
  pageFooter,
  pageFrame,
  pageHeader,
  preserveView,
  row,
  section,
  selectableCard,
  toastView,
  wideRow,
} from './shared';

const host = document.getElementById('page') as HTMLElement;
const store = new PageStore('new');

type Step = 'kind' | 'local' | 'remote';
type Kind = 'local' | 'remote';
type RuntimeId = 'npx' | 'source';

interface BusyState {
  title: string;
  sub: string;
  stage: string;
  lines: string[];
}

type ProbePhase = 'checking' | 'needsToken' | 'ready' | 'failed';
type ProbeSource = 'new' | 'instance' | 'history';

/**
 * The connection-check dialog's whole state. It is one object so an in-flight
 * probe can be discarded by identity when the user closes the dialog — a late
 * promise must never reopen a modal the user dismissed.
 */
interface PendingConnection {
  origin: string;
  /** Full token URL once known; `null` means "identify first, ask for token". */
  url: string | null;
  name: string;
  tokenInput: string;
  remote: boolean;
  allowPlaintext: boolean;
  phase: ProbePhase;
  error: string | null;
  detail: string | null;
  source: ProbeSource;
}

const state = {
  step: 'kind' as Step,
  /** The card chosen on step one; `null` until the user picks a side. */
  kind: null as Kind | null,
  runtimeId: 'npx' as RuntimeId,
  /** The new-connection card's form. Port 3080 is `dsh web`'s default. */
  remoteIp: '127.0.0.1',
  remotePort: '3080',
  pending: null as PendingConnection | null,
  /** The user has accepted "another backend is already running". */
  confirmExisting: false,
  advancedOpen: false,
  dshHomeMode: 'system' as DshHomeSetting['mode'],
  dshHomeCustom: '',
  killOnClose: true,
  busy: null as BusyState | null,
  error: null as string | null,
  status: null as LocalRuntimeState | null,
  /** What the source-location field shows; `null` follows the effective path. */
  sourceDraft: null as string | null,
};

/** Origins whose connection check succeeded during this visit. */
const detected = new Set<string>();

const STEP_LABELS = ['选择方式', '详细设置'];

// -------------------------------------------------------------------- render

/**
 * Build the page from the store.
 *
 * `render` adds [`preserveView`], because almost everything that repaints this
 * page — a rescan, an error, a modal opening or closing, a toast timing out —
 * happens *on* the step the user is already reading. Only a step change is a new
 * screen, and it calls `renderStep` to start at the top.
 */
function paint(): void {
  const info = store.state.info;
  if (!info) {
    host.innerHTML = '';
    host.appendChild(h('div', { class: 'page__scroll page__loading', text: '正在加载…' }));
    return;
  }

  // Step one is a full-height choice screen: its cards centre in the space
  // below the rail rather than sitting under it like a form. The class lives on
  // the page host because the scroll container is created by `pageFrame`.
  host.classList.toggle('page--kind', state.step === 'kind');

  const { main } = pageFrame();
  main.appendChild(intro());

  if (state.step === 'kind') renderKindStep(main);
  else if (state.step === 'local') renderLocalStep(main);
  else renderRemoteStep(main, info);

  if (state.error) {
    main.appendChild(h('div', { class: 'notice notice--danger', text: state.error }));
  }

  renderFooter();
  renderAdvanced();
  renderProbe();
  renderBusy();

  if (!state.busy) {
    const toast = toastView(store);
    if (toast) host.appendChild(toast);
  }
}

/** Repaint the current step, keeping the reader where they are. */
const render = (): void => preserveView(paint);

/**
 * Go to a different step.
 *
 * `paint` builds a fresh scroll column, so the step opens at its top — which is
 * exactly what a repaint of the current step must not do.
 */
function renderStep(): void {
  paint();
}

/**
 * The part of the page that never changes shape: the step rail, the one quiet
 * secondary action, and the title. Keeping them in one block is what makes step
 * one and step two read as the same page.
 */
function intro(): HTMLElement {
  const wrap = h('div', { class: 'page__intro' });
  const top = h('div', { class: 'page__top' });
  const actions = h('div', { class: 'page__top-actions' });

  if (state.step === 'remote') {
    actions.appendChild(
      h('button', {
        type: 'button',
        class: 'btn btn--quiet btn--sm',
        text: '重新扫描',
        disabled: state.busy ? 'disabled' : undefined,
        onclick: () => void rescan(),
      }),
    );
  }
  actions.appendChild(
    h('button', {
      type: 'button',
      class: 'btn btn--quiet btn--sm',
      text: '高级选项',
      onclick: () => {
        state.advancedOpen = true;
        render();
      },
    }),
  );

  top.appendChild(stepper());
  top.appendChild(actions);
  wrap.appendChild(top);
  wrap.appendChild(headline());
  return wrap;
}

function stepper(): HTMLElement {
  const currentIndex = state.step === 'kind' ? 0 : 1;
  const list = h('ol', { class: 'steps', 'aria-label': '新建连接步骤' });

  STEP_LABELS.forEach((label, index) => {
    if (index > 0) list.appendChild(h('li', { class: 'step__sep', 'aria-hidden': 'true' }));

    const current = index === currentIndex;
    const done = index < currentIndex;
    // The finished step is a way back; the current one is a label. That is the
    // whole interaction: a wizard that can only go back through the footer
    // button makes the rail look decorative.
    const clickable = done;
    const classes = [
      'step',
      current ? 'step--active' : '',
      done ? 'step--done' : '',
      clickable ? 'step--link' : '',
    ]
      .filter(Boolean)
      .join(' ');

    list.appendChild(
      h(
        'li',
        {
          class: classes,
          'aria-current': current ? 'step' : undefined,
          role: clickable ? 'button' : undefined,
          tabindex: clickable ? '0' : undefined,
          onclick: clickable ? () => backToKind() : undefined,
          onkeydown: clickable
            ? (event: KeyboardEvent) => {
                if (event.key !== 'Enter' && event.key !== ' ') return;
                event.preventDefault();
                backToKind();
              }
            : undefined,
        },
        h('span', { class: 'step__num', text: done ? '✓' : String(index + 1) }),
        h('span', { text: label }),
      ),
    );
  });

  return list;
}

function headline(): HTMLElement {
  if (state.step === 'kind') {
    return pageHeader('选择连接方式', '连接已有实例不需要任何本地依赖。');
  }
  if (state.step === 'local') {
    return pageHeader('本地后端', '选择 npx 或源码，dshell 负责准备并启动。');
  }
  return pageHeader('远程实例', '连接一台已经在运行的 DSH。');
}

function renderFooter(): void {
  if (state.step === 'kind') {
    pageFooter(
      h('button', { class: 'page__foot-spacer' }),
      h('button', {
        class: 'btn btn--primary',
        text: '继续',
        disabled: state.kind ? undefined : 'disabled',
        onclick: () => void advanceKind(),
      }),
    );
    return;
  }

  const back = h('button', {
    class: 'btn',
    text: '返回',
    onclick: () => backToKind(),
  });

  if (state.step === 'local') {
    const busy = state.busy !== null;
    const selectedBlocked = state.status
      ? state.runtimeId === 'npx'
        ? state.status.npx.state === 'blocked'
        : state.status.source.state === 'blocked'
      : false;
    pageFooter(
      back,
      h('button', { class: 'page__foot-spacer' }),
      h('button', {
        class: 'btn btn--primary',
        text: state.busy ? '正在准备…' : startLabel(),
        disabled: busy || !state.status || selectedBlocked ? 'disabled' : undefined,
        onclick: () => void startLocal(),
      }),
    );
    return;
  }

  // Remote connections are started from their cards; the footer only owns the
  // way back, so there is no second "connect" that could disagree with a card.
  pageFooter(back, h('button', { class: 'page__foot-spacer' }));
}

function startLabel(): string {
  const status = state.status;
  if (!status) return '启动';
  if (state.runtimeId === 'npx') {
    if (status.npx.state === 'blocked') return '不可用';
    return status.npx.state === 'ready' ? '启动' : '下载并启动';
  }
  if (status.source.state === 'blocked') return '不可用';
  return status.source.state === 'ready' ? '更新并启动' : '安装并启动';
}

// ------------------------------------------------------------------ step one

function renderKindStep(main: HTMLElement): void {
  const instances = state.status?.instances ?? [];

  // The choice cards are portrait, not wide banners: a logo, a title and one
  // line of orientation, centred as a group. The wrapper is what makes them
  // sit in the middle of the page instead of under the header.
  const center = h('div', { class: 'kind-center' });
  const cards = h('div', { class: 'kind-cards' });
  cards.appendChild(
    modeCard({
      kind: 'local',
      title: '本地',
      desc: '由 dshell 启动并托管一个后端',
      selected: state.kind === 'local',
      onSelect: () => {
        state.kind = 'local';
        state.error = null;
        render();
      },
    }),
  );
  cards.appendChild(
    modeCard({
      kind: 'remote',
      title: '远程',
      desc:
        instances.length > 0
          ? `连接已在运行的实例 · 本机发现 ${instances.length} 个`
          : '连接已在运行的实例',
      selected: state.kind === 'remote',
      onSelect: () => {
        state.kind = 'remote';
        state.error = null;
        render();
      },
    }),
  );
  center.appendChild(cards);
  main.appendChild(center);
}

/** A tall selectable mode card: the logo is the visual anchor, the title the
 *  decision, and the line underneath only enough orientation to choose. */
function modeCard(options: {
  kind: Kind;
  title: string;
  desc: string;
  selected: boolean;
  onSelect: () => void;
}): HTMLElement {
  return h(
    'button',
    {
      type: 'button',
      class: `kind-card ${options.selected ? 'kind-card--selected' : ''}`,
      'aria-pressed': options.selected ? 'true' : 'false',
      onclick: options.onSelect,
    },
    h('span', { class: 'kind-card__logo', html: modeLogo(options.kind) }),
    h('span', { class: 'kind-card__title', text: options.title }),
    h('span', { class: 'kind-card__desc', text: options.desc }),
  );
}

/** Line icons rather than glyphs: at 40px a unicode box reads as a missing
 *  character, while these stay legible and inherit the selected colour. */
function modeLogo(kind: Kind): string {
  if (kind === 'local') {
    return `<svg viewBox="0 0 48 48" fill="none" stroke="currentColor" stroke-width="2.4"
      stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="6.5" y="8" width="35" height="25" rx="4" />
      <path d="M17 41h14M24 33v8" />
      <path d="M13 16h9M13 22h18" />
    </svg>`;
  }
  return `<svg viewBox="0 0 48 48" fill="none" stroke="currentColor" stroke-width="2.4"
    stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <circle cx="24" cy="24" r="16" />
    <path d="M8 24h32" />
    <path d="M24 8c5 4.5 5 27.5 0 32" />
    <path d="M24 8c-5 4.5-5 27.5 0 32" />
  </svg>`;
}

async function advanceKind(): Promise<void> {
  const kind = state.kind;
  if (!kind) return;
  state.step = kind;
  state.error = null;
  renderStep();
  if (kind === 'local') await refreshStatus(true);
  else await rescan();
}

function backToKind(): void {
  state.step = 'kind';
  state.error = null;
  renderStep();
}

// --------------------------------------------------------------- step two

function renderLocalStep(main: HTMLElement): void {
  if (!state.status) {
    main.appendChild(aLoading());
    return;
  }
  const status = state.status;

  if (status.runningLocal.length > 0) main.appendChild(runningSection(status));
  main.appendChild(runtimeSection(status));
  if (state.runtimeId === 'source') main.appendChild(sourceLocationSection(status));

  const sourceNote = state.runtimeId === 'source' ? sourceNotice(status) : null;
  if (sourceNote) main.appendChild(sourceNote);
}

function runningSection(status: LocalRuntimeState): HTMLElement {
  const running = section(`已有 ${status.runningLocal.length} 个本地后端在运行`);
  const list = h('ul', { class: 'checks' });
  for (const entry of status.runningLocal) {
    list.appendChild(
      h(
        'li',
        { class: 'check' },
        h('span', { class: 'check__mark check__mark--warn', text: '⚠' }),
        h('span', { text: entry.title }),
        h('span', {
          class: 'check__value',
          text: `${entry.phase}${entry.origin ? ` · ${entry.origin}` : ''}`,
        }),
        h(
          'span',
          { class: 'check__action' },
          h('button', {
            class: 'btn btn--sm',
            text: '切换过去',
            onclick: () => void store.activate(entry.tabId),
          }),
        ),
      ),
    );
  }
  running.body.appendChild(list);
  running.body.appendChild(
    h(
      'div',
      { class: 'row row--plain' },
      checkboxRow(
        '仍然再启动一个',
        '两者会共用同一个 DSH_HOME。',
        state.confirmExisting,
        (checked) => {
          state.confirmExisting = checked;
          render();
        },
      ),
    ),
  );
  return running.root;
}

function runtimeSection(status: LocalRuntimeState): HTMLElement {
  const refresh = h('button', {
    type: 'button',
    class: 'btn btn--quiet btn--sm',
    text: '重新检测',
    disabled: state.busy ? 'disabled' : undefined,
    onclick: () => void refreshStatus(true),
  });

  const runtimes = section('运行时', undefined, refresh);
  const cards = h('div', { class: 'cards' });
  cards.appendChild(
    selectableCard({
      icon: '◎',
      title: 'npx 版',
      desc: npxSummary(status),
      selected: state.runtimeId === 'npx',
      disabled: status.npx.state === 'blocked',
      reason: status.npx.blocked ?? undefined,
      onSelect: () => {
        state.runtimeId = 'npx';
        state.error = null;
        render();
      },
    }),
  );
  cards.appendChild(
    selectableCard({
      icon: '⌥',
      title: '源码版',
      desc: sourceSummary(status),
      selected: state.runtimeId === 'source',
      disabled: status.source.state === 'blocked',
      reason: status.source.blocked ?? undefined,
      onSelect: () => {
        state.runtimeId = 'source';
        state.error = null;
        render();
      },
    }),
  );
  runtimes.body.appendChild(h('div', { class: 'row row--plain' }, cards));
  return runtimes.root;
}

/**
 * The source checkout location is a decision, not a detail: it decides where
 * several hundred megabytes of repository and build output land. It stays on
 * the page, with one line saying exactly which checkout the selection produces.
 */
function sourceLocationSection(status: LocalRuntimeState): HTMLElement {
  const source = status.source;
  const info = store.state.info;

  const dirInput = h('input', {
    type: 'text',
    value: state.sourceDraft ?? source.container,
    placeholder: info?.root.defaultSourceContainer ?? 'D:\\',
    spellcheck: 'false',
  }) as HTMLInputElement;
  dirInput.addEventListener('input', () => {
    state.sourceDraft = dirInput.value;
    state.error = null;
  });
  dirInput.addEventListener('change', () => render());

  const control = h('div', { class: 'row__control row__control--wide' });
  control.appendChild(dirInput);
  control.appendChild(
    h('button', {
      class: 'btn',
      text: '浏览…',
      disabled: state.busy ? 'disabled' : undefined,
      onclick: () => void browseSourceDir(),
    }),
  );

  const draft = sourceDraft();
  const valid = draft !== '' && isAbsolute(draft);
  const location = wideRow(
    '安装位置',
    control,
    valid ? `将使用 ${joinPath(draft, source.checkoutName)}` : undefined,
  );
  if (draft && !valid) {
    location.appendChild(
      h('p', { class: 'field__error', text: '必须是绝对路径，例如 D:\\dsh。' }),
    );
  }

  // No heading: the row label already says what the field is, and a "源码版"
  // heading above one row would only repeat the selected card.
  const where = section('');
  where.body.appendChild(location);
  return where.root;
}

function sourceNotice(status: LocalRuntimeState): HTMLElement | null {
  const source = status.source;
  const checkout = source.status;
  if (source.blocked) return notice('danger', source.blocked);
  if (checkout.blocked) return notice('danger', checkout.blocked);
  if (checkout.dirty) {
    return notice('warn', '检测到未提交的修改：启动时会按现状构建，不会丢弃。');
  }
  return null;
}

function npxSummary(status: LocalRuntimeState): string {
  const npx = status.npx;
  // The first clause says what the runtime *is*; the rest is Rust's live answer
  // to what starting it would do right now. Both belong on the choice card.
  if (npx.blocked) return `官方发行版 · ${npx.blocked}`;
  if (npx.state === 'ready') return `官方发行版 · 已缓存 ${npx.hit}，无需下载`;
  if (npx.state === 'install') return '官方发行版 · 首次需下载约 450 MB';
  return `官方发行版 · 将更新到 ${npx.targetVersion ?? '当前通道'}`;
}

function sourceSummary(status: LocalRuntimeState): string {
  const source = status.source;
  if (source.blocked) return `自行构建 · ${source.blocked}`;
  if (source.state === 'ready') {
    const commit = shortCommit(source.status.commit);
    return `自行构建 · 已就绪${commit ? ` ${commit}` : ''}${
      source.status.dirty ? ' · 有本地修改' : ''
    }`;
  }
  return '自行构建 · 首次需克隆并构建';
}

function shortCommit(commit: string | null | undefined): string {
  if (!commit) return '';
  return commit.length > 8 ? commit.slice(0, 8) : commit;
}

// --------------------------------------------------------------- step two
// (remote)

function renderRemoteStep(main: HTMLElement, info: AppInfo): void {
  main.appendChild(remoteCards(info));
}

/**
 * One uninterrupted row of equal portrait cards: new connection, every locally
 * detected DSH, then every remembered connection. Categories live on the cards
 * themselves, so there are no section columns to break the sequence.
 */
function remoteCards(info: AppInfo): HTMLElement {
  const cards = h('div', { class: 'remote-cards' });
  cards.appendChild(newConnectionCard());

  const instances = state.status?.instances ?? [];
  const history = info.config.recentConnections ?? [];
  const pinned = info.config.pinnedConnections ?? [];

  // One map per source makes dedup explicit: history is the authority for an
  // origin it remembers, and the local scan is only a fallback for origins that
  // are not in history. A pin sits above both but keeps the authoritative data.
  const historyByOrigin = new Map<string, RecentConnection>();
  for (const entry of history) {
    if (!historyByOrigin.has(entry.id)) historyByOrigin.set(entry.id, entry);
  }
  const instanceByOrigin = new Map<string, RunningInstance>();
  for (const instance of instances) {
    if (!instanceByOrigin.has(instance.origin)) instanceByOrigin.set(instance.origin, instance);
  }

  const seen = new Set<string>();

  // Pinned origins first, in pin order. History wins over a local duplicate.
  for (const origin of pinned) {
    if (seen.has(origin)) continue;
    const entry = historyByOrigin.get(origin);
    const instance = instanceByOrigin.get(origin);
    if (entry) {
      cards.appendChild(historyCard(entry, true));
      seen.add(origin);
    } else if (instance) {
      cards.appendChild(instanceCard(instance, true));
      seen.add(origin);
    }
  }

  // Local detection, minus history entries and pins. No empty placeholder: a
  // missing category is simply absent from the sequence.
  const local = instances
    .filter((instance) => !seen.has(instance.origin) && !historyByOrigin.has(instance.origin))
    .sort((left, right) => left.port - right.port);
  for (const instance of local) {
    cards.appendChild(instanceCard(instance, false));
    seen.add(instance.origin);
  }

  // History: reachable targets first, then most recently connected.
  const remainingHistory = history
    .filter((entry) => !seen.has(entry.id))
    .sort((left, right) => {
      const live = Number(isOriginLive(right.id)) - Number(isOriginLive(left.id));
      if (live !== 0) return live;
      return right.lastUsedAt - left.lastUsedAt;
    });
  for (const entry of remainingHistory) {
    cards.appendChild(historyCard(entry, false));
    seen.add(entry.id);
  }

  return cards;
}

function newConnectionCard(): HTMLElement {
  const icon = makeRemoteIcon(currentNewKind(), isNewTargetLive(), true);
  const ip = h('input', {
    type: 'text',
    value: state.remoteIp,
    placeholder: '127.0.0.1',
    spellcheck: 'false',
    autocomplete: 'off',
    id: 'new-connection-ip',
  }) as HTMLInputElement;
  const port = h('input', {
    type: 'text',
    value: state.remotePort,
    placeholder: '3080',
    inputmode: 'numeric',
    spellcheck: 'false',
    autocomplete: 'off',
    id: 'new-connection-port',
  }) as HTMLInputElement;

  const syncIcon = () => icon.set(currentNewKind(), isNewTargetLive());
  ip.addEventListener('input', () => {
    state.remoteIp = ip.value;
    state.error = null;
    syncIcon();
  });
  port.addEventListener('input', () => {
    state.remotePort = port.value;
    state.error = null;
    syncIcon();
  });
  const submit = () => void startNewProbe();
  ip.addEventListener('keydown', (event: KeyboardEvent) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      submit();
    }
  });
  port.addEventListener('keydown', (event: KeyboardEvent) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      submit();
    }
  });

  return h(
    'article',
    { class: 'remote-card remote-card--new' },
    icon.root,
    h('span', { class: 'remote-card__eyebrow', text: '新连接' }),
    h('span', { class: 'remote-card__title', text: '连接 DSH' }),
    h('span', { class: 'remote-card__desc', text: '输入 IP 与端口' }),
    h(
      'div',
      { class: 'remote-card__fields' },
      h(
        'label',
        { class: 'remote-field' },
        h('span', { class: 'remote-field__label', text: 'IP 地址' }),
        ip,
      ),
      h(
        'label',
        { class: 'remote-field remote-field--port' },
        h('span', { class: 'remote-field__label', text: '端口' }),
        port,
      ),
    ),
    h(
      'div',
      { class: 'remote-card__actions' },
      h('button', {
        type: 'button',
        class: 'btn btn--primary',
        text: '连接',
        onclick: submit,
      }),
    ),
  );
}

async function startNewProbe(): Promise<void> {
  const raw = state.remoteIp.trim().replace(/^dsh web:\s*/i, '');
  if (!raw) {
    state.error = '请先填写 IP 地址。';
    render();
    return;
  }

  let origin: string | null = null;
  let url: string | null = null;
  if (/^https?:\/\//i.test(raw)) {
    try {
      const parsed = new URL(raw);
      origin = `${parsed.protocol}//${parsed.host}`;
      if (parsed.searchParams.get('token')) url = parsed.toString();
    } catch {
      origin = null;
    }
  } else {
    origin = targetOrigin(raw, state.remotePort);
  }

  if (!origin) {
    state.error = '地址格式不正确，请填写 IP 与 1–65535 的端口。';
    render();
    return;
  }
  startProbe({ origin, url, name: '我的连接', source: 'new' });
}

function currentNewOrigin(): string | null {
  const raw = state.remoteIp.trim();
  if (!raw) return null;
  if (/^https?:\/\//i.test(raw)) {
    try {
      const parsed = new URL(raw);
      return `${parsed.protocol}//${parsed.host}`;
    } catch {
      return null;
    }
  }
  return targetOrigin(raw, state.remotePort);
}

function currentNewKind(): Kind {
  const origin = currentNewOrigin();
  return origin && isLoopback(origin) ? 'local' : 'remote';
}

function isNewTargetLive(): boolean {
  const origin = currentNewOrigin();
  return origin ? isOriginLive(origin) : false;
}

function targetOrigin(hostInput: string, portInput: string): string | null {
  const host = hostInput.trim();
  if (!host || host.includes('/') || host.includes('?') || host.includes('#')) return null;
  const port = portInput.trim() || '3080';
  if (!/^\d{1,5}$/.test(port)) return null;
  const value = Number(port);
  if (value < 1 || value > 65535) return null;
  const bracketed = host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
  return `http://${bracketed}:${port}`;
}

function instanceCard(instance: RunningInstance, pinned: boolean): HTMLElement {
  const saved = savedConnectionFor(instance.origin);
  const icon = makeRemoteIcon(
    isLoopback(instance.origin) ? 'local' : 'remote',
    isOriginLive(instance.origin),
    true,
  );
  const title = instance.title ?? (instance.tabId ? 'dshell 启动的实例' : '本机实例');
  const meta = instance.tabId
    ? '已在选项卡中打开'
    : saved
      ? '已保存 token'
      : '需要启动 token';

  const action = instance.tabId
    ? h('button', {
        type: 'button',
        class: 'btn',
        text: '切换过去',
        onclick: () => void store.activate(instance.tabId as string),
      })
    : h('button', {
        type: 'button',
        class: 'btn',
        text: '连接',
        onclick: () =>
          startProbe({
            origin: instance.origin,
            url: saved?.url ?? null,
            name: saved?.label ?? '我的连接',
            source: 'instance',
          }),
      });

  return h(
    'article',
    { class: `remote-card${pinned ? ' remote-card--pinned' : ''}` },
    pinButton(instance.origin, pinned),
    icon.root,
    h('span', { class: 'remote-card__eyebrow', text: '本地检测' }),
    h('span', { class: 'remote-card__title', text: title }),
    h('span', {
      class: 'remote-card__sub',
      text: instance.origin.replace(/^https?:\/\//, ''),
    }),
    h('span', { class: 'remote-card__meta', text: meta }),
    h('div', { class: 'remote-card__actions' }, action),
  );
}

function historyCard(entry: RecentConnection, pinned: boolean): HTMLElement {
  const icon = makeRemoteIcon(
    isLoopback(entry.url) ? 'local' : 'remote',
    isOriginLive(entry.id),
    true,
  );
  const when = entry.lastUsedAt
    ? new Date(entry.lastUsedAt).toLocaleString(undefined, {
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
      })
    : '';

  return h(
    'article',
    { class: `remote-card${pinned ? ' remote-card--pinned' : ''}` },
    pinButton(entry.id, pinned),
    icon.root,
    h('span', { class: 'remote-card__eyebrow', text: '历史记录' }),
    h('span', { class: 'remote-card__title', text: entry.label }),
    h('span', {
      class: 'remote-card__sub',
      text: entry.id.replace(/^https?:\/\//, ''),
    }),
    h('span', { class: 'remote-card__meta', text: when || '已记住的连接' }),
    h(
      'div',
      { class: 'remote-card__actions' },
      h('button', {
        type: 'button',
        class: 'btn',
        text: '连接',
        onclick: () =>
          startProbe({
            origin: entry.id,
            url: entry.url,
            name: entry.label,
            source: 'history',
          }),
      }),
      h('button', {
        type: 'button',
        class: 'btn btn--danger',
        text: '删除',
        onclick: () => void forget(entry.id),
      }),
    ),
  );
}

/** The small pin in a card's top-right corner; pinned cards move above local
 *  detection and history on the next redraw. */
function pinButton(origin: string, pinned: boolean): HTMLElement {
  return h('button', {
    type: 'button',
    class: `remote-card__pin${pinned ? ' remote-card__pin--active' : ''}`,
    title: pinned ? '取消置顶' : '置顶',
    'aria-label': pinned ? '取消置顶' : '置顶',
    'aria-pressed': pinned ? 'true' : 'false',
    html: pinIcon(),
    onclick: (event: Event) => {
      event.stopPropagation();
      void togglePin(origin);
    },
  });
}

function pinIcon(): string {
  return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"
    stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M9 3h6l-.7 4.2 3.7 3.7V13h-4.5v6.5L12 21l-1.5-1.5V13H6v-2.1l3.7-3.7L9 3z" />
  </svg>`;
}

async function togglePin(origin: string): Promise<void> {
  const config = store.config;
  if (!config) return;
  const current = config.pinnedConnections ?? [];
  const next = current.includes(origin)
    ? current.filter((entry) => entry !== origin)
    : [origin, ...current];
  await store.updateConfig({ pinnedConnections: next });
  render();
}

function savedConnectionFor(origin: string): RecentConnection | undefined {
  return (store.config?.recentConnections ?? []).find((entry) => entry.id === origin);
}

/** Green-dot rule: a successful check in this visit, or a DSH the local scan
 *  currently sees on that exact origin. */
function isOriginLive(origin: string): boolean {
  if (!origin) return false;
  if (detected.has(origin)) return true;
  return (state.status?.instances ?? []).some((instance) => instance.origin === origin);
}

function makeRemoteIcon(
  kind: Kind,
  live: boolean,
  large = false,
): { root: HTMLElement; set: (kind: Kind, live: boolean) => void } {
  const glyph = h('span', { class: 'remote-icon__glyph', html: modeLogo(kind) });
  const dot = h('span', { class: 'remote-icon__dot', title: '连接检测成功' });
  dot.hidden = !live;
  const root = h(
    'span',
    { class: `remote-icon ${large ? 'remote-icon--lg' : ''}` },
    glyph,
    dot,
  );
  return {
    root,
    set(nextKind: Kind, nextLive: boolean) {
      glyph.innerHTML = modeLogo(nextKind);
      dot.hidden = !nextLive;
    },
  };
}

// ------------------------------------------------------- connection check

function startProbe(options: {
  origin: string;
  url: string | null;
  name: string;
  source: ProbeSource;
}): void {
  const saved = savedConnectionFor(options.origin);
  const url = options.url ?? saved?.url ?? null;
  const pending: PendingConnection = {
    origin: options.origin,
    url,
    name: options.name.trim() || saved?.label || '我的连接',
    tokenInput: url ? tokenFromUrl(url) : '',
    remote: !isLoopback(options.origin),
    allowPlaintext: false,
    phase: 'checking',
    error: null,
    detail: null,
    source: options.source,
  };
  state.advancedOpen = false;
  state.error = null;
  state.pending = pending;
  render();
  void runProbe(pending);
}

async function runProbe(pending: PendingConnection): Promise<void> {
  try {
    const report = await api.probeConnection(pending.origin, pending.url);
    if (state.pending !== pending) return;
    pending.origin = report.origin;
    pending.remote = !isLoopback(report.origin);
    if (report.ok) {
      pending.phase = 'ready';
      pending.error = null;
      pending.detail = null;
      detected.add(pending.origin);
    } else if (report.requiresToken) {
      pending.phase = 'needsToken';
      pending.error = null;
      pending.detail = null;
    } else {
      pending.phase = 'failed';
      pending.error = '连接检测失败';
    }
  } catch (error) {
    if (state.pending !== pending) return;
    const failure = asFailure(error);
    pending.error = failure.message;
    pending.detail = failure.detail ?? null;
    if (failure.code === 'attachAuthRequired') {
      pending.phase = 'needsToken';
      pending.tokenInput = '';
    } else {
      pending.phase = 'failed';
    }
  }
  render();
}

function retryProbe(pending: PendingConnection): void {
  pending.phase = 'checking';
  pending.error = null;
  pending.detail = null;
  render();
  void runProbe(pending);
}

function submitToken(pending: PendingConnection): void {
  const target = tokenTarget(pending);
  if (!target) {
    pending.error = '请粘贴 token 或完整的启动 URL。';
    render();
    return;
  }
  pending.origin = target.origin;
  pending.url = target.url;
  pending.remote = !isLoopback(target.origin);
  pending.allowPlaintext = false;
  pending.phase = 'checking';
  pending.error = null;
  pending.detail = null;
  render();
  void runProbe(pending);
}

function tokenTarget(pending: PendingConnection): { origin: string; url: string } | null {
  const input = pending.tokenInput
    .trim()
    .replace(/^dsh web:\s*/i, '')
    .trim();
  if (!input) return null;

  if (/^https?:\/\//i.test(input)) {
    try {
      const url = new URL(input);
      if (!url.searchParams.get('token')) return null;
      return { origin: `${url.protocol}//${url.host}`, url: url.toString() };
    } catch {
      return null;
    }
  }

  const match = /(?:^|[?&])token=([^&\s]+)/.exec(input);
  const token = match ? match[1] : input.startsWith('token=') ? input.slice(6) : input;
  if (!token) return null;
  return {
    origin: pending.origin,
    url: `${pending.origin}/?token=${encodeURIComponent(token)}`,
  };
}

function tokenFromUrl(url: string): string {
  try {
    return new URL(url).searchParams.get('token') ?? '';
  } catch {
    return '';
  }
}

function closeProbe(): void {
  state.pending = null;
  render();
}

async function confirmProbe(pending: PendingConnection): Promise<void> {
  if (pending.phase !== 'ready' || !pending.url) return;
  if (pending.remote && isPlaintext(pending.origin) && !pending.allowPlaintext) {
    pending.error = '请先确认明文 HTTP 风险。';
    render();
    return;
  }
  const name = pending.name.trim().slice(0, 48) || '我的连接';
  state.pending = null;
  detected.add(pending.origin);
  await attach(pending.url, pending.remote, pending.allowPlaintext, name);
}

function renderProbe(): void {
  const pending = state.pending;
  if (!pending) return;

  const body = h('div', { class: 'modal__body' });
  body.appendChild(probeTarget(pending));

  if (pending.phase === 'checking') {
    body.appendChild(
      h(
        'div',
        { class: 'probe-status' },
        h('span', { class: 'busy__spinner' }),
        h('span', {
          class: 'probe-status__text',
          text: pending.url ? '正在验证 token 与前端…' : '正在检测 DSH 服务…',
        }),
      ),
    );
  } else if (pending.phase === 'needsToken') {
    body.appendChild(
      notice('warn', '这个地址已响应为 DSH，但需要启动时打印的 token。'),
    );
    body.appendChild(
      probeField('启动 token', pending.tokenInput, '粘贴 token 或完整的启动 URL', (value) => {
        pending.tokenInput = value;
      }),
    );
    if (pending.error) body.appendChild(notice('danger', pending.error));
    if (pending.detail) body.appendChild(h('p', { class: 'probe-detail', text: pending.detail }));
  } else if (pending.phase === 'failed') {
    body.appendChild(notice('danger', pending.error ?? '连接检测失败'));
    if (pending.detail) body.appendChild(h('p', { class: 'probe-detail', text: pending.detail }));
  } else {
    body.appendChild(
      h(
        'div',
        { class: 'probe-success' },
        h('span', { class: 'check__mark check__mark--ok', text: '✓' }),
        h('span', { text: '连接检测成功' }),
      ),
    );
    body.appendChild(
      probeField('连接名称', pending.name, '我的连接', (value) => {
        pending.name = value;
      }, 48),
    );
    if (pending.remote && isPlaintext(pending.origin)) {
      body.appendChild(
        notice(
          'warn',
          '明文 HTTP：token 与会话内容不经加密传输。建议改用 SSH 端口转发或内网/VPN。',
        ),
      );
      body.appendChild(
        h(
          'div',
          { class: 'row row--plain' },
          checkboxRow('我已了解风险，仍要继续', '', pending.allowPlaintext, (checked) => {
            pending.allowPlaintext = checked;
            render();
          }),
        ),
      );
    }
    if (pending.error) body.appendChild(notice('danger', pending.error));
  }

  host.appendChild(modal('连接检测', body, probeActions(pending), closeProbe));
}

function probeTarget(pending: PendingConnection): HTMLElement {
  const icon = makeRemoteIcon(
    pending.remote ? 'remote' : 'local',
    pending.phase === 'ready' || detected.has(pending.origin),
  );
  return h(
    'div',
    { class: 'probe-target' },
    icon.root,
    h(
      'div',
      { class: 'probe-target__body' },
      h('span', {
        class: 'probe-target__label',
        text: pending.remote ? '远程实例' : '本地实例',
      }),
      h('span', { class: 'probe-target__origin', text: pending.origin }),
    ),
  );
}

function probeField(
  label: string,
  value: string,
  placeholder: string,
  onInput: (value: string) => void,
  maxLength?: number,
): HTMLElement {
  const input = h('input', {
    type: 'text',
    value,
    placeholder,
    spellcheck: 'false',
    autocomplete: 'off',
    maxlength: maxLength ? String(maxLength) : undefined,
  }) as HTMLInputElement;
  input.addEventListener('input', () => onInput(input.value));
  return h(
    'label',
    { class: 'probe-field' },
    h('span', { class: 'probe-field__label', text: label }),
    input,
  );
}

function probeActions(pending: PendingConnection): HTMLElement[] {
  const cancel = h('button', {
    type: 'button',
    class: 'btn',
    text: '取消',
    onclick: closeProbe,
  });

  if (pending.phase === 'checking') return [cancel];
  if (pending.phase === 'needsToken') {
    return [
      cancel,
      h('button', {
        type: 'button',
        class: 'btn btn--primary',
        text: '检测',
        onclick: () => submitToken(pending),
      }),
    ];
  }
  if (pending.phase === 'failed') {
    return [
      cancel,
      h('button', {
        type: 'button',
        class: 'btn btn--primary',
        text: '重试',
        onclick: () => retryProbe(pending),
      }),
    ];
  }

  const plaintext = pending.remote && isPlaintext(pending.origin);
  const blocked = !pending.url || (plaintext && !pending.allowPlaintext);
  return [
    cancel,
    h('button', {
      type: 'button',
      class: 'btn btn--primary',
      text: '连接',
      disabled: blocked ? 'disabled' : undefined,
      onclick: () => void confirmProbe(pending),
    }),
  ];
}

/** A plain-http non-loopback target, i.e. the case that needs a confirmation. */
function isPlaintext(origin: string): boolean {
  return origin.startsWith('http://') && !isLoopback(origin);
}

function aLoading(): HTMLElement {
  return h('div', { class: 'notice', text: '正在检测本地运行时与本机端口…' });
}

function notice(kind: 'ok' | 'warn' | 'danger', text: string): HTMLElement {
  return h('div', { class: `notice notice--${kind}`, text });
}

// ------------------------------------------------------------- advanced panel

/**
 * The second-level panel. Everything here is genuinely secondary: defaults that
 * apply to the next launch, and the machine-readable state behind the short
 * status lines on the page. Nothing in this panel is needed to decide *what* to
 * connect to.
 */
function renderAdvanced(): void {
  if (!state.advancedOpen) return;
  const info = store.state.info;
  if (!info) return;

  const mode: Kind | null = state.step === 'kind' ? state.kind : state.step;

  const body = h('div', { class: 'modal__body' });
  if (mode === 'local') {
    // Editable defaults first: a panel opened to change a setting should not
    // require scrolling past read-only facts to find it.
    for (const node of advancedBehavior(info, state.status)) body.appendChild(node);
    if (state.status) body.appendChild(advancedRuntime(state.status));
    body.appendChild(advancedFacts(info));
  } else if (mode === 'remote') {
    body.appendChild(advancedConnection(info));
    body.appendChild(advancedFacts(info));
  } else {
    for (const node of advancedBehavior(info, state.status)) body.appendChild(node);
    body.appendChild(advancedFacts(info));
  }

  const custom = state.dshHomeMode === 'custom';
  const invalid =
    custom && (!state.dshHomeCustom.trim() || !isAbsolute(state.dshHomeCustom.trim()));

  // The remote panel is informational: there is nothing to save, so it gets one
  // closing action rather than a Save button that would write an unchanged
  // config just to dismiss the dialog.
  const actions =
    mode === 'remote'
      ? [
          h('button', {
            class: 'btn btn--primary',
            text: '关闭',
            onclick: () => closeAdvanced(),
          }),
        ]
      : [
          h('button', {
            class: 'btn',
            text: '取消',
            onclick: () => closeAdvanced(),
          }),
          h('button', {
            class: 'btn btn--primary',
            text: '保存',
            disabled: invalid ? 'disabled' : undefined,
            onclick: () => void saveAdvanced(),
          }),
        ];

  host.appendChild(modal('高级选项', body, actions));
}

function closeAdvanced(): void {
  state.advancedOpen = false;
  adoptConfig();
  render();
}

function advancedFacts(info: AppInfo): HTMLElement {
  const facts = section('数据与状态');
  facts.body.appendChild(
    h(
      'dl',
      { class: 'kv kv--padded' },
      h('dt', { text: '数据根' }),
      h('dd', { text: info.root.path }),
      h('dt', { text: 'DSH_HOME' }),
      h('dd', { text: dshHomeSummary(info) }),
      h('dt', { text: '项目目录' }),
      h('dd', { text: '由 DSH 自己的界面选择，dshell 不干预' }),
    ),
  );
  return facts.root;
}

function dshHomeSummary(info: AppInfo): string {
  return info.config.dshHome.mode === 'system'
    ? `${info.root.defaultDshHome ?? '~/.dsh'}（系统默认，沿用已有会话与凭据）`
    : info.config.dshHome.mode === 'instance'
      ? '每个实例独立（在 dshell 数据根下）'
      : info.config.dshHome.path;
}

function advancedRuntime(status: LocalRuntimeState): HTMLElement {
  const detail = section('运行时详情');

  if (state.runtimeId === 'npx') {
    const npx = status.npx;
    detail.body.appendChild(
      h(
        'dl',
        { class: 'kv kv--padded' },
        h('dt', { text: '包' }),
        h('dd', { text: npx.packageSpec }),
        h('dt', { text: '缓存目录' }),
        h('dd', { text: npx.cacheDir }),
        h('dt', { text: 'Node.js' }),
        h('dd', { text: npx.nodeVersion ?? '未检测到' }),
        h('dt', { text: 'npm latest' }),
        h('dd', { text: npx.latestVersion ?? '未检查' }),
      ),
    );

    const cachedVersions = npx.cached.map((entry) => entry.version);
    if (cachedVersions.length > 0) {
      const chips = h('div', { class: 'chips' });
      for (const version of cachedVersions) {
        chips.appendChild(
          h('span', {
            class: `chip ${version === npx.hit ? 'chip--ok' : ''}`,
            text: version === npx.hit ? `${version}（将使用）` : version,
          }),
        );
      }
      detail.body.appendChild(row('缓存版本', chips));
    }
    if (npx.blocked) detail.body.appendChild(notice('danger', npx.blocked));
    return detail.root;
  }

  const source = status.source;
  const checkout = source.status;
  detail.body.appendChild(
    h(
      'dl',
      { class: 'kv kv--padded' },
      h('dt', { text: '父目录' }),
      h('dd', { text: source.container }),
      h('dt', { text: '安装于' }),
      h('dd', { text: checkout.root }),
      h('dt', { text: '仓库' }),
      h('dd', { text: source.repo }),
      h('dt', { text: '提交' }),
      h('dd', {
        text: checkout.commit
          ? `${checkout.commit}${checkout.branch ? `（${checkout.branch}）` : ''}${
              checkout.dirty ? ' · 有未提交的本地修改' : ''
            }`
          : checkout.checkout
            ? '（无法读取）'
            : '尚未克隆',
      }),
      h('dt', { text: '依赖 / 前端' }),
      h('dd', {
        text: `${checkout.installed ? '已安装' : '未安装'} / ${
          checkout.frontendBuilt ? '已构建' : '未构建'
        }`,
      }),
      h('dt', { text: 'git · pnpm' }),
      h('dd', {
        text: `${source.git ?? '未找到 git'} · ${
          source.pnpm ? `pnpm ${source.pnpm}` : '未找到 pnpm'
        }`,
      }),
    ),
  );

  if (source.blocked) detail.body.appendChild(notice('danger', source.blocked));
  else if (checkout.blocked) detail.body.appendChild(notice('danger', checkout.blocked));
  return detail.root;
}

function advancedBehavior(
  info: AppInfo,
  status: LocalRuntimeState | null,
): HTMLElement[] {
  const defaultHome = info.root.defaultDshHome ?? '~/.dsh';
  const homes = section('DSH_HOME');
  const stack = h('div', { class: 'card__stack' });
  const homeChoices: [DshHomeSetting['mode'], string, string][] = [
    ['system', '使用系统默认', `DSH 将使用：${defaultHome}`],
    ['instance', '为该实例建独立状态', `DSH 将使用：${info.root.path}\\dsh-home\\<名称>`],
    ['custom', '自定义路径', '必须是绝对路径'],
  ];
  for (const [value, label, preview] of homeChoices) {
    const selected = state.dshHomeMode === value;
    const radio = h('label', { class: `radio ${selected ? 'radio--selected' : ''}` });
    const input = h('input', {
      type: 'radio',
      name: 'advanced-dsh-home',
      checked: selected ? 'checked' : undefined,
    }) as HTMLInputElement;
    input.addEventListener('change', () => {
      state.dshHomeMode = value;
      render();
    });
    radio.appendChild(input);
    radio.appendChild(
      h(
        'span',
        { class: 'radio__body' },
        h('span', { text: label }),
        h('span', { class: 'radio__preview', text: preview }),
      ),
    );
    stack.appendChild(radio);
  }
  homes.body.appendChild(h('div', { class: 'row row--plain' }, stack));

  if (status && status.runningLocal.length > 0) {
    homes.body.appendChild(
      notice(
        'warn',
        '再启动一个后端会与现有后端共用 DSH_HOME：可能并发读写 profiles/ 与 storages/，这个组合官方没有验证过。',
      ),
    );
  }

  if (state.dshHomeMode === 'custom') {
    const input = h('input', {
      type: 'text',
      value: state.dshHomeCustom,
      placeholder: 'E:\\my-dsh-home（必须绝对路径）',
    }) as HTMLInputElement;
    input.addEventListener('input', () => {
      state.dshHomeCustom = input.value;
    });
    const control = h('div', { class: 'row__control row__control--wide' });
    control.appendChild(input);
    control.appendChild(
      h('button', {
        class: 'btn',
        text: '浏览',
        onclick: async () => {
          try {
            const picked = await api.pickDirectory(state.dshHomeCustom || null);
            if (picked) {
              state.dshHomeCustom = picked;
              render();
            }
          } catch (error) {
            state.error = asFailure(error).message;
            render();
          }
        },
      }),
    );
    const pathRow = wideRow('路径', control);
    const trimmed = state.dshHomeCustom.trim();
    if (trimmed && !isAbsolute(trimmed)) {
      pathRow.appendChild(
        h('p', {
          class: 'field__error',
          text: '必须是绝对路径 —— 相对路径会相对子进程的工作目录解析，落点会随目录漂移。',
        }),
      );
    }
    homes.body.appendChild(pathRow);
  }

  const lifecycle = section('选项卡关闭时');
  lifecycle.body.appendChild(
    h(
      'div',
      { class: 'row row--plain' },
      checkboxRow(
        '关闭选项卡时结束后台进程',
        '默认结束；取消后进程会保留，可之后用启动 URL 重新连接。',
        state.killOnClose,
        (checked) => {
          state.killOnClose = checked;
        },
      ),
    ),
  );

  return [homes.root, lifecycle.root];
}

function advancedConnection(info: AppInfo): HTMLElement {
  const recent = info.config.recentConnections ?? [];
  const conn = section('连接与凭据');
  conn.body.appendChild(
    h('p', {
      class: 'section__desc',
      text:
        'DSH 的 token 只存在于启动它的进程与终端输出里，dshell 无法凭空取得；' +
        '没有凭据的条目会在连接检测时询问 token。',
    }),
  );
  const target = state.pending?.origin ?? currentNewOrigin();
  if (target && isPlaintext(target)) {
    conn.body.appendChild(
      notice('warn', '目标使用明文 HTTP：token 与会话内容不经加密传输。'),
    );
  }
  conn.body.appendChild(
    h('p', {
      class: 'field__hint',
      text:
        recent.length > 0
          ? `已记住 ${recent.length} 个连接，可一键重连。`
          : '连接一次后会记住目标，方便下次一键重连。',
    }),
  );
  return conn.root;
}

async function saveAdvanced(): Promise<void> {
  const dshHome: DshHomeSetting =
    state.dshHomeMode === 'custom'
      ? { mode: 'custom', path: state.dshHomeCustom.trim() }
      : state.dshHomeMode === 'instance'
        ? { mode: 'instance' }
        : { mode: 'system' };
  await store.updateConfig({ dshHome, killOnClose: state.killOnClose });
  state.advancedOpen = false;
  store.toast('已保存高级选项');
  render();
}

/** A modal with a scrollable body and a right-aligned action row. */
function modal(
  title: string,
  body: HTMLElement,
  actions: HTMLElement[],
  onClose: () => void = closeAdvanced,
): HTMLElement {
  const overlay = h('div', {
    class: 'modal-backdrop',
    onclick: (event: Event) => {
      if (event.target === overlay) onClose();
    },
  });
  overlay.appendChild(
    h(
      'div',
      { class: 'modal', role: 'dialog', 'aria-modal': 'true' },
      h(
        'header',
        { class: 'modal__head' },
        h('h2', { class: 'modal__title', text: title }),
      ),
      body,
      h('footer', { class: 'modal__foot' }, ...actions),
    ),
  );
  return overlay;
}

// ------------------------------------------------------------ blocking progress

function renderBusy(): void {
  if (!state.busy) return;
  const busy = state.busy;
  const overlay = h('div', { class: 'modal-backdrop modal-backdrop--blocking' });
  const body = h('div', { class: 'modal__body modal__body--busy' });
  body.appendChild(
    h(
      'div',
      { class: 'busy' },
      h('span', { class: 'busy__spinner' }),
      h(
        'div',
        { class: 'busy__text' },
        h('p', { class: 'busy__stage', text: busy.stage }),
        h('p', { class: 'field__hint', text: busy.sub }),
      ),
    ),
  );
  if (busy.lines.length > 0) {
    const log = h('pre', {
      class: 'scroll busy__log',
      text: busy.lines.slice(-200).join('\n'),
    });
    body.appendChild(log);
    // A build log that stops moving looks hung, so the newest line is the one
    // kept in view.
    queueMicrotask(() => {
      log.scrollTop = log.scrollHeight;
    });
  }
  overlay.appendChild(
    h(
      'div',
      { class: 'modal modal--busy', role: 'dialog', 'aria-modal': 'true' },
      h('header', { class: 'modal__head' }, h('h2', { class: 'modal__title', text: busy.title })),
      body,
    ),
  );
  host.appendChild(overlay);
}

// -------------------------------------------------------------------- actions

function adoptConfig(): void {
  const config = store.config;
  if (!config) return;
  state.dshHomeMode = config.dshHome?.mode ?? 'system';
  state.dshHomeCustom = config.dshHome?.mode === 'custom' ? config.dshHome.path : '';
  state.killOnClose = config.killOnClose;
}

async function refreshStatus(deep: boolean): Promise<void> {
  try {
    // First paint from the disk-only answer, so the page is usable immediately;
    // the registry check is a second round trip and only refreshes what it can.
    state.status = await api.localRuntimeStatus(false);
    if (state.sourceDraft === null) state.sourceDraft = state.status.source.container;
    if (!state.busy) render();
    if (!deep) return;
    state.status = await api.localRuntimeStatus(true);
  } catch (error) {
    state.error = asFailure(error).message;
  }
  if (!state.busy) render();
}

async function rescan(): Promise<void> {
  try {
    const instances = await api.scanInstances();
    if (state.status) state.status.instances = instances;
  } catch (error) {
    state.error = asFailure(error).message;
  }
  render();
}

async function chooseSourceDir(): Promise<string | null> {
  const info = store.state.info;
  const initial =
    sourceDraft() || state.status?.source.container || info?.root.defaultSourceContainer || null;
  try {
    const picked = await api.pickDirectory(initial);
    if (!picked) return null;
    state.sourceDraft = picked;
    await saveSourceContainer(picked);
    return picked;
  } catch (error) {
    // A picker that cannot open must say so; silently returning `null` is what
    // makes a button look dead.
    state.error = asFailure(error).message;
    render();
    return null;
  }
}

/** The 浏览… button next to the source-location field. */
async function browseSourceDir(): Promise<void> {
  await chooseSourceDir();
}

/** The container the user has typed (or the effective one), trimmed. */
function sourceDraft(): string {
  return (state.sourceDraft ?? state.status?.source.container ?? '').trim();
}

/** A typed container that differs from the one in use, or `null`. */
function pendingSourceDir(): string | null {
  const draft = sourceDraft();
  const current = state.status?.source.container ?? '';
  return draft && draft !== current ? draft : null;
}

/** `join(dir, name)` with the platform separator the path already uses. */
function joinPath(dir: string, name: string): string {
  const trimmed = dir.replace(/[\\/]+$/, '');
  const separator = trimmed.includes('\\') || /^[a-zA-Z]:/.test(trimmed) ? '\\' : '/';
  return `${trimmed}${separator}${name}`;
}

async function saveSourceContainer(path: string): Promise<void> {
  const config = store.config;
  if (!config) return;
  await store.updateConfig({ sourceContainer: path });
  await refreshStatus(false);
}

async function startLocal(): Promise<void> {
  const status = state.status;
  if (!status) return;
  state.error = null;

  if (state.runtimeId === 'npx' && status.npx.state === 'blocked') {
    state.error = status.npx.blocked ?? 'npx 版当前不可用';
    render();
    return;
  }
  if (state.runtimeId === 'source' && status.source.state === 'blocked') {
    state.error = status.source.blocked ?? '源码版当前不可用';
    render();
    return;
  }
  if (status.runningLocal.length > 0 && !state.confirmExisting) {
    state.error =
      '已经有一个本地 dsh 后端在运行。确认要再启动一个，请勾选上面的「仍然再启动一个」。';
    render();
    return;
  }

  // A source launch with no checkout asks where to keep it first: cloning into a
  // directory the user never chose is not something to decide silently. A path
  // typed into the field is an answer already, so it is used as-is.
  //
  // The value is the *container*; Rust appends the fixed `deepseek-harness`
  // subdirectory, so nothing is ever cloned into the chosen directory itself.
  let sourceContainer: string | null = null;
  if (state.runtimeId === 'source') {
    const pending = pendingSourceDir();
    if (pending) {
      if (!isAbsolute(pending)) {
        state.error = `安装位置必须是绝对路径：${pending}`;
        render();
        return;
      }
      sourceContainer = pending;
    } else if (!status.source.status.checkout) {
      sourceContainer = await chooseSourceDir();
      if (!sourceContainer) return;
    }
  }

  const needsPrepare =
    state.runtimeId === 'source' ? true : status.npx.state !== 'ready';

  if (needsPrepare) {
    const ok = await prepare(state.runtimeId, sourceContainer);
    if (!ok) return;
  }

  await openTab();
}

/** Run the preparation half, blocking the page on it. */
async function prepare(runtimeId: RuntimeId, sourceContainer: string | null): Promise<boolean> {
  state.busy = {
    title: runtimeId === 'npx' ? '正在准备 npx 版' : '正在准备源码版',
    sub:
      runtimeId === 'npx'
        ? '下载与版本切换在这里完成，完成后会自动打开选项卡。请不要关闭这个页面。'
        : '克隆、安装依赖与构建都在这台机器上进行，冷启动可能需要数分钟。请不要关闭这个页面。',
    stage: runtimeId === 'npx' ? '正在解析 npx 缓存…' : '正在检查 git 与 pnpm…',
    lines: [],
  };
  render();

  try {
    state.status = await api.prepareRuntime(runtimeId, sourceContainer);
    // The pipeline may have created the checkout; show the container it used.
    state.sourceDraft = state.status.source.container;
    state.busy = null;
    return true;
  } catch (error) {
    const failure = asFailure(error);
    state.busy = null;
    state.error = failure.detail ? `${failure.message}\n${failure.detail}` : failure.message;
    render();
    return false;
  }
}

async function openTab(): Promise<void> {
  const dshHome: DshHomeSetting =
    state.dshHomeMode === 'custom'
      ? { mode: 'custom', path: state.dshHomeCustom.trim() }
      : state.dshHomeMode === 'instance'
        ? { mode: 'instance' }
        : { mode: 'system' };
  try {
    await api.newTab({
      kind: 'launch',
      // `null`: DSH decides the project folder in its own UI, so dshell only
      // needs a starting directory, and the home directory is what a user would
      // get from their own shell.
      workspace: null,
      runtimeId: state.runtimeId,
      dshHome,
      killOnClose: state.killOnClose,
      confirmExistingRunning: state.confirmExisting,
    });
  } catch (error) {
    const failure = asFailure(error);
    if (failure.code === 'instanceAlreadyRunning') {
      state.confirmExisting = true;
      state.error = `${failure.message}\n\n${failure.detail ?? ''}`;
    } else {
      state.error = failure.detail ? `${failure.message}\n${failure.detail}` : failure.message;
    }
    render();
    return;
  }
  // The new tab is created and activated by Rust; this page's job is done.
  await store.closeSelf();
}

async function attach(
  url: string,
  remote: boolean,
  allowPlaintext: boolean,
  label: string,
): Promise<void> {
  state.busy = {
    title: `正在连接「${label}」`,
    sub: '地址已通过检测，正在建立会话…',
    stage: '正在打开新的选项卡…',
    lines: [],
  };
  render();
  try {
    await api.newTab({ kind: 'attach', url, remote, allowPlaintext, label });
  } catch (error) {
    const failure = asFailure(error);
    state.busy = null;
    state.error = failure.detail ? `${failure.message}\n${failure.detail}` : failure.message;
    render();
    return;
  }
  state.busy = null;
  await store.closeSelf();
}

async function forget(id: string): Promise<void> {
  try {
    await api.forgetConnection(id);
    await store.refreshInfo();
  } catch (error) {
    store.toast(asFailure(error).message, true);
  }
  render();
}

// ---------------------------------------------------------------------- boot

void (async () => {
  await bootPage(store, render);
  adoptConfig();
  render();

  // Escape closes whichever modal is open. The blocking preparation overlay
  // deliberately has no dismissal at all: half a prepared runtime is not a state
  // the user may act on.
  window.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    if (state.pending) {
      closeProbe();
      return;
    }
    if (state.advancedOpen) closeAdvanced();
  });

  // Progress from Rust's preparation pipeline. Redraws are throttled: a build
  // can emit thousands of lines and each one is not worth a layout pass.
  let pending = false;
  await listen<PrepareProgress>(events.prepareProgress, (progress) => {
    if (!state.busy) return;
    if (progress.stage !== 'output') state.busy.stage = progress.message;
    else {
      state.busy.lines.push(progress.message);
      if (state.busy.lines.length > 400) state.busy.lines.splice(0, 100);
    }
    if (pending) return;
    pending = true;
    window.setTimeout(() => {
      pending = false;
      if (state.busy) render();
    }, 120);
  });

  await refreshStatus(true);
})();
