use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use cargo_metadata::{Metadata, Version, VersionReq};
use serde_json::{json, Value};

use crate::{
    format::{AuditKind, Delta, DependencyCriteria, MetaConfig, PolicyEntry},
    init_files,
    resolver::Report,
    AuditEntry, AuditsFile, Cli, Config, ConfigFile, CriteriaEntry, ImportsFile, PackageExt,
    PartialConfig, StableMap, UnauditedDependency,
};

// Some room above and below
const DEFAULT_VER: u64 = 10;
const DEFAULT_CRIT: &str = "reviewed";

struct MockMetadata {
    packages: Vec<MockPackage>,
    pkgids: Vec<String>,
    idx_by_name_and_ver: BTreeMap<&'static str, BTreeMap<Version, usize>>,
}

struct MockPackage {
    name: &'static str,
    version: Version,
    deps: Vec<MockDependency>,
    dev_deps: Vec<MockDependency>,
    build_deps: Vec<MockDependency>,
    is_root: bool,
    is_first_party: bool,
}

struct MockDependency {
    name: &'static str,
    version: Version,
}

impl Default for MockPackage {
    fn default() -> Self {
        Self {
            name: "",
            version: ver(DEFAULT_VER),
            deps: vec![],
            dev_deps: vec![],
            build_deps: vec![],
            is_root: false,
            is_first_party: false,
        }
    }
}

fn ver(major: u64) -> Version {
    Version {
        major,
        minor: 0,
        patch: 0,
        pre: Default::default(),
        build: Default::default(),
    }
}

fn dep(name: &'static str) -> MockDependency {
    dep_ver(name, DEFAULT_VER)
}

fn dep_ver(name: &'static str, version: u64) -> MockDependency {
    MockDependency {
        name,
        version: ver(version),
    }
}

#[allow(dead_code)]
fn default_unaudited(version: Version, config: &ConfigFile) -> UnauditedDependency {
    UnauditedDependency {
        version,
        criteria: config.default_criteria.clone(),
        notes: None,
        suggest: true,
    }
}
fn unaudited(version: Version, criteria: &str) -> UnauditedDependency {
    UnauditedDependency {
        version,
        criteria: criteria.to_string(),
        notes: None,
        suggest: true,
    }
}

fn delta_audit(from: Version, to: Version, criteria: &str) -> AuditEntry {
    let delta = Delta { from, to };
    AuditEntry {
        who: None,
        notes: None,
        criteria: criteria.to_string(),
        kind: AuditKind::Delta {
            delta,
            dependency_criteria: DependencyCriteria::default(),
        },
    }
}

#[allow(dead_code)]
fn delta_audit_dep(
    from: Version,
    to: Version,
    criteria: &str,
    dependency_criteria: impl IntoIterator<
        Item = (
            impl Into<String>,
            impl IntoIterator<Item = impl Into<String>>,
        ),
    >,
) -> AuditEntry {
    let delta = Delta { from, to };
    AuditEntry {
        who: None,
        notes: None,
        criteria: criteria.to_string(),
        kind: AuditKind::Delta {
            delta,
            dependency_criteria: dependency_criteria
                .into_iter()
                .map(|(k, v)| {
                    (
                        k.into(),
                        v.into_iter().map(|s| s.into()).collect::<Vec<_>>(),
                    )
                })
                .collect(),
        },
    }
}

fn full_audit(version: Version, criteria: &str) -> AuditEntry {
    AuditEntry {
        who: None,
        notes: None,
        criteria: criteria.to_string(),
        kind: AuditKind::Full {
            version,
            dependency_criteria: DependencyCriteria::default(),
        },
    }
}

fn full_audit_dep(
    version: Version,
    criteria: &str,
    dependency_criteria: impl IntoIterator<
        Item = (
            impl Into<String>,
            impl IntoIterator<Item = impl Into<String>>,
        ),
    >,
) -> AuditEntry {
    AuditEntry {
        who: None,
        notes: None,
        criteria: criteria.to_string(),
        kind: AuditKind::Full {
            version,
            dependency_criteria: dependency_criteria
                .into_iter()
                .map(|(k, v)| {
                    (
                        k.into(),
                        v.into_iter().map(|s| s.into()).collect::<Vec<_>>(),
                    )
                })
                .collect(),
        },
    }
}

fn violation_hard(version: VersionReq) -> AuditEntry {
    AuditEntry {
        who: None,
        notes: None,
        criteria: "weak-reviewed".to_string(),
        kind: AuditKind::Violation { violation: version },
    }
}
#[allow(dead_code)]
fn violation(version: VersionReq, criteria: &str) -> AuditEntry {
    AuditEntry {
        who: None,
        notes: None,
        criteria: criteria.to_string(),
        kind: AuditKind::Violation { violation: version },
    }
}

