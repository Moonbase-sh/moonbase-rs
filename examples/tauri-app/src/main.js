// Aurora's frontend: the app itself, plus a badge showing the license state. The
// activation flow is the plugin's own window, which presents itself when the app
// is locked; the badge brings it up on demand. window.__TAURI__.moonbase is the
// plugin's global API.

const { moonbase } = window.__TAURI__
const { invoke } = window.__TAURI__.core

const $ = (id) => document.getElementById(id)

function toast(message) {
  const el = $('toast')
  el.textContent = message
  el.hidden = false
  clearTimeout(toast.timer)
  toast.timer = setTimeout(() => (el.hidden = true), 3200)
}

// -- The palette: the app itself. Shuffling is free; exporting is the paid part.

const palette = Array.from({ length: 5 }, () => ({ color: '#000000', locked: false }))

function randomColor(hue) {
  const h = (hue + 360) % 360
  const s = 55 + Math.random() * 35
  const l = 35 + Math.random() * 35
  const a = (s * Math.min(l, 100 - l)) / 10000
  const f = (n) => {
    const k = (n + h / 30) % 12
    const c = l / 100 - a * Math.max(-1, Math.min(k - 3, 9 - k, 1))
    return Math.round(255 * c).toString(16).padStart(2, '0')
  }
  return `#${f(0)}${f(8)}${f(4)}`
}

function shuffle() {
  const base = Math.random() * 360
  palette.forEach((swatch, index) => {
    if (!swatch.locked) swatch.color = randomColor(base + index * (28 + Math.random() * 18))
  })
  renderPalette()
}

function textColor(hex) {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16))
  return r * 0.299 + g * 0.587 + b * 0.114 > 150 ? '#0b0f1f' : '#ffffff'
}

function renderPalette() {
  $('palette').replaceChildren(
    ...palette.map((swatch) => {
      const button = document.createElement('button')
      button.type = 'button'
      button.className = `swatch${swatch.locked ? ' locked' : ''}`
      button.style.cssText = `background:${swatch.color};color:${textColor(swatch.color)}`
      button.innerHTML = '<span class="lock"></span><span class="hex"></span>'
      button.querySelector('.lock').textContent = swatch.locked ? 'Locked' : 'Click to lock'
      button.querySelector('.hex').textContent = swatch.color.toUpperCase()
      button.addEventListener('click', () => {
        swatch.locked = !swatch.locked
        renderPalette()
      })
      return button
    }),
  )
}

async function exportPalette() {
  try {
    // Gated in Rust, which also brings up the activation window when unlicensed.
    const saved = await invoke('export_palette', { colors: palette.map((swatch) => swatch.color) })
    if (saved) toast(`Saved ${saved}`)
  } catch (error) {
    toast(String(error))
  }
}

// -- The license badge.

function renderBadge(state) {
  const badge = $('license-badge')
  if (state.status === 'loading') {
    badge.hidden = true
    return
  }
  badge.hidden = false
  const license = state.license
  if (license?.trial) {
    badge.className = 'badge trial'
    badge.textContent = license.daysRemaining == null ? 'Trial' : `Trial · ${license.daysRemaining} days left`
  } else if (license) {
    badge.className = 'badge licensed'
    badge.textContent = state.update ? `Licensed · update ${state.update.latestVersion}` : 'Licensed'
  } else {
    badge.className = 'badge unlicensed'
    badge.textContent = state.status === 'activating' ? 'Activating…' : 'Activate'
  }
}

async function main() {
  renderPalette()
  shuffle()
  document.addEventListener('keydown', (event) => {
    if (event.code === 'Space') {
      event.preventDefault()
      shuffle()
    }
  })
  $('shuffle').addEventListener('click', shuffle)
  $('export').addEventListener('click', exportPalette)
  $('license-badge').addEventListener('click', () => moonbase.present({ screen: 'license' }))

  await moonbase.onStateChanged(renderBadge)
  renderBadge(await moonbase.state())
}

main()
