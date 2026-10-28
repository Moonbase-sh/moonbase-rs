# Device identity

Which device id this SDK computes, what it reads, what happens when a machine has no
hardware identity, and how to diagnose a mismatch.

Every license is bound to a machine through a device id, carried in the token's `sig`
claim and recomputed on every local validation. The default
`MoonbaseDeviceIdResolver` implements the cross-SDK
[Moonbase device fingerprint spec](https://github.com/Moonbase-sh/moonbase-cpp/blob/main/FINGERPRINT_SPEC.md)
(`moonbase:fingerprint:v2`; the canonical copy ships in the `@moonbase.sh/licensing`
npm package): a SHA-256 of stable native hardware identifiers, stamped with the spec
version.

```
mbd2_9f3c…            // 'mbd' + version + source tag + '_' + 64 lowercase hex characters
```

| Platform | Read from | Source |
| --- | --- | --- |
| macOS (and Mac Catalyst) | `IOPlatformUUID` through IOKit | hardware, `mbd2_` |
| Windows | SMBIOS system UUID and baseboard serial, through `GetSystemFirmwareTable` | hardware, `mbd2_` |
| Linux | `/etc/machine-id` (or the D-Bus copy) plus world-readable DMI | hardware, `mbd2_` |
| iOS, iPadOS, tvOS, watchOS, visionOS | `identifierForVendor` | scoped, `mbd2s_` |
| Android | `ANDROID_ID`, supplied by the host | scoped, `mbd2s_` |
| BSD and anything else | nothing defined | none |

No subprocess is spawned and no root-only file is read, so the id is the same elevated
or not, and the resolver works inside an App Sandbox.

## Conformance

The algorithm lives in `moonbase::fingerprint_spec` with no OS calls, so it is tested on
every platform against the shared conformance vectors in
[`crates/moonbase/tests/vectors/fingerprint-vectors.json`](../crates/moonbase/tests/vectors/fingerprint-vectors.json),
the Windows SMBIOS parser included. The vectors cannot prove that the platform
*readers* agree with other SDKs on real hardware, so the
[parity workflow](../.github/workflows/fingerprint-parity.yml) runs the `device_id`
example next to `@moonbase.sh/licensing` on macOS, Windows and Linux runners and
requires the two ids to match. Any two conforming SDKs compute the same id on a given
machine, so a license activated through one validates in the others.

Conformance is per SDK: this one conforms from its first release, 0.1.0;
moonbase-cpp from 4.0.0; `@moonbase.sh/licensing` from 3.0.0. Check the version of
whichever SDK you are pairing with before relying on it.

## Stability

The id survives a rename, a locale change, a firmware update, a RAM or disk swap, a
vCPU resize, OS upgrades and running elevated or not. Two Linux exceptions follow from
every per-unit hardware serial being root-only there, which ties the id to the OS
installation rather than the hardware:

- A Linux **OS reinstall** requires re-activation.
- A Linux **VM cloned without clearing `/etc/machine-id`** keeps its device id, so a
  license copied with the disk keeps validating. Images meant for reuse should ship
  that file empty, as `machine-id(5)` requires.

## When there is no hardware identity

The resolver fails with `Error::InsufficientDeviceIdentity` rather than fall back to
something weak, in two cases:

- **Nothing readable**: a locked-down process, or a platform with no defined
  parameters.
- **Only model-level values readable**: vendor, product and board names are identical
  across every unit of a product line, so fingerprinting them would let those machines
  validate one another's licenses. In practice: a Linux install with no `machine-id`
  (many containers), or SMBIOS with an unset UUID and a blank or filler baseboard
  serial, the usual shape of a cloned VM image.

A read that finds no identity is not remembered: the next call reads the machine again,
so a transient absence heals itself.

Opt in explicitly if a weaker id beats none. Those ids are stamped `mbd2n_` so the
server can tell them apart:

```rust
use moonbase::{DeviceIdFallback, MoonbaseDeviceIdResolver, MoonbaseDeviceIdResolverOptions};

let resolver = MoonbaseDeviceIdResolver::with_options(MoonbaseDeviceIdResolverOptions {
    fallback: DeviceIdFallback::DeviceName,
    ..Default::default()
});
```

The Tauri plugin's `allow_device_name_fallback(true)` does the same. The fallback is
refused on iOS and Android whatever the setting, because the host name there is the
same on every device.

## iOS and Android

Neither platform lets an app read an identifier other apps can see, so the id there is
*scoped*: stable for the device within the app's scope (the iOS vendor, the Android
signing key), and never comparable with an id from another scope. The `mbd2s_` stamp
carries that.

On Android, native code cannot read `ANDROID_ID` by itself (it needs the application
`Context`), so supply it once at startup:

```rust
moonbase::android::set_android_id_source(|| read_android_id_from_kotlin());
```

It must be `Settings.Secure.getString(resolver, ANDROID_ID)`. The static field
`Settings.Secure.ANDROID_ID` is the key name `"android_id"`, identical on every device;
the spec's `^[0-9a-f]{1,16}$` rule rejects it, so that mistake surfaces as insufficient
identity rather than as one device id for the whole install base.

## Diagnosing a mismatch

Because the version and source are part of the id, a mismatch says which case it is.
`Error::DeviceMismatch`'s message leads with "This license is not for this device" and
adds, when the stamps differ, whether the binding came from an older or newer
fingerprint version, a host-name fallback, or a scoped identity, and what to do about
it, without ever claiming the license came from this machine.

`describe_device()` returns the id, spec version, platform and the *names* of the
parameters that contributed. It is safe to log or attach to a support ticket:

```rust
if let Some(described) = licensing.describe_device()? {
    println!("{} from {:?}", described.device_id, described.param_names);
}
```

Parameter values are never exposed: they are hardware serials, and an unsalted hash of
one is no safer to publish than the value. The `device_id` example prints the same as
JSON:

```sh
cargo run -p moonbase --example device_id
```

The device id itself is a one-way hash, but it is derived identically for every
Moonbase-powered product on the machine, so treat it as a stable cross-vendor machine
identifier. If that matters for your deployment, supply a custom `DeviceIdResolver`
that mixes in a key of your own (and gives up cross-SDK compatibility).

## Migrating

`MigratingDeviceIdResolver` binds a new algorithm on every new activation while still
accepting ids the machine was bound to before, computed lazily from the machine's own
hardware and never read from disk. Use it if you ever replace the resolver for an app
that already has activated users, so they do not all have to re-activate at once.