fn default_policy() -> PolicyEntry {
    PolicyEntry {
        criteria: vec![],
        build_and_dev_criteria: vec![],
        dependency_criteria: StableMap::new(),
        targets: None,
        build_and_dev_targets: None,
    }
}

fn self_policy(criteria: impl IntoIterator<Item = impl Into<String>>) -> PolicyEntry {
    PolicyEntry {
        criteria: criteria.into_iter().map(|s| s.into()).collect(),
        ..default_policy()
    }
}

fn dep_policy(
    dependency_criteria: impl IntoIterator<
        Item = (
            impl Into<String>,
            impl IntoIterator<Item = impl Into<String>>,
        ),
    >,
) -> PolicyEntry {
    PolicyEntry {
        dependency_criteria: dependency_criteria
            .into_iter()
            .map(|(k, v)| {
                (
                    k.into(),
                    v.into_iter().map(|s| s.into()).collect::<Vec<_>>(),
                )
            })
            .collect(),
        ..default_policy()
    }
}

impl MockMetadata {
    fn simple() -> Self {
        // A simple dependency tree to test basic functionality on.
        //
        //                                    Graph
        // =======================================================================================
        //
        //                                 root-package
        //                                       |
        //                                 first-party
        //                                /           \
        //                       third-party1       third-party2
        //                            |
        //                  transitive-third-party1
        //
        MockMetadata::new(vec![
            MockPackage {
                name: "root-package",
                is_root: true,
                is_first_party: true,
                deps: vec![dep("first-party")],
                ..Default::default()
            },
            MockPackage {
                name: "first-party",
                is_first_party: true,
                deps: vec![dep("third-party1"), dep("third-party2")],
                ..Default::default()
            },
            MockPackage {
                name: "third-party1",
                deps: vec![dep("transitive-third-party1")],
                ..Default::default()
            },
            MockPackage {
                name: "third-party2",
                ..Default::default()
            },
            MockPackage {
                name: "transitive-third-party1",
                ..Default::default()
            },
        ])
    }

    fn complex() -> Self {
        // A Complex dependency tree to test more weird interactions and corner cases:
        //
        // * firstAB: first-party shared between two roots
        // * firstB-nodeps: first-party with no third-parties
        // * third-core: third-party used by everything, has two versions in-tree
        //
        //                                      Graph
        // =======================================================================================
        //
        //                         rootA                rootB
        //                        -------       ---------------------
        //                       /       \     /          |          \
        //                      /         \   /           |           \
        //                    firstA     firstAB       firstB     firstB-nodeps
        //                   /      \         \           |
        //                  /        \         \          |
        //                 /        thirdA    thirdAB     +
        //                /             \        |       /
        //               /               \       |      /
        //        third-core:v5           third-core:v10
        //
        MockMetadata::new(vec![
            MockPackage {
                name: "rootA",
                is_root: true,
                is_first_party: true,
                deps: vec![dep("firstA"), dep("firstAB")],
                ..Default::default()
            },
            MockPackage {
                name: "rootB",
                is_root: true,
                is_first_party: true,
                deps: vec![dep("firstB"), dep("firstAB"), dep("firstB-nodeps")],
                ..Default::default()
            },
            MockPackage {
                name: "firstA",
                is_first_party: true,
                deps: vec![dep("thirdA"), dep_ver("third-core", 5)],
                ..Default::default()
            },
            MockPackage {
                name: "firstAB",
                is_first_party: true,
                deps: vec![dep("thirdAB")],
                ..Default::default()
            },
            MockPackage {
                name: "firstB",
                is_first_party: true,
                deps: vec![dep("third-core")],
                ..Default::default()
            },
            MockPackage {
                name: "firstB-nodeps",
                is_first_party: true,
                ..Default::default()
            },
            MockPackage {
                name: "thirdA",
                deps: vec![dep("third-core")],
                ..Default::default()
            },
            MockPackage {
                name: "thirdAB",
                deps: vec![dep("third-core")],
                ..Default::default()
            },
            MockPackage {
                name: "third-core",
                ..Default::default()
            },
            MockPackage {
                name: "third-core",
                version: ver(5),
                ..Default::default()
            },
        ])
    }
    fn new(packages: Vec<MockPackage>) -> Self {
        let mut pkgids = vec![];
        let mut idx_by_name_and_ver = BTreeMap::<&str, BTreeMap<Version, usize>>::new();

        for (idx, package) in packages.iter().enumerate() {
            let pkgid = if package.is_first_party {
                format!(
                    "{} {} (path+file:///C:/FAKE/{})",
                    package.name, package.version, package.name
                )
            } else {
                format!(
                    "{} {} (registry+https://github.com/rust-lang/crates.io-index)",
                    package.name, package.version
                )
            };
            pkgids.push(pkgid);
            let old = idx_by_name_and_ver
                .entry(package.name)
                .or_default()
                .insert(package.version.clone(), idx);
            assert!(
                old.is_none(),
                "duplicate version {} {}",
                package.name,
                package.version
            );

            if !package.build_deps.is_empty() {
                unimplemented!("build-deps aren't mockable yet");
            }
            if !package.dev_deps.is_empty() {
                unimplemented!("dev-deps aren't mockable yet");
            }
        }

        Self {
            packages,
            pkgids,
            idx_by_name_and_ver,
        }
    }

