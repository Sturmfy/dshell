// The settings page, shown as a tab.
//
// This is a singleton tab: pressing the gear twice focuses the same tab. That
// falls out of `open_page` in Rust rather than being enforced here.
//
// The page is a category rail plus one reading column. Settings has many
// unrelated groups, and a single long column made them read as one enormous
// form; the rail lets a reader jump to the group they came for. Colour is
// punctuation — one hue per category, carried by a small icon tile — while all
// surfaces, type and controls stay on DSH's own tokens.
//
// Everything writes through `PageStore`; the page never infers or caches Rust
// state. A repaint after a save goes through `preserveView`, so changing a
// preference under the cursor does not throw the reader back to the top.

import { api, asFailure } from '../ipc';
import { PageStore } from '../store';
import type { AppInfo, Config } from '../types';
import { bootPage, h, preserveView, toastView } from './shared';

const host = document.getElementById('page') as HTMLElement;
const store = new PageStore('settings');

// ------------------------------------------------------------------- model

type SectionId =
  | 'overview'
  | 'general'
  | 'update'
  | 'data'
  | 'environment'
  | 'runtimes'
  | 'onboarding';

interface SectionSpec {
  id: SectionId;
  label: string;
  group: string;
  icon: IconName;
  color: string;
  search: string;
}

const SECTIONS: SectionSpec[] = [
  {
    id: 'overview',
    label: '概览',
    group: '应用',
    icon: 'layout',
    color: 'var(--set-c-indigo)',
    search: '概览 状态 首页 快捷入口',
  },
  {
    id: 'general',
    label: '通用',
    group: '应用',
    icon: 'sliders',
    color: 'var(--set-c-violet)',
    search: '通用 偏好 标签页 后台 托盘 端口 动画 关闭 会话',
  },
  {
    id: 'update',
    label: '更新',
    group: '应用',
    icon: 'refresh',
    color: 'var(--set-c-amber)',
    search: '更新 版本 通道 升级 启动 latest alpha',
  },
  {
    id: 'data',
    label: '数据与存储',
    group: '系统',
    icon: 'database',
    color: 'var(--set-c-blue)',
    search: '数据 存储 路径 数据根 环境变量 磁盘 配置文件',
  },
  {
    id: 'environment',
    label: '运行环境',
    group: '系统',
    icon: 'terminal',
    color: 'var(--set-c-emerald)',
    search: '运行环境 Node WebView2 pnpm 检测 依赖',
  },
  {
    id: 'runtimes',
    label: '本地运行时',
    group: '系统',
    icon: 'package',
    color: 'var(--set-c-cyan)',
    search: '本地运行时 维护 源码 安装 目录 checkout',
  },
  {
    id: 'onboarding',
    label: '首次配置',
    group: '其他',
    icon: 'sparkles',
    color: 'var(--set-c-rose)',
    search: '首次配置 向导 初始化 数据位置',
  },
];

const SECTION_BY_ID = new Map(SECTIONS.map((section) => [section.id, section]));

const state = {
  /** The category currently shown. Survives repaints caused by saves. */
  section: 'overview' as SectionId,
  /** Live filter in the rail search box. */
  search: '',
  /** Number of config writes in flight, for the auto-save pill. */
  saving: 0,
};

// ------------------------------------------------------------------- icons

const ICONS = {
  layout:
    '<rect width="7" height="9" x="3" y="3" rx="1"/><rect width="7" height="5" x="14" y="3" rx="1"/><rect width="7" height="9" x="14" y="12" rx="1"/><rect width="7" height="5" x="3" y="16" rx="1"/>',
  sliders:
    '<line x1="21" x2="14" y1="4" y2="4"/><line x1="10" x2="3" y1="4" y2="4"/><line x1="21" x2="12" y1="12" y2="12"/><line x1="8" x2="3" y1="12" y2="12"/><line x1="21" x2="16" y1="20" y2="20"/><line x1="12" x2="3" y1="20" y2="20"/><line x1="14" x2="14" y1="2" y2="6"/><line x1="8" x2="8" y1="10" y2="14"/><line x1="16" x2="16" y1="18" y2="22"/>',
  refresh:
    '<path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16"/><path d="M16 16h5v5"/>',
  database:
    '<ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5V19A9 3 0 0 0 21 19V5"/><path d="M3 12A9 3 0 0 0 21 12"/>',
  terminal:
    '<polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/>',
  package:
    '<path d="m7.5 4.27 9 5.15"/><path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z"/><path d="m3.3 7 8.7 5 8.7-5"/><path d="M12 22V12"/>',
  sparkles:
    '<path d="m12 3-1.9 5.8a2 2 0 0 1-1.3 1.3L3 12l5.8 1.9a2 2 0 0 1 1.3 1.3L12 21l1.9-5.8a2 2 0 0 1 1.3-1.3L21 12l-5.8-1.9a2 2 0 0 1-1.3-1.3Z"/><path d="M5 3v4"/><path d="M19 17v4"/><path d="M3 5h4"/><path d="M17 19h4"/>',
  search: '<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>',
  folder:
    '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>',
  copy:
    '<rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  alert:
    '<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>',
  info:
    '<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>',
  'chevron-right': '<path d="m9 18 6-6-6-6"/>',
  'chevron-down': '<path d="m6 9 6 6 6-6"/>',
  x: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
  power:
    '<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.77.04"/>',
  panel:
    '<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M3 9h18"/>',
  zap:
    '<path d="M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.92 6.02A1 1 0 0 0 13 10h7a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.92-6.02A1 1 0 0 0 11 14z"/>',
  tag:
    '<path d="M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42Z"/><circle cx="7.5" cy="7.5" r=".5" fill="currentColor"/>',
  git:
    '<circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M6 15V9a3 3 0 0 1 3-3h6"/>',
  plus: '<path d="M5 12h14"/><path d="M12 5v14"/>',
  trash:
    '<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/>',
  command:
    '<path d="M18 3a3 3 0 0 0-3 3v12a3 3 0 0 0 3 3 3 3 0 0 0 3-3 3 3 0 0 0-3-3H6a3 3 0 0 0-3 3 3 3 0 0 0 3 3 3 3 0 0 0 3-3V6a3 3 0 0 0-3-3 3 3 0 0 0-3 3 3 3 0 0 0 3 3h12a3 3 0 0 0 3-3 3 3 0 0 0-3-3Z"/>',
} as const;

