// Small DOM and formatting helpers shared by dshell's page tabs.
//
// Deliberately tiny and framework-free: the interesting part of this app is the
// Rust side, and a page here is a form plus a handful of commands. Anything more
// elaborate would be more machinery than the problem needs.

import type { PageStore } from '../store';
import type { EnvironmentSnapshot } from '../types';

export function h(
  tag: string,
  props: Record<string, unknown> = {},
  ...children: unknown[]
): HTMLElement {
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

export function shortPath(path: string, max = 46): string {
  return path.length <= max ? path : `…${path.slice(path.length - max + 1)}`;
}

export function slugOf(workspace: string): string {
  const parts = workspace.split(/[\\/]/).filter(Boolean);
  const name = parts[parts.length - 1] ?? 'home';
  return name.replace(/[^a-zA-Z0-9\u4e00-\u9fa5_-]/g, '-').slice(0, 48).toLowerCase();
}

export function isAbsolute(path: string): boolean {
  return /^[a-zA-Z]:[\\/]/.test(path) || path.startsWith('\\\\') || path.startsWith('/');
}

export function isLoopback(url: string): boolean {
  // Capture a bracketed IPv6 literal as one unit; `[^/:]+` stops at the first
  // colon inside `[::1]`, which used to classify an IPv6 loopback as remote.
  const match = /^https?:\/\/(\[[^\]]+\]|[^/:]+)/i.exec(url);
  if (!match) return false;
  const host = match[1].toLowerCase().replace(/^\[|\]$/g, '');
  return host === 'localhost' || host === '127.0.0.1' || host === '::1';
}

/**
 * One line of the environment report.
 *
 * `action` is the slot for a control that belongs *to that row* — currently only
 * the pnpm install button. It sits at the end of the row so the table stays a
 * table: a warning with a fix next to it reads as one statement, and the eye
 * keeps its column.
 */
export function checkRow(
  kind: 'ok' | 'warn' | 'bad',
  label: string,
  value: string,
  action?: HTMLElement | null,
): HTMLElement {
  const mark = kind === 'ok' ? '✓' : kind === 'warn' ? '⚠' : '✗';
  const row = h(
    'li',
    { class: 'check' },
    h('span', { class: `check__mark check__mark--${kind}`, text: mark }),
    h('span', { text: label }),
    h('span', { class: 'check__value', text: value }),
  );
  if (action) row.appendChild(h('span', { class: 'check__action' }, action));
  return row;
}

/**
 * The pnpm row: a version when it runs, and the one fix when it does not.
 *
 * dshell never runs `pnpm` — a source checkout needs it for the user's own
 * `pnpm install` / `pnpm run build`. So "未找到" is not a blocker, but it is
 * still a dead end on screen unless the report offers what the user would type
 * anyway: `npm install -g pnpm`, which Rust runs with the npm it detected.
 */
export function pnpmCheckRow(
  env: EnvironmentSnapshot,
  installing: boolean,
  onInstall: () => void,
): HTMLElement {
  const install = env.pnpmInstall ?? {};
  if (env.pnpm) {
    return checkRow('ok', 'pnpm', env.pnpm);
  }
  const value = install.npm
    ? '未找到（源码维护模式不可用）'
    : `未找到（源码维护模式不可用；${install.blocked ?? '没有可用的 npm，无法自动安装'}）`;
  const button = install.npm
    ? h('button', {
        type: 'button',
        class: 'btn btn--sm',
        text: installing ? '正在安装…' : '安装 pnpm',
        title: install.command ?? '',
        disabled: installing ? 'disabled' : undefined,
        onclick: onInstall,
      })
    : null;
  return checkRow('warn', 'pnpm', value, button);
}

/** The install the button will run, quoted so the click has no surprises. */
export function pnpmInstallHint(env: EnvironmentSnapshot): HTMLElement | null {
  const command = env.pnpmInstall?.command;
  if (env.pnpm || !command) return null;
  return h('p', { class: 'field__hint', text: `将运行：${command}（会写入 npm 的全局前缀）` });
}

/**
 * The scrolling column of a page, with its fixed frame.
 *
 * Pages are built as `main` (scrolls) plus `footer` (stays): a long form whose
 * primary action scrolls out of view is a form the user has to re-find. Callers
 * fill `main` with `.page__stack` sections and return the footer, or `null` for
 * a page whose actions belong inline.
 */
export function pageFrame(): { main: HTMLElement; foot: HTMLElement | null } {
  const host = document.getElementById('page');
  if (!host) return { main: document.createElement('div'), foot: null };
  host.innerHTML = '';
  const main = h('div', { class: 'page__scroll' });
  const stack = h('div', { class: 'page__stack' });
  main.appendChild(stack);
  host.appendChild(main);
  return { main: stack, foot: null };
}

/** The fixed action bar at the bottom of a page. */
export function pageFooter(...children: unknown[]): HTMLElement {
  const host = document.getElementById('page');
  const foot = h('footer', { class: 'page__foot' }, children);
  host?.appendChild(foot);
  return foot;
}

/** A page header: title plus one line of orientation. */
export function pageHeader(title: string, sub?: string): HTMLElement {
  const head = h('header', { class: 'page__head' });
  head.appendChild(h('h1', { class: 'page__title', text: title }));
  if (sub) head.appendChild(h('p', { class: 'page__sub', text: sub }));
  return head;
}