    fn pkgid(&self, package: &MockPackage) -> &str {
        self.pkgid_by(package.name, &package.version)
    }

    fn pkgid_by(&self, name: &str, version: &Version) -> &str {
        &self.pkgids[self.idx_by_name_and_ver[name][version]]
    }

    fn package_by(&self, name: &str, version: &Version) -> &MockPackage {
        &self.packages[self.idx_by_name_and_ver[name][version]]
    }

    fn source(&self, package: &MockPackage) -> Value {
        if package.is_first_party {
            json!(null)
        } else {
            json!("registry+https://github.com/rust-lang/crates.io-index")
        }
    }

    fn metadata(&self) -> Metadata {
        let meta_json = json!({
            "packages": self.packages.iter().map(|package| json!({
                "name": package.name,
                "version": package.version.to_string(),
                "id": self.pkgid(package),
                "license": "MIT",
                "license_file": null,
                "description": "whatever",
                "source": self.source(package),
                "dependencies": package.deps.iter().map(|dep| json!({
                    "name": dep.name,
                    "source": self.source(self.package_by(dep.name, &dep.version)),
                    "req": format!("={}", dep.version),
                    "kind": null,
                    "rename": null,
                    "optional": false,
                    "uses_default_features": true,
                    "features": [],
                    "target": null,
                    "registry": null
                })).collect::<Vec<_>>(),
                "targets": [
                    {
                        "kind": [
                            "lib"
                        ],
                        "crate_types": [
                            "lib"
                        ],
                        "name": package.name,
                        "src_path": "C:\\Users\\fake_user\\.cargo\\registry\\src\\github.com-1ecc6299db9ec823\\DUMMY\\src\\lib.rs",
                        "edition": "2015",
                        "doc": true,
                        "doctest": true,
                        "test": true
                    },
                ],
                "features": {},
                "manifest_path": "C:\\Users\\fake_user\\.cargo\\registry\\src\\github.com-1ecc6299db9ec823\\DUMMY\\Cargo.toml",
                "metadata": null,
                "publish": null,
                "authors": [],
                "categories": [],
                "keywords": [],
                "readme": "README.md",
                "repository": null,
                "homepage": null,
                "documentation": null,
                "edition": "2015",
                "links": null,
                "default_run": null,
                "rust_version": null
            })).collect::<Vec<_>>(),
            "workspace_members": self.packages.iter().filter_map(|package| {
                if package.is_root {
                    Some(self.pkgid(package))
                } else {
                    None
                }
            }).collect::<Vec<_>>(),
            "resolve": {
                "nodes": self.packages.iter().map(|package| json!({
                    "id": self.pkgid(package),
                    "dependencies": package.deps.iter().map(|dep| {
                        self.pkgid_by(dep.name, &dep.version)
                    }).collect::<Vec<_>>(),
                    "deps": package.deps.iter().map(|dep| json!({
                        "name": dep.name,
                        "pkg": self.pkgid_by(dep.name, &dep.version),
                        "dep_kinds": [
                            {
                                "kind": null,
                                "target": null,
                            }
                        ],
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "root": null,
            },
            "target_directory": "C:\\FAKE\\target",
            "version": 1,
            "workspace_root": "C:\\FAKE\\",
            "metadata": null,
        });
        serde_json::from_value(meta_json).unwrap()
    }
}

fn files_inited(metadata: &Metadata) -> (ConfigFile, AuditsFile, ImportsFile) {
    let (mut config, mut audits, imports) = init_files(metadata).unwrap();

    // Criteria hierarchy:
    //
    // * strong-reviewed
    //   * reviewed (default)
    //      * weak-reviewed
    // * fuzzed
    //
    // This lets use mess around with "strong reqs", "weaker reqs", and "unrelated reqs"
    // with "reviewed" as the implicit default everything cares about.

    audits.criteria = StableMap::from_iter(vec![
        (
            "strong-reviewed".to_string(),
            CriteriaEntry {
                implies: vec!["reviewed".to_string()],
                description: Some("strongly reviewed".to_string()),
                description_url: None,
            },
        ),
        (
            "reviewed".to_string(),
            CriteriaEntry {
                implies: vec!["weak-reviewed".to_string()],
                description: Some("reviewed".to_string()),
                description_url: None,
            },
        ),
        (
            "weak-reviewed".to_string(),
            CriteriaEntry {
                implies: vec![],
                description: Some("weakly reviewed".to_string()),
                description_url: None,
            },
        ),
        (
            "fuzzed".to_string(),
            CriteriaEntry {
                implies: vec![],
                description: Some("fuzzed".to_string()),
                description_url: None,
            },
        ),
    ]);

    // Make the root packages use our custom criteria instead of the builtins
    for pkgid in &metadata.workspace_members {
        for package in &metadata.packages {
            if package.id == *pkgid {
                config.policy.insert(
                    package.name.clone(),
                    PolicyEntry {
                        criteria: vec![DEFAULT_CRIT.to_string()],
                        build_and_dev_criteria: vec![DEFAULT_CRIT.to_string()],
                        dependency_criteria: DependencyCriteria::new(),
                        targets: None,
                        build_and_dev_targets: None,
                    },
                );
            }
        }
    }
    config.default_criteria = DEFAULT_CRIT.to_string();

    // Rewrite the default used by init
    for unaudited in &mut config.unaudited {
        for entry in unaudited.1 {
            assert_eq!(&*entry.criteria, "safe-to-deploy");
            entry.criteria = DEFAULT_CRIT.to_string();
        }
    }

    (config, audits, imports)
}

fn files_no_unaudited(metadata: &Metadata) -> (ConfigFile, AuditsFile, ImportsFile) {
    let (mut config, audits, imports) = files_inited(metadata);

    // Just clear all the unaudited entries out
    config.unaudited.clear();

    (config, audits, imports)
}

fn files_full_audited(metadata: &Metadata) -> (ConfigFile, AuditsFile, ImportsFile) {
    let (config, mut audits, imports) = files_no_unaudited(metadata);

    let mut audited = StableMap::<String, Vec<AuditEntry>>::new();
    for package in &metadata.packages {
        if package.is_third_party() {
            audited
                .entry(package.name.clone())
                .or_insert(vec![])
                .push(full_audit(package.version.clone(), DEFAULT_CRIT));
        }
    }
    audits.audits = audited;

    (config, audits, imports)
}

fn get_report(metadata: &Metadata, report: Report) -> String {
    let cfg = Config {
        metacfg: MetaConfig(vec![]),
        metadata: metadata.clone(),
        _rest: PartialConfig {
            cli: Cli::mock(),
            cargo: OsString::new(),
            tmp: PathBuf::new(),
            cargo_home: None,
        },
    };
    let mut stdout = Vec::new();
    report.print_report(&mut stdout, &cfg).unwrap();
    String::from_utf8(stdout).unwrap()
}

#[test]
fn mock_simple_init() {
    // (Pass) Should look the same as a fresh 'vet init'.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, audits, imports) = files_inited(&metadata);

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-init", stdout);
}

#[test]
fn mock_simple_no_unaudited() {
    // (Fail) Should look the same as a fresh 'vet init' but with all 'unaudited' entries deleted.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, audits, imports) = files_no_unaudited(&metadata);

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-no-unaudited", stdout);
}

#[test]
fn mock_simple_full_audited() {
    // (Pass) All entries have direct full audits.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, audits, imports) = files_full_audited(&metadata);

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-full-audited", stdout);
}

#[test]
fn mock_simple_forbidden_unaudited() {
    // (Fail) All marked 'unaudited' but a 'violation' entry matches a current version.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_inited(&metadata);

    let violation = VersionReq::parse(&format!("={DEFAULT_VER}")).unwrap();
    audits
        .audits
        .entry("third-party1".to_string())
        .or_insert(vec![])
        .push(violation_hard(violation));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-forbidden-unaudited", stdout);
}

#[test]
fn mock_simple_missing_transitive() {
    // (Fail) Missing an audit for a transitive dep

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["transitive-third-party1"].clear();

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-missing-transitive", stdout);
}

#[test]
fn mock_simple_missing_direct_internal() {
    // (Fail) Missing an audit for a direct dep that has children

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-party1"].clear();

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-missing-direct-internal", stdout);
}

#[test]
fn mock_simple_missing_direct_leaf() {
    // (Fail) Missing an entry for direct dep that has no children

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-party2"].clear();

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-missing-direct-leaf", stdout);
}

#[test]
fn mock_simple_missing_leaves() {
    // (Fail) Missing all leaf audits (but not the internal)

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-party2"].clear();
    audits.audits["transitive-third-party1"].clear();

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-missing-leaves", stdout);
}

#[test]
fn mock_simple_weaker_transitive_req() {
    // (Pass) A third-party dep with weaker requirements on a child dep

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let trans_audits = &mut audits.audits["transitive-third-party1"];
    trans_audits.clear();
    trans_audits.push(full_audit(ver(DEFAULT_VER), "weak-reviewed"));

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(full_audit_dep(
        ver(DEFAULT_VER),
        "reviewed",
        [("transitive-third-party1", ["weak-reviewed"])],
    ));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-weaker-transitive-req", stdout);
}

#[test]
fn mock_simple_weaker_transitive_req_using_implies() {
    // (Pass) A third-party dep with weaker requirements on a child dep
    // but the child dep actually has *super* reqs, to check that implies works

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let trans_audits = &mut audits.audits["transitive-third-party1"];
    trans_audits.clear();
    trans_audits.push(full_audit(ver(DEFAULT_VER), "strong-reviewed"));

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(full_audit_dep(
        ver(DEFAULT_VER),
        "reviewed",
        [("transitive-third-party1", ["weak-reviewed"])],
    ));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-weaker-transitive-req-using-implies", stdout);
}

#[test]
fn mock_simple_lower_version_review() {
    // (Fail) A dep that has a review but for a lower version.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(full_audit(ver(DEFAULT_VER - 1), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-lower-version-review", stdout);
}

#[test]
fn mock_simple_higher_version_review() {
    // (Fail) A dep that has a review but for a higher version.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(full_audit(ver(DEFAULT_VER + 1), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-higher-version-review", stdout);
}

#[test]
fn mock_simple_higher_and_lower_version_review() {
    // (Fail) A dep that has a review but for both a higher and lower version.
    // Once I mock out fake diffs it should prefer the lower one because the
    // system will make application size grow quadratically.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(full_audit(ver(DEFAULT_VER - 1), DEFAULT_CRIT));
    direct_audits.push(full_audit(ver(DEFAULT_VER + 1), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-higher-and-lower-version-review", stdout);
}

#[test]
fn mock_simple_reviewed_too_weakly() {
    // (Fail) A dep has a review but the criteria is too weak

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let trans_audits = &mut audits.audits["transitive-third-party1"];
    trans_audits.clear();
    trans_audits.push(full_audit(ver(DEFAULT_VER), "weak-reviewed"));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-reviewed-too-weakly", stdout);
}

#[test]
fn mock_simple_delta_to_unaudited() {
    // (Pass) A dep has a delta to an unaudited entry

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 5),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));

    let direct_unaudited = &mut config.unaudited;
    direct_unaudited.insert(
        "third-party1".to_string(),
        vec![unaudited(ver(DEFAULT_VER - 5), DEFAULT_CRIT)],
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-unaudited", stdout);
}

#[test]
fn mock_simple_delta_to_unaudited_overshoot() {
    // (Fail) A dep has a delta but it overshoots the unaudited entry.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 7),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));

    let direct_unaudited = &mut config.unaudited;
    direct_unaudited.insert(
        "third-party1".to_string(),
        vec![unaudited(ver(DEFAULT_VER - 5), DEFAULT_CRIT)],
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-unaudited-overshoot", stdout);
}

#[test]
fn mock_simple_delta_to_unaudited_undershoot() {
    // (Fail) A dep has a delta but it undershoots the unaudited entry.

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 3),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));

    let direct_unaudited = &mut config.unaudited;
    direct_unaudited.insert(
        "third-party1".to_string(),
        vec![unaudited(ver(DEFAULT_VER - 5), DEFAULT_CRIT)],
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-unaudited-undershoot", stdout);
}

#[test]
fn mock_simple_delta_to_full_audit() {
    // (Pass) A dep has a delta to a fully audited entry

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 5),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));
    direct_audits.push(full_audit(ver(DEFAULT_VER - 5), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-full-audit", stdout);
}

#[test]
fn mock_simple_delta_to_full_audit_overshoot() {
    // (Fail) A dep has a delta to a fully audited entry but overshoots

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 7),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));
    direct_audits.push(full_audit(ver(DEFAULT_VER - 5), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-full-audit-overshoot", stdout);
}

#[test]
fn mock_simple_delta_to_full_audit_undershoot() {
    // (Fail) A dep has a delta to a fully audited entry but undershoots

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 3),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));
    direct_audits.push(full_audit(ver(DEFAULT_VER - 5), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-full-audit-undershoot", stdout);
}

#[test]
fn mock_simple_reverse_delta_to_full_audit() {
    // (Pass) A dep has a *reverse* delta to a fully audited entry

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER + 5),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));
    direct_audits.push(full_audit(ver(DEFAULT_VER + 5), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-reverse-delta-to-full-audit", stdout);
}

#[test]
fn mock_simple_reverse_delta_to_unaudited() {
    // (Pass) A dep has a *reverse* delta to an unaudited entry

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER + 5),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));

    let direct_unaudited = &mut config.unaudited;
    direct_unaudited.insert(
        "third-party1".to_string(),
        vec![unaudited(ver(DEFAULT_VER + 5), DEFAULT_CRIT)],
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-reverse-delta-to-unaudited", stdout);
}

