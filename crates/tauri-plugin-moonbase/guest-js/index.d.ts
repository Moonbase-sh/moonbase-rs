import type { UnlistenFn } from '@tauri-apps/api/event'

export declare const STATE_CHANGED: 'moonbase://state-changed'

/** Where the license stands. Drive your UI off this. */
export type Status = 'loading' | 'unlicensed' | 'activating' | 'licensed' | 'trial' | 'trialExpired' | 'notCovered'

/** Why the app lost its license. */
export type LockReason = 'deactivated' | 'expired' | 'invalid' | 'unverified' | 'notCovered'

export type ErrorType =
  | 'apiError'
  | 'licenseInvalid'
  | 'licenseExpired'
  | 'storageError'
  | 'configurationError'
  | 'operationNotSupported'
  | 'licenseDeviceMismatch'
  | 'deviceIdentityUnavailable'
  | 'activationRequestEnded'
  | 'storeClosed'
  | 'noEligibleLicense'
  | 'releaseNotCovered'

/** How a rejected command fails. */
export interface MoonbaseError {
  kind: ErrorType | 'notLicensed' | 'busy' | 'internal' | 'unknown'
  message: string
}

/** Which versions of the app a license covers, when it doesn't cover every one. */
export interface ReleaseAccess {
  /** ISO-8601 UTC. Releases published until then are covered, and every version while it has not passed. */
  updatesUntil?: string
  /** Every version whose major is at most this is covered. */
  upToMajor?: number
  /** Once updates have ended, the last version published before they did. */
  upToVersion?: string
}

/** An add-on the license includes, as the license reports it for this device. */
export interface SubProduct {
  id: string
  active: boolean
  trial: boolean
  revoked: boolean
  expiresAt?: string
  seats?: { total: number; used: number; remaining: number }
}

/** The validated license, as the SDK serializes it, minus the raw token. */
export interface License {
  id: string
  activationId: string
  trial: boolean
  activationMethod: 'Online' | 'Offline'
  product: { id: string; name: string; currentReleaseVersion?: string; properties: Record<string, unknown> }
  issuedTo: { id: string; name: string; email: string; properties: Record<string, unknown> }
  /** ISO-8601 UTC. */
  issuedAt: string
  expiresAt?: string
  validatedAt: string
  ownedSubProductIds: string[]
  /** The add-ons the license includes, with whether each may be unlocked here. Use `activeSubProducts` to gate. */
  subProducts: SubProduct[]
  subscriptionId?: string
  seatCount?: number
  seatsUsed?: number
  /** Present only for a license that covers some versions of the app. */
  releaseAccess?: ReleaseAccess
  properties: Record<string, unknown>
  /** Whole days until `expiresAt`, rounded up; null when the license never expires. */
  daysRemaining: number | null
}

export interface SubProductPricing {
  id: string
  name: string
  originalPrice: Record<string, number>
  price: Record<string, number>
  hasDiscount: boolean
  recurrence: { type: 'OneOff' | 'Recurring' | string; cycleLength: string | null } | null
}

/** One of the product's add-ons, as Moonbase lists it for this device. */
export interface AddOn {
  id: string
  product: {
    id: string
    name: string
    tagline: string | null
    description: string | null
    website: string | null
    iconUrl: string | null
    owned: boolean
    subscribed: boolean
    defaultVariation: SubProductPricing | null
    variations: SubProductPricing[]
  }
  includedInLicense: boolean
  active: boolean
  revoked: boolean
  expiresAt: string | null
  seats: { total: number; used: number; remaining: number } | null
  trial: {
    status: 'unavailable' | 'available' | 'active' | 'expired'
    numberOfDays: number | null
    expiresAt: string | null
    requiresNewsletterOptIn: boolean
  } | null
  buyUrl: string | null
}

