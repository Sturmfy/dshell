// The first-run wizard, shown as a tab.
//
// Two steps, and no "choose a working directory": DSH picks the project folder
// itself, so asking for one here would be dshell pretending to own a decision
// that is not its. Step two is a read-only environment report.
//
// This is the one tab that refuses to close before it is finished
// (`PageKind::closable` in Rust). Rust is the authority — this page asks it, so
// the button and the rule can never disagree.
//
// The wizard is the first thing a new user sees, so it carries the most design
// weight of any page here: a numbered stepper that shows position in the flow, a
// framed action bar, and the environment report laid out as a scannable table
// rather than a loose list.

import { api, asFailure } from '../ipc';
import { PageStore } from '../store';
import type { AppInfo } from '../types';
import {
  bootPage,
  checkboxRow,
  checkRow,
  h,
  pageFooter,
  pageFrame,
  pageHeader,
  pnpmCheckRow,
  pnpmInstallHint,
  section,
  toastView,
  wideRow,
} from './shared';

const host = document.getElementById('page') as HTMLElement;
const store = new PageStore('onboarding');

const state = {
  step: 0,
  root: '',
  writeEnv: true,
  error: null as string | null,
};

const STEP_LABELS = ['数据位置', '环境检查'];

// -------------------------------------------------------------------- render

function render(): void {
  const info = store.state.info;
  if (!info) {
    host.innerHTML = '';
    host.appendChild(h('div', { class: 'page__scroll page__loading', text: '正在加载…' }));
    return;
  }

  const { main } = pageFrame();
  main.appendChild(stepper());
  main.appendChild(
    pageHeader(
      state.step === 0 ? 'dshell 的数据放在哪里' : '运行环境检查',
      state.step === 0
        ? '配置、下载的运行时、以及每个实例的 webview 存储都会放在这里。DSH 自己的会话与凭据默认不动 —— 你原有的历史、设置与 API Key 都会照旧。'
        : '检查结果不影响「连接」功能：连接已有实例与远程实例不需要任何本地依赖。',
    ),
  );

  if (state.step === 0) appendDataRootStep(main, info);
  else appendEnvironmentStep(main, info);

  renderFooter(info);

  const toast = toastView(store);
  if (toast) host.appendChild(toast);
}

function stepper(): HTMLElement {
  const list = h('ol', { class: 'steps' });
  STEP_LABELS.forEach((label, index) => {
    if (index > 0) list.appendChild(h('li', { class: 'step__sep' }));
    const cls =
      index === state.step ? 'step step--active' : index < state.step ? 'step step--done' : 'step';
    list.appendChild(
      h(
        'li',
        { class: cls },
        h('span', { class: 'step__num', text: index < state.step ? '✓' : String(index + 1) }),
        h('span', { text: label }),
      ),
    );
  });
  return list;
}

function renderFooter(info: AppInfo): void {
  if (state.step === 0) {
    pageFooter(
      h('button', {
        class: 'btn btn--primary',
        text: '继续',
        onclick: () => void advance(),
      }),
      h('button', {
        class: 'btn',
        text: '使用默认位置',
        onclick: () => {
          state.root = info.root.path;
          render();
        },
      }),
      h('button', { class: 'page__foot-spacer' }),
      h('span', { class: 'field__hint', text: '首次配置完成前不能关闭这个选项卡' }),
    );
    return;
  }

  pageFooter(
    h('button', {
      class: 'btn',
      text: '返回',
      onclick: () => {
        state.step = 0;
        render();
      },
    }),
    h('button', { class: 'page__foot-spacer' }),
    h('button', {
      class: 'btn',
      text: '重新检测',
      onclick: () => void store.refreshInfo(true),
    }),
    h('button', {
      class: 'btn btn--primary',
      text: '完成',
      onclick: () => void finish(),
    }),
  );
}