#[test]
fn mock_simple_wrongly_reversed_delta_to_unaudited() {
    // (Fail) A dep has a *reverse* delta to an unaudited entry but they needed a normal one

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER),
        ver(DEFAULT_VER - 5),
        DEFAULT_CRIT,
    ));

    let direct_unaudited = &mut config.unaudited;
    direct_unaudited.insert(
        "third-party1".to_string(),
        vec![unaudited(ver(DEFAULT_VER - 5), DEFAULT_CRIT)],
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-wrongly-reversed-delta-to-unaudited", stdout);
}

#[test]
fn mock_simple_wrongly_reversed_delta_to_full_audit() {
    // (Fail) A dep has a *reverse* delta to a fully audited entry but they needed a normal one

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER),
        ver(DEFAULT_VER - 5),
        DEFAULT_CRIT,
    ));
    direct_audits.push(full_audit(ver(DEFAULT_VER - 5), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-wrongly-reversed-delta-to-full-audit", stdout);
}

#[test]
fn mock_simple_needed_reversed_delta_to_unaudited() {
    // (Fail) A dep has a delta to an unaudited entry but they needed a reversed one

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER),
        ver(DEFAULT_VER + 5),
        DEFAULT_CRIT,
    ));

    let direct_unaudited = &mut config.unaudited;
    direct_unaudited.insert(
        "third-party1".to_string(),
        vec![unaudited(ver(DEFAULT_VER + 5), DEFAULT_CRIT)],
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-needed-reversed-delta-to-unaudited", stdout);
}