type IconName = keyof typeof ICONS;
type BadgeTone = 'ok' | 'warn';

function icon(name: IconName, className = ''): SVGSVGElement {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('class', `set-icon${className ? ` ${className}` : ''}`);
  svg.setAttribute('aria-hidden', 'true');
  svg.innerHTML = ICONS[name];
  return svg;
}

// ------------------------------------------------------------------- render

/**
 * Build the page from the store.
 *
 * `render` adds [`preserveView`]: saving a preference refreshes the snapshot and
 * repaints, and that repaint must not throw away the reader's place or drop the
 * focus from the control they just used.
 */
function paint(): void {
  const info = store.state.info;
  if (!info) {
    host.innerHTML = '';
    host.appendChild(h('div', { class: 'page__scroll page__loading', text: '正在加载…' }));
    return;
  }

  const app = h('div', { class: 'set-app' });
  app.appendChild(rail(info));
  app.appendChild(main(info));
  host.innerHTML = '';
  host.appendChild(app);

  applyFilter(state.search);
  updateSavedPill();

  const toast = toastView(store);
  if (toast) host.appendChild(toast);
}

/** Repaint without moving the reader or stealing focus. */
const render = (): void => preserveView(paint);

function rail(info: AppInfo): HTMLElement {
  const railEl = h('aside', { class: 'set-rail' });

  const head = h('div', { class: 'set-rail__head' });
  head.appendChild(h('span', { class: 'set-brand__mark', role: 'img', 'aria-label': 'dshell' }));
  head.appendChild(
    h(
      'div',
      { class: 'set-brand__text' },
      h('div', { class: 'set-brand__title', text: '设置' }),
      h('div', { class: 'set-brand__meta', text: `dshell ${info.version}` }),
    ),
  );
  railEl.appendChild(head);

  const search = h('label', { class: 'set-search' });
  search.appendChild(icon('search'));
  const input = h('input', {
    type: 'search',
    placeholder: '搜索设置',
    autocomplete: 'off',
    spellcheck: 'false',
    'aria-label': '搜索设置',
  }) as HTMLInputElement;
  input.value = state.search;
  input.addEventListener('input', () => {
    state.search = input.value;
    applyFilter(state.search);
  });
  input.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    input.value = '';
    state.search = '';
    applyFilter('');
    input.blur();
  });
  search.appendChild(input);
  search.appendChild(h('kbd', { text: 'Ctrl K' }));
  railEl.appendChild(search);

  const nav = h('nav', { class: 'set-nav', 'aria-label': '设置分类' });
  let lastGroup = '';
  let groupEl: HTMLElement | null = null;
  for (const section of SECTIONS) {
    if (section.group !== lastGroup) {
      lastGroup = section.group;
      groupEl = h('div', { class: 'set-nav-group' });
      groupEl.appendChild(h('div', { class: 'set-nav-label', text: section.group }));
      nav.appendChild(groupEl);
    }
    const active = section.id === state.section;
    const button = h(
      'button',
      {
        type: 'button',
        class: `set-nav-item${active ? ' is-active' : ''}`,
        style: `--set-c: ${section.color}`,
        'data-section': section.id,
        'data-search': section.search,
        'aria-current': active ? 'page' : undefined,
        onclick: () => selectSection(section.id),
      },
      h('span', { class: 'set-nav-icon' }, icon(section.icon)),
      h('span', { class: 'set-nav-text', text: section.label }),
    );
    if (section.id === 'environment' && environmentNeedsAttention(info)) {
      button.appendChild(h('span', { class: 'set-nav-dot', title: '运行环境有待处理项' }));
    }
    if (section.id === 'runtimes' && info.config.managedInstalls.length > 0) {
      button.appendChild(h('span', { class: 'set-nav-count', text: String(info.config.managedInstalls.length) }));
    }
    if (section.id === 'onboarding' && !info.config.onboardingDone) {
      button.appendChild(h('span', { class: 'set-nav-dot', title: '首次配置尚未完成' }));
    }
    groupEl?.appendChild(button);
  }
  nav.appendChild(h('div', { class: 'set-nav-empty', text: '没有匹配的分类。', hidden: 'hidden' }));
  railEl.appendChild(nav);

  railEl.appendChild(railStatus(info));
  return railEl;
}

function railStatus(info: AppInfo): HTMLElement {
  const box = h('div', { class: 'set-rail__status' });
  const running = info.tabs.filter((tab) => !tab.pageKind && tab.phase === 'ready').length;

  let text = 'dshell 已启动';
  let tone = '';
  if (!info.config.onboardingDone) {
    text = '尚未完成首次配置';
    tone = ' set-status-dot--warn';
  } else if (running > 0) {
    text = `${running} 个 DSH 选项卡运行中`;
  }

  box.appendChild(
    h(
      'div',
      { class: 'set-status-line' },
      h('span', { class: `set-status-dot${tone}` }),
      h('span', { text }),
    ),
  );
  box.appendChild(
    h('div', {
      class: 'set-status-meta',
      text: `DSH ${info.pinnedDshVersion} · ${channelShort(info.config.dshVersionChannel)}`,
    }),
  );
  return box;
}

