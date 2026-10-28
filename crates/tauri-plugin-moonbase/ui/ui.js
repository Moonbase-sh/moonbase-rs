'use strict'

// The activation UI, shared by the plugin's own window (window.js) and the in-app
// overlay (overlay.js). Every screen renders from the license state the plugin
// pushes in, and every action is a plugin command. Server-provided text only ever
// goes into the page as text.
//
// mode is 'window' or 'overlay'. In a window the page is the panel and reports its
// height so the window fits each screen; as an overlay the panel floats over the
// app, which it locks while there is no license.
// eslint-disable-next-line no-unused-vars
function createActivationUI({ root, config, invoke, mode, onHeight }) {
  const product = config.productName
  const account = config.manufacturerName || 'Moonbase'
  const overlay = mode === 'overlay'
  const MOONBASE_SITE = 'https://moonbase.sh'

  // -- Building blocks ------------------------------------------------------------

  function h(tag, props = {}, ...children) {
    const el = document.createElement(tag)
    for (const [key, value] of Object.entries(props)) {
      if (value === undefined || value === null || value === false) continue
      if (key.startsWith('on')) el.addEventListener(key.slice(2).toLowerCase(), value)
      else if (key === 'class') el.className = value
      else if (key === 'style') el.style.cssText = value
      else el.setAttribute(key, value === true ? '' : value)
    }
    for (const child of children.flat(Infinity)) {
      if (child === null || child === undefined || child === false) continue
      el.append(child instanceof Node ? child : document.createTextNode(String(child)))
    }
    return el
  }

  // Static markup only: icons defined in this file.
  function svg(markup) {
    const template = document.createElement('template')
    template.innerHTML = markup.trim()
    return template.content.firstChild
  }

  const icon = (path, { size = 16, stroke = 'currentColor', width = 1.8 } = {}) =>
    svg(`<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="${stroke}" stroke-width="${width}" stroke-linecap="round" stroke-linejoin="round"><path d="${path}"></path></svg>`)

  const paths = {
    check: 'M4.5 12.75l6 6 9-13.5',
    cross: 'M6 18 18 6M6 6l12 12',
    lock: 'M16.5 10.5V6.75a4.5 4.5 0 1 0-9 0v3.75m-.75 11.25h10.5a2.25 2.25 0 0 0 2.25-2.25v-6.75a2.25 2.25 0 0 0-2.25-2.25H6.75a2.25 2.25 0 0 0-2.25 2.25v6.75a2.25 2.25 0 0 0 2.25 2.25z',
    back: 'M15.75 19.5 8.25 12l7.5-7.5',
    chevron: 'm8.25 4.5 7.5 7.5-7.5 7.5',
    signal:
      'M9.348 14.652a3.75 3.75 0 010-5.304m5.304 0a3.75 3.75 0 010 5.304m-7.425 2.121a6.75 6.75 0 010-9.546m9.546 0a6.75 6.75 0 010 9.546M5.106 18.894c-3.808-3.807-3.808-9.98 0-13.788m13.788 0c3.808 3.807 3.808 9.98 0 13.788M12 12h.008v.008H12V12z',
    computer:
      'M9 17.25v1.007a3 3 0 0 1-.879 2.122L7.5 21h9l-.621-.621A3 3 0 0 1 15 18.257V17.25m6-12V15a2.25 2.25 0 0 1-2.25 2.25H5.25A2.25 2.25 0 0 1 3 15V5.25m18 0A2.25 2.25 0 0 0 18.75 3H5.25A2.25 2.25 0 0 0 3 5.25m18 0V12a2.25 2.25 0 0 1-2.25 2.25H5.25A2.25 2.25 0 0 1 3 12V5.25',
    fileDown:
      'M19.5 14.25v-2.625a3.375 3.375 0 0 0-3.375-3.375h-1.5A1.125 1.125 0 0 1 13.5 7.125v-1.5a3.375 3.375 0 0 0-3.375-3.375H8.25m.75 12 3 3m0 0 3-3m-3 3v-6m-1.5-9H5.625c-.621 0-1.125.504-1.125 1.125v17.25c0 .621.504 1.125 1.125 1.125h12.75c.621 0 1.125-.504 1.125-1.125V11.25a9 9 0 0 0-9-9z',
    upload:
      'M7.5 7.5h-.75A2.25 2.25 0 0 0 4.5 9.75v7.5a2.25 2.25 0 0 0 2.25 2.25h7.5a2.25 2.25 0 0 0 2.25-2.25v-7.5a2.25 2.25 0 0 0-2.25-2.25h-.75m-6 3.75 3-3m0 0 3 3m-3-3v11.25m6-2.25h.75a2.25 2.25 0 0 0 2.25-2.25v-7.5a2.25 2.25 0 0 0-2.25-2.25h-.75',
    okCircle: 'M9 12.75 11.25 15 15 9.75M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0z',
    warning:
      'M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z',
    blocked:
      'M18.364 18.364A9 9 0 0 0 5.636 5.636m12.728 12.728A9 9 0 0 1 5.636 5.636m12.728 12.728L5.636 5.636',
    copy: 'M15.75 17.25v3.375c0 .621-.504 1.125-1.125 1.125h-9.75a1.125 1.125 0 0 1-1.125-1.125V7.875c0-.621.504-1.125 1.125-1.125H6.75a9.06 9.06 0 0 1 1.5.124m7.5 10.376h3.375c.621 0 1.125-.504 1.125-1.125V11.25c0-4.46-3.243-8.161-7.5-8.876a9.06 9.06 0 0 0-1.5-.124H9.375c-.621 0-1.125.504-1.125 1.125v3.5m7.5 10.375H9.375',
    download:
      'M3 16.5v2.25A2.25 2.25 0 0 0 5.25 21h13.5A2.25 2.25 0 0 0 21 18.75V16.5M16.5 12 12 16.5m0 0L7.5 12m4.5 4.5V3',
    puzzle:
      'M14.25 6.087c0-.355.186-.676.401-.959.221-.29.349-.634.349-1.003 0-1.036-1.007-1.875-2.25-1.875s-2.25.84-2.25 1.875c0 .369.128.713.349 1.003.215.283.401.604.401.959v0a.64.64 0 0 1-.657.643 48.4 48.4 0 0 1-4.163-.3c.186 1.613.293 3.25.315 4.907a.656.656 0 0 1-.658.663v0c-.355 0-.676-.186-.959-.401a1.647 1.647 0 0 0-1.003-.349c-1.036 0-1.875 1.007-1.875 2.25s.84 2.25 1.875 2.25c.369 0 .713-.128 1.003-.349.283-.215.604-.401.959-.401v0c.31 0 .555.26.532.57a48.04 48.04 0 0 1-.642 5.056c1.518.19 3.058.309 4.616.354a.64.64 0 0 0 .657-.643v0c0-.355-.186-.676-.401-.959a1.647 1.647 0 0 1-.349-1.003c0-1.035 1.008-1.875 2.25-1.875 1.243 0 2.25.84 2.25 1.875 0 .369-.128.713-.349 1.003-.215.283-.4.604-.4.959v0c0 .333.277.599.61.58a48.1 48.1 0 0 0 5.427-.63 48.05 48.05 0 0 0 .582-4.717.532.532 0 0 0-.533-.57v0c-.355 0-.676.186-.959.401-.29.221-.634.349-1.003.349-1.035 0-1.875-1.007-1.875-2.25s.84-2.25 1.875-2.25c.37 0 .713.128 1.003.349.283.215.604.401.96.401v0a.656.656 0 0 0 .658-.663 48.4 48.4 0 0 0-.37-5.36c-1.886.342-3.81.574-5.766.689a.578.578 0 0 1-.61-.58v0z',
    external:
      'M13.5 6H5.25A2.25 2.25 0 0 0 3 8.25v10.5A2.25 2.25 0 0 0 5.25 21h10.5A2.25 2.25 0 0 0 18 18.75V10.5m-10.5 6L21 3m0 0h-5.25M21 3v5.25',
    sparkles:
      'M9.813 15.904 9 18.75l-.813-2.846a4.5 4.5 0 0 0-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 0 0 3.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 0 0 3.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 0 0-3.09 3.09Z',
  }

  const spinner = (size = 17) =>
    svg(`<svg class="spin" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none"><circle cx="12" cy="12" r="9" stroke="rgba(255,255,255,0.3)" stroke-width="3"></circle><path d="M21 12a9 9 0 0 0-9-9" stroke="#fff" stroke-width="3" stroke-linecap="round"></path></svg>`)

  const bigSpinner = (small = false) =>
    h(
      'div',
      { class: small ? 'spinner small' : 'spinner' },
      svg(
        `<svg width="100%" height="100%" viewBox="0 0 24 24" fill="none"><circle cx="12" cy="12" r="9.5" stroke="rgba(255,255,255,0.1)" stroke-width="1.6"></circle><path d="M21.5 12a9.5 9.5 0 0 0-9.5-9.5" stroke="#82cef1" stroke-width="1.6" stroke-linecap="round"></path></svg>`,
      ),
    )

  // The product mark: the app's own logo, or the design's sun in the accent.
  function mark() {
    if (config.logoSvg) {
      // The app's own SVG, inlined: an overlay cannot load images from the plugin.
      const template = document.createElement('template')
      template.innerHTML = config.logoSvg
      const logo = template.content.querySelector('svg')
      if (logo) {
        logo.classList.add('mark')
        return logo
      }
    }
    if (config.logo) return h('img', { class: 'mark', src: config.logo, alt: '' })
    return svg(
      `<svg class="mark" viewBox="0 0 38 38" fill="none"><circle cx="19" cy="19" r="13" stroke="var(--accent)" stroke-width="2" opacity="0.9"></circle><circle cx="19" cy="19" r="4.5" fill="var(--accent)"></circle><g stroke="var(--accent)" stroke-width="2" stroke-linecap="round"><line x1="19" y1="1.5" x2="19" y2="6"></line><line x1="19" y1="32" x2="19" y2="36.5"></line><line x1="1.5" y1="19" x2="6" y2="19"></line><line x1="32" y1="19" x2="36.5" y2="19"></line><line x1="6.6" y1="6.6" x2="9.8" y2="9.8"></line><line x1="28.2" y1="28.2" x2="31.4" y2="31.4"></line><line x1="31.4" y1="6.6" x2="28.2" y2="9.8"></line><line x1="9.8" y1="28.2" x2="6.6" y2="31.4"></line></g></svg>`,
    )
  }

  const lockup = ({ large = false, dim = false } = {}) =>
    h(
      'div',
      { class: `lockup${large ? ' large' : ''}${dim ? ' dim' : ''}` },
      mark(),
      h('div', {}, h('div', { class: 'name' }, product), config.manufacturerName && h('div', { class: 'maker' }, config.manufacturerName)),
    )

  const header = (pill, options) => h('div', { class: 'header' }, lockup(options), pill)

  const button = (label, onClick, { kind = 'primary', iconPath, disabled, short } = {}) =>
    h(
      'button',
      { class: `btn ${kind}${short ? ' short' : ''}`, type: 'button', disabled, onClick },
      iconPath && icon(iconPath, { width: 1.8 }),
      label,
    )

  const textButton = (label, onClick, { link = false, iconPath, disabled } = {}) =>
    h('button', { class: `text-button${link ? ' link' : ''}`, type: 'button', disabled, onClick }, iconPath && icon(iconPath, { size: 14, width: 1.7 }), label)

  const backButton = (label, onClick) =>
    h('div', { class: 'back' }, textButton(label, onClick, { iconPath: paths.back }))

  const errorLine = (message) =>
    message && h('div', { class: 'error', role: 'alert' }, icon(paths.warning, { size: 15 }), h('span', {}, message))

  const formatDate = (iso) => new Date(iso).toLocaleDateString(undefined, { dateStyle: 'long' })
  const dayMs = 86400000
  const daysUntil = (iso) => Math.max(0, Math.ceil((new Date(iso) - Date.now()) / dayMs))
  const join = (a, b) => (!a ? b : !b ? a : `${a} · ${b}`)
  const osLabel = { macos: 'macOS', windows: 'Windows', linux: 'Linux', ios: 'iOS', android: 'Android' }[config.os] || config.os

  function trialLength(license) {
    const days = license?.expiresAt && license?.issuedAt ? Math.round((new Date(license.expiresAt) - new Date(license.issuedAt)) / dayMs) : 0
    return days >= 1 ? days : config.trialLengthDays
  }

  async function copyText(text) {
    try {
      await navigator.clipboard.writeText(text)
      return
    } catch {
      // Custom schemes are not always secure contexts; fall back to a selection.
    }
    const area = h('textarea', { style: 'position:fixed;opacity:0' })
    area.value = text
    root.append(area)
    area.select()
    document.execCommand('copy')
    area.remove()
  }

  // -- Copy -------------------------------------------------------------------------

  const strings = config.strings || {}
  const pick = (custom, fallback) => (custom ? custom : fallback)

  // What a license that covers only some versions covers, in a sentence or two:
  // "Your updates ended on 1 June 2026. Your license covers releases up to 2.3.0."
  function releaseAccessText(access) {
    if (!access) return ''
    const sentences = []
    if (access.updatesUntil && new Date(access.updatesUntil) < new Date()) sentences.push(`Your updates ended on ${formatDate(access.updatesUntil)}.`)
    const covers = []
    if (access.upToMajor != null) covers.push(`versions up to ${access.upToMajor}.x`)
    if (access.upToVersion) covers.push(`releases up to ${access.upToVersion}`)
    if (covers.length) sentences.push(`Your license covers ${covers.join(' and ')}.`)
    return sentences.join(' ')
  }

  // "Renew updates" for a license whose updates ran out, "Upgrade license" for one
  // limited to some majors, as the customer portal words it.
  const upgradeLabel = (access) => (access?.updatesUntil ? 'Renew updates' : 'Upgrade license')

  function welcomeCopy(reason) {
    switch (reason) {
      case 'deactivated':
        return [pick(strings.deactivatedTitle, 'Deactivated on this computer'), pick(strings.deactivatedBody, `${product} no longer has an active license here. Activate it again to keep using it.`)]
      case 'expired':
        return [pick(strings.licenseExpiredTitle, 'Your license has expired'), pick(strings.licenseExpiredBody, `Renew your ${product} license in your ${account} account, then activate it again.`)]
      case 'invalid':
        return [
          pick(strings.licenseInvalidTitle, 'Your license is no longer valid'),
          pick(strings.licenseInvalidBody, `${account} no longer accepts this license on this computer. Activate again, or contact ${account} if this seems wrong.`),
        ]
      case 'unverified':
        return [
          pick(strings.licenseUnverifiedTitle, "Couldn't verify your license"),
          pick(strings.licenseUnverifiedBody, `${product} has gone too long without checking its license online. Connect to the internet and activate again.`),
        ]
      default:
        return [pick(strings.welcomeTitle, `Activate ${product}`), pick(strings.welcomeBody, `Unlock ${product} through your ${account} account, or take it for a spin with a free trial.`)]
    }
  }

  // -- State --------------------------------------------------------------------------

  let state = null
  let device = null
  let view = null // a screen the user navigated to, over the one the status implies
  let cameFromDetails = false
  let update = null // release notes, fetched when the update screen opens
  let download = { phase: 'idle' }
  let idCopied = false
  let copiedLink = null // the activation link just copied, while "Link copied" shows
  let copyToken = 0
  const offline = { saved: null, file: null, busy: false, error: '' }

  function statusScreen() {
    switch (state?.status) {
      case 'unlicensed':
        return 'welcome'
      case 'activating':
        return 'browser'
      case 'licensed':
        return 'details'
      case 'trial':
        return 'trial'
      case 'trialExpired':
        return 'expired'
      case 'notCovered':
        return 'notCovered'
      default:
        return 'loading'
    }
  }

  const screenName = () => view || statusScreen()

  function go(name) {
    view = name
    if (name === 'update') loadUpdate()
    if (name === 'addons') loadAddOns()
    render()
  }

  const loadAddOns = () => run(() => invoke('load_add_ons'))

  async function run(action) {
    try {
      await action()
    } catch (error) {
      // The plugin puts the customer-facing reason in state.error and pushes it.
      if (!state?.error) console.warn('moonbase:', error)
    }
  }

  async function loadUpdate() {
    update = null
    try {
      update = await invoke('check_update')
    } catch (error) {
      update = { available: false, error: error?.message ?? String(error) }
    }
    render()
  }

  // -- Screens ------------------------------------------------------------------------

  const screens = {
    loading: () => h('div', { class: 'center' }, bigSpinner(true), h('p', {}, 'Checking your license…')),

    welcome: () => {
      const [title, body] = welcomeCopy(state.lockReason)
      return h(
        'div',
        {},
        h('div', { style: 'margin-bottom:30px' }, lockup({ large: true })),
        h('h1', { class: 'large' }, title),
        h('p', { style: 'margin-bottom:28px; max-width:430px' }, body),
        h(
          'div',
          { class: 'stack' },
          button(pick(strings.activateOnline, 'Activate online'), () => run(() => invoke('activate', { offline: false })), { short: true }),
        ),
        errorLine(state.error?.message),
        h(
          'div',
          { style: 'margin-top:24px' },
          textButton(pick(strings.activateOffline, 'No internet? Activate offline'), () => go('offline'), { iconPath: paths.signal }),
        ),
      )
    },

    browser: () => {
      const url = state.activation?.browserUrl
      const host = url ? new URL(url).host : null
      // The way through when the browser didn't open: a quiet link while it
      // probably did, the main action when it couldn't be opened at all (no
      // default browser, say). Either copies the link for any browser, on this
      // computer or another.
      const failed = url && state.activation.browserOpenFailed
      const copied = url && copiedLink === url
      const copyLink = async () => {
        await copyText(url)
        copiedLink = url
        render()
        const token = ++copyToken
        setTimeout(() => {
          if (copyToken !== token) return
          copiedLink = null
          render()
        }, 2000)
      }
      return h(
        'div',
        { class: 'center' },
        bigSpinner(),
        h('h1', {}, url ? 'Waiting for activation' : 'Starting activation…'),
        h(
          'p',
          { style: 'margin:0 auto 22px; max-width:420px' },
          !url
            ? 'Creating a secure activation request with your store.'
            : failed
              ? "We couldn't open your browser. Copy the activation link, open it in any browser on this computer or another, then sign in and confirm this device."
              : ['We opened ', h('span', { class: 'em' }, host), " in your browser. Sign in and confirm this device; we'll pick it up automatically."],
        ),
        failed &&
          h(
            'div',
            { class: 'narrow', style: 'max-width:300px; margin-bottom:18px' },
            button(copied ? 'Link copied' : 'Copy activation link', copyLink, { iconPath: copied ? paths.check : paths.copy, short: true }),
          ),
        h('div', { class: 'chip' }, icon(paths.computer, { size: 15, stroke: '#768aa4', width: 1.6 }), h('span', {}, `${device?.name || 'This device'} · ${osLabel}`)),
        h('div', { class: 'narrow', style: 'max-width:300px' }, button('Cancel activation', () => run(() => invoke('cancel_activation')), { kind: 'ghost', short: true })),
        url &&
          !failed &&
          h(
            'div',
            { class: 'links' },
            textButton('Open the page again', () => run(() => invoke('open_link', { url })), {}),
            textButton(copied ? 'Link copied' : "Browser didn't open? Copy the link", copyLink, { link: true, iconPath: copied ? paths.check : paths.copy }),
          ),
        errorLine(state.error?.message),
      )
    },

    success: () => {
      const license = state.license
      if (!license) return screens[statusScreen()]()
      return h(
        'div',
        { class: 'center' },
        h('div', { class: 'check' }, icon(paths.check, { size: 34, stroke: '#34d27b', width: 2.4 })),
        h('h1', {}, license.trial ? 'Your trial has started' : "You're all set"),
        h(
          'p',
          { style: 'margin:0 auto 24px; max-width:380px' },
          license.trial
            ? `Enjoy ${product}${license.daysRemaining != null ? ` for the next ${license.daysRemaining} days` : ''}. Unlock the full version any time.`
            : `${product} is activated on this machine. Your license is linked to your account.`,
        ),
        h(
          'div',
          { class: 'card mini' },
          h('div', { class: 'row' }, h('span', { class: 'label' }, 'Plan'), h('span', { class: 'value' }, planName(license))),
          h('div', { class: 'row' }, h('span', { class: 'label' }, 'Licensed to'), h('span', { class: 'value' }, license.issuedTo.email || license.issuedTo.name)),
        ),
        h('div', { class: 'narrow' }, button(`Open ${product}`, () => invoke('close_ui'))),
        h('div', { class: 'links' }, textButton('View license details', () => go('details'), { link: true })),
      )
    },

    offline: () => {
      const host = config.activationHost
      const saveSub = offline.saved ? `Saved to ${offline.saved}` : 'Choose where to save it'
      const input = h('input', { type: 'file', accept: '.mb,.jwt,.txt,.lic', onChange: (event) => pickFile(event.target.files?.[0]) })
      const drop = h(
        'label',
        {
          class: 'drop',
          onDragover: (event) => {
            event.preventDefault()
            drop.classList.add('over')
          },
          onDragleave: () => drop.classList.remove('over'),
          onDrop: (event) => {
            event.preventDefault()
            drop.classList.remove('over')
            pickFile(event.dataTransfer?.files?.[0])
          },
        },
        input,
        offline.file
          ? [icon(paths.okCircle, { size: 24, stroke: '#34d27b', width: 1.7 }), h('span', { class: 'title mono' }, offline.file.name), h('span', { class: 'sub' }, 'Click to choose a different file')]
          : [
              icon(paths.upload, { size: 24, stroke: '#768aa4', width: 1.6 }),
              // An app window usually takes dropped files itself, so the overlay only offers a picker.
              h('span', { class: 'title' }, overlay ? 'Choose your license file' : 'Drop your license file here'),
              h('span', { class: 'sub' }, overlay ? 'Click to browse · .mb' : 'or click to browse · .mb'),
            ],
      )
      return h(
        'div',
        {},
        backButton('Back', () => go(null)),
        h('h1', {}, 'Offline activation'),
        h(
          'p',
          { style: 'margin-bottom:24px' },
          'For computers without internet. Save your request file, take it to ',
          h('span', { class: 'em' }, `${host}/activate`),
          ' on a connected device, and bring back the license file.',
        ),
        h('div', { class: 'step' }, h('span', { class: 'n' }, '1'), h('span', { class: 'label' }, 'Save your request file')),
        h(
          'button',
          { class: 'file', type: 'button', onClick: saveRequest },
          icon(paths.fileDown, { size: 22, stroke: '#82cef1', width: 1.6 }),
          h('span', { class: 'grow' }, h('span', { class: 'title' }, config.requestFileName), h('span', { class: 'sub' }, saveSub)),
          offline.saved ? icon(paths.check, { size: 20, stroke: '#34d27b', width: 2 }) : h('span', { class: 'action' }, 'Save…'),
        ),
        h('div', { class: 'step' }, h('span', { class: 'n' }, '2'), h('span', { class: 'label' }, 'Add the license file')),
        drop,
        errorLine(offline.error),
        h(
          'div',
          { class: 'offline-submit' },
          button(offline.busy ? [spinner(), 'Verifying…'] : 'Activate offline', activateOffline, { disabled: offline.busy }),
        ),
      )
    },

    trial: () => {
      const license = state.license
      const left = license.daysRemaining
      const total = trialLength(license)
      const endless = left == null
      return h(
        'div',
        {},
        header(h('span', { class: 'pill trial' }, endless ? 'Trial' : `Trial · ${left} ${left === 1 ? 'day' : 'days'} left`)),
        h('h1', {}, "You're on a free trial"),
        h(
          'p',
          { style: 'margin-bottom:8px' },
          endless ? 'Unlock the full version any time to keep everything.' : `${left} of ${total} days remaining. Unlock the full version any time to keep everything.`,
        ),
        endless ? h('div', { style: 'height:18px' }) : h('div', { class: 'bar' }, h('div', { style: `width:${Math.min(100, Math.max(0, (left / total) * 100))}%` })),
        features(),
        button('Unlock full version', () => run(() => invoke('activate', { offline: false })), { iconPath: paths.lock }),
        errorLine(state.error?.message),
        h('div', { class: 'links' }, textButton('Continue trial', () => invoke('close_ui')), textButton('License details', () => go('details'))),
      )
    },

    expired: () => {
      const trial = state.expiredTrial
      const days = trialLength(trial)
      return h(
        'div',
        {},
        header(h('span', { class: 'pill expired' }, h('span', { class: 'dot' }), 'Trial expired'), { dim: true }),
        h('h1', {}, 'Your trial has ended'),
        h(
          'p',
          { style: 'margin-bottom:8px' },
          trial?.expiresAt
            ? [`Your ${days}-day trial of ${product} ended on `, h('span', { class: 'em' }, formatDate(trial.expiresAt)), '. Unlock the full version to keep using it.']
            : `Your trial of ${product} has ended. Unlock the full version to keep using it.`,
        ),
        h('div', { class: 'bar expired', style: 'margin-bottom:22px' }, h('div', { style: 'width:100%' })),
        h(
          'div',
          { class: 'notice' },
          icon(paths.blocked, { size: 18, stroke: '#f08a8a' }),
          h('span', {}, pick(strings.expiredNotice, `${product}'s full features are locked until it is activated. Your work is safe and opens as usual.`)),
        ),
        button('Unlock full version', () => run(() => invoke('activate', { offline: false })), { iconPath: paths.lock }),
        errorLine(state.error?.message),
        h('div', { class: 'links' }, textButton('Activate offline instead', () => go('offline'))),
      )
    },

    // A valid license that doesn't cover this version of the app, such as one whose
    // updates ended before it came out. Locked like an ended trial, but the license
    // stays: an upgrade or renewal makes it cover this one.
    notCovered: () => {
      const info = state.notCovered
      const access = info?.releaseAccess
      const renewal = !!access?.updatesUntil
      const offlineLicense = info?.license?.activationMethod === 'Offline'
      const title = pick(strings.notCoveredTitle, info?.appVersion ? `Version ${info.appVersion} needs an upgrade` : 'This version needs an upgrade')
      const body = pick(strings.notCoveredBody, releaseAccessText(access) || `Your ${product} license doesn't cover this version.`)
      return h(
        'div',
        {},
        header(h('span', { class: 'pill upgrade' }, h('span', { class: 'dot' }), 'Upgrade needed'), { dim: true }),
        h('h1', {}, title),
        h('p', { style: 'margin-bottom:22px' }, body),
        h(
          'div',
          { class: 'notice upgrade' },
          icon(paths.warning, { size: 18, stroke: 'var(--trial)' }),
          h('span', {}, `${product} is locked. Your license still works with the versions it covers, so you can go back to one of those, or upgrade to use this one.`),
        ),
        // Opens the browser either way: the upgrade page, or the activation page
        // that offers it.
        button(upgradeLabel(access), () => run(() => invoke('upgrade_license')), { iconPath: paths.external }),
        errorLine(state.error?.message),
        h(
          'div',
          { class: 'links' },
          // An offline license is never checked online: the upgraded one comes as a
          // new license file.
          offlineLicense
            ? textButton('Load an upgraded license file', () => go('offline'))
            : textButton(state.refreshing ? 'Checking your license…' : renewal ? 'Already renewed? Check again' : 'Already upgraded? Check again', () => run(() => invoke('refresh', { force: true })), {
                disabled: state.refreshing,
              }),
        ),
      )
    },

    details: () => {
      const license = state.license
      if (!license) return screens[statusScreen()]()
      const pills = h(
        'div',
        { class: 'pills' },
        state.update && h('button', { class: 'pill update', type: 'button', onClick: () => openUpdate(true) }, icon(paths.download, { size: 13, stroke: 'var(--accent)', width: 1.9 }), 'Update'),
        h('span', { class: 'pill active' }, h('span', { class: 'dot' }), license.trial ? 'Trial' : 'Active'),
      )
      const seatCount = license.seatCount
      const used = license.seatsUsed
      const pipCount = seatCount ? Math.min(seatCount, 8) : 0
      return h(
        'div',
        {},
        header(pills),
        h(
          'div',
          { class: 'card rows' },
          h('div', { class: 'row' }, h('span', { class: 'label' }, 'Licensed to'), h('span', { class: 'value' }, license.issuedTo.name || license.issuedTo.email)),
          h('div', { class: 'row' }, h('span', { class: 'label' }, 'Email'), h('span', { class: 'value soft' }, license.issuedTo.email)),
          h('div', { class: 'row' }, h('span', { class: 'label' }, 'Plan'), h('span', { class: 'value' }, `${planName(license)} · ${license.expiresAt ? `until ${formatDate(license.expiresAt)}` : 'perpetual'}`)),
          h(
            'div',
            { class: 'row id' },
            h('span', { class: 'label' }, 'License ID'),
            h(
              'span',
              { class: 'copy-group' },
              h('span', { class: 'license-id' }, license.id),
              h(
                'button',
                {
                  class: 'small-button',
                  type: 'button',
                  title: 'Copy license ID for support',
                  onClick: async () => {
                    await copyText(license.id)
                    idCopied = true
                    render()
                    setTimeout(() => {
                      idCopied = false
                      render()
                    }, 1800)
                  },
                },
                idCopied ? [icon(paths.check, { size: 14, stroke: '#34d27b', width: 2 }), 'Copied'] : [icon(paths.copy, { size: 14, width: 1.6 }), 'Copy'],
              ),
            ),
          ),
        ),
        h(
          'div',
          { class: 'card seats' },
          h(
            'div',
            {},
            h('div', { class: 'title' }, seatCount ? `Activations · ${used ?? '?'} of ${seatCount} ${seatCount === 1 ? 'device' : 'devices'}` : license.activationMethod === 'Offline' ? 'Activated offline on this device' : 'Activated on this device'),
            pipCount > 0 && h('div', { class: 'pips' }, Array.from({ length: pipCount }, (_, index) => h('span', { class: `pip${index < (used ?? 0) ? ' used' : ''}` }))),
          ),
          h(
            'button',
            { class: 'danger-button', type: 'button', disabled: state.busy, onClick: () => run(() => invoke('deactivate')) },
            state.busy ? [spinner(13), 'Deactivating…'] : license.trial || license.activationMethod === 'Offline' ? 'Remove from this device' : 'Deactivate this device',
          ),
        ),
        errorLine(state.error?.message),
        // Add-ons are activated and tried from a full license.
        config.showAddOns &&
          !license.trial &&
          h(
            'button',
            { class: 'card nav-card', type: 'button', onClick: () => go('addons') },
            icon(paths.puzzle, { size: 20, stroke: 'var(--accent)', width: 1.7 }),
            h(
              'span',
              { class: 'grow' },
              h('span', { class: 'title' }, 'Add-ons'),
              h('span', { class: 'sub' }, state.activeSubProducts.length ? `${state.activeSubProducts.length} active on this computer` : 'See what you can add'),
            ),
            icon(paths.chevron, { size: 18, stroke: '#768aa4', width: 2 }),
          ),
      )
    },

    // The product's add-ons, each with where it stands on this computer and what
    // the customer can do about it: try it, buy it, or activate it here.
    addons: () => {
      const license = state.license
      if (!license) return screens[statusScreen()]()
      const info = state.addOns
      const loading = info.phase === 'loading'
      const failed = !!info.error && info.addOns.length === 0
      const rows = info.addOns.map((entry) => describeAddOn(entry, license))
      let list
      if (rows.length) list = rows.map(addOnRow)
      else if (info.phase !== 'ready') list = [0, 1, 2].map(() => h('div', { class: 'card addon skeleton' }, h('div', { class: 'icon' }), h('div', { class: 'grow' }, h('span'), h('span'))))
      else if (failed) list = errorLine(info.error)
      else list = h('p', { class: 'empty' }, `There are no add-ons for ${product} yet.`)
      // Under the list: why an activation or trial failed, or why a reload did while
      // the earlier list stays up.
      const message = info.actionError || (info.addOns.length ? info.error : null)
      // Not while an add-on is being set up: its license is on its way.
      const waiting = !!info.busyId
      return h(
        'div',
        {},
        backButton('License details', () => go('details')),
        h('h1', {}, 'Add-ons'),
        h('p', { style: 'margin-bottom:20px' }, `Everything you can add to ${product}.`),
        h('div', { class: 'addon-list' }, list),
        errorLine(message),
        h(
          'div',
          { class: 'links start' },
          textButton(loading ? 'Checking…' : failed ? 'Try again' : 'Bought an add-on? Refresh', () => run(() => invoke(failed ? 'load_add_ons' : 'refresh_add_ons')), {
            link: true,
            disabled: loading || waiting,
          }),
        ),
      )
    },

    update: () => {
      const back = () => (cameFromDetails ? go(null) : invoke('close_ui'))
      if (!update) return h('div', { class: 'center' }, bigSpinner(true), h('p', {}, 'Fetching the release notes…'))
      if (update.error || !update.available) {
        return h(
          'div',
          {},
          header(null),
          h('h1', {}, `${product} is up to date`),
          h('p', {}, update.error ? `Couldn't check for updates: ${update.error}` : `You're on the latest version.`),
          h('div', { class: 'links' }, textButton('Back', back)),
        )
      }
      const notes = update.releaseNotes
        .split('\n')
        .map((line) => line.replace(/^\s*[-*•]\s*/, '').trim())
        .filter(Boolean)
      const downloading = download.phase === 'downloading'
      const percent = download.progress != null ? Math.round(download.progress * 100) : null
      // Moonbase says the customer's licenses don't cover the new version, when the
      // notes load or when the download is refused: offer the upgrade instead.
      const notCovered = update.covered === false || download.phase === 'notCovered'
      // A trial that may not download it is offered the full version. Anyone else
      // Moonbase doesn't let download it, such as a customer outside the group a
      // release is limited to, is just told so.
      const unlock = !update.canDownload && !notCovered && !!state.license?.trial
      const access = download.releaseAccess || update.releaseAccess
      const upgradeUrl = download.upgradeUrl || update.upgradeUrl
      return h(
        'div',
        {},
        h('div', { style: 'margin-bottom:24px' }, lockup()),
        h('div', { style: 'margin-bottom:14px' }, h('span', { class: 'pill update' }, icon(paths.download, { size: 14, stroke: 'var(--accent)', width: 1.9 }), 'Update available')),
        h('h1', {}, `${product} ${update.latestVersion} is ready`),
        h(
          'p',
          { style: 'margin-bottom:20px' },
          "You're on ",
          h('span', { class: 'mono em' }, update.currentVersion || 'an earlier version'),
          '. The latest is ',
          h('span', { class: 'mono', style: 'color:#82cef1' }, update.latestVersion),
          update.canDownload && !notCovered
            ? ', included in your license.'
            : notCovered
              ? ", which your license doesn't cover."
              : unlock
                ? ", which your license doesn't include yet."
                : ", which isn't available for you to download.",
        ),
        notes.length > 0 &&
          h(
            'div',
            { class: 'card notes' },
            h('div', { class: 'heading' }, "What's new"),
            h('div', { class: 'items' }, notes.map((line) => h('div', { class: 'note' }, icon(paths.check, { stroke: 'var(--accent)', width: 2 }), h('span', {}, line)))),
          ),
        downloading && h('div', { class: 'bar accent', style: 'margin:0 0 14px' }, h('div', { style: `width:${percent ?? 8}%` })),
        download.phase === 'done'
          ? button('Show in folder', () => run(() => invoke('reveal_update')), { iconPath: paths.download })
          : notCovered
            ? upgradeUrl && button(upgradeLabel(access), () => run(() => invoke('upgrade_license')), { iconPath: paths.external })
            : update.canDownload
            ? button(downloading ? [spinner(), percent != null ? `Downloading… ${percent}%` : 'Downloading…'] : `Download ${update.latestVersion}`, () => run(() => invoke('download_update')), {
                iconPath: downloading ? null : paths.download,
                disabled: downloading,
              })
            : unlock && button('Unlock full version', () => run(() => invoke('activate', { offline: false })), { iconPath: paths.lock }),
        download.phase === 'done' && h('p', { style: 'margin-top:12px; text-align:center; font-size:12.5px' }, `Saved to ${download.path}`),
        notCovered && h('p', { class: 'fine-print' }, [`Version ${update.latestVersion} needs an upgrade.`, releaseAccessText(access)].filter(Boolean).join(' ')),
        errorLine(download.phase === 'failed' ? download.error : null),
        h(
          'div',
          { class: 'links' },
          textButton('Remind me later', back),
          textButton('Skip this version', async () => {
            await run(() => invoke('skip_update', { version: update.latestVersion }))
            back()
          }),
        ),
      )
    },
  }

  function planName(license) {
    if (license.trial) return `${license.product.name} trial`
    if (license.activationMethod === 'Offline') return `${license.product.name} (offline)`
    return license.product.name
  }

  function features() {
    if (!config.trialFeatures.length) return null
    return h(
      'div',
      { class: 'features' },
      config.trialFeatures.map((feature) =>
        h(
          'div',
          { class: `feature${feature.included ? '' : ' excluded'}` },
          icon(feature.included ? paths.check : paths.cross, { size: 17, stroke: feature.included ? '#34d27b' : '#5a6b82', width: 2 }),
          h('span', {}, feature.label),
        ),
      ),
    )
  }

  // -- Add-ons --------------------------------------------------------------------

  const cycleSuffix = { Daily: ' / day', Weekly: ' / week', Monthly: ' / month', Quarterly: ' / quarter', Yearly: ' / year' }

  function formatAmount(amount, currency) {
    const whole = Math.abs(amount - Math.round(amount)) < 0.005
    try {
      return new Intl.NumberFormat(undefined, { style: 'currency', currency, minimumFractionDigits: whole ? 0 : 2, maximumFractionDigits: whole ? 0 : 2 }).format(amount)
    } catch {
      return `${whole ? Math.round(amount) : amount.toFixed(2)} ${currency}`
    }
  }

  // What the add-on costs in the list's currency: its default price, or its first.
  // Empty when it isn't for sale in that currency.
  function priceText(entry, currency) {
    const pricing = entry.product.defaultVariation || entry.product.variations?.[0]
    const amount = currency && pricing?.price?.[currency]
    if (amount == null) return ''
    const recurring = pricing.recurrence?.type === 'Recurring'
    return formatAmount(amount, currency) + (recurring ? cycleSuffix[pricing.recurrence.cycleLength] || '' : '')
  }

  // The add-on as the license lists it, by its own part of its ID or its whole ID.
  function findSubProduct(license, id) {
    const prefix = `${license.product.id}.`
    const own = id.startsWith(prefix) && id.length > prefix.length ? id.slice(prefix.length) : id
    return (license.subProducts || []).find((entry) => entry.id === own) || null
  }

  // One add-on as its row shows it: where it stands here, and what to do about it.
  function describeAddOn(entry, license) {
    const price = priceText(entry, state.addOns.currency)
    const owns = entry.product.owned || entry.product.subscribed
    const forSale = !!entry.buyUrl
    const row = { id: entry.id, name: entry.product.name, status: '', tone: 'muted', primary: null, secondary: null }

    // The list says where each add-on stood when it was loaded. For one the
    // license lists, the license says where it stands on this computer now (on a
    // seat or a trial, revoked here, until when), and keeps saying it as time
    // passes and the license is refreshed: a trial that runs out with the screen
    // open shows as ended, and a seat revoked here since shows as revoked.
    const held = findSubProduct(license, entry.id)
    const active = entry.includedInLicense ? state.activeSubProducts.includes(entry.id) : entry.active
    const wasActive = held ? held.active : entry.active
    const revoked = held ? held.revoked : entry.revoked
    const ends = held ? held.expiresAt : entry.expiresAt || entry.trial?.expiresAt || null
    const lapsed = wasActive && !active && ends && daysUntil(ends) === 0
    // Whether a trial rather than a seat unlocks it here. A customer who buys an
    // add-on during its trial keeps the trial until a seat replaces it.
    const onTrial = held ? held.trial : entry.trial?.status === 'active'
    let trial = entry.trial?.status || 'unavailable'
    if (held?.trial) trial = active ? 'active' : 'expired'
    else if (trial === 'active' && lapsed) trial = 'expired'

    if (active && owns && !onTrial) {
      row.status = entry.includedInLicense ? 'Active on this computer' : entry.product.subscribed ? 'Subscribed' : 'Owned'
      row.tone = 'success'
    } else if (active) {
      const days = ends ? daysUntil(ends) : null
      row.status = join('Trial', days == null ? '' : days === 1 ? '1 day left' : `${days} days left`)
      row.tone = 'trial'
      // Bought during the trial: a seat of it is theirs to take.
      if (owns && entry.includedInLicense) row.primary = 'activate'
      else if (forSale) row.primary = 'buy'
    } else if (lapsed && owns && !onTrial) {
      // Owned until now: a subscription to it that ended.
      row.status = join('Expired', price)
      if (forSale) row.primary = 'buy'
    } else if (entry.includedInLicense && owns) {
      // Owned, but this computer holds no seat of it. Its seats are in use only when
      // neither the list nor the license knows of one free: either can be the older.
      const listKnows = !!entry.seats
      const licenseKnows = !!held?.seats
      const seatsInUse = (listKnows || licenseKnows) && (!listKnows || entry.seats.remaining <= 0) && (!licenseKnows || held.seats.remaining <= 0)
      if (revoked) {
        row.status = 'Deactivated on this computer'
        row.primary = 'activate'
      } else if (seatsInUse) {
        const total = listKnows ? entry.seats.total : held.seats.total
        row.status = total === 1 ? 'Its seat is in use on another computer' : `All ${total} seats are in use`
      } else {
        row.status = 'Not activated on this computer'
        row.primary = 'activate'
      }
    } else if (trial === 'available') {
      const days = entry.trial.numberOfDays
      row.status = join(days ? `${days}-day free trial` : 'Free trial', price)
      row.primary = 'tryIt'
      if (forSale) row.secondary = 'buy'
    } else if (trial === 'expired') {
      row.status = join('Trial ended', price)
      if (forSale) row.primary = 'buy'
    } else if (trial === 'unavailable' && entry.trial?.requiresNewsletterOptIn) {
      // Its trial is for newsletter subscribers, which the customer isn't yet.
      row.status = join('Join the newsletter to try it free', price)
      if (forSale) row.primary = 'buy'
    } else {
      row.status = join(price, entry.product.tagline || '')
      if (forSale) row.primary = 'buy'
    }
    return row
  }

  const addOnActions = {
    tryIt: { label: 'Try free', command: 'request_sub_product_trial' },
    activate: { label: 'Activate', command: 'activate_sub_product' },
    buy: { label: 'Buy', command: 'buy_sub_product' },
  }

  function addOnRow(row) {
    // While an add-on is being activated or tried its button spins, and the rest
    // wait their turn, as they do while the list is reloading.
    const busyId = state.addOns.busyId
    const anyBusy = !!busyId || state.addOns.phase === 'loading'
    const action = (kind, primary) => {
      const spins = primary && busyId === row.id
      return h(
        'button',
        { class: `btn small ${primary ? 'primary' : 'ghost'}`, type: 'button', disabled: anyBusy, onClick: () => run(() => invoke(addOnActions[kind].command, { id: row.id })) },
        spins ? spinner(14) : addOnActions[kind].label,
      )
    }
    return h(
      'div',
      { class: `card addon ${row.tone}` },
      h('div', { class: 'icon' }, icon(row.tone === 'success' ? paths.check : paths.sparkles, { size: 20, width: 1.7 })),
      h('div', { class: 'grow' }, h('div', { class: 'name' }, row.name), row.status && h('div', { class: 'status' }, row.status)),
      (row.primary || row.secondary) && h('div', { class: 'actions' }, row.secondary && action(row.secondary, false), row.primary && action(row.primary, true)),
    )
  }

  function openUpdate(fromDetails) {
    cameFromDetails = fromDetails
    go('update')
  }

  async function saveRequest() {
    offline.error = ''
    try {
      const saved = await invoke('save_machine_file')
      if (saved) offline.saved = saved
    } catch (error) {
      offline.error = error?.message ?? String(error)
    }
    render()
  }

  function pickFile(file) {
    if (!file) return
    const reader = new FileReader()
    reader.onload = () => {
      offline.file = { name: file.name, text: String(reader.result) }
      offline.error = ''
      render()
    }
    reader.onerror = () => {
      offline.error = "Couldn't read that file."
      render()
    }
    reader.readAsText(file)
  }

  async function activateOffline() {
    if (offline.busy) return
    if (!offline.file) {
      offline.error = `Add the license file from ${config.activationHost} to continue.`
      render()
      return
    }
    offline.busy = true
    offline.error = ''
    render()
    try {
      await invoke('activate_offline', { licenseFile: offline.file.text })
      offline.file = null
      offline.saved = null
      view = 'success'
    } catch {
      offline.error = state?.error?.message || "That license file isn't valid for this device."
    }
    offline.busy = false
    render()
  }

  // -- Structure, rendering and sizing ---------------------------------------------

  const screenEl = h('div')
  const closeButton = overlay ? h('button', { class: 'mb-close', type: 'button', 'aria-label': 'Close', onClick: () => invoke('close_ui') }, icon(paths.cross, { size: 18, width: 1.8 })) : null
  // The Moonbase brand mark, verbatim from the marketing site's
  // images/logos/moonbase.svg (as in moonbase-cpp's JUCE module), in one colour.
  const moonbaseMark = svg(
    '<svg width="14" height="14" viewBox="0 0 512 512" fill="currentColor" aria-hidden="true"><path d="m493.17795,174.6135c6.257,75.3 -30.577,150.948 -100.579,190.758c-66.292,37.697 -145.001,33.405 -205.402,-4.415c0.409,-1.248 0.638,-2.577 0.638,-3.96c0,-7.026 -5.717,-12.743 -12.743,-12.743c-2.876,0 -5.524,0.969 -7.659,2.582c-18.813,-15.259 -35.159,-34.207 -47.856,-56.534c-54.663,-96.122 -21.054,-218.358 75.069,-273.021a201.218,201.218 0 0 1 33.302,-15.124c-32.428,3.667 -64.663,13.809 -94.827,30.962c-104.526,59.443 -149.511,183.241 -113.473,293.249c-3.376,3.057 -5.513,7.459 -5.513,12.362c0,9.207 7.491,16.698 16.698,16.698c0.136,0 0.267,-0.017 0.401,-0.02a252.205,252.205 0 0 0 8.683,16.691c67.869,119.344 219.635,161.073 338.98,93.204a250.416,250.416 0 0 0 32.893,-22.333c1.045,0.331 2.157,0.513 3.311,0.513c6.047,0 10.968,-4.921 10.968,-10.968c0,-0.605 -0.063,-1.196 -0.157,-1.775c69.545,-64.988 96.008,-165.054 67.266,-256.126m-446.526,25.112a5.071,5.071 0 0 1 -5.066,-5.066a5.072,5.072 0 0 1 5.066,-5.067a5.073,5.073 0 0 1 5.066,5.067a5.072,5.072 0 0 1 -5.066,5.066m-15.819,153.944c-8.238,0 -14.94,-6.702 -14.94,-14.94c0,-8.239 6.702,-14.941 14.94,-14.941c8.239,0 14.941,6.702 14.941,14.941c-0.001,8.238 -6.703,14.94 -14.941,14.94m144.258,-7.659c6.057,0 10.985,4.928 10.985,10.986c0,6.058 -4.928,10.985 -10.985,10.985c-6.058,0 -10.986,-4.928 -10.986,-10.985c0.001,-6.059 4.928,-10.986 10.986,-10.986m70.829,133.004c-5.079,0 -9.21,-4.132 -9.21,-9.21c0,-5.079 4.131,-9.21 9.21,-9.21c5.078,0 9.209,4.131 9.209,9.21c0,5.078 -4.13,9.21 -9.209,9.21m169.18,-37.289c-5.078,0 -9.209,-4.132 -9.209,-9.21s4.131,-9.209 9.209,-9.209s9.21,4.131 9.21,9.209s-4.132,9.21 -9.21,9.21"></path><path d="m144.78195,275.9675c46.746,82.201 151.278,110.943 233.48,64.197c59.864,-34.044 91.363,-98.736 86.013,-163.13a170.094,170.094 0 0 0 -5.515,-31.052c-23.045,0.567 -63.864,10.137 -104.194,61.671c-52.045,66.5 -186.285,49.153 -228.855,17.372c3.503,17.443 9.796,34.632 19.071,50.942m233.513,-65.677a8.663,8.663 0 1 1 -8.663,8.663a8.662,8.662 0 0 1 8.663,-8.663m-37.006,62.618c7.02,0 12.712,5.691 12.712,12.712s-5.692,12.712 -12.712,12.712c-7.021,0 -12.712,-5.691 -12.712,-12.712s5.691,-12.712 12.712,-12.712m-102.544,12.712a6.78,6.78 0 1 1 0,13.56a6.78,6.78 0 0 1 0,-13.56"></path><ellipse cx="251.33138" cy="152.05851" rx="7.3075" ry="7.3075"></ellipse><ellipse cx="204.89499" cy="210.31151" rx="5.35076" ry="5.35076"></ellipse><ellipse cx="302.28597" cy="200.15625" rx="12.09273" ry="12.09273"></ellipse></svg>',
  )
  const badge =
    config.showBadge &&
    h(
      'footer',
      { class: 'badge' },
      icon(paths.lock, { size: 14, width: 1.5 }),
      h('span', {}, 'Licensing secured by'),
      h('button', { class: 'moonbase-link', type: 'button', onClick: () => invoke('open_link', { url: MOONBASE_SITE }).catch(() => {}) }, h('span', { class: 'moon' }, moonbaseMark), h('span', { class: 'wordmark' }, 'moonbase')),
    )
  const panel = h('main', { class: 'panel' }, closeButton, screenEl, badge)

  root.classList.add('mb-root')
  if (overlay) root.classList.add('mb-overlay-root')
  if (!overlay && config.os === 'macos') root.classList.add('mac')
  if (config.reduceMotion) root.classList.add('reduce-motion')
  root.style.setProperty('--accent', config.accent)
  root.replaceChildren(panel)

  let lastScreen = null
  let reportedHeight = 0

  // Closing is always possible in a window, which never locks the app. An overlay
  // stays up while there is no license: that is the lock.
  function canClose() {
    if (!state || state.status === 'activating' || state.status === 'loading') return false
    return !overlay || state.licensed
  }

  function render() {
    if (!state) return
    const name = screenName()
    const node = (screens[name] || screens.loading)()
    // Animate only real screen changes, not every state push.
    const wrapper = h('div', { class: name === lastScreen ? '' : 'screen' }, node)
    screenEl.replaceChildren(wrapper)
    lastScreen = name
    if (closeButton) closeButton.hidden = !canClose()
    // Measured straight away rather than on the next frame: WebKit pauses frames
    // for a window that is covered, and the window should fit its screen anyway.
    reportHeight()
  }

  function reportHeight() {
    if (!onHeight) return
    const height = Math.ceil(panel.getBoundingClientRect().height)
    if (height === reportedHeight) return
    reportedHeight = height
    onHeight(height)
  }

  function show(screen) {
    if (screen === 'update') return openUpdate(false)
    if (screen === 'license') screen = 'details'
    view = screen === 'welcome' || !screen ? null : screen
    if ((view === 'details' || view === 'addons') && !state?.license) view = null
    if (view === 'addons') loadAddOns()
    render()
  }

  function onState(next) {
    const previous = state
    state = next
    // A completed activation gets its own moment. Cancelling one while a trial runs
    // also leaves a license, but the same one, so compare activations.
    const activated = previous?.status === 'activating' && !next.activation && next.license && next.license.activationId !== previous.license?.activationId
    if (activated) view = 'success'
    // Screens about a license make no sense without one.
    if (['details', 'addons', 'update'].includes(view) && !next.license) view = null
    // Activating supersedes whatever the user was looking at, and so does a license
    // turning out not to cover this version.
    if (next.status === 'activating') view = null
    if (next.status === 'notCovered' && previous?.status !== 'notCovered') view = null
    if (view === 'success' && !next.license) view = null
    // The add-ons were listed for a license that has since been replaced.
    if (view === 'addons' && next.license && next.addOns.phase === 'idle' && previous?.addOns?.phase !== 'idle') loadAddOns()
    render()
  }

  // Start with the state handed over, or ask for it.
  async function start(initial, screen) {
    try {
      device = await invoke('device')
    } catch {
      device = null
    }
    onState(initial || (await invoke('state')))
    if (screen) show(screen)
  }

  return {
    update: onState,
    show,
    start,
    canClose,
    focus() {
      panel.querySelector('button, [href], input, label')?.focus({ preventScroll: true })
    },
    download(next) {
      download = next
      render()
    },
  }
}