#[test]
fn mock_simple_delta_to_unaudited_too_weak() {
    // (Fail) A dep has a delta to an unaudited entry but it's too weak

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 5),
        ver(DEFAULT_VER),
        "weak-reviewed",
    ));

    let direct_unaudited = &mut config.unaudited;
    direct_unaudited.insert(
        "third-party1".to_string(),
        vec![unaudited(ver(DEFAULT_VER - 5), DEFAULT_CRIT)],
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-unaudited-too-weak", stdout);
}

#[test]
fn mock_simple_delta_to_full_audit_too_weak() {
    // (Fail) A dep has a delta to a fully audited entry but it's too weak

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 5),
        ver(DEFAULT_VER),
        "weak-reviewed",
    ));
    direct_audits.push(full_audit(ver(DEFAULT_VER - 5), DEFAULT_CRIT));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-full-audit-too-weak", stdout);
}

#[test]
fn mock_simple_delta_to_too_weak_full_audit() {
    // (Fail) A dep has a delta to a fully audited entry but it's too weak

    let mock = MockMetadata::simple();

    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    let direct_audits = &mut audits.audits["third-party1"];
    direct_audits.clear();
    direct_audits.push(delta_audit(
        ver(DEFAULT_VER - 5),
        ver(DEFAULT_VER),
        DEFAULT_CRIT,
    ));
    direct_audits.push(full_audit(ver(DEFAULT_VER - 5), "weak-reviewed"));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);

    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-simple-delta-to-too-weak-full-audit", stdout);
}

