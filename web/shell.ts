// Logic for dshell's own pages inside a content webview: the loading page and
// the failure page.
//
// These pages exist so a slow or broken backend never shows up as a blank white
// rectangle. They are dshell assets, which is why they can call back into dshell
// to fetch the failure detail — a DSH page never can, and never needs to.

import { api } from './ipc';
import type { LogEntry, TabFailureDetail, TabView } from './types';

const params = new URLSearchParams(window.location.search);
const tabId = params.get('tab') ?? params.get('tabId') ?? '';
const code = params.get('code') ?? '';
const stage = params.get('stage') ?? '';

const titleEl = document.getElementById('shell-title') as HTMLElement;
const subEl = document.getElementById('shell-subtitle') as HTMLElement;
const metaEl = document.getElementById('shell-meta') as HTMLElement | null;
const logEl = document.getElementById('shell-log') as HTMLElement;
const actionsEl = document.getElementById('shell-actions') as HTMLElement;
const hintEl = document.getElementById('shell-hint') as HTMLElement;

const mode = (document.getElementById('shell') as HTMLElement).dataset.mode ?? 'placeholder';

/** Plain-language cause + what to do, per failure code. */
const EXPLAIN: Record<
  string,
  { title: string; body: string; actions?: { id: string; label: string }[] }
> = {
  noNodeRuntime: {
    title: '未检测到 Node.js',
    body: '启动本地 DSH 需要 Node.js（^22.19 或 >=24）。连接已有实例与远程实例不需要它。',
    actions: [
      { id: 'open-nodejs-org', label: '下载 Node.js（官方）' },
      { id: 'redetect', label: '重新检测' },
    ],
  },
  workspaceMissing: {
    title: '工作目录不可用',
    body: '所选目录不存在或不可写。DSH 会把该目录作为会话的工作根目录。',
    actions: [{ id: 'new-tab', label: '重新选择工作目录' }],
  },
  dshHomeInvalid: {
    title: 'DSH_HOME 不合法',
    body: 'DSH_HOME 必须是绝对路径。相对路径会相对子进程的工作目录解析，导致数据落点漂移。',
    actions: [{ id: 'new-tab', label: '重新配置' }],
  },
  dshHomeUnwritable: {
    title: 'DSH 无法写入状态目录',
    body:
      'DSH 在不可写的 DSH_HOME 上会以原始 V8 崩溃退出，不写任何诊断日志。' +
      '请换一个可写目录，或改用系统默认（~/.dsh）。',
    actions: [{ id: 'new-tab', label: '重新配置 DSH_HOME' }],
  },
  frontendMissing: {
    title: '前端资源缺失',
    body:
      '服务器已经启动并打印了就绪 URL，但页面是 404 —— 这通常意味着这个安装不完整' +
      '（缺少 dsh-web-frontend/dist），或源码未构建。',
    actions: [{ id: 'reinstate-runtime', label: '重新安装运行时' }],
  },
  npxInstallFailed: {
    title: '运行时安装失败',
    body: '下载或安装 DSH 依赖失败。可以重试，或改用「本地维护 / 其他后端二进制」。',
    actions: [
      { id: 'retry', label: '重试' },
      { id: 'new-tab', label: '换一种运行时' },
    ],
  },
  buildFailed: {
    title: '构建失败',
    body: '源码模式的安装或构建没有成功。请查看日志末尾的输出。',
    actions: [{ id: 'retry', label: '重新构建' }],
  },
  dshFlagUnsupported: {
    title: '该后端不支持所需参数',
    body: '这个版本的 DSH 不认识 dshell 需要的参数。请升级 DSH，或改用其他运行时。',
    actions: [{ id: 'new-tab', label: '换一种运行时' }],
  },
  startupTimeout: {
    title: '启动超时',
    body:
      '在限定时间内没有出现 `dsh web:` 就绪行。首次使用 npx 需要下载约 450 MB，' +
      '网络慢时会超过这个时限。',
    actions: [
      { id: 'retry', label: '重试' },
      { id: 'open-log', label: '打开完整日志' },
    ],
  },
  portBindFailed: {
    title: '端口被占用',
    body: '固定端口模式下该端口已被占用。可以自动回落到随机端口重试一次（需要重新登录）。',
    actions: [{ id: 'retry-free-port', label: '改用随机端口重试' }],
  },
  tokenUrlUnparsed: {
    title: '无法解析启动 URL',
    body: 'stdout 有输出但没有出现可识别的 `dsh web: <url>` 行。这多半是版本不兼容。',
    actions: [{ id: 'diagnostics', label: '查看诊断信息' }],
  },
  attachAuthRequired: {
    title: '需要提供启动 URL',
    body:
      '连接已有实例需要它的启动 URL（含 token）。token 不会被写入磁盘，' +
      '所以重启后需要重新粘贴。',
    actions: [{ id: 'new-tab', label: '粘贴启动 URL' }],
  },
  attachNotDsh: {
    title: '该地址不是 DSH web 服务',
    body: '探测没有得到预期的 401。请核对地址与端口，或在运行 dsh web 的终端里确认实际端口。',
    actions: [{ id: 'retry', label: '重新检测' }],
  },
  remoteUnreachable: {
    title: '无法连接',
    body: '连接被拒绝或超时。请检查 IP、端口、防火墙，以及服务端是否已经启动。',
    actions: [{ id: 'retry', label: '重新检测' }],
  },
  remoteForbidden: {
    title: '服务端拒绝了本机',
    body: '信任围栏要求服务端显式信任你使用的 authority。按下面的命令重启服务端即可。',
    actions: [{ id: 'copy-fix-command', label: '复制修复命令' }],
  },
  childExited: {
    title: 'DSH 进程已退出',
    body: '后端进程在运行中退出了。可以重启该选项卡，或查看日志定位原因。',
    actions: [
      { id: 'retry', label: '重启' },
      { id: 'open-log', label: '打开完整日志' },
    ],
  },
  dataRootUnwritable: {
    title: '数据根不可写',
    body: 'dshell 无法在所选位置写入配置。请换一个可写位置。',
    actions: [{ id: 'new-tab', label: '重新选择' }],
  },
  cancelled: { title: '已取消', body: '操作已取消。', actions: [{ id: 'retry', label: '重试' }] },
};