function main(info: AppInfo): HTMLElement {
  const spec = SECTION_BY_ID.get(state.section) ?? SECTIONS[0];
  const mainEl = h('main', { class: 'set-main' });

  const top = h('div', { class: 'set-top' });
  const crumb = h('nav', { class: 'set-crumb', 'aria-label': '面包屑' });
  crumb.appendChild(h('span', { text: '设置' }));
  crumb.appendChild(icon('chevron-right'));
  crumb.appendChild(h('span', { text: spec.group }));
  crumb.appendChild(icon('chevron-right'));
  crumb.appendChild(h('strong', { text: spec.label }));
  top.appendChild(crumb);

  const actions = h('div', { class: 'set-top-actions' });
  actions.appendChild(
    h(
      'span',
      { class: 'set-saved', title: '更改会立即写入 config.json' },
      h('span', { class: 'set-status-dot' }),
      h('span', { class: 'set-saved__text', text: state.saving > 0 ? '正在保存…' : '自动保存' }),
    ),
  );
  actions.appendChild(
    h(
      'button',
      {
        type: 'button',
        class: 'set-icon-btn',
        title: '打开数据根',
        'aria-label': '打开数据根',
        onclick: () => openRoot(info),
      },
      icon('folder'),
    ),
  );
  actions.appendChild(
    h(
      'button',
      {
        type: 'button',
        class: 'set-icon-btn set-icon-btn--danger',
        title: '关闭这个选项卡',
        'aria-label': '关闭这个选项卡',
        onclick: () => void store.closeSelf(),
      },
      icon('x'),
    ),
  );
  top.appendChild(actions);

  const scroll = h('div', { class: 'set-scroll' });
  scroll.appendChild(panel(info, spec));
  mainEl.appendChild(top);
  mainEl.appendChild(scroll);
  return mainEl;
}

function panel(info: AppInfo, spec: SectionSpec): HTMLElement {
  const panelEl = h('section', { class: 'set-panel', style: `--set-c: ${spec.color}` });
  const inner = h('div', { class: 'set-inner' });
  inner.appendChild(hero(info, spec));
  for (const node of sectionNodes(info, spec.id)) inner.appendChild(node);
  panelEl.appendChild(inner);
  return panelEl;
}

function hero(info: AppInfo, spec: SectionSpec): HTMLElement {
  const status = heroStatus(info, spec.id);
  const el = h('header', { class: 'set-hero' });
  el.appendChild(h('span', { class: 'set-hero__icon' }, icon(spec.icon)));
  el.appendChild(
    h(
      'div',
      { class: 'set-hero__body' },
      h('div', { class: 'set-hero__eyebrow', text: spec.group }),
      h('h1', { text: spec.label }),
      h('p', { text: heroDescription(info, spec.id) }),
    ),
  );
  el.appendChild(
    h(
      'span',
      { class: `set-hero__status${status.tone ? ` set-hero__status--${status.tone}` : ''}` },
      h('span', { class: 'set-status-dot' }),
      h('span', { text: status.text }),
    ),
  );
  return el;
}

function heroDescription(info: AppInfo, id: SectionId): string {
  switch (id) {
    case 'overview':
      return 'dshell 的运行状态与常用入口。所有更改都会即时保存，不需要确认。';
    case 'general':
      return '决定标签页、启动与关闭行为。更改立即生效，并写入数据根里的 config.json。';
    case 'update':
      return '选择 DSH 的版本策略。固定版本最稳；浮动通道可能随时引入破坏性变更。';
    case 'data':
      return 'dshell 自己的配置、运行时与每个选项卡的界面数据都集中在这里。';
    case 'environment':
      return '「连接」不依赖其中任何一项；缺少 Node.js 只会让「启动本地」不可用。';
    case 'runtimes':
      return '指向已经维护好的 DSH 安装树或源码目录。dshell 直接使用它，不复制、不修改。';
    case 'onboarding':
      return '两步确认数据位置与运行环境。完成之后，这里只是重新检查的入口。';
  }
  return info.version;
}

function heroStatus(info: AppInfo, id: SectionId): { text: string; tone: '' | 'warn' | 'neutral' } {
  if (id === 'overview') {
    return { text: `v${info.version}`, tone: 'neutral' };
  }
  if (id === 'general') {
    return { text: '即时生效', tone: '' };
  }
  if (id === 'update') {
    const channel = normalizeChannel(info.config.dshVersionChannel);
    return {
      text: channelShort(info.config.dshVersionChannel),
      tone: channel === 'pinned' ? '' : 'warn',
    };
  }
  if (id === 'data') {
    return { text: '当前数据根', tone: 'neutral' };
  }
  if (id === 'environment') {
    const { ready, total } = environmentScore(info);
    return { text: `${ready} / ${total} 就绪`, tone: ready === total ? '' : 'warn' };
  }
  if (id === 'runtimes') {
    return {
      text: `${info.config.managedInstalls.length} 个目录`,
      tone: info.config.managedInstalls.length > 0 ? '' : 'neutral',
    };
  }
  return info.config.onboardingDone
    ? { text: '2 / 2 步', tone: '' }
    : { text: '1 / 2 步', tone: 'warn' };
}

function sectionNodes(info: AppInfo, id: SectionId): Node[] {
  switch (id) {
    case 'overview':
      return overviewNodes(info);
    case 'general':
      return generalNodes(info);
    case 'update':
      return updateNodes(info);
    case 'data':
      return dataNodes(info);
    case 'environment':
      return environmentNodes(info);
    case 'runtimes':
      return runtimeNodes(info);
    case 'onboarding':
      return onboardingNodes(info);
  }
}

// ------------------------------------------------------------------ overview