#[test]
fn mock_complex_no_unaudited() {
    // (Fail) Should look the same as a fresh 'vet init' but with all 'unaudited' entries deleted.

    let mock = MockMetadata::complex();
    let metadata = mock.metadata();
    let (config, audits, imports) = files_no_unaudited(&metadata);

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-complex-no-unaudited", stdout);
}

#[test]
fn mock_complex_full_audited() {
    // (Pass) All entries have direct full audits.

    let mock = MockMetadata::complex();
    let metadata = mock.metadata();
    let (config, audits, imports) = files_full_audited(&metadata);

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-complex-full-audited", stdout);
}

#[test]
fn mock_complex_missing_core5() {
    // (Fail) Missing an audit for the v5 version of third-core

    let mock = MockMetadata::complex();
    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-core"] = vec![full_audit(ver(DEFAULT_VER), "reviewed")];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-complex-missing-core5", stdout);
}

#[test]
fn mock_complex_missing_core10() {
    // (Fail) Missing an audit for the v10 version of third-core

    let mock = MockMetadata::complex();
    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-core"] = vec![full_audit(ver(5), "reviewed")];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-complex-missing-core10", stdout);
}

#[test]
fn mock_complex_core10_too_weak() {
    // (Fail) Criteria for core10 is too weak

    let mock = MockMetadata::complex();
    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-core"] = vec![
        full_audit(ver(DEFAULT_VER), "weak-reviewed"),
        full_audit(ver(5), "reviewed"),
    ];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-complex-core10-too-weak", stdout);
}

#[test]
fn mock_complex_core10_partially_too_weak() {
    // (Fail) Criteria for core10 is too weak for thirdA but not thirdA and thirdAB (full)

    let mock = MockMetadata::complex();
    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-core"] = vec![
        full_audit(ver(DEFAULT_VER), "weak-reviewed"),
        full_audit(ver(5), "reviewed"),
    ];

    let audit_with_weaker_req = full_audit_dep(
        ver(DEFAULT_VER),
        "reviewed",
        [("third-core", ["weak-reviewed"])],
    );
    audits.audits["thirdA"] = vec![audit_with_weaker_req.clone()];
    audits.audits["thirdAB"] = vec![audit_with_weaker_req];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("mock-complex-core10-partially-too-weak", stdout);
}

