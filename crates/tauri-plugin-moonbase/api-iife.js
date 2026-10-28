// window.__TAURI__.moonbase: the plugin's frontend API for apps that set
// "withGlobalTauri": true. Tauri injects this file after its own global API.
// Bundled apps can import the same functions from guest-js/ instead.
if ('__TAURI__' in window) {
  var __TAURI_PLUGIN_MOONBASE__ = (function () {
    'use strict'

    var STATE_CHANGED = 'moonbase://state-changed'

    function invoke(command, args) {
      return window.__TAURI_INTERNALS__.invoke('plugin:moonbase|' + command, args || {})
    }

    // Whether the state says to unlock an add-on: by its own part of its product ID
    // ("drum-kit") or its whole ID ("my-app.drum-kit").
    function hasSubProduct(state, id) {
      if (!state || !state.license) return false
      var prefix = state.license.product.id + '.'
      var own = id.indexOf(prefix) === 0 && id.length > prefix.length ? id.slice(prefix.length) : id
      return state.activeSubProducts.indexOf(own) !== -1
    }

    return {
      STATE_CHANGED: STATE_CHANGED,
      state: function () { return invoke('state') },
      activate: function (options) { return invoke('activate', { offline: Boolean(options && options.offline) }) },
      cancelActivation: function () { return invoke('cancel_activation') },
      refresh: function (options) { return invoke('refresh', { force: !(options && options.force === false) }) },
      deactivate: function () { return invoke('deactivate') },
      forget: function () { return invoke('forget') },
      deviceToken: function () { return invoke('device_token') },
      activateOffline: function (licenseFile) { return invoke('activate_offline', { licenseFile: licenseFile }) },
      dismissError: function () { return invoke('dismiss_error') },
      device: function () { return invoke('device') },
      checkUpdate: function () { return invoke('check_update') },
      updateDownloadUrl: function () { return invoke('update_download_url') },
      present: function (options) { return invoke('present', { screen: (options && options.screen) || null }) },
      upgradeLicense: function () { return invoke('upgrade_license') },
      hasSubProduct: function (state, id) { return hasSubProduct(state, id) },
      loadAddOns: function () { return invoke('load_add_ons') },
      refreshAddOns: function () { return invoke('refresh_add_ons') },
      activateSubProduct: function (id) { return invoke('activate_sub_product', { id: id }) },
      requestSubProductTrial: function (id) { return invoke('request_sub_product_trial', { id: id }) },
      buySubProduct: function (id) { return invoke('buy_sub_product', { id: id }) },
      onStateChanged: function (handler) {
        return window.__TAURI__.event.listen(STATE_CHANGED, function (event) { handler(event.payload) })
      },
    }
  })()
  Object.defineProperty(window.__TAURI__, 'moonbase', { value: __TAURI_PLUGIN_MOONBASE__ })
}