function overviewNodes(info: AppInfo): Node[] {
  const { ready, total } = environmentScore(info);
  const node = info.environment.node;
  const installs = info.config.managedInstalls;

  const stats = h('div', { class: 'set-stats' });
  stats.appendChild(
    stat({
      label: '数据根',
      value: info.root.path,
      mono: true,
      sub: `生效层 · ${info.root.sourceLabel}`,
      badge: badge('当前', undefined, true),
      icon: 'database',
    }),
  );
  stats.appendChild(
    stat({
      label: 'DSH 版本',
      value: info.pinnedDshVersion,
      mono: true,
      sub: `npx 通道 · ${channelShort(info.config.dshVersionChannel)}`,
      badge: badge(channelShort(info.config.dshVersionChannel), undefined, true),
      icon: 'tag',
    }),
  );
  stats.appendChild(
    stat({
      label: '运行环境',
      value: node ? `Node ${node.version}` : '未检测到 Node.js',
      sub: `WebView2 ${info.environment.webview2Version ?? '缺失'} · pnpm ${info.environment.pnpm ?? '缺失'}`,
      badge: badge(`${ready} / ${total}`, ready === total ? 'ok' : 'warn', true),
      icon: 'terminal',
    }),
  );
  stats.appendChild(
    stat({
      label: '本地运行时',
      value: `${installs.length} 个目录`,
      sub: installs.length > 0 ? installs.map((install) => install.label).join(' · ') : '尚未添加',
      badge: installs.length > 0 ? badge('已维护') : null,
      icon: 'package',
    }),
  );

  const quick = card({
    title: '快捷入口',
    desc: '常用的维护动作，不必翻页去找。',
    icon: 'command',
    bodyClass: 'set-card__body set-card__body--flush',
  });
  const grid = h('div', { class: 'set-quick-grid' });
  grid.appendChild(
    quickAction('打开数据根', info.root.path, 'folder', () => openRoot(info)),
  );
  grid.appendChild(
    quickAction('重新检测环境', 'Node、WebView2 与 pnpm', 'terminal', () => {
      void store.refreshInfo(true);
    }),
  );
  grid.appendChild(
    quickAction('管理本地运行时', '添加或移除已维护的目录', 'package', () => {
      selectSection('runtimes');
    }),
  );
  grid.appendChild(
    quickAction('检查 DSH 更新', `当前通道 · ${channelShort(info.config.dshVersionChannel)}`, 'refresh', () => {
      selectSection('update');
    }),
  );
  quick.body.appendChild(grid);

  return [
    stats,
    quick.root,
    notice(
      'info',
      h('strong', { text: 'DSH 的数据仍归 DSH 管理。' }),
      '会话、模型与 API Key 保存在 DSH_HOME，dshell 不读取、不修改。',
    ),
  ];
}

function stat(options: {
  label: string;
  value: string;
  sub: string;
  mono?: boolean;
  badge?: HTMLElement | null;
  icon: IconName;
}): HTMLElement {
  const root = h('article', { class: 'set-stat' });
  const top = h('div', { class: 'set-stat__top' });
  top.appendChild(h('span', { class: 'set-stat__icon' }, icon(options.icon)));
  top.appendChild(h('span', { class: 'set-stat__label', text: options.label }));
  if (options.badge) top.appendChild(h('span', { class: 'set-stat__badge' }, options.badge));
  root.appendChild(top);
  root.appendChild(
    h('div', { class: `set-stat__value${options.mono ? ' mono' : ''}`, text: options.value }),
  );
  root.appendChild(h('div', { class: 'set-stat__sub', text: options.sub }));
  return root;
}

function quickAction(
  title: string,
  sub: string,
  iconName: IconName,
  run: () => void,
): HTMLElement {
  return h(
    'button',
    {
      type: 'button',
      class: 'set-quick',
      onclick: run,
    },
    h('span', { class: 'set-quick__icon' }, icon(iconName)),
    h(
      'span',
      { class: 'set-quick__text' },
      h('span', { class: 'set-quick__title', text: title }),
      h('span', { class: 'set-quick__sub', text: sub }),
    ),
    icon('chevron-right'),
  );
}

// ------------------------------------------------------------------- general

function generalNodes(info: AppInfo): Node[] {
  const config = info.config;

  const tabs = card({
    title: '标签页与会话',
    desc: '后台选项卡如何存活，以及登录态能否跨重启保留。',
    icon: 'panel',
  });
  tabs.body.appendChild(
    settingRow(
      '非活动选项卡',
      'DSH 是长连接 agent 会话：隐藏后台选项卡可能被浏览器节流，导致丢事件或审批请求卡住。',
      selectControl(
        '非活动选项卡',
        config.backgroundMode === 'hide' ? 'hide' : 'move-offscreen',
        [
          ['move-offscreen', '保持活跃（推荐）'],
          ['hide', '隐藏以省内存'],
        ],
        (value) => void saveConfig({ backgroundMode: value }),
      ),
    ),
  );
  tabs.body.appendChild(
    settingRow(
      '固定端口以保持登录态',
      'cookie 绑定端口。固定端口可在重启后保持登录，但可能撞上端口占用（EADDRINUSE）。',
      switchControl('固定端口以保持登录态', config.fixedPort, (checked) =>
        void saveConfig({ fixedPort: checked }),
      ),
    ),
  );

  const closing = card({
    title: '启动与关闭',
    desc: '窗口与后台进程的收尾方式。',
    icon: 'power',
  });
  closing.body.appendChild(
    settingRow(
      '关闭窗口时最小化到托盘',
      '点关闭或 Alt+F4 时收进托盘，后端与选项卡继续运行；退出请使用托盘菜单底部的「退出」。',
      switchControl('关闭窗口时最小化到托盘', config.closeToTray, (checked) =>
        void saveConfig({ closeToTray: checked }),
      ),
    ),
  );
  closing.body.appendChild(
    settingRow(
      '关闭选项卡时结束后台进程',
      '关闭后 dshell 不再监管该后端。关闭此项可保留进程，稍后用启动 URL 重新连接。',
      switchControl('关闭选项卡时结束后台进程', config.killOnClose, (checked) =>
        void saveConfig({ killOnClose: checked }),
      ),
    ),
  );

  const motion = card({
    title: '内容与动效',
    desc: 'DSH 内容页的动画策略；dshell 自己的页面不受影响。',
    icon: 'zap',
  });
  motion.body.appendChild(
    settingRow(
      '在 DSH 内容页保持动画播放',
      '即使 Windows 开启了「减少动态效果」，DSH 的工作圆环等动画仍会播放。修改后新建的选项卡生效。',
      switchControl('在 DSH 内容页保持动画播放', config.forceDshMotion, (checked) =>
        void saveConfig({ forceDshMotion: checked }),
      ),
    ),
  );

  return [tabs.root, closing.root, motion.root];
}

