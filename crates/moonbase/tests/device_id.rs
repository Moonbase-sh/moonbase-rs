//! The default device id resolver and the migrating resolver.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use moonbase::fingerprint_spec::{self as fp, DeviceIdSource};
use moonbase::{
    DeviceIdFallback, DeviceIdResolver, DeviceIdentity, Error, ErrorType, MigratingDeviceIdResolver,
    MoonbaseDeviceIdResolver, MoonbaseDeviceIdResolverOptions, StaticDeviceIdResolver,
};

const MACHINE_ID: &str = "b08dfa6083e7567a1921a715000001fb";

fn identity(params: &[(&str, &str)], device_name: &str) -> DeviceIdentity {
    DeviceIdentity {
        params: params.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect(),
        device_name: device_name.to_owned(),
    }
}

fn resolver(
    platform: &str,
    fallback: DeviceIdFallback,
    read: impl Fn() -> DeviceIdentity + Send + Sync + 'static,
) -> MoonbaseDeviceIdResolver {
    MoonbaseDeviceIdResolver::with_options(MoonbaseDeviceIdResolverOptions {
        fallback,
        reader: Some(Arc::new(read)),
        platform: Some(platform.to_owned()),
    })
}

#[test]
fn reads_identity_once_and_shares_it_across_threads() {
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let resolver = Arc::new(resolver("linux", DeviceIdFallback::None, move || {
        counter.fetch_add(1, Ordering::SeqCst);
        identity(&[("machineId", MACHINE_ID)], "host-1")
    }));

    let results: Vec<(String, String)> = (0..8)
        .map(|_| {
            let resolver = resolver.clone();
            thread::spawn(move || (resolver.device_id().unwrap(), resolver.device_name()))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();

    // Reading per call would double the cost of every activation and let the name
    // and the id come from two different reads of the machine.
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert!(results.iter().all(|result| result == &results[0]));
    assert!(results[0].0.starts_with("mbd2_"));
    assert_eq!(results[0].1, "host-1");
}

#[test]
fn a_read_with_no_identity_is_retried_not_cached() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    // Nothing identifying on the first read (an iOS device before first unlock),
    // a real id afterwards. A sticky failure would outlive its cause.
    let resolver = resolver("linux", DeviceIdFallback::None, move || {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            identity(&[("sysVendor", "LENOVO")], "host-1")
        } else {
            identity(&[("machineId", MACHINE_ID)], "host-1")
        }
    });

    let error = resolver.device_id().unwrap_err();
    assert_eq!(error.error_type(), ErrorType::DeviceIdentityUnavailable);
    assert!(error.to_string().contains("only model-level parameters could be read (sysVendor)"));
    assert!(resolver.device_id().unwrap().starts_with("mbd2_"));
    // Memoized once it succeeds.
    resolver.device_id().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn the_device_name_survives_a_machine_with_no_identity() {
    let resolver = resolver("unknown", DeviceIdFallback::None, || identity(&[], "PC-1"));
    assert_eq!(resolver.device_name(), "PC-1");
    assert!(matches!(resolver.device_id().unwrap_err(), Error::InsufficientDeviceIdentity { .. }));
}

#[test]
fn the_host_name_fallback_is_opt_in_and_separately_stamped() {
    let resolver = resolver("unknown", DeviceIdFallback::DeviceName, || identity(&[], "PC-1"));
    // The spec's worked example.
    assert_eq!(resolver.device_id().unwrap(), "mbd2n_493978eb157552e60a13694bd6861b2a82d0dba2746431a5f7921951ed460045");
    let described = resolver.describe_device().unwrap().unwrap();
    assert_eq!(described.source, DeviceIdSource::DeviceName);
    assert_eq!(described.param_names, ["deviceName"]);

    let empty = resolver_with_empty_name();
    assert!(matches!(empty.device_id().unwrap_err(), Error::InsufficientDeviceIdentity { .. }));
}

fn resolver_with_empty_name() -> MoonbaseDeviceIdResolver {
    resolver("unknown", DeviceIdFallback::DeviceName, || identity(&[], ""))
}

#[test]
fn the_host_name_fallback_is_refused_on_ios_and_android() {
    // Since iOS 17 gethostname() returns "localhost" on every device: falling back
    // there would hand a whole install base one device id.
    for platform in ["ios", "android"] {
        let resolver = resolver(platform, DeviceIdFallback::DeviceName, || identity(&[], "localhost"));
        assert!(matches!(resolver.device_id().unwrap_err(), Error::InsufficientDeviceIdentity { .. }), "{platform}");
        // Enforced at the material too, so it binds custom readers.
        assert!(fp::build_fingerprint_material(platform, &[("deviceName", "localhost")]).is_err());
    }
    assert!(fp::build_fingerprint_material("unknown", &[("deviceName", "PC-1")]).is_ok());
}

#[test]
fn describe_device_reports_names_only_and_hands_out_a_copy() {
    let resolver = resolver("linux", DeviceIdFallback::None, || {
        identity(&[("machineId", MACHINE_ID), ("sysVendor", "LENOVO"), ("productName", "")], "host-1")
    });
    let mut described = resolver.describe_device().unwrap().unwrap();
    assert_eq!(described.version, 2);
    assert_eq!(described.platform, "linux");
    assert_eq!(described.source, DeviceIdSource::Identity);
    // Dropped parameters are absent, and no value appears anywhere.
    assert_eq!(described.param_names, ["machineId", "sysVendor"]);
    assert!(!serde_json::to_string(&described).unwrap().contains(MACHINE_ID));

    let before = resolver.device_id().unwrap();
    described.device_id = "tampered".into();
    described.param_names.clear();
    assert_eq!(resolver.device_id().unwrap(), before);
    assert_eq!(resolver.describe_device().unwrap().unwrap().param_names.len(), 2);
}

#[test]
fn the_host_resolver_either_produces_a_stamped_id_or_refuses() {
    // A container with no DMI and no machine-id legitimately has no identity. What
    // must never happen is a third outcome: a bare digest, an unstamped id, or a
    // silent host-name substitution.
    let resolver = MoonbaseDeviceIdResolver::new();
    match resolver.device_id() {
        Ok(id) => {
            assert_eq!(id.len(), 69, "{id}");
            let stamp = fp::parse_device_id_stamp(&id).expect("a stamped id");
            assert_eq!(stamp.version, fp::VERSION);
            let expected = if cfg!(any(target_os = "ios", target_os = "android")) {
                DeviceIdSource::Scoped
            } else {
                DeviceIdSource::Identity
            };
            assert_eq!(stamp.source, Some(expected));
        }
        Err(error) => assert_eq!(error.error_type(), ErrorType::DeviceIdentityUnavailable),
    }
}

#[test]
fn ios_and_android_ids_are_stamped_as_scoped() {
    // The spec's worked examples: the same material as any other platform, but the
    // `s` in the stamp says the id is only comparable within its app scope.
    let ios = resolver("ios", DeviceIdFallback::None, || {
        identity(&[("identifierForVendor", "0123456789ABCDEF0123456789ABCDEF")], "iPhone")
    });
    assert_eq!(ios.device_id().unwrap(), "mbd2s_298ced47f8d983939db1d5fce6d4b4f2f8766aa19e3e17536fcd1604a81febf1");
    assert_eq!(ios.describe_device().unwrap().unwrap().source, DeviceIdSource::Scoped);

    let android =
        resolver("android", DeviceIdFallback::None, || identity(&[("androidId", "a1b2c3d4e5f60718")], "Pixel"));
    assert_eq!(android.device_id().unwrap(), "mbd2s_ca988ecf5c529964bfaa80734da3dbe070dd41881aa43f8c472f3a5d512b4eff");
    // The static field's key name is not an id.
    let misread = resolver("android", DeviceIdFallback::None, || identity(&[("androidId", "android_id")], "Pixel"));
    assert!(matches!(misread.device_id().unwrap_err(), Error::InsufficientDeviceIdentity { .. }));
}

#[test]
fn mac_catalyst_takes_the_macos_tag() {
    if cfg!(target_os = "macos") {
        assert_eq!(fp::platform_tag(), "mac");
    }
    if cfg!(target_os = "linux") {
        assert_eq!(fp::platform_tag(), "linux");
    }
    if cfg!(target_os = "windows") {
        assert_eq!(fp::platform_tag(), "windows");
    }
}

struct CountingResolver {
    id: String,
    calls: AtomicUsize,
}

impl DeviceIdResolver for CountingResolver {
    fn device_name(&self) -> String {
        "Studio Mac".into()
    }

    fn device_id(&self) -> moonbase::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.id.clone())
    }
}

