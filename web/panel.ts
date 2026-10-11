// dshell's panel UI: the new-connection form, settings, and the first-run wizard.
//
// This is a separate webview *below* the tab strip, not an overlay. That is what
// keeps the strip — and therefore the window's drag region and window buttons —
// visible and usable while a panel is open, and it is why this page needs no
// title bar of its own.
//
// It shares `Store` with the strip, so both views see exactly the same state and
// neither can drift.

import { api, asFailure } from './ipc';
import { Store } from './store';
import type { AppInfo, DshHomeSetting, RuntimeOption } from './types';

const host = document.getElementById('panel') as HTMLElement;
const store = new Store();

/** Which panel to show. Rust navigates this webview with `?panel=…`. */
const requested = new URLSearchParams(window.location.search).get('panel') ?? 'newTab';
const kind: 'newTab' | 'settings' | 'onboarding' =
  requested === 'settings' || requested === 'onboarding' ? requested : 'newTab';

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

function shortPath(path: string, max = 46): string {
  return path.length <= max ? path : `…${path.slice(path.length - max + 1)}`;
}

function slugOf(workspace: string): string {
  const parts = workspace.split(/[\\/]/).filter(Boolean);
  const name = parts[parts.length - 1] ?? 'home';
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

// -------------------------------------------------------------------- render

function render() {
  const info = store.state.info;
  host.innerHTML = '';
  if (!info) {
    host.appendChild(h('div', { class: 'panel__scroll', text: '正在加载…' }));
    return;
  }
  const body = h('div', { class: 'panel__scroll' });
  host.appendChild(body);

  if (kind === 'newTab') renderNewTab(body, info);
  else if (kind === 'settings') renderSettings(body, info);
  else renderOnboarding(body, info);

  if (store.state.toast) {
    host.appendChild(
      h('div', {
        class: `toast ${store.state.toast.error ? 'toast--error' : ''}`,
        text: store.state.toast.text,
      }),
    );
  }
}

// -------------------------------------------------------------- new connection

function renderNewTab(body: HTMLElement, info: AppInfo): void {
  const draft = store.state.draft;

  const head = h('div', { class: 'panel__head' });
  head.appendChild(h('h2', { class: 'panel__title', text: '新建连接' }));
  head.appendChild(
    h('p', {
      class: 'panel__sub',
      text:
        '连接已有实例与远程实例不需要任何本地依赖；启动本地需要 Node，' +
        '或一个独立的后端二进制。',
    }),
  );
  body.appendChild(head);

  const choices = h('div', { class: 'choices' });
  const kinds: [typeof draft.kind, string, string][] = [
    ['launch', '启动本地', '由 dshell 启动并监管一个 dsh 后端进程'],
    ['attach', '连接已有本地实例', '粘贴 `dsh web` 打印的那一行启动 URL'],
    ['remote', '连接远程实例', '粘贴远端 `dsh web` 的启动 URL'],
  ];
  for (const [value, label, desc] of kinds) {
    choices.appendChild(
      h(
        'button',
        {
          class: `choice ${draft.kind === value ? 'choice--active' : ''}`,
          onclick: () => {
            draft.kind = value;
            draft.error = null;
            render();
          },
        },
        h(
          'span',
          { class: 'choice__body' },
          h('span', { class: 'choice__label', text: label }),
          h('span', { class: 'choice__desc', text: desc }),
        ),
      ),
    );
  }
  body.appendChild(choices);

  if (draft.kind === 'launch') renderLaunchForm(body, info);
  else renderAttachForm(body);

  if (draft.error) {
    body.appendChild(h('div', { class: 'notice notice--danger', text: draft.error }));
  }

  const actions = h('div', { class: 'actions' });
  actions.appendChild(
    h('button', {
      class: 'btn btn--primary',
      text: draft.busy ? '正在创建…' : draft.kind === 'launch' ? '启动' : '连接并测试',
      disabled: draft.busy ? 'disabled' : undefined,
      onclick: () => void store.submitDraft(),
    }),
  );
  actions.appendChild(h('button', { class: 'btn', text: '取消', onclick: () => void store.openPanel('none') }));
  body.appendChild(actions);
}

function renderLaunchForm(body: HTMLElement, info: AppInfo): void {
  const draft = store.state.draft;

  // --- starting directory (optional)
  //
  // Deliberately not "工作目录": DSH decides the project folder itself, inside
  // its own UI. This only controls where the backend process starts, and leaving
  // it empty is the normal case.
  const dirField = h('div', { class: 'field' });
  dirField.appendChild(h('span', { class: 'field__label', text: '起始目录（可选）' }));
  const dirRow = h('div', { class: 'field__row' });
  const dirInput = h('input', {
    type: 'text',
    value: draft.workspace,
    placeholder: '留空即可 —— 项目目录在 DSH 里选择',
  }) as HTMLInputElement;
  dirInput.addEventListener('input', () => {
    draft.workspace = dirInput.value;
  });
  dirRow.appendChild(dirInput);
  dirRow.appendChild(
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
  if (draft.workspace.trim()) {
    dirRow.appendChild(
      h('button', {
        class: 'btn',
        text: '清除',
        onclick: () => {
          draft.workspace = '';
          render();
        },
      }),
    );
  }
  dirField.appendChild(dirRow);
  dirField.appendChild(
    h('span', {
      class: 'field__hint',
      text:
        '只影响后端进程启动时所在的目录。项目与会话的工作根目录由 DSH 自己的界面决定；' +
        '留空时 dshell 使用你的用户主目录。',
    }),
  );
  const recents = info.config.recentWorkspaces ?? [];
  if (recents.length > 0) {
    const recentRow = h('div', { class: 'field__hint' });
    recentRow.appendChild(document.createTextNode('最近：'));
    for (const path of recents.slice(0, 4)) {
      recentRow.appendChild(
        h('button', {
          class: 'btn',
          style: 'padding:2px 8px;margin:0 4px',
          text: shortPath(path, 26),
          title: path,
          onclick: () => {
            draft.workspace = path;
            render();
          },
        }),
      );
    }
    dirField.appendChild(recentRow);
  }
  body.appendChild(dirField);

  // --- runtime
  const runtimeField = h('div', { class: 'field' });
  runtimeField.appendChild(h('span', { class: 'field__label', text: '运行时' }));
  const runtimes = h('div', { class: 'choices' });
  for (const runtime of store.state.runtimes) {
    runtimes.appendChild(runtimeChoice(runtime));
  }
  if (store.state.runtimes.length === 0) {
    runtimes.appendChild(h('div', { class: 'field__hint', text: '正在检测运行时…' }));
  }
  runtimeField.appendChild(runtimes);

  if (!info.environment.node) {
    runtimeField.appendChild(
      h('div', {
        class: 'notice notice--warn',
        text:
          '未检测到 Node.js。可以安装 Node、使用便携 Node，或改选「其他后端二进制」——' +
          '连接模式完全不需要 Node。',
      }),
    );
    runtimeField.appendChild(
      h('button', {
        class: 'btn',
        text: '重新检测',
        onclick: () => void store.refreshInfo(true).then(() => store.refreshRuntimes()),
      }),
    );
  }

  const extraRow = h('div', { class: 'field__row', style: 'margin-top:8px' });
  extraRow.appendChild(
    h('button', {
      class: 'btn',
      text: info.config.extraBinary ? '更换后端二进制…' : '选择后端二进制…',
      onclick: async () => {
        const picked = await api.pickFile('选择能运行 `dsh web` 的可执行文件');
        if (!picked) return;
        try {
          await api.setExtraBinary(picked);
          await store.refreshInfo();
          await store.refreshRuntimes();
          store.state.draft.runtimeId = 'external';
          store.toast('已通过验证并采用该二进制');
        } catch (error) {
          store.toast(asFailure(error).message, true);
        }
        render();
      },
    }),
  );
  if (info.config.extraBinary) {
    extraRow.appendChild(
      h('span', { class: 'field__hint', text: shortPath(info.config.extraBinary, 40), title: info.config.extraBinary }),
    );
  }
  runtimeField.appendChild(extraRow);
  body.appendChild(runtimeField);

  // --- DSH_HOME
  const homeField = h('div', { class: 'field' });
  homeField.appendChild(h('span', { class: 'field__label', text: 'DSH_HOME（DSH 的状态目录）' }));
  const group = h('div', { class: 'radio-group' });
  const defaultHome = info.root.defaultDshHome ?? '~/.dsh';
  const instanceHome = `${info.root.path}\\dsh-home\\${slugOf(draft.workspace)}`;
  const options: [DshHomeSetting['mode'], string, string][] = [
    ['system', '使用系统默认', `DSH 将使用：${defaultHome}`],
    [
      'instance',
      '为该实例建独立状态',
      `DSH 将使用：${instanceHome}（空目录，不含你现有的会话历史）`,
    ],
    ['custom', '自定义路径', '必须是绝对路径；留空等同于使用系统默认'],
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
    radio.appendChild(
      h(
        'span',
        { class: 'radio__body' },
        h('span', { text: label }),
        h('span', { class: 'radio__preview', text: preview }),
      ),
    );
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
      homeField.appendChild(
        h('span', { class: 'field__error', text: '必须是绝对路径（相对路径会随工作目录漂移）' }),
      );
    }
  }
  homeField.appendChild(
    h('span', {
      class: 'field__hint',
      text: '留空即不设置 DSH_HOME，DSH 使用它自己的默认目录，因此你已有的会话、设置与 API Key 会被沿用。',
    }),
  );
  body.appendChild(homeField);

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
  body.appendChild(kill);
}

function runtimeChoice(runtime: RuntimeOption): HTMLElement {
  const draft = store.state.draft;
  return h(
    'button',
    {
      class: `choice ${draft.runtimeId === runtime.id ? 'choice--active' : ''} ${runtime.available ? '' : 'choice--disabled'}`,
      title: runtime.reason ?? '',
      onclick: () => {
        if (!runtime.available) {
          store.toast(runtime.reason ?? '该运行时当前不可用', true);
          return;
        }
        draft.runtimeId = runtime.id;
        render();
      },
    },
    h(
      'span',
      { class: 'choice__body' },
      h('span', {
        class: 'choice__label',
        text: runtime.label + (runtime.available ? '' : '（不可用）'),
      }),
      h('span', { class: 'choice__desc', text: runtime.reason ?? runtime.description }),
    ),
  );
}

function renderAttachForm(body: HTMLElement): void {
  const draft = store.state.draft;
  const field = h('div', { class: 'field' });
  field.appendChild(h('span', { class: 'field__label', text: '启动 URL（含 token）' }));
  const input = h('input', {
    type: 'text',
    value: draft.url,
    placeholder: 'http://127.0.0.1:9808/?token=…',
  }) as HTMLInputElement;
  input.addEventListener('input', () => {
    draft.url = input.value;
  });
  field.appendChild(input);
  field.appendChild(
    h('span', {
      class: 'field__hint',
      text:
        draft.kind === 'remote'
          ? '在远端终端运行 `dsh web --host <网卡IP> --trusted-host <authority>`，复制它打印的整行输出。'
          : '在运行 dsh web 的终端里复制整行 `dsh web: …`（包含 token）。',
    }),
  );

  const plaintext = draft.kind === 'remote' && draft.url.trim().startsWith('http://') && !isLoopback(draft.url);
  if (plaintext) {
    field.appendChild(
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
    field.appendChild(confirm);
  }
  body.appendChild(field);
}

// -------------------------------------------------------------------- settings

function renderSettings(body: HTMLElement, info: AppInfo): void {
  const config = info.config;

  const head = h('div', { class: 'panel__head' });
  head.appendChild(h('h2', { class: 'panel__title', text: '设置' }));
  head.appendChild(
    h('p', { class: 'panel__sub', text: `dshell ${info.version} · 固定 DSH ${info.pinnedDshVersion}` }),
  );
  body.appendChild(head);

  // --- data root
  body.appendChild(h('h3', { class: 'panel__title', text: '数据根（DSHELL_HOME）' }));
  body.appendChild(
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
    body.appendChild(h('div', { class: 'notice notice--warn', text: info.root.note }));
  }
  body.appendChild(
    h(
      'div',
      { class: 'actions' },
      h('button', {
        class: 'btn',
        text: '打开数据根',
        onclick: () =>
          void api.openPath(info.root.path).catch((error) => store.toast(asFailure(error).message, true)),
      }),
      h('button', {
        class: 'btn btn--danger',
        text: `移除 $${info.root.envVar} 环境变量`,
        onclick: async () => {
          try {
            await api.removeEnvVar();
            store.toast('已移除环境变量；dshell 之后按回落链定位数据根');
            await store.refreshInfo();
          } catch (error) {
            store.toast(asFailure(error).message, true);
          }
        },
      }),
    ),
  );

  // --- environment
  body.appendChild(h('h3', { class: 'panel__title', text: '运行环境' }));
  const node = info.environment.node;
  const checks = h('ul', { class: 'checks' });
  checks.appendChild(
    checkRow(
      node && !node.versionIssue ? 'ok' : node ? 'warn' : 'bad',
      'Node.js',
      node ? `${node.version}（${node.node}）` : '未检测到 —— 启动本地不可用，连接不受影响',
    ),
  );
  checks.appendChild(
    checkRow(info.environment.webview2Version ? 'ok' : 'bad', 'WebView2', info.environment.webview2Version ?? '未检测到'),
  );
  checks.appendChild(
    checkRow(info.environment.pnpm ? 'ok' : 'warn', 'pnpm', info.environment.pnpm ?? '未找到（源码维护模式不可用）'),
  );
  body.appendChild(checks);
  body.appendChild(
    h(
      'div',
      { class: 'actions' },
      h('button', { class: 'btn', text: '重新检测', onclick: () => void store.refreshInfo(true) }),
    ),
  );

  // --- preferences
  body.appendChild(h('h3', { class: 'panel__title', text: '偏好' }));
  const prefs = h('div', { class: 'radio-group' });

  prefs.appendChild(
    labeledSelect(
      'DSH 版本通道',
      config.dshVersionChannel,
      [
        ['pinned', `固定 ${info.pinnedDshVersion}（推荐）`],
        ['latest', 'latest（可能引入破坏性变更）'],
        ['alpha', 'alpha'],
      ],
      async (value) => {
        await store.updateConfig({ dshVersionChannel: value });
        await store.refreshRuntimes();
      },
    ),
  );

  prefs.appendChild(
    labeledSelect(
      '非活动选项卡',
      config.backgroundMode,
      [
        ['move-offscreen', '保持活跃（推荐，长会话不断流）'],
        ['hide', '隐藏以省内存（可能中断流）'],
      ],
      (value) => store.updateConfig({ backgroundMode: value }),
    ),
  );

  prefs.appendChild(
    checkboxRow(
      '固定端口以保持登录态',
      '默认用随机端口。cookie 绑定端口，端口不变才能跨重启保持登录；代价是可能撞上 EADDRINUSE。',
      config.fixedPort,
      (checked) => store.updateConfig({ fixedPort: checked }),
    ),
  );
  prefs.appendChild(
    checkboxRow(
      '新建选项卡默认在关闭时结束后台进程',
      '',
      config.killOnClose,
      (checked) => store.updateConfig({ killOnClose: checked }),
    ),
  );
  body.appendChild(prefs);

  // --- managed runtimes
  body.appendChild(h('h3', { class: 'panel__title', text: '本地维护的运行时' }));
  if (config.managedInstalls.length === 0) {
    body.appendChild(h('div', { class: 'field__hint', text: '还没有添加。可以指向你已经维护的 DSH 目录。' }));
  }
  for (const install of config.managedInstalls) {
    const row = h('div', { class: 'field__row', style: 'margin-bottom:6px' });
    row.appendChild(
      h('span', {
        class: 'field__hint',
        text: `${install.label} — ${install.root}（${install.mode}）`,
        style: 'flex:1',
      }),
    );
    row.appendChild(
      h('button', {
        class: 'btn btn--danger',
        text: '移除',
        onclick: async () => {
          await api.removeManagedInstall(install.id);
          await store.refreshInfo();
          await store.refreshRuntimes();
          render();
        },
      }),
    );
    body.appendChild(row);
  }
  body.appendChild(
    h(
      'div',
      { class: 'actions' },
      h('button', {
        class: 'btn',
        text: '添加本地维护目录…',
        onclick: async () => {
          const picked = await api.pickDirectory(null);
          if (!picked) return;
          try {
            await api.addManagedInstall(picked);
            await store.refreshInfo();
            await store.refreshRuntimes();
            store.toast('已添加');
          } catch (error) {
            store.toast(asFailure(error).message, true);
          }
          render();
        },
      }),
    ),
  );

  body.appendChild(
    h('div', { class: 'actions' }, h('button', { class: 'btn', text: '关闭', onclick: () => void store.openPanel('none') })),
  );
}

function checkboxRow(
  label: string,
  hint: string,
  checked: boolean,
  onChange: (checked: boolean) => void,
): HTMLElement {
  const row = h('label', { class: 'radio' });
  const box = h('input', { type: 'checkbox', checked: checked ? 'checked' : undefined }) as HTMLInputElement;
  box.addEventListener('change', () => onChange(box.checked));
  row.appendChild(box);
  const body = h('span', { class: 'radio__body' });
  body.appendChild(h('span', { text: label }));
  if (hint) body.appendChild(h('span', { class: 'radio__preview', text: hint }));
  row.appendChild(body);
  return row;
}

// ------------------------------------------------------------------ onboarding

/**
 * First-run wizard: two steps, and no "choose a working directory".
 *
 * DSH picks the project folder itself, so asking for one here would be dshell
 * pretending to own a decision that is not its. Step two is a read-only
 * environment report.
 */
function renderOnboarding(body: HTMLElement, info: AppInfo): void {
  const steps = h('div', { class: 'steps' });
  ['数据位置', '环境检查'].forEach((label, index) => {
    if (index > 0) steps.appendChild(h('span', { class: 'step__sep', text: '›' }));
    const step = h('span', {
      class: `step ${store.state.onboardingStep === index ? 'step--active' : ''}`,
    });
    step.appendChild(h('span', { class: 'step__num', text: String(index + 1) }));
    step.appendChild(h('span', { text: label }));
    steps.appendChild(step);
  });
  body.appendChild(steps);

  if (store.state.onboardingStep === 0) appendDataRootStep(body, info);
  else appendEnvironmentStep(body, info);
}

function appendDataRootStep(body: HTMLElement, info: AppInfo): void {
  body.appendChild(h('h2', { class: 'panel__title', text: '选择一个位置存放 dshell 的数据' }));
  body.appendChild(
    h('p', {
      class: 'panel__sub',
      text:
        '配置、下载的运行时、以及每个实例的 webview 存储都会放在这里。' +
        'DSH 自己的会话与凭据默认不动 —— 你原有的历史、设置与 API Key 都会照旧。',
    }),
  );

  const row = h('div', { class: 'field__row', style: 'margin:16px 0' });
  const input = h('input', { type: 'text', value: store.state.onboardingRoot }) as HTMLInputElement;
  input.addEventListener('input', () => {
    store.state.onboardingRoot = input.value;
  });
  row.appendChild(input);
  row.appendChild(
    h('button', {
      class: 'btn',
      text: '浏览',
      onclick: async () => {
        const picked = await api.pickDirectory(store.state.onboardingRoot || null);
        if (picked) {
          store.state.onboardingRoot = picked;
          render();
        }
      },
    }),
  );
  body.appendChild(row);

  body.appendChild(
    h('div', {
      class: 'notice',
      text: `最终生效层：${info.root.sourceLabel}\n当前进程实际使用：${info.root.path}`,
    }),
  );

  body.appendChild(
    checkboxRow(
      `把该位置写入用户环境变量 ${info.root.envVar}`,
      '这样把 dshell.exe 挪到别处也能找到同一份数据。写入失败时回落链仍然有效。',
      store.state.onboardingWriteEnv,
      (checked) => {
        store.state.onboardingWriteEnv = checked;
      },
    ),
  );

  body.appendChild(
    h('div', {
      class: 'notice notice--warn',
      text: '⚠ 请不要选网络盘或同步盘（OneDrive 等）：会话历史频繁写入，且其中包含凭据。',
    }),
  );

  if (store.state.onboardingError) {
    body.appendChild(h('div', { class: 'notice notice--danger', text: store.state.onboardingError }));
  }

  body.appendChild(
    h(
      'div',
      { class: 'actions' },
      h('button', {
        class: 'btn btn--primary',
        text: '继续',
        onclick: async () => {
          const root = store.state.onboardingRoot;
          const writable = await api.probeWritable(root).catch(() => false);
          if (!writable) {
            store.state.onboardingError = `该位置不可写：${root}`;
            render();
            return;
          }
          store.state.onboardingError = null;
          store.state.onboardingStep = 1;
          render();
        },
      }),
      h('button', {
        class: 'btn',
        text: '使用默认位置',
        onclick: () => {
          store.state.onboardingRoot = info.root.path;
          render();
        },
      }),
    ),
  );
}

function appendEnvironmentStep(body: HTMLElement, info: AppInfo): void {
  body.appendChild(h('h2', { class: 'panel__title', text: '运行环境检查' }));
  body.appendChild(h('p', { class: 'panel__sub', text: '检查结果不影响「连接」功能。' }));

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
      node && !node.versionIssue ? 'ok' : 'warn',
      'Node.js',
      node
        ? `${node.version}${node.versionIssue ? ` — ${node.versionIssue}` : '（满足 ^22.19 || >=24）'}`
        : '未找到（启动本地不可用；连接不受影响）',
    ),
  );
  checks.appendChild(
    checkRow(info.environment.pnpm ? 'ok' : 'warn', 'pnpm', info.environment.pnpm ?? '未找到（源码维护模式不可用）'),
  );
  body.appendChild(checks);

  body.appendChild(
    h(
      'dl',
      { class: 'kv', style: 'margin-bottom:12px' },
      h('dt', { text: '数据位置' }),
      h('dd', { text: store.state.onboardingRoot }),
      h('dt', { text: 'DSH_HOME' }),
      h('dd', {
        text: `${info.root.defaultDshHome ?? '~/.dsh'}（默认不设置环境变量，复用你已有的会话与凭据）`,
      }),
      h('dt', { text: '项目目录' }),
      h('dd', { text: '由 DSH 自己的界面选择，dshell 不做干预' }),
    ),
  );

  body.appendChild(
    h('div', {
      class: 'notice',
      text:
        '完成后进入「新建连接」：连接已有实例或远程实例不需要任何本地依赖；' +
        '启动本地需要 Node 或一个独立的后端二进制。',
    }),
  );

  body.appendChild(
    h(
      'div',
      { class: 'actions' },
      h('button', {
        class: 'btn',
        text: '返回',
        onclick: () => {
          store.state.onboardingStep = 0;
          render();
        },
      }),
      h('button', { class: 'btn', text: '重新检测', onclick: () => void store.refreshInfo(true) }),
      h('button', {
        class: 'btn btn--primary',
        text: '完成',
        onclick: async () => {
          try {
            await api.completeOnboarding({
              dataRoot: store.state.onboardingRoot,
              writeEnv: store.state.onboardingWriteEnv,
            });
            store.state.onboardingStep = 0;
            await store.refreshInfo();
            await store.refreshRuntimes();
            await store.openPanel('newTab');
          } catch (error) {
            store.state.onboardingError = asFailure(error).message;
            store.toast(store.state.onboardingError, true);
          }
        },
      }),
    ),
  );
}

// ---------------------------------------------------------------------- boot

// Escape closes the panel from inside it too, so the muscle memory works
// wherever the focus happens to be.
window.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') void store.openPanel('none');
});

void (async () => {
  store.subscribe(render);
  await store.start();
  render();
})();