function appendDataRootStep(main: HTMLElement, info: AppInfo): void {
  // No section heading here: the page title already says what step one decides,
  // so a "数据位置" heading above a single path row only added a level.
  const where = section('');

  const input = h('input', { type: 'text', value: state.root }) as HTMLInputElement;
  input.addEventListener('input', () => {
    state.root = input.value;
  });
  const control = h('div', { class: 'row__control row__control--wide' });
  control.appendChild(input);
  control.appendChild(
    h('button', {
      class: 'btn',
      text: '浏览',
      onclick: async () => {
        try {
          const picked = await api.pickDirectory(state.root || null);
          if (picked) {
            state.root = picked;
            state.error = null;
            render();
          }
        } catch (error) {
          // The native picker can be refused by the OS; say so rather than
          // leaving the button looking dead.
          state.error = asFailure(error).message;
          render();
        }
      },
    }),
  );
  where.body.appendChild(wideRow('路径', control));
  main.appendChild(where.root);

  // --- where that path came from, and whether it will be remembered
  const resolved = section(
    '这个位置是怎么定下来的',
    'dshell 按四级回落链解析数据根；下面是本次进程实际生效的那一层。',
  );
  resolved.body.appendChild(
    h(
      'dl',
      { class: 'kv kv--padded' },
      h('dt', { text: '生效层' }),
      h('dd', { text: info.root.sourceLabel }),
      h('dt', { text: '当前使用' }),
      h('dd', { text: info.root.path }),
    ),
  );
  resolved.body.appendChild(
    h(
      'div',
      { class: 'row row--plain' },
      checkboxRow(
        `把该位置写入用户环境变量 ${info.root.envVar}`,
        '这样把 dshell.exe 挪到别处也能找到同一份数据。写入失败时回落链仍然有效。',
        state.writeEnv,
        (checked) => {
          state.writeEnv = checked;
        },
      ),
    ),
  );
  resolved.body.appendChild(
    h('div', {
      class: 'notice notice--warn',
      text: '请不要选网络盘或同步盘（OneDrive 等）：会话历史频繁写入，且其中包含凭据。',
    }),
  );
  main.appendChild(resolved.root);

  if (state.error) {
    main.appendChild(h('div', { class: 'notice notice--danger', text: state.error }));
  }
}

async function advance(): Promise<void> {
  const root = state.root;
  const writable = await api.probeWritable(root).catch(() => false);
  if (!writable) {
    state.error = `该位置不可写：${root}`;
    render();
    return;
  }
  state.error = null;
  state.step = 1;
  render();
}

function appendEnvironmentStep(main: HTMLElement, info: AppInfo): void {
  const node = info.environment.node;

  const checks = section('检查项');
  const list = h('ul', { class: 'checks' });
  list.appendChild(
    checkRow(
      info.environment.webview2Version ? 'ok' : 'bad',
      'WebView2 运行时',
      info.environment.webview2Version ?? '未检测到（应用无法运行）',
    ),
  );
  list.appendChild(
    checkRow(
      node && !node.versionIssue ? 'ok' : 'warn',
      'Node.js',
      node
        ? `${node.version}${node.versionIssue ? ` — ${node.versionIssue}` : '（满足 ^22.19 || >=24）'}`
        : '未找到（启动本地不可用；连接不受影响）',
    ),
  );
  list.appendChild(
    pnpmCheckRow(info.environment, store.state.pnpmInstalling, () => void store.installPnpm()),
  );
  checks.body.appendChild(list);

  // What the button would run, and what it means. Neither is a warning: pnpm is
  // only ever needed by the user's own source checkout, never by dshell.
  const hint = pnpmInstallHint(info.environment);
  if (hint) checks.body.appendChild(hint);
  if (store.state.pnpmInstalling) {
    checks.body.appendChild(
      h('p', {
        class: 'field__hint',
        text: '正在通过 npm 全局安装 pnpm，通常需要 10–30 秒，请不要关闭这个窗口。',
      }),
    );
  }
  main.appendChild(checks.root);

  const summary = section('配置摘要');
  summary.body.appendChild(
    h(
      'dl',
      { class: 'kv kv--padded' },
      h('dt', { text: '数据位置' }),
      h('dd', { text: state.root }),
      h('dt', { text: 'DSH_HOME' }),
      h('dd', {
        text: `${info.root.defaultDshHome ?? '~/.dsh'}（默认不设置环境变量，复用你已有的会话与凭据）`,
      }),
      h('dt', { text: '项目目录' }),
      h('dd', { text: '由 DSH 自己的界面选择，dshell 不做干预' }),
    ),
  );
  summary.body.appendChild(
    h('div', {
      class: 'notice',
      text:
        '完成后会打开「新建连接」：连接已有实例或远程实例不需要任何本地依赖；' +
        '启动本地需要 Node 或一个独立的后端二进制。',
    }),
  );
  main.appendChild(summary.root);
}

async function finish(): Promise<void> {
  try {
    await api.completeOnboarding({ dataRoot: state.root, writeEnv: state.writeEnv });
    await store.refreshInfo();
    await store.refreshRuntimes();
    // Hand the window over to the new-connection page, then step out of the way:
    // with the wizard done, this tab is closable and no longer needed.
    await store.openPage('new');
    await store.closeSelf();
  } catch (error) {
    state.error = asFailure(error).message;
    store.toast(state.error, true);
  }
}

// ---------------------------------------------------------------------- boot

void (async () => {
  await bootPage(store, render);
  state.root = store.state.info?.root.path ?? '';
  render();
})();