struct FailingResolver;

impl DeviceIdResolver for FailingResolver {
    fn device_name(&self) -> String {
        String::new()
    }

    fn device_id(&self) -> moonbase::Result<String> {
        Err(Error::Configuration("this algorithm no longer works here".into()))
    }
}

#[test]
fn the_migrating_resolver_binds_the_current_id_and_accepts_historical_ones_lazily() {
    let current = format!("mbd2_{}", "a".repeat(64));
    let legacy = "b".repeat(64);
    let counted = Arc::new(CountingResolver { id: legacy.clone(), calls: AtomicUsize::new(0) });
    let resolver = MigratingDeviceIdResolver::new(
        Arc::new(StaticDeviceIdResolver::new("Studio Mac", current.clone())),
        vec![Arc::new(FailingResolver), counted.clone()],
    );

    // Every new activation binds the current algorithm, and the happy path never
    // pays for the migration.
    assert_eq!(resolver.device_id().unwrap(), current);
    assert_eq!(resolver.device_name(), "Studio Mac");
    assert_eq!(counted.calls.load(Ordering::SeqCst), 0);

    assert!(resolver.accepts_device_id(&legacy));
    assert!(!resolver.accepts_device_id("something-else"));
    assert!(resolver.accepts_device_id(&legacy));
    assert_eq!(counted.calls.load(Ordering::SeqCst), 1);

    // An empty historical id never matches.
    let blank = MigratingDeviceIdResolver::new(
        Arc::new(StaticDeviceIdResolver::new("Studio Mac", current)),
        vec![Arc::new(StaticDeviceIdResolver::new("Studio Mac", ""))],
    );
    assert!(!blank.accepts_device_id(""));
}

#[test]
fn the_migrating_resolver_forwards_diagnostics() {
    let current =
        Arc::new(resolver("linux", DeviceIdFallback::None, || identity(&[("machineId", MACHINE_ID)], "host-1")));
    let resolver = MigratingDeviceIdResolver::new(current, vec![Arc::new(StaticDeviceIdResolver::new("x", "y"))]);
    let described = resolver.describe_device().unwrap().unwrap();
    assert_eq!(described.platform, "linux");
    assert_eq!(described.param_names, ["machineId"]);

    // A plain resolver has no history and no description.
    let plain = StaticDeviceIdResolver::new("x", "y");
    assert!(plain.describe_device().unwrap().is_none());
    assert!(!plain.accepts_device_id("y"));
}
