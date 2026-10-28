# Core SDK: the `moonbase` crate

The library at [`crates/moonbase/`](../crates/moonbase/). It covers activation
requests, polling for fulfilled activations, local RS256 JWT validation, the
cross-SDK device fingerprint, offline activation, revocation, add-ons, licenses that
cover only some versions, update checks and overridable license storage. No UI and no
framework.

Building a Tauri app? The [plugin](tauri.md) wraps this same core in an activation
controller your frontend drives.

## Requirements

- Rust 1.89 or newer (edition 2024)
- Windows, macOS or Linux for hardware identity; iOS and Android get a
  [scoped identity](device-identity.md#ios-and-android)
- No system libraries: crypto is `ring`, and the bundled HTTP transport is `ureq` with
  `rustls`

## Installation

```toml
[dependencies]
moonbase = { git = "https://github.com/Moonbase-sh/moonbase-rs", tag = "v0.1.0" }
```

| Feature | Default | Purpose |
| --- | --- | --- |
| `ureq-transport` | on | The bundled blocking HTTP transport. Turn it off (`default-features = false`) to drop `ureq` and `rustls`, and supply your own [`HttpTransport`](#custom-storage-resolvers-and-transports). |

## The API is blocking

Every call that touches the network or the disk blocks the calling thread, as the C++
SDK does. In an async app, run them on a blocking pool:

```rust
let licensing = Arc::new(licensing);
let license = tokio::task::spawn_blocking({
    let licensing = licensing.clone();
    move || licensing.validate_token_online(&token)
})
.await??;
```

`Licensing` is `Send + Sync`; share one instance behind an `Arc`. There is deliberately
no async wrapper: it would tie the crate to one runtime for what is a few lines of
`spawn_blocking`, and the calls are seconds apart at most, so a blocking pool thread is
the right home for them.

## Basic usage

```rust
use std::time::Duration;
use moonbase::{FileLicenseStore, Licensing, LicensingOptions};

let options = LicensingOptions::new("https://demo.moonbase.sh", "demo-app", PUBLIC_KEY_PEM)
    .account_id("tenant-id")                 // optional issuer check
    .application_version("1.4.0")            // reported to Moonbase, compared with releases
    .client_info("my-framework/1.2.0")       // optional, see below
    .http_timeouts(Duration::from_secs(10), Duration::from_secs(30));

let licensing = Licensing::builder(options)
    .store(FileLicenseStore::new(data_dir.join("license.mb")))
    .build()?;
```

The public key is the product's RSA key from the Moonbase dashboard. PEM (`BEGIN
PUBLIC KEY` or `BEGIN RSA PUBLIC KEY`) and bare base64 DER are accepted, including
shapes a trip through a config file leaves behind: CRLF line endings, indentation, or
the whole key on one line. A key that cannot be parsed fails `build()` with
`Error::Configuration`.

`client_info` identifies a higher-level integration built on the SDK. It is appended
to the `User-Agent` after `moonbase-rs/<version>`, so requests report every layer,
outermost last:

```
User-Agent: moonbase-rs/0.1.0 tauri-plugin-moonbase/0.1.0 (Tauri 2.12.4; macos) my-app/2.0
```

Control characters are stripped and the value is capped at 256 characters, so a stray
newline can never inject a header.

### Browser activation

```rust
use moonbase::ActivationMethod;

let request = licensing.request_activation(ActivationMethod::Online)?;
println!("Open: {}", request.browser_url);

let license = loop {
    std::thread::sleep(Duration::from_secs(2));
    match licensing.complete_activation(&request) {
        Ok(Some(license)) => break license,                   // stored too
        Ok(None) => continue,                                 // still pending
        Err(moonbase::Error::Api(_)) => continue,             // offline, busy: retry
        Err(error) => return Err(error.into()),               // ended or refused: start over
    }
};
```

`complete_activation` polls, and stores the license once it lands, under the store's
lock as a re-check stores its answer. `complete_activation_with(request,
should_persist)` asks a predicate before the write, so an activation the user cancelled
meanwhile isn't stored. `get_requested_activation` only polls, for an app that stores
the license its own way.

Poll every 2 to 3 seconds. The SDK sends at most one poll per second for each request
and answers anything faster with `Ok(None)` without contacting the API, so a fast UI
timer cannot flood the server. Each request has its own floor, so polling several in
turn never holds one back. When the server rate limits with a `Retry-After`, every poll
answers `Ok(None)` until it has passed (at most 5 minutes).

A 404 from the poll also reads as pending: the API reads requests with eventual
consistency, and a proxy's 404 cannot be told apart from Moonbase's. A 400 means the
request will never complete (it expired after an hour, was cancelled or refused), and
fails with `Error::ActivationRequestEnded`.

Pass `ActivationMethod::Offline` to have the same browser flow mint an *offline*
license: validated locally for good, never revocable. Use it when the machine has
network now but will not later. The product must allow offline activations, or the
request fails with `Error::LicenseInvalid("Product does not allow offline activations")`.

### Startup validation

```rust
match licensing.load_and_validate()? {
    Some(license) => unlock(license),
    None => offer_activation(),
}
```

`load_and_validate` loads the stored license and checks it with
`validate_token_online(token)`, which validates locally (signature, audience, issuer,
device, expiry), then re-validates against the API when it is due, and persists the
refreshed license. A refusal leaves the stored license in place, so the app decides what
it means: see the example on `load_and_validate`, and
[Licenses that cover some versions](#licenses-that-cover-some-versions) for the one
refusal not to delete it for. Two options govern the check:

- `online_validation_min_interval` (default 5 minutes): validated more recently than
  this, the API is skipped. Cheap enough to call on every window or document open.
- `online_validation_grace_period` (default 7 days): the longest a license may go
  without a successful online check. Within it, a failure that is not a verdict falls
  back to the local result; beyond it, the failure propagates.

A verdict (`Error::is_license_rejection()`: invalid, bound to another device, not for
this version, or expired) always propagates. Offline licenses never contact the API.

Concurrent callers take turns. Within the process a mutex serializes them; across
processes the store's lock does, and a later caller reloads the store under the lock,
sees the license the earlier one just refreshed, and skips the API. Ten windows
opening at once make one request. `validate_token_online_with(token, should_persist)`
asks a predicate, under the lock, before writing, so a caller can cancel the write if
the user pressed "Deactivate" while the check was in flight.

## Licenses that cover some versions

A license bought outright can cover only some versions of your app: the releases
published while its updates lasted ("12 months of updates"), or every version up to a
major ("versions up to 2.x"). It never expires because of it; it just doesn't cover the
versions after that. `license.release_access` says what it covers, and is `None` for a
license that covers every version:

- `updates_until`: releases published until then are covered, and every version while
  it has not passed.
- `up_to_major`: every version whose major is at most this is covered.
- `up_to_version`: once updates have ended, the last version published before they did,
  covered along with every older one. `None` while updates last, and when nothing was
  published before they ended (then every version is covered).

The limits add up: a license with both covers a version that either one covers.

Set `application_version` to the version that has to be covered. A license that doesn't
cover it fails with `Error::ReleaseNotCovered`. For an online license,
`validate_token_online` leaves that to Moonbase when it asks it, since Moonbase knows of
an upgrade bought since the token was issued. The device judges it by its own clock
when Moonbase isn't asked: for an offline license, within
`online_validation_min_interval`, and when Moonbase can't be reached within the grace
period. `validate_token_local` and `read_offline_license` check it too, but
`get_requested_activation` does not: Moonbase activates only a license that covers the
version the request named, and the seat is taken by then. Set
`enforce_release_access(false)` to turn the check on the device off; Moonbase checks
either way. A version that can't be read is not checked.
`moonbase::version::parse_app_version` reads one the way Moonbase does: an optional
leading `v`, then two to four numbers (a fourth is ignored), with any `-beta` or
`+build` suffix dropped, so `3.0.0-beta.2` is judged as 3.0.0.

An offline license is never checked online, so it is judged by what its token said when
the license file was made. A version limit ("up to 2.x") holds for good, and so does a
time limit in a file made after updates ended, which names the last version covered. A
file made while updates last can't name it, so once they end it keeps covering later
versions until the customer downloads a new license file, which does.

`ReleaseNotCovered` is an `is_license_invalid()` error, so code that refuses to run on
an invalid license refuses here too. But don't delete the stored license for it: it
still covers the versions it did, so the customer can go back to one of them, or
upgrade and keep going. Match it first and offer the upgrade:

```rust
use moonbase::Error;

match licensing.load_and_validate() {
    Ok(Some(license)) => unlock(license),
    Ok(None) => offer_activation(),
    Err(Error::ReleaseNotCovered(refusal)) => {
        // Keep the license. The check already stored the copy Moonbase sent along
        // with the refusal (refusal.license()).
        offer_upgrade(refusal.app_version(), refusal.release_access(), refusal.upgrade_url());
    }
    Err(error) if error.is_license_invalid() => {
        licensing.store().delete_local_license()?; // revoked, or not for this device
    }
    Err(error) => return Err(error),
}
```

`upgrade_url()` is where the customer buys the upgrade or renewal. Only Moonbase knows
it, so it is `None` when the device refused, and when the store sells none. Moonbase's
browser activation page offers the upgrade as well, to a signed-in customer, and then
activates the device, so starting a `request_activation` works without a link.

To check another version, such as an update before offering to download it, use
`license.covers_version(version)`, which applies the same rule as Moonbase and every
other Moonbase SDK. The vendored
[`release-access-vectors.json`](../crates/moonbase/tests/vectors/release-access-vectors.json)
pins it down case by case. `revoke_activation` and the add-on calls never judge the
version, so a license can always be deactivated and its add-ons managed.

## Revoking an activation

```rust
licensing.revoke_activation(&license.token)?; // frees the seat and clears the store
```

Only online-activated paid licenses can be revoked; offline and trial licenses fail
with `Error::OperationNotSupported` without contacting the API. A token that has aged
out locally can still be revoked, since its seat stays allocated until it is. There is
no grace period: revocation is one-shot. Revoking frees the device's add-on seats too.

## Add-ons

An add-on (a sub-product, with a product ID like `my-app.drum-kit`) that the merchant
includes in its main product's license travels in the main license, so one validation
answers for both. When a device activates or validates the main product, it takes a
seat of each such add-on the customer owns, and the license lists them in
`sub_products`:

```rust
if license.has_sub_product("drum-kit") {
    // unlock the drum kit
}
```

`has_sub_product` takes the add-on's own part of its ID or the whole ID. It is true
while the license lists the add-on as active and it has not expired, which matters for
an offline license, whose add-on trial can run out long before the license is checked
again. An add-on the customer owns, but with every seat taken on other devices, is
listed with `active` false. One bought after the device activated shows up at the next
online validation: `validate_token_online` asks the server at most once per
`online_validation_min_interval`, and `client().validate_token_online` asks right away.
`find_sub_product` returns the entry itself, with its `trial`, `revoked`, `expires_at`
and `seats`. `owned_sub_product_ids` still lists every add-on the customer owns,
included in the license or not, without saying whether it may be used on this device.

If the customer revokes an add-on's seat on this device (from their account, or the
merchant does it for them), validating does not take it back: the license lists it with
`revoked` set until the app activates it again. To take seats yourself instead of on
every activation and validation, turn off `auto_activate_sub_products`. Either way, take
a seat with:

```rust
let updated = licensing.activate_sub_product(&license.token, "drum-kit")?;
```

To let a customer try an add-on that offers trials, start its trial from the main
license:

```rust
let updated = licensing.request_sub_product_trial(&license.token, "drum-kit")?;
```

Both return the main license again, now reporting the add-on, and store it as
`validate_token_online` stores its own, holding the store's lock from the request until
the write (the `_with` variants take a `should_persist` predicate, as
`validate_token_online_with` does). The license must be a full license, not a trial: a
trial token fails with `Error::OperationNotSupported` without contacting the API. A
trial also needs an online license. A customer who owns the add-on gets a seat of it
instead of a trial, and a device gets one trial of each add-on: asking again returns
the license with the trial it had, expired or not.

To show the customer what else there is, for example on an upgrade screen, list the
product's add-ons from the license (or trial) the device holds. Each comes with its
prices when it is for sale, whether the customer owns it, whether to unlock it here,
and its trial:

```rust
use moonbase::SubProductTrialStatus;

let catalog = licensing.list_sub_products(&license.token)?;
for add_on in &catalog.sub_products {
    if add_on.active {
        // owned, or trialing, on this device
    } else if add_on.trial.as_ref().is_some_and(|trial| trial.status == SubProductTrialStatus::Available) {
        // offer "Try it", which calls request_sub_product_trial
    } else if let Some(url) = &add_on.buy_url {
        // offer "Buy" for add_on.product.default_variation's price[&catalog.currency]
    }
}
```

It lists only the add-ons the customer can do something with: ones they own or
subscribe to, ones on sale, and ones whose trial can be started or is running on this
device. A trial `status` is `Available` when `request_sub_product_trial` can start one
now, `Active` or `Expired` for the trial the device had, and `Unavailable` otherwise,
including for a status this release does not know. Listing changes nothing, and is not
stored.

The add-on calls refuse in two ways, and only one of them is about the license:

| Error | Meaning |
| --- | --- |
| `NoEligibleLicense` | The customer has no seat of the add-on to give: they don't own it, or every seat is taken. The license is fine; offer to buy the add-on or free a seat |
| `OperationNotSupported` | The add-on can't be activated or tried from this license: it is not included in the license, a trial was asked for from an offline license, or the token is a trial. The license is fine |
| `LicenseInvalid`, `LicenseExpired` | The license itself is no longer good, as from `validate_token_online` |

## Errors

Everything fails with `moonbase::Error`. `error.error_type()` gives a plain
`ErrorType` value to log or persist. For calls to the API, the variant says whether
trying again can help:

| Variant | Meaning | What to do |
| --- | --- | --- |
| `Api` | No definitive answer: unreachable (`status_code()` 0), rate limited (429), failing (5xx), or something other than the API answered, such as a captive portal | Retry later; `retry_after()` says when, if the server did |
| `LicenseInvalid` | Refused for good: bad signature or product, revoked | Lock, offer to activate again |
| `DeviceMismatch` | Genuine, but bound to another device (the message says whether a fingerprint version differs) | Lock, offer to activate again |
| `ReleaseNotCovered` | Valid, but doesn't cover `application_version` | Lock, keep the license, and [offer the upgrade](#licenses-that-cover-some-versions) |
| `LicenseExpired` | A trial ended, or a subscription lapsed | Lock, offer to buy |
| `StoreClosed` | The merchant closed their account; no verdict on the license | Treat it like being offline |
| `ActivationRequestEnded` | A browser activation can never complete | Start a new request |
| `NoEligibleLicense` | No seat of an add-on to give; no verdict on the license | Offer to buy the add-on or free a seat |
| `InsufficientDeviceIdentity` | This machine has no readable hardware identity | See [device identity](device-identity.md) |
| `Configuration` | Bad options, an unparseable key, an empty device id | Fix the setup |
| `Storage` | Reading or writing the stored license, or its lock, failed; `io_error()` (and `source()`) is the I/O failure behind it | |
| `OperationNotSupported` | The call can't apply to this license: revoking an offline or trial license, an add-on it can't take | |

`is_license_invalid()` is true for `LicenseInvalid`, `DeviceMismatch` and
`ReleaseNotCovered`, so a check for "refused" covers all three; the grace period relies
on that. Match `ReleaseNotCovered` first to keep the license.

How each call maps what the API returns:

| Call | Response | Result |
| --- | --- | --- |
| `request_activation` | 400 | `LicenseInvalid` with the server's reason |
| `get_requested_activation` | 204 or 404 | `Ok(None)` |
| | 400 | `ActivationRequestEnded` (`LicenseExpired` only for an explicit `LicenseExpired`) |
| `validate_token_online`, `revoke_activation` | 400 | `LicenseInvalid`, or `LicenseExpired` when `errorType` is `LicenseExpired`, or `ReleaseNotCovered` when it is `ReleaseNotCovered` |
| | 403 or 404 | `Api`, covered by the grace period |
| `activate_sub_product`, `request_sub_product_trial`, `list_sub_products` | 400 with `errorType` `LicenseInvalid`, `LicenseRevoked` or `LicenseActivationRevoked` | `LicenseInvalid` (`LicenseExpired` for `LicenseExpired`) |
| | 400 with `errorType: NoEligibleLicense` | `NoEligibleLicense` |
| | any other 400 | `OperationNotSupported` with the server's reason |
| | 404 | `Api` |
| inventory calls | 403 with `errorType: ReleaseNotCovered` | `ReleaseNotCovered` with the upgrade link: the customer's licenses don't cover the release |
| | any other failure but a closed store | `Api` with the status |
| any | 410 with `errorType: StoreClosed` | `StoreClosed`, read before the status |
| any | 429, 5xx, a transport failure | `Api` |

Only an answer that can only have come from Moonbase is definitive, because a verdict
skips the grace period. A 2xx whose body is not a license token, such as a hotel
Wi-Fi sign-in page, is an `Api` error. So is a 403 or 404: `/validate` and `/revoke`
send those for a license that no longer exists, but as stock ASP.NET ProblemDetails,
which a misrouted request gets from any other ASP.NET service too.

## Offline activation

There are two routes to an offline license, depending on whether the machine has
network *at activation time*:

- **It does:** `request_activation(ActivationMethod::Offline)`, the browser flow above.
- **It does not:** the file exchange below, with no network on the device at all.

Either way the license is permanent and unrevokable; it stays valid until the
machine's device id changes.

1. Write the device token ("machine file"), conventionally `.dt`:

   ```rust
   std::fs::write("device-token.dt", licensing.generate_device_token()?)?;
   ```

2. The customer uploads it at `https://<tenant>.moonbase.sh/activate` (or through the
   embedded storefront's `activate_product` intent, or your own flow on the Moonbase
   APIs) and gets a license file back, conventionally `license.mb`.

3. Read it back, validate it locally, and keep it:

   ```rust
   let license = licensing.import_offline_license(&std::fs::read_to_string("license.mb")?)?;
   ```

`import_offline_license` runs the same local checks as `validate_token_local`,
including `application_version`, requires the token to have been issued through offline
activation, and stores it. A file that doesn't cover the version is stored too, since it
is this device's and still covers the versions it did, and fails with
`ReleaseNotCovered`. `read_offline_license` does the same without storing anything.

## Updates

The license carries the product's current release (`license.licensed_product.current_release_version`).
Compare it with your version, and fetch the notes and a download link only when it is
newer:

```rust
use moonbase::{Platform, can_download, version::update_available};

if let Some(latest) = &license.licensed_product.current_release_version {
    if update_available(APP_VERSION, latest) {
        let inventory = licensing.inventory();
        let release = inventory.get_release(latest, &license.token)?;
        if release.has_downloads && can_download(&license, &release) {
            let target = inventory.get_download_url(Platform::current().as_str(), &license.token)?;
            // target.url is presigned and short-lived; target.filename is a suggestion
        } else if !release.release_covered {
            // The customer's licenses don't cover it: offer release.upgrade_url
        }
    }
}
```

For a customer whose licenses cover only some versions, `get_release` says whether they
cover this one (`release_covered`, with `release_access` and `upgrade_url`),
`can_download` takes that into account, and `get_download_url` fails with
`ReleaseNotCovered` for a release they don't cover.

## Custom storage, resolvers and transports

```rust
use moonbase::{DeviceIdResolver, HttpRequest, HttpResponse, HttpTransport, LicenseStore};

let licensing = Licensing::builder(options)
    .store(MyStore)                 // impl LicenseStore
    .device_id_resolver(MyResolver) // impl DeviceIdResolver
    .transport(MyTransport)         // impl HttpTransport
    .build()?;
```

Pass each by value, or as an `Arc` of one your app keeps a handle to.

- **Stores.** The default is `MemoryLicenseStore`: nothing is persisted unless you
  choose a store. `FileLicenseStore` writes JSON in the same format as moonbase-cpp's
  `file_license_store` (so the two can share a file), replaces it atomically, keeps a
  restricted file's permissions, writes through symlinks, and coordinates processes
  through a `<path>.lock` sidecar. Call `cancel_lock_waits()` when shutting down so a
  wait for another process's check ends at once. A custom store reports its own
  failures as `StorageError::new(..)` or, with the I/O error kept as the source,
  `StorageError::io(..)`. Its `lock_for_update` must let the holding thread keep calling
  its other methods.
- **Resolvers.** A custom resolver's id is compared literally, need not follow the
  [`mbd2_` format](device-identity.md), and gives up cross-SDK compatibility.
  `StaticDeviceIdResolver` is a fixed identity; `MigratingDeviceIdResolver` binds a new
  algorithm while still accepting ids from older ones.
- **Transports.** `send` returns any status as a response. When no response arrives at
  all, return `ApiError::unreachable(..)`, which tells "Moonbase could not be reached"
  apart from a failure on this machine.

## Coming from moonbase-cpp

The behaviour, error mapping, throttling, grace period, file format and device id are
the same. The API follows Rust conventions:

| moonbase-cpp | moonbase (Rust) |
| --- | --- |
| `moonbase::licensing licensing(options, store, resolver, transport)` | `Licensing::builder(options).store(..).device_id_resolver(..).transport(..).build()?` |
| `licensing_options` fields | `LicensingOptions::new(..)` plus builder methods, or public fields |
| exceptions (`license_invalid_error`, ...) | `Result<T, moonbase::Error>` with one variant per exception |
| `catch (const license_invalid_error&)` (catches device mismatch too) | `error.is_license_invalid()` |
| `catch (const release_not_covered_error& ex)` | `Err(Error::ReleaseNotCovered(refusal))` |
| `has_sub_product(license, id)`, `find_sub_product(license, id)` | `license.has_sub_product(id)`, `license.find_sub_product(id)` |
| `covers_version(license, version)`, `parse_app_version(version)` | `license.covers_version(version)`, `version::parse_app_version(version)` |
| `activate_sub_product(token, id, should_persist)` | `activate_sub_product_with(token, id, \|\| ..)` |
| `std::optional<license>` from a poll | `Result<Option<License>>` |
| `validate_token_online(token, should_persist)` | `validate_token_online_with(token, \|\| ..)` |
| `curl_http_transport` | `UreqTransport` (feature `ureq-transport`) |
| `std::chrono::system_clock::time_point` | `std::time::SystemTime` |
| `nlohmann::json` properties | `serde_json::Value` |

Deliberate differences:

- The `User-Agent` and `x-mb-client` header say `moonbase-rs`.
- A public key that does not parse is `Error::Configuration` at construction, rather
  than a license error.
- iOS and Android ids are stamped `mbd2s_` (scoped), as the fingerprint spec and its
  vectors require.
- On Android the host supplies `ANDROID_ID` through
  `moonbase::android::set_android_id_source` rather than JNI handles.
- A device id read that finds no identity is retried on the next call instead of being
  kept for the life of the resolver, so a transient absence (an iOS device not yet
  unlocked since boot) heals itself.
- There is no `legacy_cpp_device_id_resolver`: a new SDK has no fleet bound to the
  pre-spec algorithm.
