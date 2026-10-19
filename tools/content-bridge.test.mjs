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
  const document = {
    addEventListener(type, listener) {
      documentListeners.push({ type, listener })
    },
  }
  const window = {
    chrome: { webview },
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
    console,
    HTMLInputElement: class HTMLInputElement {},
  }
  sandbox.globalThis = sandbox
  vm.runInNewContext(source, sandbox, { filename: 'content.js' })
  // The real host answers the script's `ready` with `hello`; tests do the same
  // before exercising picker promises.
  if (messages.some(message => message.type === 'ready')) {
    for (const listener of listeners) listener({ data: { v: 1, type: 'hello' } })
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
  assert.equal(bridge.messages.at(-1).type, 'ready')
  assert.equal(bridge.messages.at(-1).features.hostPaths, false)
})

test('ignores messages from another protocol version', async () => {
  const bridge = boot()
  const picked = bridge.window.__DSH_DIRECTORY_PICKER__.pick()
  const request = bridge.messages.find(message => message.type === 'pickDirectory')
  bridge.emit({ v: 2, type: 'pickDirectoryResult', id: request.id, ok: true, path: 'nope' })
  bridge.emit({ v: 1, type: 'pickDirectoryResult', id: request.id, ok: true, path: 'C:\\ok' })
  assert.equal(await picked, 'C:\\ok')
})
