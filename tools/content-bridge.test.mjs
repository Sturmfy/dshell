// Unit tests for the injected content bridge. These run against a fake
// `chrome.webview` so the DOM contract and the message protocol can be checked
// without launching WebView2.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import vm from 'node:vm'

const source = readFileSync(new URL('../src-tauri/src/bridge/content.js', import.meta.url), 'utf8')

function boot(options = {}) {
  const messages = []
  const listeners = []
  const webview = {
    postMessage(message) {
      messages.push(typeof message === 'string' ? JSON.parse(message) : message)
    },
    addEventListener(type, listener) {
      if (type === 'message') listeners.push(listener)
    },
  }
  if (options.additionalObjects !== false) {
    webview.postMessageWithAdditionalObjects = (json, files) => {
      messages.push({ json: JSON.parse(json), files })
    }
  }
  const documentListeners = []
  const body = {
    className: '',
    getBoundingClientRect: () => ({ x: 0, y: 0, width: 1280, height: 800 }),
  }
  const brand = options.brand
  const sessionRows = options.rows ?? []
  const document = {
    readyState: 'complete',
    body,
    documentElement: body,
    querySelectorAll(selector) {
      if (selector === '[class*="brandName" i]') return brand ? [brand] : []
      if (selector === '[data-row-key^="session:"]') return sessionRows
      return []
    },
    elementsFromPoint: () => [],
    addEventListener(type, listener) {
      documentListeners.push({ type, listener })
    },
  }
  const window = {
    chrome: { webview },
    innerWidth: 1280,
    innerHeight: 800,
    matchMedia: () => ({ addEventListener() {} }),
    __dshellContentBridgeInstalled: undefined,
  }
  window.window = window
  const sandbox = {
    window,
    document,
    File,
    crypto,
    setTimeout,
    clearTimeout,
    setInterval: () => 0,
    clearInterval: () => {},
    MutationObserver: class MutationObserver {
      observe() {}
    },
    getComputedStyle: () => ({
      backgroundColor: 'rgb(21, 21, 23)',
      color: 'rgb(249, 250, 251)',
    }),
    console,
    HTMLInputElement: class HTMLInputElement {},
  }
  sandbox.globalThis = sandbox
  vm.runInNewContext(source, sandbox, { filename: 'content.js' })
  // The real host answers the script's `ready` with `hello`; tests do the same
  // before exercising picker promises.
  if (messages.some(message => message.type === 'ready')) {
    for (const listener of listeners) {
      listener({
        data: {
          v: 1,
          type: 'hello',
          ...(options.title === undefined ? {} : { title: options.title }),
        },
      })
    }
  }
  return {
    window,
    messages,
    webview,
    emit(data) {
      for (const listener of listeners) listener({ data })
    },
    drag(files) {
      const items = files.map(file => ({ kind: 'file', getAsFile: () => file }))
      for (const entry of documentListeners) {
        if (entry.type === 'dragenter') entry.listener({ dataTransfer: { items } })
      }
    },
  }
}

function file(name, content = 'x') {
  return new File([content], name, { lastModified: 1 })
}

function brandElement(text = 'DSH 本地构建') {
  const attributes = {}
  return {
    textContent: text,
    innerHTML: `<span>${text}</span>`,
    style: {},
    querySelector() {
      return null
    },
    setAttribute(name, value) {
      attributes[name] = value
    },
    getAttribute(name) {
      return attributes[name]
    },
  }
}

/** A local-build brand: the title and the version line are separate elements. */
function localBuildBrand(titleText = 'DSH 本地构建', versionText = '0.2.1-rc.2-687f282') {
  const title = brandElement(titleText)
  const version = { textContent: versionText }
  const brand = brandElement(`${titleText}${versionText}`)
  brand.querySelector = selector => (
    selector === '[class*="localBuildTitle" i]' ? title : null
  )
  return { brand, title, version }
}

function sessionRow(state) {
  const dot = {
    getAttribute(name) {
      return name === 'data-state' ? state : null
    },
  }
  return {
    querySelector(selector) {
      return selector === '[data-state]' ? dot : null
    },
  }
}

test('defines both globals and picks a directory through the message channel', async () => {
  const bridge = boot()
  assert.equal(typeof bridge.window.__DSH_DIRECTORY_PICKER__.pick, 'function')
  assert.equal(typeof bridge.window.__DSH_HOST_PATHS__.pathFor, 'function')

  const picked = bridge.window.__DSH_DIRECTORY_PICKER__.pick()
  const request = bridge.messages.find(message => message.type === 'pickDirectory')
  assert.ok(request, 'a pickDirectory message was posted')
  assert.equal(request.v, 1)

  bridge.emit({ v: 1, type: 'pickDirectoryResult', id: request.id, ok: true, path: 'C:\\work' })
  assert.equal(await picked, 'C:\\work')
})

