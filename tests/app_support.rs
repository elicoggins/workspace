use std::collections::HashSet;

use workspace::app_support::{
    full_restore_apps, support_for_bundle_id, KnownApp, SupportLevel, KNOWN_APPS,
};

#[test]
fn known_app_registry_has_unique_bundle_ids() {
    let mut seen = HashSet::new();

    for app in KNOWN_APPS {
        assert!(
            seen.insert(app.bundle_id),
            "duplicate bundle id: {}",
            app.bundle_id
        );
        assert!(
            !app.name.trim().is_empty(),
            "{} has no display name",
            app.bundle_id
        );
        assert!(
            !app.support.reason.trim().is_empty(),
            "{} has no support reason",
            app.bundle_id
        );
    }
}

#[test]
fn all_known_apps_are_enabled_for_full_restore() {
    let enabled: Vec<&KnownApp> = full_restore_apps().collect();

    assert_eq!(enabled.len(), KNOWN_APPS.len());
    for app in KNOWN_APPS {
        assert_eq!(
            support_for_bundle_id(Some(app.bundle_id)).level,
            SupportLevel::FullRestore,
            "{} should be enabled",
            app.bundle_id
        );
    }
}

#[test]
fn common_dev_apps_are_supported() {
    for bundle_id in [
        "com.apple.Terminal",
        "com.googlecode.iterm2",
        "dev.warp.Warp-Stable",
        "com.todesktop.230313mzl4w4u92",
        "com.apple.dt.Xcode",
    ] {
        let support = support_for_bundle_id(Some(bundle_id));
        assert_eq!(support.level, SupportLevel::FullRestore, "{bundle_id}");
        assert!(support.reason.contains("enabled"));
    }
}

#[test]
fn browsers_and_special_apps_are_supported() {
    for bundle_id in [
        "com.google.Chrome",
        "com.apple.Safari",
        "com.apple.finder",
        "com.apple.Notes",
        "com.apple.Music",
        "com.apple.MobileSMS",
    ] {
        let support = support_for_bundle_id(Some(bundle_id));
        assert_eq!(support.level, SupportLevel::FullRestore, "{bundle_id}");
        assert!(support.reason.contains("enabled"));
    }
}

#[test]
fn unknown_and_missing_bundle_ids_are_never_restored() {
    assert_eq!(
        support_for_bundle_id(Some("dev.example.Unknown")).level,
        SupportLevel::Unsupported
    );
    assert_eq!(support_for_bundle_id(None).level, SupportLevel::Unsupported);
}
