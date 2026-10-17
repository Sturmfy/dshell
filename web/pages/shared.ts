// Small DOM and formatting helpers shared by dshell's page tabs.
//
// Deliberately tiny and framework-free: the interesting part of this app is the
// Rust side, and a page here is a form plus a handful of commands. Anything more
// elaborate would be more machinery than the problem needs.

import type { PageStore } from '../store';

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
  const match = /^https?:\/\/([^/:]+)/i.exec(url);
  if (!match) return false;
  const host = match[1].toLowerCase();
  return host === 'localhost' || host === '127.0.0.1' || host === '[::1]' || host === '::1';
}

export function checkRow(kind: 'ok' | 'warn' | 'bad', label: string, value: string): HTMLElement {
  const mark = kind === 'ok' ? '✓' : kind === 'warn' ? '⚠' : '✗';
  return h(
    'li',
    { class: 'check' },
    h('span', { class: `check__mark check__mark--${kind}`, text: mark }),
    h('span', { text: label }),
    h('span', { class: 'check__value', text: value }),
  );
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
 */
export function section(title?: string, desc?: string): { root: HTMLElement; body: HTMLElement } {
  const body = h('div', { class: 'section__body' });
  const root = h('section', { class: 'section' });
  if (title || desc) {
    const head = h('div', { class: 'section__head' });
    if (title) head.appendChild(h('h2', { class: 'section__title', text: title }));
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
