// The plugin's frontend API for bundled apps. Apps that set "withGlobalTauri"
// get the same functions as window.__TAURI__.moonbase.
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export const STATE_CHANGED = 'moonbase://state-changed'

const call = (command, args = {}) => invoke(`plugin:moonbase|${command}`, args)

export const state = () => call('state')
export const activate = ({ offline = false } = {}) => call('activate', { offline })
export const cancelActivation = () => call('cancel_activation')
export const refresh = ({ force = true } = {}) => call('refresh', { force })
export const deactivate = () => call('deactivate')
export const forget = () => call('forget')
export const deviceToken = () => call('device_token')
export const activateOffline = (licenseFile) => call('activate_offline', { licenseFile })
export const dismissError = () => call('dismiss_error')
export const device = () => call('device')
export const checkUpdate = () => call('check_update')
export const updateDownloadUrl = () => call('update_download_url')
export const present = ({ screen = null } = {}) => call('present', { screen })
export const upgradeLicense = () => call('upgrade_license')
export const loadAddOns = () => call('load_add_ons')
export const refreshAddOns = () => call('refresh_add_ons')
export const activateSubProduct = (id) => call('activate_sub_product', { id })
export const requestSubProductTrial = (id) => call('request_sub_product_trial', { id })
export const buySubProduct = (id) => call('buy_sub_product', { id })

// Whether the state says to unlock an add-on: by its own part of its product ID
// ("drum-kit") or its whole ID ("my-app.drum-kit").
export function hasSubProduct(state, id) {
  if (!state?.license) return false
  const prefix = `${state.license.product.id}.`
  const own = id.startsWith(prefix) && id.length > prefix.length ? id.slice(prefix.length) : id
  return state.activeSubProducts.includes(own)
}
export const onStateChanged = (handler) => listen(STATE_CHANGED, (event) => handler(event.payload))