// -------------------------------------------------------------------- update

function updateNodes(info: AppInfo): Node[] {
  const config = info.config;
  const channel = normalizeChannel(config.dshVersionChannel);

  const strategy = card({
    title: '版本策略',
    desc: '只影响 npx 启动的本地选项卡；源码版每次启动会自行同步。',
    icon: 'refresh',
  });
  strategy.body.appendChild(
    settingRow(
      'DSH 版本通道',
      '决定「新建连接」时 npx 使用哪个版本。DSH 官方明示仍处于开发者预览。',
      selectControl(
        'DSH 版本通道',
        channel,
        [
          ['pinned', `固定 ${info.pinnedDshVersion}（推荐）`],
          ['latest', 'latest（可能引入破坏性变更）'],
          ['alpha', 'alpha'],
        ],
        (value) => void saveConfig({ dshVersionChannel: value }),
      ),
    ),
  );
  strategy.body.appendChild(
    settingRow(
      '启动时的版本行为',
      '只影响重启后自动恢复的本地 npx 选项卡。检测新版本需要能访问 npm，失败时会按现有版本启动。',
      selectControl(
        '启动时的版本行为',
        normalizeStartupPolicy(config.dshStartupPolicy),
        [
          ['ask', '每次询问（发现新版本时提示）'],
          ['keep', '维持旧版本'],
          ['update', '更新到新版本（自动）'],
        ],
        (value) => void saveConfig({ dshStartupPolicy: value }),
      ),
    ),
  );

  const current = card({
    title: '当前通道',
    desc: '这些值会随下一次启动生效。',
    icon: 'package',
  });
  const facts = h('div', { class: 'set-facts' });
  facts.appendChild(fact('固定版本', info.pinnedDshVersion, false, badge('推荐', 'ok')));
  facts.appendChild(fact('版本通道', channelShort(config.dshVersionChannel)));
  facts.appendChild(fact('适用运行时', 'npx（本地启动）'));
  facts.appendChild(fact('源码版', '每次启动自行同步，不受此处控制', true));
  current.body.appendChild(facts);

  return [
    strategy.root,
    notice(
      'warn',
      h('strong', { text: '开发者预览。' }),
      'latest 与 alpha 可能随时改变启动参数或数据结构；遇到问题请切回固定版本。',
    ),
    current.root,
  ];
}

function normalizeChannel(value: string): 'pinned' | 'latest' | 'alpha' {
  return value === 'latest' || value === 'alpha' ? value : 'pinned';
}

function channelShort(value: string): string {
  if (value === 'latest') return 'latest';
  if (value === 'alpha') return 'alpha';
  return '固定';
}

/** Unknown stored values (from a newer build) read as the product default. */
function normalizeStartupPolicy(value: string): 'ask' | 'keep' | 'update' {
  return value === 'keep' || value === 'update' ? value : 'ask';
}

// ---------------------------------------------------------------------- data

function dataNodes(info: AppInfo): Node[] {
  const root = info.root;
  const data = card({
    title: '数据根',
    desc: '更改位置需要迁移数据；这里显示的是本次进程实际使用的路径。',
    icon: 'folder',
    action: h(
      'button',
      { type: 'button', class: 'btn btn--sm', onclick: () => openRoot(info) },
      icon('folder'),
      h('span', { text: '打开' }),
    ),
  });

  const path = h('div', { class: 'set-path' });
  path.appendChild(icon('folder'));
  path.appendChild(h('code', { text: root.path }));
  path.appendChild(
    h(
      'button',
      {
        type: 'button',
        class: 'set-icon-btn',
        title: '复制路径',
        'aria-label': '复制路径',
        onclick: () => void copyText(root.path, '路径已复制到剪贴板'),
      },
      icon('copy'),
    ),
  );
  data.body.appendChild(path);

  const facts = h('div', { class: 'set-facts' });
  facts.appendChild(fact('生效层', root.sourceLabel, true));
  facts.appendChild(fact(`$${root.envVar}`, root.envValue ?? '（未设置）'));
  facts.appendChild(fact('runtimes', root.runtimesDir));
  facts.appendChild(fact('webview 存储', root.webviewDataDir));
  facts.appendChild(fact('配置文件', root.configFile));
  data.body.appendChild(facts);
  data.body.appendChild(
    settingRow(
      `用户环境变量 ${root.envVar}`,
      '移除后 dshell 仍会沿回落链找到数据根；但把 dshell.exe 挪到别处时不再自动跟随。',
      h('button', {
        type: 'button',
        class: 'btn btn--sm btn--danger',
        text: '移除',
        onclick: () => void removeEnvVar(),
      }),
    ),
  );

  return [
    data.root,
    ...(root.note ? [notice('warn', root.note)] : []),
    notice(
      'warn',
      h('strong', { text: '不要选网络盘或同步盘。' }),
      '会话历史会频繁写入，并且其中包含凭据；OneDrive 之类的同步会让写入延迟且互相冲突。',
    ),
  ];
}