async function runAction(id: string, detail: TabFailureDetail | null) {
  switch (id) {
    case 'retry':
    case 'restart':
      await api.restartTab(tabId).catch(() => undefined);
      break;
    case 'retry-free-port':
      await api.restartTab(tabId).catch(() => undefined);
      break;
    case 'new-tab':
      await api.closeTab(tabId).catch(() => undefined);
      await api.focusChrome().catch(() => undefined);
      break;
    case 'open-log': {
      const path = detail?.failure?.logPath;
      if (path) await api.openPath(path).catch(() => undefined);
      break;
    }
    case 'open-browser':
      await api.openInSystemBrowser(tabId).catch(() => undefined);
      break;
    case 'copy-fix-command': {
      const text = detail?.failure?.detail ?? '';
      await navigator.clipboard.writeText(text).catch(() => undefined);
      break;
    }
    case 'open-nodejs-org':
      window.open('https://nodejs.org/en/download', '_blank');
      break;
    case 'redetect':
    case 'diagnostics':
    case 'reinstate-runtime':
    case 'use-external-binary':
    case 'confirm-plaintext':
      await api.closeTab(tabId).catch(() => undefined);
      await api.focusChrome().catch(() => undefined);
      break;
    default:
      break;
  }
}

function renderActions(
  actions: { id: string; label: string; kind?: 'primary' | 'secondary' }[],
  detail: TabFailureDetail | null,
) {
  actionsEl.innerHTML = '';
  const seen = new Set<string>();
  for (const action of actions) {
    if (seen.has(action.id)) continue;
    seen.add(action.id);
    const button = document.createElement('button');
    button.className = `btn ${action.kind === 'primary' ? 'btn--primary' : ''}`;
    button.textContent = action.label;
    button.addEventListener('click', () => void runAction(action.id, detail));
    actionsEl.appendChild(button);
  }
}

function tailText(entries: LogEntry[]): string {
  return entries
    .slice(-40)
    .map((entry) => `[${entry.stream}] ${entry.text}`)
    .join('\n');
}

