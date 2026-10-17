// The settings page, shown as a tab.
//
// A singleton tab: pressing the gear twice focuses the same tab. That falls out
// of `open_page` in Rust rather than being enforced here.
//
// The layout is deliberately DSH's settings pattern — titled sections, each a
// stack of label/control rows separated by hairlines. That is what makes a long
// list of preferences scannable instead of a wall of stacked labels.

import { api, asFailure } from '../ipc';
import { PageStore } from '../store';
import type { AppInfo } from '../types';
import {
  bootPage,
  checkboxRow,
  checkRow,
  h,
  labeledSelect,
  pageFooter,
  pageFrame,
  pageHeader,
  row,
  section,
} from './shared';

const host = document.getElementById('page') as HTMLElement;
const store = new PageStore('settings');

// -------------------------------------------------------------------- render

function render(): void {
  const info = store.state.info;
  if (!info) {
    host.innerHTML = '';
    host.appendChild(h('div', { class: 'page__scroll page__loading', text: '正在加载…' }));
    return;
  }
  const { main } = pageFrame();
  main.appendChild(pageHeader('设置', `dshell ${info.version} · 固定 DSH ${info.pinnedDshVersion}`));

  renderDataRoot(main, info);
  renderEnvironment(main, info);
  renderPreferences(main, info);
  renderRuntimes(main, info);
  renderOnboarding(main, info);

  pageFooter(
    h('button', {
      class: 'btn',
      text: '打开数据根',
      onclick: () =>
        void api
          .openPath(info.root.path)
          .catch((error) => store.toast(asFailure(error).message, true)),
    }),
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

function renderDataRoot(main: HTMLElement, info: AppInfo): void {
  const root = section('数据根', 'dshell 自己的配置、运行时与 webview 存储都在这里。');
  root.body.appendChild(
    h(
      'dl',
      { class: 'kv kv--padded' },
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
    root.body.appendChild(h('div', { class: 'notice notice--warn', text: info.root.note }));
  }
  root.body.appendChild(
    row(
      `用户环境变量 ${info.root.envVar}`,
      h('button', {
        class: 'btn btn--danger',
        text: '移除',
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
      '移除后 dshell 仍会按回落链找到数据根，但把 dshell.exe 挪到别处就不再自动跟随。',
    ),
  );
  main.appendChild(root.root);
}

function renderEnvironment(main: HTMLElement, info: AppInfo): void {
  const node = info.environment.node;
  const env = section('运行环境', '「连接」功能不依赖其中任何一项；缺 Node 只影响「启动本地」。');
  const list = h('ul', { class: 'checks' });
  list.appendChild(
    checkRow(
      node && !node.versionIssue ? 'ok' : node ? 'warn' : 'bad',
      'Node.js',
      node ? `${node.version}（${node.node}）` : '未检测到 —— 启动本地不可用，连接不受影响',
    ),
  );
  list.appendChild(
    checkRow(
      info.environment.webview2Version ? 'ok' : 'bad',
      'WebView2',
      info.environment.webview2Version ?? '未检测到',
    ),
  );
  list.appendChild(
    checkRow(
      info.environment.pnpm ? 'ok' : 'warn',
      'pnpm',
      info.environment.pnpm ?? '未找到（源码维护模式不可用）',
    ),
  );
  env.body.appendChild(list);
  env.body.appendChild(
    h(
      'div',
      { class: 'actions' },
      h('button', { class: 'btn', text: '重新检测', onclick: () => void store.refreshInfo(true) }),
    ),
  );
  main.appendChild(env.root);
}

function renderPreferences(main: HTMLElement, info: AppInfo): void {
  const config = info.config;
  const prefs = section('偏好');

  prefs.body.appendChild(
    row(
      'DSH 版本通道',
      labeledSelect(
        '',
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
      '官方明示处于开发者预览：固定版本最稳，浮动通道可能随时引入破坏性变更。',
    ),
  );

  prefs.body.appendChild(
    row(
      '非活动选项卡',
      labeledSelect(
        '',
        config.backgroundMode,
        [
          ['move-offscreen', '保持活跃（推荐）'],
          ['hide', '隐藏以省内存'],
        ],
        (value) => store.updateConfig({ backgroundMode: value }),
      ),
      'DSH 是长连接 agent 会话：隐藏后台选项卡可能被浏览器节流，导致丢事件或审批请求卡住。',
    ),
  );

  prefs.body.appendChild(
    h(
      'div',
      { class: 'row row--plain' },
      checkboxRow(
        '固定端口以保持登录态',
        '默认用随机端口。cookie 绑定端口，端口不变才能跨重启保持登录；代价是可能撞上 EADDRINUSE。',
        config.fixedPort,
        (checked) => void store.updateConfig({ fixedPort: checked }),
      ),
    ),
  );

  prefs.body.appendChild(
    h(
      'div',
      { class: 'row row--plain' },
      checkboxRow(
        '新建选项卡默认在关闭时结束后台进程',
        '关闭后 dshell 不再监管该后端；不勾选则保留进程，方便之后用启动 URL 重新连接。',
        config.killOnClose,
        (checked) => void store.updateConfig({ killOnClose: checked }),
      ),
    ),
  );

  main.appendChild(prefs.root);
}

function renderRuntimes(main: HTMLElement, info: AppInfo): void {
  const config = info.config;
  const runtimes = section(
    '本地维护的运行时',
    '指向你已经维护好的 DSH 目录（npm/pnpm 安装树或源码 checkout），dshell 会直接使用它。',
  );

  if (config.managedInstalls.length === 0) {
    runtimes.body.appendChild(
      h('div', { class: 'row row--plain' }, h('p', {
        class: 'field__hint',
        text: '还没有添加。',
      })),
    );
  }
  for (const install of config.managedInstalls) {
    runtimes.body.appendChild(
      row(
        install.label,
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
        `${install.root}（${install.mode}）`,
      ),
    );
  }
  runtimes.body.appendChild(
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
  main.appendChild(runtimes.root);
}

function renderOnboarding(main: HTMLElement, info: AppInfo): void {
  const first = section(
    '首次配置',
    info.config.onboardingDone
      ? '已完成。需要重新确认数据位置与环境检查时可以再打开一次。'
      : '尚未完成 —— 它决定 dshell 的数据根位置，完成前那个选项卡无法关闭。',
  );
  first.body.appendChild(
    h(
      'div',
      { class: 'actions' },
      h('button', {
        class: 'btn',
        text: info.config.onboardingDone ? '重新打开首次配置' : '去完成首次配置',
        onclick: () => void store.openPage('onboarding'),
      }),
    ),
  );
  main.appendChild(first.root);
}

// ---------------------------------------------------------------------- boot

void (async () => {
  await bootPage(store, render);
})();