// --------------------------------------------------------------- environment

function environmentNodes(info: AppInfo): Node[] {
  const checks = environmentChecks(info);
  const cardEl = card({
    title: '环境检测',
    desc: '重新检测会读取当前环境变量与已知安装目录，不使用启动时的缓存。',
    icon: 'terminal',
    action: h(
      'button',
      {
        type: 'button',
        class: 'btn btn--sm',
        onclick: () => void store.refreshInfo(true),
      },
      icon('refresh'),
      h('span', { text: '重新检测' }),
    ),
  });

  const list = h('div', { class: 'set-check-list' });
  for (const check of checks.items) {
    const row = h('div', { class: 'set-check' });
    row.appendChild(
      h(
        'span',
        { class: `set-check__status${check.ok ? '' : ' set-check__status--warn'}` },
        icon(check.ok ? 'check' : 'alert'),
      ),
    );
    row.appendChild(
      h(
        'div',
        { class: 'set-check__body' },
        h(
          'div',
          { class: 'set-check__name' },
          h('span', { text: check.name }),
          badge(check.badge, check.ok ? 'ok' : 'warn'),
        ),
        h('div', { class: 'set-check__value', text: check.value }),
      ),
    );
    if (check.action) row.appendChild(h('div', { class: 'set-check__action' }, check.action));
    list.appendChild(row);
  }
  cardEl.body.appendChild(list);

  const hint = pnpmHint(info);
  if (hint) cardEl.body.appendChild(h('p', { class: 'set-check-note', text: hint }));
  if (store.state.pnpmInstalling) {
    cardEl.body.appendChild(
      h('p', { class: 'set-check-note', text: '正在通过 npm 全局安装 pnpm，通常需要 10–30 秒…' }),
    );
  }

  return [
    cardEl.root,
    notice(
      'info',
      h('strong', { text: '连接已有实例不需要本地依赖。' }),
      'Node.js 只用于 dshell 启动新的本地后端；WebView2 由 Windows 提供。',
    ),
  ];
}

interface CheckView {
  name: string;
  ok: boolean;
  badge: string;
  value: string;
  action?: HTMLElement;
}

function environmentChecks(info: AppInfo): { items: CheckView[]; ready: number; total: number } {
  const env = info.environment;
  const node = env.node;
  const nodeOk = !!node && !node.versionIssue;
  const webview2Ok = !!env.webview2Version;
  const pnpmOk = !!env.pnpm;
  const install = env.pnpmInstall ?? {};

  const items: CheckView[] = [
    {
      name: 'Node.js',
      ok: nodeOk,
      badge: node ? (node.versionIssue ? '版本不符' : '就绪') : '缺失',
      value: node
        ? `${node.version}${node.versionIssue ? ` · ${node.versionIssue}` : ''} · ${node.node}`
        : '未检测到 · 启动本地不可用',
    },
    {
      name: 'WebView2',
      ok: webview2Ok,
      badge: webview2Ok ? '就绪' : '缺失',
      value: env.webview2Version ?? '未检测到',
    },
    {
      name: 'pnpm',
      ok: pnpmOk,
      badge: pnpmOk ? '就绪' : '缺失',
      value: pnpmOk
        ? (env.pnpm as string)
        : install.blocked
          ? `未找到 · ${install.blocked}`
          : '未找到 · 源码维护模式不可用',
    },
  ];

  if (!pnpmOk && install.npm) {
    const installing = store.state.pnpmInstalling;
    items[2].action = h('button', {
      type: 'button',
      class: 'btn btn--sm',
      text: installing ? '正在安装…' : '安装 pnpm',
      title: install.command ?? '',
      disabled: installing ? 'disabled' : undefined,
      onclick: () => void store.installPnpm(),
    });
  }

  return {
    items,
    ready: [nodeOk, webview2Ok, pnpmOk].filter(Boolean).length,
    total: 3,
  };
}

function pnpmHint(info: AppInfo): string | null {
  const env = info.environment;
  if (env.pnpm) return null;
  const command = env.pnpmInstall?.command;
  if (!command) return null;
  return `将运行：${command}（会写入 npm 的全局前缀）`;
}

function environmentScore(info: AppInfo): { ready: number; total: number } {
  const env = info.environment;
  const nodeOk = !!env.node && !env.node.versionIssue;
  const ready = [nodeOk, !!env.webview2Version, !!env.pnpm].filter(Boolean).length;
  return { ready, total: 3 };
}

function environmentNeedsAttention(info: AppInfo): boolean {
  const score = environmentScore(info);
  return score.ready < score.total;
}

// ------------------------------------------------------------------ runtimes

