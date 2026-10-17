// The new-connection page, shown as a tab.
//
// A three-step flow, not one long form:
//
//   1. 方式   local (dshell starts a backend) or remote (a `dsh web` is already
//             running somewhere, localhost included)
//   2. 本地   exactly two backends — npx and source — each showing what it would
//             do *right now* (download? upgrade? clone? rebuild?)
//   2. 远程   a URL box plus the two lists that make reconnecting quick: the DSH
//             servers already listening on this machine, and the remembered
//             connections
//   3. launch / connect, closing this tab once the new one exists
//
// Two rules survive from the design and are the reason for several choices here:
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
  row,
  section,
  selectableCard,
  wideRow,
} from './shared';

const host = document.getElementById('page') as HTMLElement;
const store = new PageStore('new');

type Step = 'kind' | 'local' | 'remote';
type RuntimeId = 'npx' | 'source';

interface BusyState {
  title: string;
  sub: string;
  stage: string;
  lines: string[];
}

const state = {
  step: 'kind' as Step,
  runtimeId: 'npx' as RuntimeId,
  url: '',
  urlTouched: false,
  allowPlaintext: false,
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
  /** A one-line answer to "why did nothing happen", cleared on the next click. */
  hint: null as string | null,
};

const STEP_LABELS = ['连接方式', '详细设置'];

// -------------------------------------------------------------------- render

function render(): void {
  const info = store.state.info;
  if (!info) {
    host.innerHTML = '';
    host.appendChild(h('div', { class: 'page__scroll page__loading', text: '正在加载…' }));
    return;
  }

  const { main } = pageFrame();
  if (state.step !== 'kind') main.appendChild(stepper());
  main.appendChild(headline());

  if (state.step === 'kind') renderKindStep(main, info);
  else if (state.step === 'local') renderLocalStep(main);
  else renderRemoteStep(main, info);

  if (state.error) {
    main.appendChild(h('div', { class: 'notice notice--danger', text: state.error }));
  }

  renderFooter();
  renderAdvanced();
  renderBusy();

  if (store.state.toast && !state.busy) {
    host.appendChild(
      h('div', {
        class: `toast ${store.state.toast.error ? 'toast--error' : ''}`,
        text: store.state.toast.text,
      }),
    );
  }
}

function stepper(): HTMLElement {
  const list = h('ol', { class: 'steps' });
  STEP_LABELS.forEach((label, index) => {
    if (index > 0) list.appendChild(h('li', { class: 'step__sep' }));
    const current = index === 0 ? state.step === 'kind' : state.step !== 'kind';
    const done = index === 0 && state.step !== 'kind';
    const cls = current ? 'step step--active' : done ? 'step step--done' : 'step';
    list.appendChild(
      h(
        'li',
        { class: cls },
        h('span', { class: 'step__num', text: done ? '✓' : String(index + 1) }),
        h('span', { text: label }),
      ),
    );
  });
  return list;
}

function headline(): HTMLElement {
  if (state.step === 'kind') {
    return pageHeader(
      '新建连接',
      '「本地」由 dshell 启动并监管一个 dsh 后端；「远程」连接一个已经在运行的 dsh —— ' +
        '本机 127.0.0.1 上的实例也属于这一种。',
    );
  }
  if (state.step === 'local') {
    return pageHeader(
      '本地 · 选择运行时',
      '两种后端：npx 运行官方发行版，源码版则克隆仓库自行构建。两者都会在启动前把需要的东西准备好。',
    );
  }
  return pageHeader(
    '远程 · 连接地址',
    '粘贴 `dsh web` 打印的整行输出（含 token）。下面两个列表可以直接复用已知的实例。',
  );
}

