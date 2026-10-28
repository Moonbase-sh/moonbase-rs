// Mounts the activation UI over the app's own content: the panel floats on a
// blurred, dimmed copy of the app, which is inert behind it, so the app is locked
// until there is a license. Injected into the app's webviews, and dormant until the
// plugin mounts it. The UI lives in a closed shadow root, so the app's styles cannot
// reach it and its styles cannot reach the app.
;(() => {
  if (window.__moonbaseOverlay || location.protocol === 'moonbase:' || location.hostname === 'moonbase.localhost') return

  const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(`plugin:moonbase|${command}`, args)
  const keys = ['keydown', 'keyup', 'keypress']
  let host = null
  let ui = null
  let previousInert = false
  let previousFocus = null

  // While mounted, keys outside the overlay go nowhere, so the app's shortcuts
  // cannot act behind it. Registered before any of the app's own listeners. Focus
  // can fall out of the overlay when a screen re-renders, so a stray key also
  // brings it back, and Escape still dismisses an overlay that may be dismissed.
  for (const type of keys) {
    window.addEventListener(
      type,
      (event) => {
        if (!host || event.composedPath().includes(host)) return
        event.preventDefault()
        event.stopImmediatePropagation()
        if (type !== 'keydown') return
        if (event.key === 'Escape' && ui?.canClose()) invoke('close_ui')
        else ui?.focus()
      },
      true,
    )
  }

  function element(tag, className) {
    const node = document.createElement(tag)
    if (className) node.className = className
    return node
  }

  function adoptStyles(shadow) {
    try {
      // A constructed sheet is CSSOM, which the app's content security policy allows.
      const sheet = new CSSStyleSheet()
      sheet.replaceSync(CSS)
      shadow.adoptedStyleSheets = [sheet]
    } catch {
      const style = element('style')
      style.textContent = CSS
      shadow.append(style)
    }
  }

  // The app's content goes inert behind the overlay: no clicks, no focus, no
  // assistive-technology access. Waits for <body> if the overlay arrived first.
  function lockBody() {
    if (!host) return
    if (!document.body) {
      document.addEventListener('DOMContentLoaded', lockBody, { once: true })
      return
    }
    previousInert = document.body.inert
    document.body.inert = true
  }

  function mount(config, state, screen) {
    if (host) {
      if (state) ui.update(state)
      if (screen) ui.show(screen)
      return
    }
    host = element('moonbase-activation')
    host.style.cssText = 'all: initial; position: fixed; inset: 0; z-index: 2147483647; display: block;'
    const shadow = host.attachShadow({ mode: 'closed' })
    adoptStyles(shadow)

    const scrim = element('div', config.reduceMotion ? 'mb-scrim reduce-motion' : 'mb-scrim')
    const frame = element('div', 'mb-frame')
    const card = element('div', 'mb-card')
    const root = element('div')
    card.append(root)
    frame.append(element('div', 'mb-glow'), element('div', 'mb-bloom'), card)
    scrim.append(frame)
    shadow.append(scrim)

    // Keys typed in the overlay stop here rather than reaching the app's handlers.
    for (const type of keys) host.addEventListener(type, (event) => event.stopPropagation())
    host.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && ui?.canClose()) invoke('close_ui')
    })

    previousFocus = document.activeElement
    document.documentElement.append(host)
    lockBody()

    ui = createActivationUI({ root, config, invoke, mode: 'overlay' })
    ui.start(state, screen).then(() => ui?.focus())
  }

  function unmount() {
    if (!host) return
    host.remove()
    host = null
    ui = null
    if (document.body) document.body.inert = previousInert
    previousFocus?.focus?.({ preventScroll: true })
  }

  Object.defineProperty(window, '__moonbaseOverlay', {
    value: Object.freeze({
      mount,
      unmount,
      update: (state) => ui?.update(state),
      show: (screen) => ui?.show(screen),
      download: (next) => ui?.download(next),
    }),
  })
})()