/**
 * A titled group of rows, separated by hairlines rather than boxed in cards.
 *
 * The title is optional: a group whose heading would only restate the page title
 * gets no heading rather than an extra level of nesting.
 *
 * `action` is a control that belongs to the heading — a small "重新检测" or
 * "重新扫描" next to the group it refreshes, rather than in the page footer
 * where it would compete with the primary action.
 */
export function section(
  title?: string,
  desc?: string,
  action?: HTMLElement | null,
): { root: HTMLElement; body: HTMLElement } {
  const body = h('div', { class: 'section__body' });
  const root = h('section', { class: 'section' });
  if (title || desc || action) {
    const head = h('div', { class: 'section__head' });
    const top = h('div', { class: 'section__head-top' });
    if (title) top.appendChild(h('h2', { class: 'section__title', text: title }));
    if (action) top.appendChild(action);
    if (title || action) head.appendChild(top);
    if (desc) head.appendChild(h('p', { class: 'section__desc', text: desc }));
    root.appendChild(head);
  }
  root.appendChild(body);
  return { root, body };
}

/** A settings-style row: a label, an optional control and an optional hint. */
export function row(label: string, control?: HTMLElement | null, hint?: string): HTMLElement {
  const main = h('div', { class: 'row__main' });
  main.appendChild(h('div', { class: 'row__label', text: label }));
  if (control) {
    const slot = h('div', { class: 'row__control' });
    slot.appendChild(control);
    main.appendChild(slot);
  }
  const root = h('div', { class: 'row' }, main);
  if (hint) root.appendChild(h('p', { class: 'field__hint', text: hint }));
  return root;
}

/**
 * A row whose control needs the full width — a filesystem path, a URL.
 *
 * Same markup as [`row`], with the label moved above the control: a path
 * squeezed into the 220px gutter meant for a select is unusable.
 */
export function wideRow(label: string, control: HTMLElement, hint?: string): HTMLElement {
  const main = h('div', { class: 'row__main' });
  main.appendChild(h('div', { class: 'row__label', text: label }));
  const slot = h('div', { class: 'row__control row__control--wide' });
  slot.appendChild(control);
  main.appendChild(slot);
  const root = h('div', { class: 'row row--wide' }, main);
  if (hint) root.appendChild(h('p', { class: 'field__hint', text: hint }));
  return root;
}

/** A selectable card, used for the connection modes and the runtime choices. */
export function selectableCard(options: {
  title: string;
  desc: string;
  icon?: string;
  selected?: boolean;
  disabled?: boolean;
  /** Tooltip, used to explain *why* a disabled card is disabled. */
  reason?: string;
  onSelect: () => void;
}): HTMLElement {
  const root = h('button', {
    type: 'button',
    class: [
      'card',
      options.selected ? 'card--selected' : '',
      options.disabled ? 'card--disabled' : '',
    ]
      .filter(Boolean)
      .join(' '),
    title: options.reason ?? '',
    'aria-pressed': options.selected ? 'true' : undefined,
    // A blocked runtime is not a choice: the real `disabled` attribute keeps
    // the card out of the tab order and off the click path, rather than only
    // painting it dim.
    disabled: options.disabled ? 'disabled' : undefined,
    onclick: options.onSelect,
  });
  if (options.icon) root.appendChild(h('span', { class: 'card__icon', text: options.icon }));
  root.appendChild(h('span', { class: 'card__title', text: options.title }));
  root.appendChild(h('span', { class: 'card__desc', text: options.desc }));
  return root;
}

/**
 * A `<select>` from `[value, label]` pairs.
 *
 * `label` may be empty: inside a `row(...)` the row already supplies the label,
 * and a second one would read as a duplicated caption. The wrapper element is
 * still needed so the control is a single node the caller can place.
 */
export function labeledSelect(
  label: string,
  value: string,
  options: [string, string][],
  onChange: (value: string) => void | Promise<void>,
): HTMLElement {
  const field = h('div', { class: label ? 'field' : 'field field--bare' });
  if (label) field.appendChild(h('span', { class: 'field__label', text: label }));
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

export function checkboxRow(
  label: string,
  hint: string,
  checked: boolean,
  onChange: (checked: boolean) => void,
): HTMLElement {
  const row = h('label', { class: 'radio' });
  const box = h('input', {
    type: 'checkbox',
    checked: checked ? 'checked' : undefined,
  }) as HTMLInputElement;
  box.addEventListener('change', () => onChange(box.checked));
  row.appendChild(box);
  const body = h('span', { class: 'radio__body' });
  body.appendChild(h('span', { text: label }));
  if (hint) body.appendChild(h('span', { class: 'radio__preview', text: hint }));
  row.appendChild(body);
  return row;
}

/**
 * Start a page tab, and make a failed start-up legible.
 *
 * Every page begins the same way: subscribe, load state, render. The part worth
 * centralising is the failure path — a page whose `invoke` is refused (wrong
 * capability, unexpected origin) otherwise renders its static skeleton forever
 * and looks exactly like a slow load. Here it says so on screen and reports the
 * browser's own message to Rust, so the reason is in the debug log rather than
 * guessed at.
 */
export async function bootPage(store: PageStore, render: () => void): Promise<void> {
  const host = document.getElementById('page');
  store.subscribe(render);
  await store.step('script running');
  try {
    await store.start();
  } catch (error) {
    await store.reportBootFailure(error);
    if (host) {
      host.innerHTML = '';
      host.appendChild(
        h('div', {
          class: 'notice notice--danger',
          text: `这个页面无法启动：${String(error)}`,
        }),
      );
    }
    return;
  }
  render();
}
