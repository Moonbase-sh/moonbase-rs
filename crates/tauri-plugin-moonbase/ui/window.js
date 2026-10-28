// Boots the activation UI as the plugin's own window: the page is the panel, and
// the window is resized to fit each screen.
;(() => {
  const config = window.__MOONBASE_UI__
  const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(`plugin:moonbase|${command}`, args)
  document.title = config.productName
  if (config.reduceMotion) document.body.classList.add('reduce-motion')

  let ready = false
  const ui = createActivationUI({
    root: document.getElementById('app'),
    config,
    invoke,
    mode: 'window',
    async onHeight(height) {
      if (ready) return invoke('ui_resize', { height })
      ready = true
      const opened = await invoke('ui_ready', { height })
      if (opened?.screen) ui.show(opened.screen)
    },
  })
  window.__moonbase = ui

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && ui.canClose()) invoke('close_ui')
  })
  ui.start()
})()
