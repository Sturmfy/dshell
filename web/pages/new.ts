// The new-connection page, shown as a tab.
//
// It used to be a panel below the tab strip. It is a tab now, which is why this
// file looks like a small application: it owns its own form state, its own
// submit path, and it closes itself when the connection is created.
//
// Layout follows the shape a settings-style product uses — a page header, then
// titled sections separated by hairlines, with the primary action pinned to a
// footer so it stays reachable however long the form gets.
//
// Two rules from the design survive verbatim:
//   * connecting to an existing/remote instance needs no local dependencies;
//   * dshell does not choose the project folder — DSH does. The directory here
//     is only where the backend process starts, and it is optional.

import { api, asFailure } from '../ipc';
import { PageStore } from '../store';
import type { AppInfo, DshHomeSetting, RuntimeOption } from '../types';
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
  shortPath,
  slugOf,
  wideRow,
} from './shared';

const host = document.getElementById('page') as HTMLElement;
const store = new PageStore('new');

const draft = {
  kind: 'launch' as 'launch' | 'attach' | 'remote',
  url: '',
  runtimeId: 'npm',
  workspace: '',
  dshHomeMode: 'system' as DshHomeSetting['mode'],
  dshHomeCustom: '',
  allowPlaintext: false,
  killOnClose: false,
  busy: false,
  error: null as string | null,
};

const MODES: [typeof draft.kind, string, string, string][] = [
  ['launch', '⊕', '启动本地', '由 dshell 启动并监管一个 dsh 后端进程'],
  ['attach', '⇥', '连接已有实例', '粘贴本机 `dsh web` 打印的启动 URL'],
  ['remote', '⇄', '连接远程实例', '粘贴远端 `dsh web` 的启动 URL'],
];

// -------------------------------------------------------------------- render

function render(): void {
  const info = store.state.info;
  if (!info) {
    host.innerHTML = '';
    host.appendChild(h('div', { class: 'page__scroll page__loading', text: '正在加载…' }));
    return;
  }

  const { main } = pageFrame();
  main.appendChild(
    pageHeader(
      '新建连接',
      '连接已有实例与远程实例不需要任何本地依赖；启动本地需要 Node，或一个独立的后端二进制。',
    ),
  );

  // --- how to connect (the three modes, as cards)
  const modeSection = section('连接方式');
  const cards = h('div', { class: 'cards' });
  for (const [value, icon, label, desc] of MODES) {
    cards.appendChild(
      selectableCard({
        icon,
        title: label,
        desc,
        selected: draft.kind === value,
        onSelect: () => {
          draft.kind = value;
          draft.error = null;
          render();
        },
      }),
    );
  }
  modeSection.body.appendChild(cards);
  main.appendChild(modeSection.root);

  if (draft.kind === 'launch') renderLaunchForm(main, info);
  else renderAttachForm(main);

  if (draft.error) {
    main.appendChild(h('div', { class: 'notice notice--danger', text: draft.error }));
  }

  const submitButton = h('button', {
    class: 'btn btn--primary',
    text: draft.busy ? '正在创建…' : draft.kind === 'launch' ? '启动' : '连接并测试',
    disabled: draft.busy ? 'disabled' : undefined,
    onclick: () => void submit(),
  });

  pageFooter(
    submitButton,
    h('button', { class: 'page__foot-spacer' }),
    h('button', {
      class: 'btn',
      text: '关闭这个选项卡',
      onclick: () => void store.closeSelf(),
    }),
  );

  if (store.state.toast) {
    host.appendChild(
      h('div', {
        class: `toast ${store.state.toast.error ? 'toast--error' : ''}`,
        text: store.state.toast.text,
      }),
    );
  }
}