function runtimeNodes(info: AppInfo): Node[] {
  const installs = info.config.managedInstalls;
  const cardEl = card({
    title: '本地维护的运行时',
    desc: '启动时从这些目录解析版本，优先级高于自动下载的 npx 缓存。',
    icon: 'package',
  });

  if (installs.length === 0) {
    cardEl.body.appendChild(
      h('p', { class: 'set-empty', text: '还没有添加。dshell 会继续使用 npx 缓存或源码容器。' }),
    );
  }

  for (const install of installs) {
    const row = h('div', { class: 'set-runtime' });
    row.appendChild(
      h(
        'span',
        { class: 'set-runtime__icon' },
        icon(install.mode === 'source' ? 'git' : 'package'),
      ),
    );
    const name = h('div', { class: 'set-runtime__name' });
    name.appendChild(h('span', { text: install.label }));
    name.appendChild(badge(install.mode === 'source' ? '源码' : install.mode));
    row.appendChild(
      h(
        'div',
        { class: 'set-runtime__main' },
        name,
        h('div', { class: 'set-runtime__path', text: install.root }),
        h('div', { class: 'set-runtime__meta', text: '直接使用 · 不复制 · 不修改' }),
      ),
    );
    row.appendChild(
      h(
        'div',
        { class: 'set-runtime__action' },
        h(
          'button',
          {
            type: 'button',
            class: 'set-icon-btn set-icon-btn--danger',
            title: '移除',
            'aria-label': `移除 ${install.label}`,
            onclick: () => void removeInstall(install.id),
          },
          icon('trash'),
        ),
      ),
    );
    cardEl.body.appendChild(row);
  }

  cardEl.body.appendChild(
    h(
      'button',
      {
        type: 'button',
        class: 'set-add-row',
        onclick: () => void addInstall(),
      },
      h('span', { class: 'set-add-row__plus' }, icon('plus')),
      h(
        'span',
        { class: 'set-add-row__text' },
        h('span', { class: 'set-add-row__title', text: '添加本地维护目录' }),
        h('span', { class: 'set-add-row__sub', text: '选择一个已有的 DSH 安装树或源码 checkout' }),
      ),
      icon('chevron-right'),
    ),
  );

  return [
    cardEl.root,
    notice(
      'info',
      h('strong', { text: '源码版每次启动会自行同步。' }),
      '本地维护目录不会被「DSH 版本通道」或启动时的版本策略改动。',
    ),
  ];
}

// ---------------------------------------------------------------- onboarding

function onboardingNodes(info: AppInfo): Node[] {
  const done = info.config.onboardingDone;
  const score = environmentScore(info);
  const cardEl = card({
    title: '配置状态',
    desc: '重新打开不会迁移数据，只会再次确认位置并重跑环境检测。',
    icon: 'sparkles',
    action: badge(done ? '已完成' : '未完成', done ? 'ok' : 'warn', true),
  });

  const flow = h('div', { class: 'set-flow' });
  flow.appendChild(flowStep('数据位置', true, info.root.path));
  flow.appendChild(h('span', { class: 'set-flow__line' }));
  flow.appendChild(flowStep('环境检查', done, `${score.ready} / ${score.total} 项通过`));
  cardEl.body.appendChild(flow);

  const facts = h('div', { class: 'set-facts' });
  facts.appendChild(fact('数据位置', info.root.path));
  facts.appendChild(fact('生效层', info.root.sourceLabel, true));
  facts.appendChild(
    fact(
      '环境检查',
      `Node.js ${info.environment.node ? '可用' : '缺失'} · WebView2 ${
        info.environment.webview2Version ? '可用' : '缺失'
      } · pnpm ${info.environment.pnpm ? '可用' : '缺失'}`,
      true,
    ),
  );
  cardEl.body.appendChild(facts);

  const actions = h('div', { class: 'set-actions' });
  actions.appendChild(
    h('button', {
      type: 'button',
      class: 'btn btn--primary',
      text: done ? '重新打开首次配置' : '去完成首次配置',
      onclick: () => void store.openPage('onboarding'),
    }),
  );
  actions.appendChild(
    h('button', {
      type: 'button',
      class: 'btn',
      text: '查看环境详情',
      onclick: () => selectSection('environment'),
    }),
  );
  cardEl.body.appendChild(actions);

  return [
    cardEl.root,
    notice(
      'info',
      h('strong', { text: '它只是一个普通选项卡。' }),
      '完成之后可以随时关闭或重新打开；数据根不会因此改变。',
    ),
  ];
}

function flowStep(label: string, done: boolean, detail: string): HTMLElement {
  return h(
    'div',
    { class: 'set-flow__step' },
    h(
      'span',
      { class: `set-flow__num${done ? '' : ' set-flow__num--pending'}` },
      done ? icon('check') : h('span', { text: '2' }),
    ),
    h('span', {}, h('strong', { text: label }), ` · ${detail}`),
  );
}

// ---------------------------------------------------------------- primitives

function card(options: {
  title: string;
  desc: string;
  icon: IconName;
  action?: HTMLElement | null;
  bodyClass?: string;
}): { root: HTMLElement; body: HTMLElement } {
  const root = h('section', { class: 'set-card' });
  const head = h('div', { class: 'set-card__head' });
  head.appendChild(h('span', { class: 'set-card__icon' }, icon(options.icon)));
  head.appendChild(
    h(
      'div',
      { class: 'set-card__head-text' },
      h('h2', { class: 'set-card__title', text: options.title }),
      h('p', { class: 'set-card__desc', text: options.desc }),
    ),
  );
  if (options.action) head.appendChild(h('div', { class: 'set-card__action' }, options.action));
  root.appendChild(head);
  const body = h('div', { class: options.bodyClass ?? 'set-card__body' });
  root.appendChild(body);
  return { root, body };
}

function settingRow(label: string, hint: string, control: HTMLElement): HTMLElement {
  return h(
    'div',
    { class: 'set-row' },
    h(
      'div',
      { class: 'set-row__text' },
      h('div', { class: 'set-row__label', text: label }),
      h('p', { class: 'set-row__hint', text: hint }),
    ),
    h('div', { class: 'set-row__control' }, control),
  );
}

function switchControl(
  label: string,
  checked: boolean,
  onChange: (checked: boolean) => void,
): HTMLElement {
  const input = h('input', {
    type: 'checkbox',
    checked: checked ? 'checked' : undefined,
    'aria-label': label,
  }) as HTMLInputElement;
  input.addEventListener('change', () => onChange(input.checked));
  return h(
    'label',
    { class: 'set-switch' },
    input,
    h('span', { class: 'set-switch__track' }, h('span', { class: 'set-switch__thumb' })),
  );
}