#[test]
fn mock_complex_core10_partially_too_weak_via_weak_delta() {
    // (Fail) Criteria for core10 is too weak for thirdA but not thirdA and thirdAB (weak delta)

    let mock = MockMetadata::complex();
    let metadata = mock.metadata();
    let (config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-core"] = vec![
        delta_audit(ver(5), ver(DEFAULT_VER), "weak-reviewed"),
        full_audit(ver(5), "reviewed"),
    ];

    let audit_with_weaker_req = full_audit_dep(
        ver(DEFAULT_VER),
        "reviewed",
        [("third-core", ["weak-reviewed"])],
    );
    audits.audits["thirdA"] = vec![audit_with_weaker_req.clone()];
    audits.audits["thirdAB"] = vec![audit_with_weaker_req];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!(
        "mock-complex-core10-partially-too-weak-via-weak-delta",
        stdout
    );
}

#[test]
fn mock_complex_core10_partially_too_weak_via_strong_delta() {
    // (Fail) Criteria for core10 is too weak for thirdA but not thirdA and thirdAB
    // because there's a strong delta from 5->10 but 0->5 is still weak!

    let mock = MockMetadata::complex();
    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    audits.audits["third-core"] = vec![
        delta_audit(ver(5), ver(DEFAULT_VER), "reviewed"),
        full_audit(ver(5), "weak-reviewed"),
    ];

    let audit_with_weaker_req = full_audit_dep(
        ver(DEFAULT_VER),
        "reviewed",
        [("third-core", ["weak-reviewed"])],
    );
    audits.audits["thirdA"] = vec![audit_with_weaker_req.clone()];
    audits.audits["thirdAB"] = vec![audit_with_weaker_req];

    config.policy.insert(
        "firstA".to_string(),
        dep_policy([("third-core", ["weak-reviewed"])]),
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!(
        "mock-complex-core10-partially-too-weak-via-strong-delta",
        stdout
    );
}

#[test]
fn mock_simple_policy_root_too_strong() {
    // (Fail) Root policy is too strong

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config
        .policy
        .insert("root-package".to_string(), self_policy(["strong-reviewed"]));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-root-too-strong", stdout);
}

#[test]
fn mock_simple_policy_root_weaker() {
    // (Pass) Root policy weaker than necessary

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config
        .policy
        .insert("root-package".to_string(), self_policy(["weak-reviewed"]));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-root-weaker", stdout);
}

#[test]
fn mock_simple_policy_first_too_strong() {
    // (Fail) First-party policy is too strong

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config
        .policy
        .insert("first-party".to_string(), self_policy(["strong-reviewed"]));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-too-strong", stdout);
}

#[test]
fn mock_simple_policy_first_weaker() {
    // (Pass) First-party policy weaker than necessary

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config
        .policy
        .insert("first-party".to_string(), self_policy(["weak-reviewed"]));

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-weaker", stdout);
}

#[test]
fn mock_simple_policy_root_dep_weaker() {
    // (Pass) root->first-party policy weaker than necessary

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "root-package".to_string(),
        dep_policy([("first-party", ["weak-reviewed"])]),
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-root-dep-weaker", stdout);
}

#[test]
fn mock_simple_policy_root_dep_too_strong() {
    // (Pass) root->first-party policy stronger than necessary

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "root-package".to_string(),
        dep_policy([("first-party", ["strong-reviewed"])]),
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-root-dep-too-strong", stdout);
}

#[test]
fn mock_simple_policy_first_dep_weaker() {
    // (Pass) first-party->third-party policy weaker than necessary

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "first-party".to_string(),
        dep_policy([("third-party1", ["weak-reviewed"])]),
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-dep-weaker", stdout);
}

#[test]
fn mock_simple_policy_first_dep_too_strong() {
    // (Pass) first-party->third-party policy too strong

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "first-party".to_string(),
        dep_policy([("third-party1", ["strong-reviewed"])]),
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-dep-too-strong", stdout);
}

#[test]
fn mock_simple_policy_first_dep_stronger() {
    // (Pass) first-party->third-party policy stronger but satisfied

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "first-party".to_string(),
        dep_policy([("third-party2", ["strong-reviewed"])]),
    );

    audits.audits["third-party2"] = vec![full_audit(ver(DEFAULT_VER), "strong-reviewed")];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-dep-stronger", stdout);
}

#[test]
fn mock_simple_policy_first_dep_weaker_needed() {
    // (Pass) first-party->third-party policy weaker out of necessity

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "first-party".to_string(),
        dep_policy([("third-party1", ["weak-reviewed"])]),
    );

    audits.audits["third-party1"] = vec![full_audit(ver(DEFAULT_VER), "weak-reviewed")];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-dep-weaker-needed", stdout);
}