function renderFooter(): void {
  if (state.step === 'kind') {
    pageFooter(
      h('span', {
        class: 'field__hint',
        text: '选择一种方式后进入详细设置。',
      }),
    );
    return;
  }

  const back = h('button', {
    class: 'btn',
    text: '返回',
    onclick: () => {
      state.step = 'kind';
      state.error = null;
      state.hint = null;
      render();
    },
  });

  if (state.step === 'local') {
    const busy = state.busy !== null;
    pageFooter(
      back,
      h('button', {
        class: 'btn',
        text: state.status ? '重新检测' : '检测中…',
        disabled: busy || !state.status ? 'disabled' : undefined,
        onclick: () => void refreshStatus(true),
      }),
      h('button', {
        class: 'btn',
        text: '高级选项',
        disabled: busy ? 'disabled' : undefined,
        onclick: () => {
          state.advancedOpen = true;
          render();
        },
      }),
      h('button', { class: 'page__foot-spacer' }),
      h('button', {
        class: 'btn btn--primary',
        text: state.busy ? '正在准备…' : startLabel(),
        disabled: busy || !state.status ? 'disabled' : undefined,
        onclick: () => void startLocal(),
      }),
    );
    return;
  }

  const busy = state.busy !== null;
  pageFooter(
    back,
    h('button', {
      class: 'btn',
      text: '重新扫描',
      disabled: busy ? 'disabled' : undefined,
      onclick: () => void rescan(),
    }),
    h('button', { class: 'page__foot-spacer' }),
    h('button', {
      class: 'btn btn--primary',
      text: state.busy ? '正在连接…' : '连接',
      disabled: busy ? 'disabled' : undefined,
      onclick: () => void connectRemote(),
    }),
  );
}

function startLabel(): string {
  const status = state.status;
  if (!status) return '启动';
  if (state.runtimeId === 'npx') {
    return status.npx.state === 'ready' ? '启动' : '下载并启动';
  }
  return status.source.state === 'ready' ? '更新并启动' : '安装并启动';
}

// ------------------------------------------------------------------ step one

function renderKindStep(main: HTMLElement, info: AppInfo): void {
  const instances = state.status?.instances ?? [];
  const running = state.status?.runningLocal ?? [];

  const modes = section('你要连接哪一种？');
  const cards = h('div', { class: 'cards cards--wide' });
  cards.appendChild(
    selectableCard({
      icon: '▣',
      title: '本地',
      desc: 'dshell 启动并监管一个 dsh 后端。可以选择 npx 版或源码版。',
      onSelect: () => {
        state.step = 'local';
        state.error = null;
        render();
        void refreshStatus(true);
      },
    }),
  );
  cards.appendChild(
    selectableCard({
      icon: '⇄',
      title: '远程',
      desc:
        instances.length > 0
          ? `连接一个已经在运行的 dsh（本机检测到 ${instances.length} 个）。`
          : '连接一个已经在运行的 dsh，填写它的启动 URL 即可，不需要任何本地依赖。',
      onSelect: () => {
        state.step = 'remote';
        state.error = null;
        render();
        void rescan();
      },
    }),
  );
  modes.body.appendChild(cards);
  main.appendChild(modes.root);

  if (running.length > 0) {
    main.appendChild(
      h('div', {
        class: 'notice notice--warn',
        text: `dshell 现在正在监管 ${running.length} 个本地后端（${running
          .map((entry) => entry.title)
          .join('、')}）。`,
      }),
    );
  }

  const facts = section('连接已有实例不需要本地依赖');
  facts.body.appendChild(
    h(
      'dl',
      { class: 'kv kv--padded' },
      h('dt', { text: '数据根' }),
      h('dd', { text: info.root.path }),
      h('dt', { text: 'DSH_HOME' }),
      h('dd', {
        text:
          info.config.dshHome.mode === 'system'
            ? `${info.root.defaultDshHome ?? '~/.dsh'}（系统默认，沿用你已有的会话与凭据）`
            : info.config.dshHome.mode === 'instance'
              ? '每个实例独立（在 dshell 数据根下）'
              : info.config.dshHome.path,
      }),
      h('dt', { text: '项目目录' }),
      h('dd', { text: '由 DSH 自己的界面选择，dshell 不干预' }),
    ),
  );
  main.appendChild(facts.root);
}

// ------------------------------------------------------------------ step two

function renderLocalStep(main: HTMLElement): void {
  if (!state.status) {
    main.appendChild(aLoading());
    return;
  }
  const status = state.status;

  // --- one warning before a second backend ever starts
  if (status.runningLocal.length > 0) {
    const running = section(
      '已经有一个本地后端在运行',
      '两个 dsh web 共用同一个 DSH_HOME 会并发读写 profiles/ 与 storages/，这个组合官方没有验证过。',
    );
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
          '我了解风险，仍然再启动一个',
          '启动时会走完整的准备流程；如果只是想回到已有实例，点上面的「切换过去」。',
          state.confirmExisting,
          (checked) => {
            state.confirmExisting = checked;
            render();
          },
        ),
      ),
    );
    main.appendChild(running.root);
  }

  // --- the two runtimes
  const runtimes = section('运行时');
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
  main.appendChild(runtimes.root);

  if (state.runtimeId === 'npx') renderNpxDetail(main, status);
  else renderSourceDetail(main, status);
}