function selectControl(
  label: string,
  value: string,
  options: [string, string][],
  onChange: (value: string) => void,
): HTMLElement {
  const wrap = h('label', { class: 'set-select' });
  const select = h('select', { 'aria-label': label }) as HTMLSelectElement;
  for (const [id, text] of options) {
    const option = h('option', { value: id, text }) as HTMLOptionElement;
    if (id === value) option.selected = true;
    select.appendChild(option);
  }
  select.addEventListener('change', () => onChange(select.value));
  wrap.appendChild(select);
  wrap.appendChild(icon('chevron-down'));
  return wrap;
}

function fact(key: string, value: string, plain = false, extra?: Node | null): HTMLElement {
  const val = h('span', {
    class: `set-fact__val${plain ? ' set-fact__val--plain' : ''}`,
  });
  val.appendChild(h('span', { text: value }));
  if (extra) val.appendChild(extra);
  return h('div', { class: 'set-fact' }, h('span', { class: 'set-fact__key', text: key }), val);
}

function badge(text: string, tone?: BadgeTone, dot = false): HTMLElement {
  const root = h('span', { class: `set-badge${tone ? ` set-badge--${tone}` : ''}` });
  if (dot) root.appendChild(h('span', { class: 'set-badge__dot' }));
  root.appendChild(h('span', { text }));
  return root;
}

function notice(kind: 'info' | 'warn', ...content: (string | Node)[]): HTMLElement {
  return h(
    'div',
    { class: `set-notice set-notice--${kind}` },
    icon(kind === 'warn' ? 'alert' : 'info'),
    h('span', {}, ...content),
  );
}

// ------------------------------------------------------------------- actions

async function saveConfig(patch: Partial<Config>): Promise<void> {
  state.saving += 1;
  updateSavedPill();
  try {
    await store.updateConfig(patch);
  } finally {
    state.saving -= 1;
    updateSavedPill();
  }
}

function updateSavedPill(): void {
  const pill = host.querySelector<HTMLElement>('.set-saved');
  if (!pill) return;
  const saving = state.saving > 0;
  pill.classList.toggle('is-saving', saving);
  const text = pill.querySelector<HTMLElement>('.set-saved__text');
  if (text) text.textContent = saving ? '正在保存…' : '自动保存';
}

function selectSection(id: SectionId): void {
  if (state.section === id && state.search === '') return;
  state.section = id;
  state.search = '';
  paint();
  // The rail was rebuilt, so the button that was just activated no longer
  // exists. Put focus on its replacement: a keyboard user should land on the
  // category they chose, not back at the top of the document.
  host
    .querySelector<HTMLElement>(`.set-nav-item[data-section="${id}"]`)
    ?.focus({ preventScroll: true });
}

function applyFilter(query: string): void {
  const nav = host.querySelector<HTMLElement>('.set-nav');
  if (!nav) return;
  const needle = query.trim().toLowerCase();
  let any = false;

  for (const item of nav.querySelectorAll<HTMLElement>('.set-nav-item')) {
    const haystack = `${item.dataset.search ?? ''} ${item.textContent ?? ''}`.toLowerCase();
    const hit = needle === '' || haystack.includes(needle);
    item.hidden = !hit;
    if (hit) any = true;
  }
  for (const group of nav.querySelectorAll<HTMLElement>('.set-nav-group')) {
    group.hidden = !group.querySelector('.set-nav-item:not([hidden])');
  }
  const empty = nav.querySelector<HTMLElement>('.set-nav-empty');
  if (empty) empty.hidden = any;
}

function openRoot(info: AppInfo): void {
  void api
    .openPath(info.root.path)
    .catch((error) => store.toast(asFailure(error).message, true));
}

async function removeEnvVar(): Promise<void> {
  try {
    await api.removeEnvVar();
    store.toast('已移除环境变量；dshell 之后按回落链定位数据根');
    await store.refreshInfo();
  } catch (error) {
    store.toast(asFailure(error).message, true);
  }
}

async function addInstall(): Promise<void> {
  // The pick itself can fail (the OS may refuse a native dialog), so it sits in
  // the same try as the add: a dead button is worse than an error message.
  try {
    const picked = await api.pickDirectory(null);
    if (!picked) return;
    await api.addManagedInstall(picked);
    await store.refreshInfo();
    await store.refreshRuntimes();
    store.toast('已添加');
    render();
  } catch (error) {
    store.toast(asFailure(error).message, true);
  }
}

async function removeInstall(id: string): Promise<void> {
  try {
    await api.removeManagedInstall(id);
    await store.refreshInfo();
    await store.refreshRuntimes();
    render();
  } catch (error) {
    store.toast(asFailure(error).message, true);
  }
}

async function copyText(text: string, success: string): Promise<void> {
  let copied = false;
  try {
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      copied = true;
    }
  } catch {
    /* fall through to the selection-based copy */
  }
  if (!copied) {
    const area = document.createElement('textarea');
    area.value = text;
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.appendChild(area);
    area.select();
    try {
      copied = document.execCommand('copy');
    } catch {
      copied = false;
    }
    area.remove();
  }
  store.toast(copied ? success : '复制失败，请手动选择路径', !copied);
}

// ---------------------------------------------------------------------- boot

document.addEventListener('keydown', (event) => {
  if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== 'k') return;
  const input = host.querySelector<HTMLInputElement>('.set-search input');
  if (!input) return;
  event.preventDefault();
  input.focus();
  input.select();
});

void (async () => {
  await bootPage(store, render);
})();