#[test]
fn mock_simple_policy_first_dep_extra() {
    // (Pass) first-party->third-party policy has extra satisfied criteria

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "first-party".to_string(),
        dep_policy([("third-party2", ["reviewed", "fuzzed"])]),
    );

    audits.audits["third-party2"] = vec![
        full_audit(ver(DEFAULT_VER), "reviewed"),
        full_audit(ver(DEFAULT_VER), "fuzzed"),
    ];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-dep-extra", stdout);
}

#[test]
fn mock_simple_policy_first_dep_extra_missing() {
    // (Fail) first-party->third-party policy has extra unsatisfied criteria

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "first-party".to_string(),
        dep_policy([("third-party2", ["reviewed", "fuzzed"])]),
    );

    audits.audits["third-party2"] = vec![full_audit(ver(DEFAULT_VER), "reviewed")];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-dep-extra-missing", stdout);
}

#[test]
fn mock_simple_policy_first_extra_partially_missing() {
    // (Fail) first-party policy has extra unsatisfied criteria

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, mut audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "first-party".to_string(),
        self_policy(["reviewed", "fuzzed"]),
    );

    audits.audits["third-party2"] = vec![
        full_audit(ver(DEFAULT_VER), "reviewed"),
        full_audit(ver(DEFAULT_VER), "fuzzed"),
    ];

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-extra-partially-missing", stdout);
}

#[test]
fn mock_simple_first_policy_redundant() {
    // (Pass) first-party policy has redundant implied things

    let mock = MockMetadata::simple();
    let metadata = mock.metadata();
    let (mut config, audits, imports) = files_full_audited(&metadata);

    config.policy.insert(
        "first-party".to_string(),
        self_policy(["reviewed", "weak-reviewed"]),
    );

    let report = crate::resolver::resolve(&metadata, &config, &audits, &imports, false);
    let stdout = get_report(&metadata, report);
    insta::assert_snapshot!("simple-policy-first-policy-redundant", stdout);
}

// TESTING BACKLOG:
//
// * custom policies
//   * basic
//   * custom criteria to third-party
//   * custom criteria to first-party
//   * two first-parties depending on the same thing
//      * which is itself first-party
//      * which is a third-party
//      * with different policies
//         * where only the weaker one is satisfied (fail but give good diagnostic)
//
// * delta cycles (don't enter infinite loops!)
//   * no-op delta (x -> x), should diagnostic for this..?
//   * A -> B -> A
//   * A -> B -> C -> A
//   * try both success and failure cases, failure more likely to infinite loop
//
// * unaudited entry dependencies
//   * (fail) dep unaudited but claims too-weak criteria
//   * (pass) dep unaudited and his exactly the right criteria
//   * (pass) dep unaudited and has superset of criteria
//   * all of the above but for dep-audited
//   * dep has no audits
//
// * interesting situations for vet-init
//   * build-deps
//   * dev-deps
//   * "cargo resolver 2.0 situations"?
//
// * foreign mappings
//   * only using builtins
//   * 1:1 explicit mappings
//   * asymmetric cases
//   * missing mappings
//   * foreign has criteria with the same name, unmapped (don't accidentally mix it up)
//   * foreign has criteria with the same name, mapped to that name
//   * foreign has criteria with the same name, mapped to a different name
//
// * detecting unused unaudited entries
//   * no crate in the project with that name (removed dep)
//   * completely unreachable unaudited entry
//   * unaudited entry is reachable but not needed
//   * there is a full audit for exactly the unaudited entry
//   * this is a delta chain that passes through the unaudited entry
//   * situations that shouldn't warn
//     * two versions of the same crate, one needs an unaudited, the other doesn't
//     * two versions and two unauditeds, each used by one of them
//     * two unauditeds, one is needed, the other isn't (warn about exactly one!)
//
// * violations
//   * matching the current version
//   * matching an unaudited entry
//   * matching a delta audit (from)
//   * matching a delta audit (to)
//   * matching a full audit
//   * violations "masking" out higher criteria but preserving lower criteria?
//
// * misc
//   * git deps are first party but not in workspace
//   * path deps are first party but not in workspace
//   * multiple root packages
//   * weird workspaces
//   * running from weird directories
//   * a node explicitly setting all its dependency_criteria to "no reqs"
//     * ...should this just be an error? that feels wrong to do. otherwise:
//       * with perfectly fine children
//       * with children that fail to validate at all
//
// * malformed inputs:
//   * no default criteria specified
//   * referring to non-existent criteria
//   * referring to non-existent crates (in crates.io? or just in our dep graph?)
//   * referring to non-existent versions?
//   * Bad delta syntax
//   * Bad version syntax
//   * entries in tomls that don't map to anything (at least warn to catch typos?)
//     * might be running an old version of cargo-vet on a newer repo?
//
// * builtin-criteria..?
//
// * dev-deps
//
// * build-deps
//