function renderNpxDetail(main: HTMLElement, status: LocalRuntimeState): void {
  const npx = status.npx;
  const detail = section(
    'npx 版',
    '用 npx 直接运行官方发行版，dshell 把 npm 缓存放在自己的数据根里，不影响你的全局 npm 状态。',
  );

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
    ),
  );

  if (npx.blocked) {
    detail.body.appendChild(
      h('div', { class: 'notice notice--danger', text: npx.blocked }),
    );
    main.appendChild(detail.root);
    return;
  }

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
    detail.body.appendChild(row('本地缓存里的版本', chips));
  }

  detail.body.appendChild(
    h('div', {
      class: 'notice',
      text:
        npx.state === 'ready'
          ? `已缓存 ${npx.hit}，启动不需要网络。`
          : npx.state === 'install'
            ? '本地还没有这份包：点「下载并启动」后，dshell 会先在这一页把它下载好（约 450 MB / ' +
              '26,000 个文件，可能数分钟），期间不能进行其他操作，完成后再打开。'
            : `缓存里已有其他版本，启动前会切换到 ${npx.targetVersion ?? '当前通道的版本'}。` +
              '下载与切换都在这一页完成，期间不能进行其他操作。',
    }),
  );

  if (npx.latestVersion) {
    detail.body.appendChild(
      h('p', {
        class: 'field__hint',
        text: `npm 上的 latest：${npx.latestVersion}（可在「设置」里切换版本通道）`,
      }),
    );
  }
  main.appendChild(detail.root);
}

function renderSourceDetail(main: HTMLElement, status: LocalRuntimeState): void {
  const source = status.source;
  const checkout = source.status;
  const detail = section(
    '源码版',
    '克隆官方仓库并自行构建。每次启动前都会用 git 检查远程更新，有新提交就拉取、重新安装并构建。',
  );

  detail.body.appendChild(
    h(
      'dl',
      { class: 'kv kv--padded' },
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
        text: `${source.git ?? '未找到 git'} · ${source.pnpm ? `pnpm ${source.pnpm}` : '未找到 pnpm'}`,
      }),
    ),
  );

  if (source.blocked) {
    detail.body.appendChild(
      h('div', { class: 'notice notice--danger', text: source.blocked }),
    );
  } else if (checkout.blocked) {
    detail.body.appendChild(
      h('div', { class: 'notice notice--danger', text: checkout.blocked }),
    );
  } else if (checkout.dirty) {
    detail.body.appendChild(
      h('div', {
        class: 'notice notice--warn',
        text:
          '这个 checkout 有未提交的本地修改。启动时会跳过 git 更新，按现状安装并构建 —— ' +
          'dshell 不会丢弃你的改动。',
      }),
    );
  }

  // The location is an editable field, not only a button: a native dialog can be
  // refused by the OS (no interactive desktop, a locked-down session), and a
  // button that does nothing would then be the end of the flow.
  const info = store.state.info;
  const dirInput = h('input', {
    type: 'text',
    value: state.sourceDraft ?? source.container,
    placeholder: info?.root.defaultSourceContainer ?? 'D:\\（绝对路径）',
  }) as HTMLInputElement;
  dirInput.addEventListener('input', () => {
    state.sourceDraft = dirInput.value;
    state.error = null;
  });
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
  // The picked path is a *container*: the clone always lands in its
  // `<container>\deepseek-harness` subdirectory, so choosing `E:\projects` does not
  // take over `E:\projects` itself.
  const locationRow = wideRow(
    '安装位置（父目录）',
    control,
    `仓库会被克隆到该目录下的 ${source.checkoutName} 子目录，所选目录本身不会被占用或清空。`,
  );
  const draft = sourceDraft();
  if (draft) {
    locationRow.appendChild(
      h('p', {
        class: 'field__hint',
        text: `实际 checkout：${joinPath(draft, source.checkoutName)}`,
      }),
    );
    if (!isAbsolute(draft)) {
      locationRow.appendChild(
        h('p', { class: 'field__error', text: '必须是绝对路径，例如 D:\\dsh。' }),
      );
    }
  }
  detail.body.appendChild(locationRow);

  detail.body.appendChild(
    h('div', {
      class: 'notice',
      text:
        source.state === 'ready'
          ? '已就绪。点「更新并启动」会在这一页检查远程更新并（需要时）重新构建。'
          : '尚未安装。点「安装并启动」后，dshell 会在这一页依次克隆仓库、pnpm install、' +
            'pnpm run build —— 冷机器上可能需要数分钟，期间不能进行其他操作。',
    }),
  );
  main.appendChild(detail.root);
}

