/**
 * dshell's content-tab bridge.
 *
 * Injected before every content page script. It defines the two optional
 * globals DSH reads:
 *
 *   __DSH_DIRECTORY_PICKER__.pick()  -> Promise<string | null>
 *   __DSH_HOST_PATHS__.pathFor(file) -> absolute path or ''
 *
 * All communication uses the WebView2 message channel. The page cannot reach
 * Tauri commands: this file and the Rust handler are the only surface.
 *
 * Host-path support is best-effort. A path that is not ready by the time DSH
 * reads it is simply absent, and DSH uploads the file as it did before.
 */
(() => {
  'use strict'

  // One switch for a field rollback that must not touch the directory picker.
  const ENABLE_HOST_PATHS = true
  const PROTOCOL = 1
  const PICK_TIMEOUT_MS = 10 * 60 * 1000
  const PATH_TIMEOUT_MS = 3000

  const webview = window.chrome && window.chrome.webview
  if (!webview || typeof webview.postMessage !== 'function') return
  if (window.__dshellContentBridgeInstalled === true) return
  try {
    Object.defineProperty(window, '__dshellContentBridgeInstalled', { value: true })
  } catch {
    // A page that froze the global is not a reason to skip the bridge.
  }

  const pendingPicks = new Map()
  const pendingPaths = new Map()
  /** key -> unbounded queue of paths returned for that key in this batch. */
  const pathCache = new Map()
  /** File -> path, so repeated pathFor() calls stay stable. */
  const assignedPaths = new WeakMap()
  let sequence = 0
  let hostReady = false

  function nextId() {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
      return crypto.randomUUID()
    }
    sequence += 1
    return `${Date.now()}-${sequence}`
  }

  function post(message) {
    try {
      // A string keeps `TryGetWebMessageAsString` usable on every WebView2
      // version; the host also accepts JSON objects as a fallback.
      webview.postMessage(JSON.stringify(message))
    } catch {
      // The host may be tearing the webview down; nothing to recover.
    }
  }

  function postWithFiles(message, files) {
    if (typeof webview.postMessageWithAdditionalObjects !== 'function') return false
    try {
      webview.postMessageWithAdditionalObjects(JSON.stringify(message), files)
      return true
    } catch {
      // WebView2 rejects a File that is not backed by a disk path. The caller
      // treats that exactly like "no path": DSH uploads as before.
      return false
    }
  }

  function keyOf(file) {
    return `${file.name}\u0000${file.size}\u0000${file.lastModified}`
  }

  function isFile(value) {
    return typeof File === 'function' && value instanceof File
  }

  // DSH renders its own theme and sidebar. The strip cannot read that cross-
  // origin DOM, so the content page reports the sidebar surface here. The host
  // validates the origin, then forwards it to chrome as `dshell://tab-theme`.
  const THEME_INTERVAL_MS = 2000
  let lastThemeSignature = ''

  function cssColor(value) {
    const color = typeof value === 'string' ? value.trim() : ''
    return /^(rgb|rgba|hsl|hsla)\(/i.test(color) || /^#[0-9a-f]{3,8}$/i.test(color)
      ? color
      : ''
  }

  function isDarkColor(color) {
    const match = color.match(/rgba?\(\s*(\d+)[,\s]+(\d+)[,\s]+(\d+)/i)
    if (!match) return true
    const channels = [match[1], match[2], match[3]].map(Number).map((value) => {
      const channel = value / 255
      return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
    })
    return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722 < 0.5
  }

  function hasSurface(element) {
    const style = getComputedStyle(element)
    return Boolean(cssColor(style.backgroundColor)) && style.backgroundColor !== 'rgba(0, 0, 0, 0)'
  }

  function findSidebar() {
    // DSH's semantic class survives minification (`…_sidebarCol`), while the
    // exact prefix does not. Pick the largest matching surface, not the first
    // nested sidebar item.
    let best = null
    let bestArea = 0
    for (const element of document.querySelectorAll('[class*="sidebar" i], [data-testid*="sidebar" i]')) {
      const rect = element.getBoundingClientRect()
      if (rect.width < 120 || rect.height < window.innerHeight * 0.4 || !hasSurface(element)) continue
      const area = rect.width * rect.height
      if (area > bestArea) {
        best = element
        bestArea = area
      }
    }
    if (best) return best

    // Fallback for a renamed class: the sidebar is the background surface at
    // the left edge, so sample a few points instead of scanning the whole DOM.
    const points = [
      [1, window.innerHeight / 2],
      [24, window.innerHeight / 2],
      [window.innerWidth * 0.08, window.innerHeight / 2],
    ]
    for (const [x, y] of points) {
      for (const element of document.elementsFromPoint(x, y)) {
        const rect = element.getBoundingClientRect()
        if (rect.width < 120 || rect.width > window.innerWidth * 0.5) continue
        if (rect.height < window.innerHeight * 0.4 || !hasSurface(element)) continue
        return element
      }
    }
    return document.body
  }

  function reportTheme(force) {
    if (!document.body) return
    const sidebar = findSidebar()
    const sidebarStyle = getComputedStyle(sidebar)
    const bodyStyle = getComputedStyle(document.body)
    const background =
      cssColor(sidebarStyle.backgroundColor) || cssColor(bodyStyle.backgroundColor)
    const foreground = cssColor(sidebarStyle.color) || cssColor(bodyStyle.color)
    if (!background || !foreground) return
    const mode = isDarkColor(background) ? 'dark' : 'light'
    const signature = `${background}|${foreground}|${mode}`
    if (!force && signature === lastThemeSignature) return
    lastThemeSignature = signature
    post({ v: PROTOCOL, type: 'theme', background, foreground, mode })
  }

  function installThemeReporter() {
    let started = false
    const start = () => {
      if (started) return
      started = true
      reportTheme(true)
      new MutationObserver(() => reportTheme(false)).observe(document.documentElement, {
        attributes: true,
        attributeFilter: ['class', 'style', 'data-theme', 'data-ds-theme-source'],
      })
      if (document.body) {
        new MutationObserver(() => reportTheme(false)).observe(document.body, {
          attributes: true,
          attributeFilter: ['class', 'style'],
        })
      }
      const media = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)')
      if (media && typeof media.addEventListener === 'function') {
        media.addEventListener('change', () => reportTheme(true))
      }
      // Theme tokens can change without an attribute mutation (CSS variables
      // on an ancestor); a cheap poll keeps the strip in step with that. It is
      // forced because chrome may not have been listening when the first
      // report was posted during boot.
      setInterval(() => reportTheme(true), THEME_INTERVAL_MS)
      document.addEventListener('visibilitychange', () => {
        if (!document.hidden) reportTheme(true)
      })
    }
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', start, { once: true })
    } else {
      start()
    }
    // DSH's client bootstrap can replace the document early enough that the
    // load event is missed; the timer makes the reporter self-starting.
    setTimeout(start, 500)
    // The body may be replaced by DSH's client-side bootstrap after load.
    setTimeout(() => reportTheme(true), 1000)
  }

  installThemeReporter()

  function settlePick(message) {
    const pending = pendingPicks.get(message.id)
    if (!pending) return
    pendingPicks.delete(message.id)
    if (message.ok) pending.resolve(message.path ?? null)
    else pending.reject(new Error(typeof message.error === 'string' ? message.error : 'directory picker failed'))
  }

  function settlePaths(message) {
    const pending = pendingPaths.get(message.id)
    if (!pending) return
    pendingPaths.delete(message.id)
    const paths = Array.isArray(message.paths) ? message.paths : []
    for (let index = 0; index < pending.keys.length; index += 1) {
      const path = paths[index]
      if (typeof path !== 'string' || path === '') continue
      const key = pending.keys[index]
      const bucket = pathCache.get(key)
      if (bucket === undefined) pathCache.set(key, [path])
      else bucket.push(path)
    }
    pending.resolve()
  }

  webview.addEventListener('message', (event) => {
    const message = event && event.data
    if (!message || message.v !== PROTOCOL) return
    if (message.type === 'hello') {
      hostReady = true
      reportTheme(true)
    } else if (message.type === 'pickDirectoryResult') settlePick(message)
    else if (message.type === 'hostPathsResult') settlePaths(message)
  })

  if (window.__DSH_DIRECTORY_PICKER__ === undefined) {
    window.__DSH_DIRECTORY_PICKER__ = {
      async pick() {
        // The host answers the `ready` announcement with `hello`. If the
        // WebView2 registration failed, fail in 1.5s instead of looking stuck
        // for the full dialog timeout.
        if (!hostReady) {
          const started = Date.now()
          while (!hostReady && Date.now() - started < 1500) {
            await new Promise(resolve => setTimeout(resolve, 25))
          }
        }
        if (!hostReady) throw new Error('directory picker bridge unavailable')
        return new Promise((resolve, reject) => {
          const id = nextId()
          const timer = setTimeout(() => {
            if (!pendingPicks.delete(id)) return
            reject(new Error('directory picker timed out'))
          }, PICK_TIMEOUT_MS)
          pendingPicks.set(id, {
            resolve(value) {
              clearTimeout(timer)
              resolve(value)
            },
            reject(error) {
              clearTimeout(timer)
              reject(error)
            },
          })
          post({ v: PROTOCOL, type: 'pickDirectory', id })
        })
      },
    }
  }

  const hostPathsAvailable = ENABLE_HOST_PATHS
    && typeof webview.postMessageWithAdditionalObjects === 'function'

  if (!hostPathsAvailable) {
    post({ v: PROTOCOL, type: 'ready', features: { hostPaths: false } })
    return
  }

  /**
   * Ask the host for the on-disk paths of one batch of File objects. Resolves
   * even when the host rejects the files; the event that is waiting on it must
   * never be held hostage by the bridge.
   */
  function requestPaths(files) {
    return new Promise((resolve) => {
      if (!hostReady) {
        resolve()
        return
      }
      const real = files.filter(isFile)
      if (real.length === 0) {
        resolve()
        return
      }
      const id = nextId()
      const keys = real.map(keyOf)
      const timer = setTimeout(() => {
        pendingPaths.delete(id)
        resolve()
      }, PATH_TIMEOUT_MS)
      pendingPaths.set(id, {
        keys,
        resolve() {
          clearTimeout(timer)
          resolve()
        },
      })
      if (!postWithFiles({ v: PROTOCOL, type: 'hostPaths', id, keys }, real)) {
        clearTimeout(timer)
        pendingPaths.delete(id)
        resolve()
      }
    })
  }

  window.__DSH_HOST_PATHS__ = {
    pathFor(file) {
      if (!isFile(file)) return ''
      const assigned = assignedPaths.get(file)
      if (assigned !== undefined) return assigned
      const bucket = pathCache.get(keyOf(file))
      const path = bucket !== undefined && bucket.length > 0 ? bucket.shift() : ''
      if (path !== '') assignedPaths.set(file, path)
      return path
    },
  }

  // One path request per drag gesture. The DOM drop handler still runs; this
  // listener only observes and caches, so DSH keeps its native directory
  // detection and image handling.
  let dragGeneration = 0
  let dragRequested = -1
  let dragActive = false

  function observeDrag(event) {
    const dataTransfer = event.dataTransfer
    if (!dataTransfer || !dataTransfer.items) return
    const files = []
    for (const item of dataTransfer.items) {
      if (item.kind !== 'file') continue
      let file = null
      try {
        file = item.getAsFile()
      } catch {
        file = null
      }
      if (isFile(file)) files.push(file)
    }
    if (files.length === 0) return
    if (dragRequested === dragGeneration) return
    dragRequested = dragGeneration
    void requestPaths(files)
  }

  function endDrag() {
    if (!dragActive) return
    dragActive = false
    dragGeneration += 1
    // Let a drop that settles in the same task read its paths first.
    setTimeout(() => pathCache.clear(), 0)
  }

  document.addEventListener('dragenter', (event) => {
    if (!dragActive) {
      dragActive = true
      dragGeneration += 1
    }
    observeDrag(event)
  }, true)
  document.addEventListener('dragover', observeDrag, true)
  document.addEventListener('dragleave', (event) => {
    // relatedTarget is null when the pointer left the document, not when it
    // crossed between two elements inside the page.
    if (event.relatedTarget === null) endDrag()
  }, true)
  document.addEventListener('dragend', endDrag, true)
  document.addEventListener('drop', endDrag, true)

  // A picked file is a normal DOM change whose File objects have real paths.
  // Hold that one event until the paths arrive, then replay it once; DSH reads
  // the same input.files it would have read on the original dispatch.
  let replaying = false
  document.addEventListener('change', (event) => {
    if (replaying) return
    const target = event.target
    if (!(target instanceof HTMLInputElement) || target.type !== 'file' || !target.files) return
    const files = Array.from(target.files)
    if (files.length === 0) return
    if (files.every(file => assignedPaths.has(file) || pathCache.has(keyOf(file)))) return
    event.stopImmediatePropagation()
    event.preventDefault()
    void requestPaths(files).then(() => {
      if (!target.isConnected) return
      replaying = true
      try {
        target.dispatchEvent(new Event('change', { bubbles: true }))
      } finally {
        replaying = false
      }
      setTimeout(() => pathCache.clear(), 0)
    })
  }, true)

  post({ v: PROTOCOL, type: 'ready', features: { hostPaths: true } })
})()