async function renderError() {
  const known = EXPLAIN[code];
  titleEl.textContent = known?.title ?? '出现问题';
  subEl.textContent = known?.body ?? '';

  let detail: TabFailureDetail | null = null;
  try {
    detail = await api.tabFailureDetail(tabId);
  } catch {
    detail = null;
  }

  if (detail?.failure?.message) subEl.textContent = detail.failure.message;

  if (metaEl) {
    const rows: string[] = [];
    if (detail?.workspace) rows.push(`工作目录　${detail.workspace}`);
    if (detail?.dshHome) rows.push(`DSH_HOME　${detail.dshHome}`);
    else rows.push('DSH_HOME　（未设置 → DSH 使用 ~/.dsh）');
    if (detail?.runtime) rows.push(`运行时　${detail.runtime}`);
    if (detail?.commandLine) rows.push(`命令　${detail.commandLine}`);
    metaEl.innerHTML = '';
    for (const row of rows) {
      const line = document.createElement('div');
      line.textContent = row;
      metaEl.appendChild(line);
    }
  }

  const evidence = detail?.failure?.detail
    ? `${detail.failure.detail}\n\n--- 最近输出 ---\n${detail.tailText || tailText(detail.logTail)}`
    : detail
      ? tailText(detail.logTail)
      : '';
  if (evidence) {
    logEl.hidden = false;
    logEl.textContent = evidence;
  }

  const actions =
    detail?.failure?.actions && detail.failure.actions.length > 0
      ? detail.failure.actions
      : (known?.actions ?? [{ id: 'retry', label: '重试' }]).map((action) => ({
          ...action,
          kind: 'primary' as const,
        }));
  renderActions(
    actions.map((action) => ({ ...action, kind: action.kind ?? 'secondary' })),
    detail,
  );

  hintEl.textContent = detail?.failure?.logPath
    ? `完整诊断日志：${detail.failure.logPath}`
    : 'DSH 自己写的启动诊断日志位于 <DSH_HOME>/logs/startup-*.log';
}

/** What the loading page says under its headline for this tab.
 *
 * The document ships a generic line, so the first paint never claims a download
 * that is not happening. This refines it from the tab's own view: "npx" is the
 * only runtime that may pull ~450 MB on first use, a source checkout instead
 * wastes its time on `pnpm`/build steps, and an attach/remote tab is connecting
 * to an instance dshell did not start.
 */
function startupSubtitle(view: TabView | null): string {
  if (!view) return '请稍候…';
  if (view.badge !== 'local') {
    return '正在连接这个实例，就绪后会自动切换到这个选项卡。';
  }
  switch (view.runtime) {
    case 'npx':
      return 'npx 正在启动后端；首次使用需要下载依赖（约 450 MB），请耐心等待。';
    case 'source':
    case 'managed/source':
      return '源码版正在启动后端，就绪后会自动切换到这个选项卡。';
    default:
      return 'dshell 已在后台启动后端，就绪后会自动切换到这个选项卡。';
  }
}

/** The loading page keeps refreshing its log tail until the tab leaves the
 *  starting phases, then hands over to the real DSH page. */
async function renderPlaceholder() {
  let elapsed = 0;
  let stopped = false;
  const tick = window.setInterval(() => {
    elapsed += 1;
    if (metaEl) metaEl.textContent = `已等待 ${elapsed}s`;
  }, 1000);

  const poll = async () => {
    if (stopped) return;
    try {
      const view = await currentView();
      if (!view) {
        stopped = true;
        return;
      }
      if (view.phase === 'failed') {
        stopped = true;
        window.location.replace(`error.html?tab=${encodeURIComponent(tabId)}&code=${view.failure?.code ?? 'internal'}`);
        return;
      }
      if (view.phase === 'ready' || view.phase === 'waitingForReady') {
        stopped = true;
        return;
      }
      const subtitle = startupSubtitle(view);
      if (subtitle !== subEl.textContent) subEl.textContent = subtitle;
      if (view.notice) {
        if (metaEl) metaEl.textContent = `${view.notice}（已等待 ${elapsed}s）`;
      }
      if (view.logPath && hintEl) hintEl.textContent = `日志：${view.logPath}`;
    } catch {
      stopped = true;
    }
    window.setTimeout(() => void poll(), 900);
  };
  void poll();

  renderActions([{ id: 'new-tab', label: '取消并关闭', kind: 'secondary' }], null);
  window.addEventListener('beforeunload', () => window.clearInterval(tick));
}

async function currentView(): Promise<TabView | null> {
  const snapshot = await api.listTabs();
  return snapshot.tabs.find((tab) => tab.id === tabId) ?? null;
}

async function boot() {
  if (!tabId) {
    titleEl.textContent = '缺少选项卡标识';
    subEl.textContent = '这个页面应该由 dshell 在选项卡内打开。';
    return;
  }
  if (mode === 'error') {
    await renderError();
  } else {
    await renderPlaceholder();
  }
}

void boot();

// Surface the failure stage when nothing else explains it.
if (stage && hintEl && mode === 'error' && !hintEl.textContent) {
  hintEl.textContent = `阶段：${stage}`;
}