function npxSummary(status: LocalRuntimeState): string {
  const npx = status.npx;
  if (npx.blocked) return npx.blocked;
  if (npx.state === 'ready') return `已缓存 ${npx.hit} · 启动不需要下载`;
  if (npx.state === 'install') return '本地还没有 · 启动前需要下载约 450 MB';
  return `缓存里是 ${npx.cached[0]?.version ?? '其他版本'} · 将切换到 ${
    npx.targetVersion ?? '当前通道'
  }`;
}

function sourceSummary(status: LocalRuntimeState): string {
  const source = status.source;
  if (source.blocked) return source.blocked;
  if (source.state === 'ready') {
    return `已就绪 · 提交 ${source.status.commit ?? '未知'}${
      source.status.dirty ? '（有本地修改）' : ''
    } · 启动前会同步更新`;
  }
  return '尚未安装 · 启动前克隆仓库并 pnpm install + build';
}

// ------------------------------------------------------------------ step two
// (remote)

function renderRemoteStep(main: HTMLElement, info: AppInfo): void {
  const target = section(
    '启动 URL',
    '在运行 dsh web 的终端里复制整行 `dsh web: http://…/?token=…`，连 token 一起粘贴。',
  );

  const input = h('input', {
    type: 'text',
    value: state.url,
    placeholder: 'http://127.0.0.1:3080/?token=…',
    id: 'remote-url',
  }) as HTMLInputElement;
  // Typing must not re-render: rebuilding the page would drop the caret. Only a
  // change that flips the plaintext warning redraws, because that warning is the
  // one thing on screen that depends on what is being typed.
  const wasPlaintext = plaintextRemote();
  input.addEventListener('input', () => {
    state.url = input.value;
    state.urlTouched = true;
    state.error = null;
    if (plaintextRemote() !== wasPlaintext) render();
  });
  const control = h('div', { class: 'row__control row__control--wide' });
  control.appendChild(input);
  target.body.appendChild(wideRow('URL（含 token）', control));

  if (plaintextRemote()) {
    target.body.appendChild(
      h('div', {
        class: 'notice notice--warn',
        text: '明文 HTTP：token 与会话内容不经加密传输。建议改用 SSH 端口转发或内网/VPN。',
      }),
    );
    target.body.appendChild(
      h(
        'div',
        { class: 'row row--plain' },
        checkboxRow('我已了解风险，仍要继续', '', state.allowPlaintext, (checked) => {
          state.allowPlaintext = checked;
        }),
      ),
    );
  }
  main.appendChild(target.root);

  main.appendChild(instanceSection());

  const recent = info.config.recentConnections ?? [];
  const history = section(
    '历史连接',
    recent.length > 0
      ? '点击「连接」直接重连。条目里保存了当时的启动 URL（含 token）。'
      : '还没有历史记录：连过一次之后，这里会出现可以一键重连的条目。',
  );
  if (recent.length > 0) {
    const list = h('ul', { class: 'checks' });
    for (const entry of recent.slice(0, 6)) list.appendChild(historyRow(entry));
    history.body.appendChild(list);
  }
  main.appendChild(history.root);

  if (state.hint) {
    main.appendChild(h('div', { class: 'notice notice--warn', text: state.hint }));
  }
}