function renderLaunchForm(main: HTMLElement, info: AppInfo): void {
  // --- starting directory (optional)
  //
  // Deliberately not "工作目录": DSH decides the project folder itself, inside
  // its own UI. This only controls where the backend process starts, and leaving
  // it empty is the normal case.
  const dir = section(
    '起始目录（可选）',
    '只影响后端进程启动时所在的目录。项目与会话的工作根目录由 DSH 自己的界面决定；' +
      '留空时 dshell 使用你的用户主目录。',
  );

  const dirInput = h('input', {
    type: 'text',
    value: draft.workspace,
    placeholder: '留空即可 —— 项目目录在 DSH 里选择',
  }) as HTMLInputElement;
  dirInput.addEventListener('input', () => {
    draft.workspace = dirInput.value;
  });

  const dirControl = h('div', { class: 'row__control row__control--wide' });
  dirControl.appendChild(dirInput);
  dirControl.appendChild(
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
    dirControl.appendChild(
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
  dir.body.appendChild(wideRow('起始目录', dirControl));

  const recents = info.config.recentWorkspaces ?? [];
  if (recents.length > 0) {
    const chips = h('div', { class: 'chips' });
    for (const path of recents.slice(0, 4)) {
      chips.appendChild(
        h('button', {
          class: 'chip',
          text: shortPath(path, 30),
          title: path,
          onclick: () => {
            draft.workspace = path;
            render();
          },
        }),
      );
    }
    dir.body.appendChild(row('最近使用', chips, '点一下即可填入上面的输入框。'));
  }
  main.appendChild(dir.root);

  // --- runtime
  const runtime = section(
    '运行时',
    '决定 dshell 用哪一种后端启动 DSH。「其他后端二进制」完全不需要 Node。',
  );
  const runtimeCards = h('div', { class: 'cards' });
  for (const option of store.state.runtimes) {
    runtimeCards.appendChild(runtimeChoice(option));
  }
  if (store.state.runtimes.length === 0) {
    runtimeCards.appendChild(h('div', { class: 'field__hint', text: '正在检测运行时…' }));
  }
  runtime.body.appendChild(h('div', { class: 'row row--plain' }, runtimeCards));

  const extraControl = h('div', { class: 'row__control' });
  extraControl.appendChild(
    h('button', {
      class: 'btn',
      text: info.config.extraBinary ? '更换二进制…' : '选择二进制…',
      onclick: async () => {
        const picked = await api.pickFile('选择能运行 `dsh web` 的可执行文件');
        if (!picked) return;
        try {
          await api.setExtraBinary(picked);
          await store.refreshInfo();
          await store.refreshRuntimes();
          draft.runtimeId = 'external';
          store.toast('已通过验证并采用该二进制');
        } catch (error) {
          store.toast(asFailure(error).message, true);
        }
        render();
      },
    }),
  );
  runtime.body.appendChild(
    row(
      '其他后端二进制',
      extraControl,
      info.config.extraBinary
        ? `当前：${shortPath(info.config.extraBinary, 60)}`
        : '指定任意能运行 `dsh web` 的可执行文件，不需要 Node。采用前会验证它真的是 DSH 后端。',
    ),
  );

  if (!info.environment.node) {
    runtime.body.appendChild(
      h('div', {
        class: 'notice notice--warn',
        text:
          '未检测到 Node.js。可以安装 Node、使用便携 Node，或改选「其他后端二进制」——' +
          '连接模式完全不需要 Node。',
      }),
    );
    runtime.body.appendChild(
      h('div', { class: 'actions' }, h('button', {
        class: 'btn',
        text: '重新检测',
        onclick: () => void store.refreshInfo(true).then(() => store.refreshRuntimes()),
      })),
    );
  }
  main.appendChild(runtime.root);

  // --- DSH_HOME
  const home = section(
    'DSH_HOME（DSH 的状态目录）',
    '留空即不设置该变量，DSH 使用它自己的默认目录，因此你已有的会话、设置与 API Key 会被沿用。',
  );
  const defaultHome = info.root.defaultDshHome ?? '~/.dsh';
  const instanceHome = `${info.root.path}\\dsh-home\\${slugOf(draft.workspace)}`;
  const homes: [DshHomeSetting['mode'], string, string][] = [
    ['system', '使用系统默认', `DSH 将使用：${defaultHome}`],
    ['instance', '为该实例建独立状态', `DSH 将使用：${instanceHome}`],
    ['custom', '自定义路径', '必须是绝对路径'],
  ];
  const homeStack = h('div', { class: 'card__stack' });
  for (const [value, label, preview] of homes) {
    const selected = draft.dshHomeMode === value;
    const radio = h('label', { class: `radio ${selected ? 'radio--selected' : ''}` });
    const input = h('input', {
      type: 'radio',
      name: 'dsh-home',
      checked: selected ? 'checked' : undefined,
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
    homeStack.appendChild(radio);
  }
  home.body.appendChild(h('div', { class: 'row row--plain' }, homeStack));

  if (draft.dshHomeMode === 'custom') {
    const input = h('input', {
      type: 'text',
      value: draft.dshHomeCustom,
      placeholder: 'E:\\my-dsh-home（必须绝对路径）',
    }) as HTMLInputElement;
    input.addEventListener('input', () => {
      draft.dshHomeCustom = input.value;
    });
    const control = h('div', { class: 'row__control row__control--wide' });
    control.appendChild(input);
    control.appendChild(
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
    const pathRow = wideRow('路径', control);
    const trimmed = draft.dshHomeCustom.trim();
    if (trimmed && !isAbsolute(trimmed)) {
      pathRow.appendChild(
        h('p', {
          class: 'field__error',
          text: '必须是绝对路径 —— 相对路径会相对子进程的工作目录解析，落点会随目录漂移。',
        }),
      );
    }
    home.body.appendChild(pathRow);
  }

  home.body.appendChild(
    h(
      'div',
      { class: 'row row--plain' },
      checkboxRow('关闭选项卡时结束后台进程', '', draft.killOnClose, (checked) => {
        draft.killOnClose = checked;
      }),
    ),
  );
  main.appendChild(home.root);
}

function runtimeChoice(option: RuntimeOption): HTMLElement {
  return selectableCard({
    title: option.label,
    desc: option.available ? option.description : (option.reason ?? '当前不可用'),
    selected: draft.runtimeId === option.id,
    disabled: !option.available,
    reason: option.reason ?? undefined,
    onSelect: () => {
      if (!option.available) {
        store.toast(option.reason ?? '该运行时当前不可用', true);
        return;
      }
      draft.runtimeId = option.id;
      render();
    },
  });
}

function renderAttachForm(main: HTMLElement): void {
  const isRemote = draft.kind === 'remote';
  const target = section(
    '启动 URL',
    isRemote
      ? '在远端终端运行 `dsh web --host <网卡IP> --trusted-host <authority>`，复制它打印的整行输出。'
      : '在运行 dsh web 的终端里复制整行 `dsh web: …`（包含 token）。',
  );

  const input = h('input', {
    type: 'text',
    value: draft.url,
    placeholder: 'http://127.0.0.1:9808/?token=…',
  }) as HTMLInputElement;
  input.addEventListener('input', () => {
    draft.url = input.value;
  });
  const control = h('div', { class: 'row__control row__control--wide' });
  control.appendChild(input);
  target.body.appendChild(wideRow('URL（含 token）', control));

  target.body.appendChild(
    h('div', {
      class: 'notice',
      text:
        'token 是这次连接的凭据，并且**不会被写入磁盘** —— 它只留在内存里，' +
        '所以重启 dshell 后需要重新粘贴一次。',
    }),
  );

  const plaintext = isRemote && draft.url.trim().startsWith('http://') && !isLoopback(draft.url);
  if (plaintext) {
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
        checkboxRow('我已了解风险，仍要继续', '', draft.allowPlaintext, (checked) => {
          draft.allowPlaintext = checked;
        }),
      ),
    );
  }
  main.appendChild(target.root);
}

// -------------------------------------------------------------------- submit

async function submit(): Promise<void> {
  draft.busy = true;
  draft.error = null;
  render();
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
    // The new tab is created and activated by Rust, so this page's job is done:
    // close it. Failing to close must not lose the connection that just worked.
    await store.closeSelf();
  } catch (error) {
    const failure = asFailure(error);
    draft.busy = false;
    draft.error = failure.detail ? `${failure.message}\n${failure.detail}` : failure.message;
    render();
  }
}

// ---------------------------------------------------------------------- boot

void (async () => {
  await bootPage(store, render);
  const config = store.config;
  if (config) {
    draft.runtimeId = config.defaultRuntime || draft.runtimeId;
    draft.killOnClose = config.killOnClose;
    draft.workspace = config.recentWorkspaces[0] ?? '';
    draft.dshHomeMode = config.dshHome?.mode ?? 'system';
    draft.dshHomeCustom = config.dshHome?.mode === 'custom' ? config.dshHome.path : '';
    render();
  }
})();