export interface LicenseState {
  status: Status
  /** Is there a valid license right now? Gate features on this. */
  licensed: boolean
  license: License | null
  expiredTrial: License | null
  /** A license that doesn't cover this version of the app: kept, but the app is locked. */
  notCovered: {
    license: License
    /** The version it doesn't cover, as MAJOR.MINOR.PATCH. */
    appVersion: string
    releaseAccess: ReleaseAccess | null
    /** Where to buy the upgrade; without one, `upgradeLicense()` starts an online activation. */
    upgradeUrl: string | null
  } | null
  lockReason: LockReason | null
  activation: {
    browserUrl: string | null
    method: 'Online' | 'Offline'
    /** The browser couldn't be opened: show the link, or a way to copy it. */
    browserOpenFailed: boolean
  } | null
  /** A deactivation is in flight. */
  busy: boolean
  /** A re-check of the license is waiting for Moonbase. */
  refreshing: boolean
  /** The last failed operation: `message` is for the customer, `detail` for logs. */
  error: { message: string; detail: string; kind: ErrorType | null } | null
  update: { currentVersion: string; latestVersion: string } | null
  /** The add-ons to unlock on this device right now, by their own part of their product ID. */
  activeSubProducts: string[]
  /** The add-on list, once `loadAddOns()` has run. */
  addOns: {
    phase: 'idle' | 'loading' | 'ready'
    currency: string | null
    addOns: AddOn[]
    error: string | null
    busyId: string | null
    actionError: string | null
  }
}

export interface DeviceInfo {
  name: string
  platform: 'Mac' | 'Windows' | 'Linux' | 'Unknown'
  description: {
    deviceId: string
    version: number
    platform: string
    source: 'identity' | 'deviceName' | 'scoped'
    paramNames: string[]
  } | null
  error: string | null
}

export interface UpdateInfo {
  available: boolean
  currentVersion: string
  latestVersion: string | null
  releaseNotes: string
  hasDownloads: boolean
  canDownload: boolean
  /** False when the customer's licenses don't cover the new version: offer `upgradeLicense()`. */
  covered: boolean
  releaseAccess: ReleaseAccess | null
  upgradeUrl: string | null
}

export declare function state(): Promise<LicenseState>
/** Start a browser activation; resolves once the request exists, then polls in the background. */
export declare function activate(options?: { offline?: boolean }): Promise<LicenseState>
export declare function cancelActivation(): Promise<LicenseState>
/** Re-check the license online. `force` (default true) skips the throttle. */
export declare function refresh(options?: { force?: boolean }): Promise<LicenseState>
export declare function deactivate(): Promise<LicenseState>
export declare function forget(): Promise<LicenseState>
/** The machine file for offline activation, to save as a `.dt` file. */
export declare function deviceToken(): Promise<string>
/** Activate from the contents of the license file the customer downloaded. */
export declare function activateOffline(licenseFile: string): Promise<LicenseState>
export declare function dismissError(): Promise<LicenseState>
export declare function device(): Promise<DeviceInfo>
export declare function checkUpdate(): Promise<UpdateInfo>
export declare function updateDownloadUrl(): Promise<{ url: string; filename: string }>
/** Show the built-in activation window, optionally on a given screen. */
export declare function present(options?: { screen?: 'license' | 'update' | 'offline' | 'addons' }): Promise<void>
/** Open the upgrade for a license that doesn't cover this version, or for a new version it doesn't cover. */
export declare function upgradeLicense(): Promise<LicenseState>
/** Whether `state` says to unlock an add-on, by its own part of its product ID or its whole ID. */
export declare function hasSubProduct(state: LicenseState, id: string): boolean
/** List the product's add-ons into `state.addOns`. Can take a seat of one bought since the last check. */
export declare function loadAddOns(): Promise<LicenseState>
/** Re-check the license so an add-on bought since takes its seat, then list the add-ons again. */
export declare function refreshAddOns(): Promise<LicenseState>
/** Take a seat of an add-on the customer owns, on this device. */
export declare function activateSubProduct(id: string): Promise<LicenseState>
/** Start an add-on's trial on this device. */
export declare function requestSubProductTrial(id: string): Promise<LicenseState>
/** Open an add-on's buy link; the add-ons refresh when the app comes back to the front. */
export declare function buySubProduct(id: string): Promise<void>
export declare function onStateChanged(handler: (state: LicenseState) => void): Promise<UnlistenFn>