function instanceSection(): HTMLElement {
  const instances = state.status?.instances ?? [];
  const found = section(
    '本机正在运行的 dsh',
    instances.length > 0
      ? '这些是 dshell 扫描本机端口找到的实例。'
      : '没有扫描到正在运行的 dsh 后端。可以先用「本地」启动一个，或在终端里自己运行 `dsh web`。',
  );
  if (instances.length === 0) return found.root;

  const list = h('ul', { class: 'checks' });
  for (const instance of instances) list.appendChild(instanceRow(instance));
  found.body.appendChild(list);
  found.body.appendChild(
    h('p', {
      class: 'field__hint',
      text:
        'DSH 的 token 只存在于启动它的那个进程的内存与终端输出里，dshell 无法凭空取得。' +
        '没有凭据的条目只能帮你填好地址。',
    }),
  );
  return found.root;
}

function instanceRow(instance: RunningInstance): HTMLElement {
  const action = h('span', { class: 'check__action' });
  if (instance.tabId) {
    action.appendChild(
      h('button', {
        class: 'btn btn--sm',
        text: '切换过去',
        onclick: () => void store.activate(instance.tabId as string),
      }),
    );
  } else if (instance.connectable) {
    action.appendChild(
      h('button', {
        class: 'btn btn--sm',
        text: '连接',
        onclick: () => void connectKnown(instance.origin),
      }),
    );
  } else {
    action.appendChild(
      h('button', {
        class: 'btn btn--sm',
        text: '填入地址',
        onclick: () => {
          state.url = `${instance.origin}/`;
          state.urlTouched = true;
          state.hint = `已填入 ${instance.origin}/：请把那次启动打印的 token 补到 URL 里。`;
          render();
        },
      }),
    );
  }

  return h(
    'li',
    { class: 'check' },
    h('span', {
      class: `check__mark ${
        instance.tabId ? 'check__mark--ok' : instance.connectable ? 'check__mark--ok' : 'check__mark--warn'
      }`,
      text: instance.tabId ? '✓' : instance.connectable ? '✓' : '⚠',
    }),
    h('span', { text: instance.title ?? (instance.tabId ? 'dshell 启动的实例' : '本机实例') }),
    h('span', {
      class: 'check__value',
      text: `${instance.origin}${instance.notice ? ` · ${instance.notice}` : ''}`,
    }),
    action,
  );
}

function historyRow(entry: RecentConnection): HTMLElement {
  const when = entry.lastUsedAt
    ? new Date(entry.lastUsedAt).toLocaleString(undefined, {
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
      })
    : '';
  return h(
    'li',
    { class: 'check' },
    h('span', {
      class: `check__mark ${entry.remote ? 'check__mark--warn' : 'check__mark--ok'}`,
      text: entry.remote ? '⇄' : '▣',
    }),
    h('span', { text: entry.label }),
    h('span', {
      class: 'check__value',
      text: `${redact(entry.url)}${when ? ` · ${when}` : ''}`,
    }),
    h(
      'span',
      { class: 'check__action' },
      h('button', {
        class: 'btn btn--sm',
        text: '连接',
        onclick: () => void connectSaved(entry),
      }),
      h('button', {
        class: 'btn btn--sm btn--danger',
        text: '删除',
        onclick: () => void forget(entry.id),
      }),
    ),
  );
}

/** Never render a token, even whole. */
function redact(url: string): string {
  const index = url.indexOf('token=');
  return index >= 0 ? `${url.slice(0, index)}token=***` : url;
}

/** A plain-http non-loopback target, i.e. the case that needs a confirmation. */
function plaintextRemote(): boolean {
  const url = state.url.trim();
  return url.startsWith('http://') && !isLoopback(url);
}

function aLoading(): HTMLElement {
  return h('div', { class: 'notice', text: '正在检测本地运行时与本机端口…' });
}

// ------------------------------------------------------------- advanced modal