test('propagates a picker error as a rejection', async () => {
  const bridge = boot()
  const picked = bridge.window.__DSH_DIRECTORY_PICKER__.pick()
  const request = bridge.messages.find(message => message.type === 'pickDirectory')
  bridge.emit({ v: 1, type: 'pickDirectoryResult', id: request.id, ok: false, error: 'picker busy' })
  await assert.rejects(picked, /picker busy/)
})

test('matches host paths by key and keeps repeated pathFor stable', async () => {
  const bridge = boot()
  const first = file('note.md', 'one')
  const second = file('note.md', 'two')

  // No path has arrived yet: DSH falls back to upload.
  assert.equal(bridge.window.__DSH_HOST_PATHS__.pathFor(first), '')

  bridge.drag([first, second])
  const request = bridge.messages.find(message => message.json?.type === 'hostPaths')
  assert.ok(request, 'a hostPaths message was posted')
  assert.deepEqual(request.json.keys, [
    'note.md\u00003\u00001',
    'note.md\u00003\u00001',
  ])

  bridge.emit({
    v: 1,
    type: 'hostPathsResult',
    id: request.json.id,
    paths: ['C:\\a\\note.md', 'C:\\b\\note.md'],
  })

  assert.equal(bridge.window.__DSH_HOST_PATHS__.pathFor(first), 'C:\\a\\note.md')
  assert.equal(bridge.window.__DSH_HOST_PATHS__.pathFor(first), 'C:\\a\\note.md')
  assert.equal(bridge.window.__DSH_HOST_PATHS__.pathFor(second), 'C:\\b\\note.md')
})

test('missing additional-object support leaves host paths disabled', () => {
  const bridge = boot({ additionalObjects: false })
  assert.equal(bridge.window.__DSH_HOST_PATHS__, undefined)
  assert.equal(typeof bridge.window.__DSH_DIRECTORY_PICKER__.pick, 'function')
  const ready = bridge.messages.find(message => message.type === 'ready')
  assert.ok(ready)
  assert.equal(ready.features.hostPaths, false)
})

test('reports the sidebar surface for the strip', () => {
  const bridge = boot()
  const theme = bridge.messages.find(message => message.type === 'theme')
  assert.ok(theme, 'a theme message was posted')
  assert.equal(theme.background, 'rgb(21, 21, 23)')
  assert.equal(theme.foreground, 'rgb(249, 250, 251)')
  assert.equal(theme.mode, 'dark')
})

test('paints the custom name over the DSH brand and follows later renames', () => {
  const brand = brandElement()
  const bridge = boot({ brand, title: '我的工作台' })
  assert.equal(brand.textContent, '我的工作台')

  bridge.emit({ v: 1, type: 'title', title: '另一个名字' })
  assert.equal(brand.textContent, '另一个名字')
})

test('replaces only the title above the version line', () => {
  const { brand, title, version } = localBuildBrand()
  const bridge = boot({ brand, title: '我的工作台' })
  assert.equal(title.textContent, '我的工作台')
  assert.equal(version.textContent, '0.2.1-rc.2-687f282')

  bridge.emit({ v: 1, type: 'title', title: '新名字' })
  assert.equal(title.textContent, '新名字')
  assert.equal(version.textContent, '0.2.1-rc.2-687f282')
})

test('reports the session-row attention counts to the host', () => {
  const bridge = boot({
    rows: [
      sessionRow('warning'),
      sessionRow('warning'),
      sessionRow('done'),
      sessionRow('ongoing'),
      sessionRow('idle'),
    ],
  })
  const attention = bridge.messages.filter(message => message.type === 'sessionAttention').pop()
  assert.ok(attention, 'a sessionAttention message was posted')
  assert.equal(attention.v, 1)
  assert.equal(attention.needsInput, 2)
  assert.equal(attention.completed, 1)
  assert.equal(attention.running, 1)
  assert.equal(attention.error, 0)
})

test('ignores messages from another protocol version', async () => {
  const bridge = boot()
  const picked = bridge.window.__DSH_DIRECTORY_PICKER__.pick()
  const request = bridge.messages.find(message => message.type === 'pickDirectory')
  bridge.emit({ v: 2, type: 'pickDirectoryResult', id: request.id, ok: true, path: 'nope' })
  bridge.emit({ v: 1, type: 'pickDirectoryResult', id: request.id, ok: true, path: 'C:\\ok' })
  assert.equal(await picked, 'C:\\ok')
})