function renderAdvanced(): void {
  if (!state.advancedOpen) return;
  const info = store.state.info;
  const defaultHome = info?.root.defaultDshHome ?? '~/.dsh';

  const body = h('div', { class: 'modal__body' });
  body.appendChild(
    h('p', {
      class: 'section__desc',
      text:
        '这些是启动本地后端时使用的默认值。默认保持「使用系统默认」，因此你已有的会话、设置与 ' +
        'API Key 都会被沿用。',
    }),
  );

  const homes = section('DSH_HOME');
  const stack = h('div', { class: 'card__stack' });
  const homeChoices: [DshHomeSetting['mode'], string, string][] = [
    ['system', '使用系统默认', `DSH 将使用：${defaultHome}`],
    ['instance', '为该实例建独立状态', `DSH 将使用：${info?.root.path ?? '<数据根>'}\\dsh-home\\<名称>`],
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
  body.appendChild(homes.root);

  const lifecycle = section('选项卡关闭时');
  lifecycle.body.appendChild(
    h(
      'div',
      { class: 'row row--plain' },
      checkboxRow(
        '关闭选项卡时结束后台进程',
        '这是默认行为：选项卡是 dshell 对后端唯一的手柄。取消勾选会保留进程，方便之后用启动 URL 重新连接，代价是它不会被回收。',
        state.killOnClose,
        (checked) => {
          state.killOnClose = checked;
        },
      ),
    ),
  );
  body.appendChild(lifecycle.root);

  const custom = state.dshHomeMode === 'custom';
  const invalid = custom && (!state.dshHomeCustom.trim() || !isAbsolute(state.dshHomeCustom.trim()));

  host.appendChild(
    modal('高级选项', body, [
      h('button', {
        class: 'btn',
        text: '取消',
        onclick: () => {
          state.advancedOpen = false;
          adoptConfig();
          render();
        },
      }),
      h('button', {
        class: 'btn btn--primary',
        text: '保存',
        disabled: invalid ? 'disabled' : undefined,
        onclick: () => void saveAdvanced(),
      }),
    ]),
  );
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
): HTMLElement {
  const overlay = h('div', {
    class: 'modal-backdrop',
    onclick: (event: Event) => {
      if (event.target === overlay) {
        state.advancedOpen = false;
        adoptConfig();
        render();
      }
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
    const log = h('pre', { class: 'scroll busy__log', text: busy.lines.slice(-200).join('\n') });
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
    defaultUrl();
    if (!state.busy) render();
    if (!deep) return;
    state.status = await api.localRuntimeStatus(true);
    defaultUrl();
  } catch (error) {
    state.error = asFailure(error).message;
  }
  if (!state.busy) render();
}

async function rescan(): Promise<void> {
  try {
    const instances = await api.scanInstances();
    if (state.status) state.status.instances = instances;
    defaultUrl();
  } catch (error) {
    state.error = asFailure(error).message;
  }
  render();
}

/** Prefill the URL box with a local target, before the user has typed one. */
function defaultUrl(): void {
  if (state.urlTouched && state.url.trim()) return;
  const instances = state.status?.instances ?? [];
  const loopback = instances.find((instance) => instance.port > 0);
  state.url = loopback ? `${loopback.origin}/` : 'http://127.0.0.1:3080/';
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
  state.hint = null;

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
      '已经有一个本地 dsh 后端在运行。确认要再启动一个，请勾选上面的「我了解风险，仍然再启动一个」。';
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

async function connectRemote(): Promise<void> {
  const url = state.url.trim();
  if (!url) {
    state.error = '请先填写启动 URL（含 token）。';
    render();
    return;
  }
  await attach(url, !isLoopback(url), state.allowPlaintext);
}

/** Connect to a detected instance dshell already has a credential for. */
async function connectKnown(origin: string): Promise<void> {
  const saved = (store.config?.recentConnections ?? []).find((entry) => entry.id === origin);
  if (!saved) {
    state.hint = `${origin} 没有可用的 token：请从启动它的终端复制整行 URL。`;
    render();
    return;
  }
  await attach(saved.url, saved.remote, true);
}

async function connectSaved(entry: RecentConnection): Promise<void> {
  await attach(entry.url, entry.remote, true);
}

async function attach(url: string, remote: boolean, allowPlaintext: boolean): Promise<void> {
  state.busy = {
    title: '正在连接',
    sub: '解析地址并建立会话…',
    stage: '正在探测该地址是否是 DSH 服务…',
    lines: [],
  };
  render();
  try {
    await api.newTab({ kind: 'attach', url, remote, allowPlaintext });
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
  defaultUrl();
  render();

  // Escape closes the advanced-options dialog. The blocking preparation overlay
  // deliberately has no dismissal at all: half a prepared runtime is not a state
  // the user may act on.
  window.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape' || !state.advancedOpen) return;
    state.advancedOpen = false;
    adoptConfig();
    render();
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
