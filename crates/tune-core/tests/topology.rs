use std::{
    path::{Path, PathBuf},
    process::Command,
};
use tune_core::topology::{
    Edge, EdgeKind, Entity, EntityKind, FailureCode, ScopeStatus, TopologyLimits, TopologySnapshot, canonical_id,
    parsers::{parse_cargo, parse_go, parse_node, parse_python, parse_workflow},
};

#[test]
fn canonical_ids_are_stable_and_do_not_embed_origin_text() {
    let a = canonical_id(EntityKind::Repository, "ssh://user:token@example.com/acme/demo.git?secret=x#frag");
    let b = canonical_id(EntityKind::Repository, "ssh://user:token@example.com/acme/demo.git?secret=x#frag");
    assert_eq!(a, b);
    assert!(a.starts_with("kt.topology.v1.repository."));
    assert!(!a.contains("token"));
    assert!(!a.contains("example"));
}

#[test]
fn canonical_keys_separate_labels_checkouts_paths_and_platforms() {
    use tune_core::topology::{canonical_edge_id, canonical_entity_id, normalize_git_origin, normalize_path};
    let origin = normalize_git_origin("ssh://person:secret@example.com/group/demo.git?token=x#part").unwrap();
    assert_eq!(origin, "example.com/group/demo");
    assert_eq!(origin, normalize_git_origin("https://example.com/group/demo.git").unwrap());
    assert_eq!(origin, normalize_git_origin("git@example.com:group/demo.git").unwrap());
    let repository = canonical_entity_id(EntityKind::Repository, &origin);
    assert_eq!(repository, canonical_entity_id(EntityKind::Repository, &origin));
    assert_ne!(repository, canonical_entity_id(EntityKind::Checkout, "environment-a|/private/one"));
    assert_eq!(canonical_entity_id(EntityKind::Package, "workspace|one/path|same"), canonical_entity_id(EntityKind::Package, "workspace|one/path|same"));
    assert_ne!(canonical_entity_id(EntityKind::Package, "workspace|one/path|same"), canonical_entity_id(EntityKind::Package, "workspace|two/path|same"));
    assert_eq!(normalize_path("C:\\Work\\Demo", "windows"), normalize_path("c:/work/demo", "windows"));
    assert_ne!(normalize_path("/Work/Demo", "posix"), normalize_path("/work/demo", "posix"));
    assert_ne!(canonical_edge_id(EdgeKind::DeclaresDependency, "a:b", "c"), canonical_edge_id(EdgeKind::DeclaresDependency, "a", "b:c"));
}

fn snapshot(status: ScopeStatus, entities: Vec<&str>) -> TopologySnapshot {
    TopologySnapshot {
        scope_id: "fixture-scope".into(),
        status,
        entities: entities.into_iter().map(|label| Entity::new(EntityKind::Package, label)).collect(),
        edges: vec![Edge::new(EdgeKind::DeclaresDependency, "from", "to")],
        failures: vec![],
        counters: Default::default(),
        observed_at_ms: 0,
    }
}

#[test]
fn parsers_collect_declared_dependencies_without_self_edges() {
    let cargo = parse_cargo("[package]\nname = \"demo\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\n[dev-dependencies]\ntest-kit = \"2\"\n[workspace]\nmembers = [\"member\"]\n").unwrap();
    assert_eq!(cargo.name, "demo");
    assert_eq!(cargo.dependencies.len(), 2);
    assert!(cargo.dependencies.iter().any(|d| d.name == "serde" && d.kind == "normal"));
    assert!(cargo.dependencies.iter().any(|d| d.name == "test-kit" && d.kind == "dev"));
    assert!(cargo.dependencies.iter().all(|d| d.name != "demo"));

    let node = parse_node(r#"{"name":"demo","dependencies":{"left-pad":"1"},"peerDependencies":{"peer":"2"}}"#).unwrap();
    assert_eq!(node.dependencies.len(), 2);
    assert!(node.dependencies.iter().any(|d| d.name == "left-pad" && d.kind == "normal"));
    assert!(node.dependencies.iter().any(|d| d.name == "peer" && d.kind == "peer"));

    let python = parse_python("[project]\nname = \"demo\"\ndependencies = [\"requests>=2\"]\n[project.optional-dependencies]\ntest = [\"pytest>=8\"]\n[build-system]\nrequires = [\"setuptools>=1\"]\n").unwrap();
    assert_eq!(python.dependencies.len(), 3);
    assert!(python.dependencies.iter().any(|d| d.name == "requests" && d.kind == "normal"));
    assert!(python.dependencies.iter().any(|d| d.name == "pytest" && d.kind == tune_core::topology::DependencyKind::PythonOptional));
    assert!(python.dependencies.iter().any(|d| d.name == "setuptools" && d.kind == "build"));

    let go = parse_go("module example.com/demo\n\nrequire (\n example.com/lib v1.2.3\n)\nreplace example.com/lib => ../lib\n").unwrap();
    assert_eq!(go.name, "example.com/demo");
    assert_eq!(go.dependencies.len(), 1);
    assert_eq!(go.dependencies[0].name, "example.com/lib");
    assert!(go.dependencies[0].local);
    assert!(go.dependencies.iter().all(|d| d.name != go.name));
}

#[test]
fn manifest_dependency_kinds_and_sources_are_typed() {
    use tune_core::topology::{DependencyKind, DependencySource};
    let cargo = parse_cargo("[package]\nname='member'\n[dependencies]\nrenamed = { package='actual', git='https://token@example.com/actual.git?secret=x' }\nlocal = { path='../local' }\n[build-dependencies]\nbuilder='1'\n[target.'cfg(unix)'.dependencies]\nplatform='1'\n").unwrap();
    assert!(cargo.dependencies.iter().any(|d| d.name == "actual" && d.kind == DependencyKind::Normal && d.source == DependencySource::Git));
    assert!(cargo.dependencies.iter().any(|d| d.name == "local" && d.source == DependencySource::Path));
    assert!(cargo.dependencies.iter().any(|d| d.name == "builder" && d.kind == DependencyKind::Build));
    assert!(cargo.dependencies.iter().any(|d| d.name == "platform" && d.kind == DependencyKind::Normal && d.target.as_deref() == Some("cfg(unix)")));
    assert!(cargo.dependencies.iter().all(|d| !d.spec.as_deref().unwrap_or_default().contains("token")));
    let cargo_target = parse_cargo("[package]\nname='member'\n[target.'cfg(windows)'.dev-dependencies]\ntest = { package='test-real', path='../private' }\n[target.'cfg(windows)'.build-dependencies]\nbuild = { git='https://token@example.com/build.git?secret=x', rev='deadbeef' }\n[workspace.dependencies]\nshared = { git='ssh://token@example.com/shared.git#fragment', tag='v1' }\n").unwrap();
    assert!(cargo_target.dependencies.iter().any(|d| d.name == "test-real"
        && d.kind == DependencyKind::Dev
        && d.source == DependencySource::Path
        && d.target.as_deref() == Some("cfg(windows)")
        && d.spec.as_deref() == Some("path:../private")));
    let build = cargo_target.dependencies.iter().find(|d| d.name == "build").unwrap();
    assert_eq!(build.kind, DependencyKind::Build);
    assert_eq!(build.origin.as_deref(), Some("example.com/build"));
    assert_eq!(build.spec.as_deref(), Some("rev:deadbeef"));
    assert!(
        cargo_target
            .workspace_dependencies
            .iter()
            .any(|d| d.name == "shared" && d.origin.as_deref() == Some("example.com/shared") && d.spec.as_deref() == Some("tag:v1"))
    );

    let node = parse_node(r#"{"name":"pkg","dependencies":{"alias":"npm:actual@1","file":"file:../file","work":"workspace:*","git":"git+https://token@example.com/a.git"},"optionalDependencies":{"opt":"1"}}"#).unwrap();
    assert!(node.dependencies.iter().any(|d| d.name == "alias" && d.source == DependencySource::NpmAlias));
    assert!(node.dependencies.iter().any(|d| d.name == "file" && d.source == DependencySource::Path));
    assert!(node.dependencies.iter().any(|d| d.name == "work" && d.source == DependencySource::Workspace));
    assert!(node.dependencies.iter().any(|d| d.name == "git" && d.source == DependencySource::Git));
    assert!(node.dependencies.iter().any(|d| d.kind == DependencyKind::Optional));
}

#[test]
fn parser_keeps_workspace_extra_go_replace_and_sanitized_remote_reference() {
    use tune_core::topology::{DependencyKind, DependencySource};
    let cargo = parse_cargo("[workspace.dependencies]\nshared='1'\n[dependencies]\nshared={ workspace=true }\n").unwrap();
    assert!(cargo.dependencies.iter().any(|d| d.name == "shared" && d.kind == DependencyKind::Normal && d.source == DependencySource::Workspace));
    assert!(cargo.workspace_dependencies.iter().any(|d| d.name == "shared" && d.kind == DependencyKind::Normal && d.source == DependencySource::Registry));
    let python = parse_python("[project]\nname='p'\n[project.optional-dependencies]\ncli=['tool @ https://token@example.com/tool.whl?x=1#frag']\n").unwrap();
    let extra = python.dependencies.iter().find(|d| d.name == "tool").unwrap();
    assert_eq!(extra.extra.as_deref(), Some("cli"));
    assert_eq!(extra.source, DependencySource::Git);
    assert_eq!(extra.origin.as_deref(), Some("example.com/tool.whl"));
    let go = parse_go(
        "module example.com/p\nrequire example.com/lib v1.0.0\nreplace (\n example.com/lib => ../fallback\n example.com/lib v1.0.0 => ../exact\n example.com/lib v1.1.0 => ../other\n example.com/unused v1.0.0 => ../unused\n)\n",
    )
    .unwrap();
    let lib = go.dependencies.iter().find(|d| d.name == "example.com/lib").unwrap();
    assert_eq!(lib.source, DependencySource::Path);
    assert_eq!(lib.resolved.as_deref(), Some("path:../exact"), "an exact left-hand version wins over an unversioned replacement");
    assert_eq!(lib.spec.as_deref(), Some("path:../exact"), "the resolver needs the sanitized local replacement path, not the required module version");
    assert_eq!(go.dependencies.len(), 1, "replace only must not create a dependency");
}

#[test]
fn manifest_facts_reject_oversized_fields_and_declaration_sets() {
    let too_long_name = format!(r#"{{"name":"{}"}}"#, "n".repeat(8 * 1024 + 1));
    assert_eq!(parse_node(&too_long_name).unwrap_err().code, FailureCode::LimitReached);
    let declarations = (0..4_097).map(|index| format!(r#""dep{index}":"1""#)).collect::<Vec<_>>().join(",");
    let manifest = format!(r#"{{"name":"fixture","dependencies":{{{declarations}}}}}"#);
    assert_eq!(parse_node(&manifest).unwrap_err().code, FailureCode::LimitReached);
}

#[test]
fn parser_facts_never_persist_absolute_paths_or_direct_url_credentials() {
    use tune_core::{
        db::Store,
        topology::{SourceCacheEntry, SourceCacheKey, SourceFacts},
    };
    let canary = "CANARY_PARSER_PRIVATE";
    let cargo = parse_cargo("[package]\nname='demo'\n[dependencies]\nwin={path='C:\\\\Users\\\\username\\\\CANARY_PARSER_PRIVATE\\\\pkg'}\n").unwrap();
    let node = parse_node(r#"{"name":"demo","dependencies":{"win":"file:C:\\Users\\username\\CANARY_PARSER_PRIVATE\\pkg","posix":"file:/Users/username/CANARY_PARSER_PRIVATE/pkg","git":"git+https://user:CANARY_PARSER_PRIVATE@example.com/repo.git?token=CANARY_PARSER_PRIVATE#safe"}}"#).unwrap();
    let python = parse_python("[project]\nname='demo'\ndependencies=['pkg@https://user:CANARY_PARSER_PRIVATE@example.com/pkg.whl?token=CANARY_PARSER_PRIVATE', 'local @ file:/Users/username/CANARY_PARSER_PRIVATE/pkg']\n").unwrap();
    for facts in [SourceFacts::from(cargo), SourceFacts::from(node), SourceFacts::from(python)] {
        assert!(!serde_json::to_string(&facts).unwrap().contains(canary));
    }
    let directory = std::env::temp_dir().join(format!("tune-topology-facts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    let db = Store::open(&directory).unwrap();
    let lease = db.acquire_topology_lease_fenced("facts", 1, 10).unwrap().unwrap();
    let facts = SourceFacts::from(parse_node(r#"{"name":"demo","dependencies":{"win":"file:C:\\Users\\username\\CANARY_PARSER_PRIVATE\\pkg"}}"#).unwrap());
    let key = SourceCacheKey {
        source_id: "fixture-package".into(),
        content_sha256: "fixture-hash".into(),
        parser_version: "fixture-parser".into(),
        kind: "node".into(),
    };
    db.save_source_cache_fenced(&lease, &SourceCacheEntry { key: key.clone(), revision: None, fetched_at_ms: 1, facts }, 1).unwrap();
    drop(db);
    let reopened = Store::open(&directory).unwrap();
    assert!(!serde_json::to_string(&reopened.source_cache(&key).unwrap()).unwrap().contains(canary));
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn workflow_preserves_yaml_12_on_and_rejects_budget_overflow() {
    let workflow = parse_workflow("on:\n  push:\n    branches: [main]\njobs:\n  check:\n    if: github.ref == 'refs/heads/main'\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - uses: ./local-action\n", &TopologyLimits::default()).unwrap();
    assert!(workflow.triggers.contains(&"push".to_string()));
    assert_eq!(workflow.jobs.len(), 1);
    assert_eq!(workflow.jobs[0].uses, vec!["actions/checkout@v4", "./local-action"]);
    assert_eq!(workflow.jobs[0].job_uses, None);
    assert_eq!(workflow.jobs[0].step_uses, vec!["actions/checkout@v4", "./local-action"]);
    assert_eq!(workflow.jobs[0].if_condition.as_deref(), Some("github.ref == 'refs/heads/main'"));

    let reusable = parse_workflow(
        "on: workflow_call\njobs:\n  call:\n    uses: example/flow/.github/workflows/run.yml@v1\n    runs-on: ignored\n",
        &TopologyLimits::default(),
    )
    .unwrap();
    assert_eq!(reusable.jobs[0].uses, vec!["example/flow/.github/workflows/run.yml@v1"]);
    assert_eq!(reusable.jobs[0].job_uses.as_deref(), Some("example/flow/.github/workflows/run.yml@v1"));
    assert!(reusable.jobs[0].step_uses.is_empty());

    let conditions = parse_workflow(
        "on:\n  push:\n    branches: [main]\n    paths: ['src/**']\njobs:\n  build:\n    steps: [{run: build, if: github.ref}]\n  test:\n    needs: build\n    strategy: {matrix: {os: [ubuntu]}}\n    steps: [{uses: actions/checkout@v4}]\n",
        &TopologyLimits::default(),
    )
    .unwrap();
    assert_eq!(conditions.trigger_filters["push"]["branches"], vec!["main"]);
    assert_eq!(conditions.jobs.iter().find(|job| job.id == "test").unwrap().needs, vec!["build"]);
    assert!(conditions.coverage.iter().any(|reason| reason.contains("matrix")));
    assert!(conditions.coverage.iter().any(|reason| reason.contains("step conditions")));

    let limits = TopologyLimits { workflow_bytes: 8, ..Default::default() };
    let err = parse_workflow("on: push\n", &limits).unwrap_err();
    assert_eq!(err.code, FailureCode::LimitReached);
}

#[test]
fn workflow_parser_enforces_yaml_boundaries_without_source_leaks() {
    let merged = parse_workflow(
        "on: push\ndefaults: &defaults\n  runs-on: ubuntu-latest\njobs:\n  test:\n    <<: *defaults\n    if: github.ref == 'refs/heads/main'\n    environment:\n      name: staging\n    steps:\n      - uses: actions/checkout@v4\n",
        &TopologyLimits::default(),
    ).unwrap();
    assert_eq!(merged.jobs[0].runs_on, vec!["ubuntu-latest"]);
    assert_eq!(merged.jobs[0].environment.as_deref(), Some("staging"));
    assert_eq!(merged.jobs[0].step_uses, vec!["actions/checkout@v4"]);

    let quoted = parse_workflow(
        "on: \"push & keep * literal\"\n# an & and * comment are valid\njobs:\n  check:\n    environment: test\n    runs-on: [ubuntu-latest, macos-latest]\n    steps:\n      - uses: ./action-*\n",
        &TopologyLimits::default(),
    ).unwrap();
    assert_eq!(quoted.triggers, vec!["push & keep * literal"]);
    assert_eq!(quoted.jobs[0].runs_on, vec!["ubuntu-latest", "macos-latest"]);
    assert_eq!(quoted.jobs[0].environment.as_deref(), Some("test"));

    for (input, code) in [
        ("on: push\non: pull_request\n", FailureCode::ParseError),
        ("on: push\n---\non: pull_request\n", FailureCode::LimitReached),
        ("on: !include another.yml\n", FailureCode::Unsupported),
        ("on: !custom push\n", FailureCode::Unsupported),
        ("? [not, a, string]\n: value\n", FailureCode::ParseError),
    ] {
        let err = parse_workflow(input, &TopologyLimits::default()).unwrap_err();
        assert_eq!(err.code, code, "input: {input:?}");
        assert!(!err.message.contains(input));
        assert!(err.line.is_some(), "expected a safe location for {input:?}");
        assert!(err.column.is_some(), "expected a safe location for {input:?}");
    }

    for input in ["plain scalar\n", "on: push\n", "on: push\njobs: []\n", "on: push\njobs: {check: scalar}\n"] {
        let err = parse_workflow(input, &TopologyLimits::default()).unwrap_err();
        assert_eq!(err.code, FailureCode::Unsupported, "input: {input:?}");
        assert!(!err.message.contains(input));
    }

    let mut aliases = String::from("on: push\nbase: &base {value: ok}\n");
    for index in 0..65 {
        aliases.push_str(&format!("a{index}: *base\n"));
    }
    assert_eq!(parse_workflow(&aliases, &TopologyLimits::default()).unwrap_err().code, FailureCode::LimitReached);

    let mut deep = String::from("on: push\nroot:");
    for level in 0..65 {
        deep.push('\n');
        deep.push_str(&"  ".repeat(level + 1));
        deep.push_str("child:");
    }
    deep.push_str(" value\n");
    assert_eq!(parse_workflow(&deep, &TopologyLimits::default()).unwrap_err().code, FailureCode::LimitReached);

    let nodes = format!("on: push\nvalues: [{}]\n", std::iter::repeat_n("x", 20_001).collect::<Vec<_>>().join(", "));
    assert_eq!(parse_workflow(&nodes, &TopologyLimits::default()).unwrap_err().code, FailureCode::LimitReached);

    let file_limit = TopologyLimits { workflow_bytes: 512 * 1024, ..Default::default() };
    let mut at_limit = String::from("on: push\njobs:\n  check:\n    steps: []\n#");
    at_limit.push_str(&"x".repeat(file_limit.workflow_bytes - at_limit.len()));
    assert!(parse_workflow(&at_limit, &file_limit).is_ok());
    at_limit.push('x');
    assert_eq!(parse_workflow(&at_limit, &file_limit).unwrap_err().code, FailureCode::LimitReached);

    let too_long = format!("on: push\njobs:\n  check:\n    if: \"{}\"\n    steps: []\n", "x".repeat(8 * 1024 + 1));
    assert_eq!(parse_workflow(&too_long, &TopologyLimits::default()).unwrap_err().code, FailureCode::LimitReached);
}

fn fixture_repo(path: &Path, origin: &str) {
    std::fs::create_dir_all(path).unwrap();
    let init = Command::new("git").args(["init", "-q", path.to_str().unwrap()]).output().unwrap();
    assert!(init.status.success());
    let remote = Command::new("git").args(["-C", path.to_str().unwrap(), "remote", "add", "origin", origin]).output().unwrap();
    assert!(remote.status.success());
}

#[test]
fn local_capture_is_bounded_and_assembles_repository_scoped_graphs() {
    use serde_json::json;
    use tune_core::topology::{
        EdgeKind,
        adapters::{assemble_capture, capture_local},
    };
    let base = std::env::temp_dir().join(format!("topology-local-fixture-{}", tune_core::db::now_ms()));
    let root = base.join("allowed");
    let first = root.join("one");
    let second = root.join("two");
    fixture_repo(&first, "https://example.com/group/project.git");
    fixture_repo(&second, "git@example.com:group/project.git");
    std::fs::create_dir_all(first.join("member")).unwrap();
    std::fs::create_dir_all(first.join(".github/workflows")).unwrap();
    std::fs::create_dir_all(first.join("target")).unwrap();
    std::fs::create_dir_all(first.join(".venv")).unwrap();
    std::fs::create_dir_all(first.join("vendor")).unwrap();
    std::fs::create_dir_all(first.join(".branch-recovery/objects")).unwrap();
    std::fs::write(first.join("Cargo.toml"), "[workspace]\nmembers = [\"member\"]\n").unwrap();
    std::fs::write(first.join("member/Cargo.toml"), "[package]\nname = \"same\"\nversion = \"0.1.0\"\n[dependencies]\nleft = \"1\"\n").unwrap();
    std::fs::write(
        first.join(".github/workflows/ci.yml"),
        "on: push\njobs:\n  check:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    )
    .unwrap();
    std::fs::write(first.join("target/package.json"), "{ invalid").unwrap();
    std::fs::write(first.join(".venv/package.json"), "{ invalid").unwrap();
    std::fs::write(first.join("vendor/package.json"), "{ invalid").unwrap();
    std::fs::write(first.join(".branch-recovery/objects/package.json"), "{ invalid").unwrap();
    std::fs::write(second.join("package.json"), "{\"name\":\"same\",\"workspaces\":[\"packages/*\"]}").unwrap();
    std::fs::create_dir_all(second.join("packages/child")).unwrap();
    std::fs::write(second.join("packages/child/package.json"), "{\"name\":\"child\"}").unwrap();
    let settings = base.join("settings.json");
    std::fs::write(&settings, "{\"safe\":\"value\",\"token\":\"private\"}").unwrap();
    let configs = vec![json!({"path": settings, "format":"json", "keys":[{"pointer":"/safe","mode":"hash"}]})];
    let capture = capture_local("fixture-node:native", std::slice::from_ref(&root), &configs, &TopologyLimits::default());
    assert!(capture.failures.is_empty(), "{:?}", capture.failures);
    assert_eq!(capture.repositories.len(), 3);
    assert!(capture.counters.entries > capture.counters.manifests, "all directory entries must count");
    assert!(capture.repositories.iter().flat_map(|repository| &repository.manifests).all(|manifest| {
        !manifest.relative_path.contains("/.venv/") && !manifest.relative_path.contains("/vendor/") && !manifest.relative_path.contains("/.branch-recovery/")
    }));
    let snapshots = assemble_capture(&capture);
    assert_eq!(snapshots.len(), 3);
    assert!(snapshots.iter().all(|snapshot| snapshot.status == ScopeStatus::Complete));
    let repository_ids =
        snapshots.iter().map(|snapshot| snapshot.entities.iter().find(|entity| entity.kind == EntityKind::Repository).unwrap().id.clone()).collect::<Vec<_>>();
    assert_eq!(repository_ids[0], repository_ids[1], "equivalent origins share only the repository identity");
    let checkout_ids =
        snapshots.iter().map(|snapshot| snapshot.entities.iter().find(|entity| entity.kind == EntityKind::Checkout).unwrap().id.clone()).collect::<Vec<_>>();
    assert_ne!(checkout_ids[0], checkout_ids[1]);
    let package_ids = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.entities.iter())
        .filter(|entity| entity.kind == EntityKind::Package && entity.label == "same")
        .map(|entity| entity.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(package_ids.len(), 2);
    assert_ne!(package_ids[0], package_ids[1]);
    assert!(snapshots.iter().flat_map(|snapshot| snapshot.entities.iter()).any(|entity| entity.kind == EntityKind::Workspace));
    assert!(snapshots.iter().flat_map(|snapshot| snapshot.edges.iter()).any(|edge| edge.kind == EdgeKind::ContainsCheckout));
    assert!(snapshots.iter().flat_map(|snapshot| snapshot.edges.iter()).any(|edge| edge.kind == EdgeKind::ContainsPackage));
    assert!(snapshots.iter().flat_map(|snapshot| snapshot.edges.iter()).any(|edge| edge.kind == EdgeKind::WorkspaceMember));
    assert!(snapshots.iter().flat_map(|snapshot| snapshot.edges.iter()).any(|edge| edge.kind == EdgeKind::UsesAction));
    assert!(snapshots.iter().flat_map(|snapshot| snapshot.edges.iter()).any(|edge| edge.kind == EdgeKind::RunsOn));
    assert!(snapshots.iter().flat_map(|snapshot| snapshot.entities.iter()).any(|entity| entity.kind == EntityKind::Configuration));

    let limited = TopologyLimits { entries_per_environment: 1, ..Default::default() };
    let limited_capture = capture_local("fixture-node:native", &[root], &[], &limited);
    assert!(limited_capture.failures.iter().any(|failure| failure.code == FailureCode::LimitReached));
    assert!(assemble_capture(&limited_capture).iter().any(|snapshot| snapshot.scope_id.ends_with(":coverage") && snapshot.status == ScopeStatus::Partial));
    let no_root = capture_local("fixture-node:native", &Vec::<PathBuf>::new(), &[], &TopologyLimits::default());
    assert_eq!(assemble_capture(&no_root)[0].status, ScopeStatus::Partial);

    // A repository walk that could not finish must be Partial, so the store
    // keeps the previously observed graph instead of deleting the missing part.
    let walk_base = std::env::temp_dir().join(format!("topology-walk-failure-{}", tune_core::db::now_ms()));
    let first = walk_base.join("first");
    let second = walk_base.join("second");
    fixture_repo(&first, "https://example.com/group/first.git");
    fixture_repo(&second, "https://example.com/group/second.git");
    std::fs::write(first.join("package.json"), r#"{"name":"first"}"#).unwrap();
    std::fs::create_dir_all(first.join("sub")).unwrap();
    std::fs::write(first.join("sub/package.json"), [0xff, 0xfe]).unwrap();
    std::fs::write(second.join("package.json"), r#"{"name":"second"}"#).unwrap();
    let walk_capture = capture_local("fixture-node:native", std::slice::from_ref(&walk_base), &[], &TopologyLimits::default());
    assert!(walk_capture.failures.iter().any(|failure| failure.code == FailureCode::ParseError));
    assert!(walk_capture.repositories.iter().any(|repository| repository.failures.iter().any(|failure| failure.code == FailureCode::ParseError)));
    let walk_snapshots = assemble_capture(&walk_capture);
    let checkout_for = |label: &str| {
        walk_snapshots
            .iter()
            .find(|snapshot| {
                snapshot.scope_id.contains(":checkout:") && snapshot.entities.iter().any(|entity| entity.kind == EntityKind::Package && entity.label == label)
            })
            .unwrap()
    };
    let incomplete = checkout_for("first");
    assert_eq!(incomplete.status, ScopeStatus::Partial);
    assert!(incomplete.failures.iter().any(|failure| failure.code == FailureCode::ParseError));
    assert_eq!(checkout_for("second").status, ScopeStatus::Complete);
    let _ = std::fs::remove_dir_all(walk_base);
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn environment_coverage_failure_does_not_downgrade_completed_checkout_scope() {
    use tune_core::topology::{
        Failure,
        adapters::{assemble_capture, capture_local},
    };
    let base = std::env::temp_dir().join(format!("topology-coverage-{}", tune_core::db::now_ms()));
    let repo = base.join("repo");
    fixture_repo(&repo, "https://example.com/complete.git");
    std::fs::write(repo.join("package.json"), "{\"name\":\"complete\"}").unwrap();
    let mut capture = capture_local("fixture-node:native", std::slice::from_ref(&base), &[], &TopologyLimits::default());
    capture.failures.push(Failure::new(FailureCode::LimitReached, "coverage is incomplete"));
    let snapshots = assemble_capture(&capture);
    assert!(snapshots.iter().any(|snapshot| snapshot.scope_id.contains(":checkout:") && snapshot.status == ScopeStatus::Complete));
    assert!(snapshots.iter().any(|snapshot| snapshot.scope_id.ends_with(":coverage") && snapshot.status == ScopeStatus::Partial));
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn local_capture_rejects_literal_secret_configuration() {
    use serde_json::json;
    use tune_core::topology::adapters::capture_local;
    let base = std::env::temp_dir().join(format!("topology-config-fixture-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    let config = base.join("settings.json");
    std::fs::write(&config, "{\"token\":\"private\"}").unwrap();
    let capture = capture_local(
        "fixture-node:native",
        std::slice::from_ref(&base),
        &[json!({"path":config,"keys":[{"pointer":"/token","mode":"literal"}]})],
        &TopologyLimits::default(),
    );
    assert!(capture.failures.iter().any(|failure| failure.code == FailureCode::PermissionDenied));
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn private_topology_configuration_is_opt_in_and_has_no_default_roots() {
    let config = tune_core::nodes::parse_config(r#"{"topology":{"enabled":true,"owners":["example-owner"]},"schedule":{"topology_minutes":30},"nodes":[{"id":"fixture-node","alias":"fixture-alias","os":"macos","topology":{"environments":[{"id":"native","roots":["/fixture/root"]}]}}]}"#, Path::new("nodes.json"), "other").unwrap();
    assert!(config.topology.enabled);
    assert_eq!(config.topology.owners, vec!["example-owner"]);
    assert_eq!(config.schedule.unwrap()["topology_minutes"], 30);
    assert_eq!(config.nodes[0].topology()["environments"][0]["roots"][0], "/fixture/root");
}

#[test]
fn topology_configuration_rejects_invalid_limits_and_owners() {
    for input in [
        r#"{"topology":{"limits":{"file_bytes":18446744073709551615}},"nodes":[]}"#,
        r#"{"topology":{"limits":{"depth":65}},"nodes":[]}"#,
        r#"{"topology":{"github":{"cache_minutes":18446744073709551615}},"nodes":[]}"#,
        r#"{"topology":{"limits":{"file_bytes":1024,"workflow_bytes":2048}},"nodes":[]}"#,
        r#"{"topology":{"enabled":true,"owners":[""],"limits":{"depth":0}},"nodes":[]}"#,
        r#"{"topology":{"enabled":true,"owners":"owner","limits":{"file_bytes":0}},"nodes":[]}"#,
        r#"{"topology":{"enabled":true,"owners":["owner"],"limits":{"local_timeout_seconds":0}},"nodes":[]}"#,
    ] {
        assert!(tune_core::nodes::parse_config(input, Path::new("nodes.json"), "fixture").is_err());
    }
}

#[test]
fn complete_diffs_are_semantic_and_partial_does_not_remove_previous_success() {
    let store = tune_core::db::Store::open_in_memory().unwrap();
    let first = store.record_topology("run-one", &snapshot(ScopeStatus::Complete, vec!["alpha"]), 1_000).unwrap();
    assert_eq!(first.len(), 2);
    let same = store.record_topology("run-two", &snapshot(ScopeStatus::Complete, vec!["alpha"]), 2_000).unwrap();
    assert!(same.is_empty());
    let partial = store.record_topology("run-three", &snapshot(ScopeStatus::Partial, vec![]), 3_000).unwrap();
    assert!(partial.is_empty());
    assert!(store.topology_entities("fixture-scope").unwrap().iter().any(|row| row["label"] == "alpha"));
}

#[test]
fn observation_time_is_persisted_but_not_a_semantic_change() {
    let store = tune_core::db::Store::open_in_memory().unwrap();
    let mut first = snapshot(ScopeStatus::Complete, vec!["alpha"]);
    first.entities[0].attributes = serde_json::json!({"content_sha256":"one","observed_at_ms":100});
    first.observed_at_ms = 100;
    assert!(!store.record_topology("first", &first, 100).unwrap().is_empty());
    let mut later = first.clone();
    later.entities[0].attributes["observed_at_ms"] = serde_json::json!(200);
    later.observed_at_ms = 200;
    assert!(store.record_topology("later", &later, 200).unwrap().is_empty());
    let view = store.topology_view().unwrap();
    assert_eq!(view["scopes"][0]["run_id"], "later");
}

#[test]
fn topology_lease_allows_one_holder_and_recovers_expired_holder() {
    let store = tune_core::db::Store::open_in_memory().unwrap();
    assert!(store.acquire_topology_lease_fenced("one", 1_000, 50).unwrap().is_some());
    assert!(store.acquire_topology_lease_fenced("two", 1_010, 50).unwrap().is_none());
    assert!(store.acquire_topology_lease_fenced("two", 1_051, 50).unwrap().is_some());
}

#[test]
fn fenced_lease_rejects_expired_holder_write_and_view_reads_without_rowid() {
    let dir = std::env::temp_dir().join(format!("katala-topology-lease-{}", tune_core::db::now_ms()));
    let first = tune_core::db::Store::open(&dir).unwrap();
    let second = tune_core::db::Store::open(&dir).unwrap();
    let old = first.acquire_topology_lease_fenced("first", 1_000, 10).unwrap().unwrap();
    let new = second.acquire_topology_lease_fenced("second", 1_011, 10).unwrap().unwrap();
    assert!(second.record_topology_fenced(&new, "run-new", &snapshot(ScopeStatus::Complete, vec!["fresh"]), 1_012).is_ok());
    assert!(first.record_topology_fenced(&old, "run-old", &snapshot(ScopeStatus::Complete, vec!["stale"]), 1_012).is_err());
    let view = second.topology_view().unwrap();
    assert!(view["entities"].as_array().unwrap().iter().any(|row| row["label"] == "fresh"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn heartbeat_and_release_are_fenced_and_invalid_inputs_rollback() {
    let store = tune_core::db::Store::open_in_memory().unwrap();
    assert!(store.acquire_topology_lease_fenced("", 1, 1).is_err());
    assert!(store.acquire_topology_lease_fenced("holder", 1, 0).is_err());
    let old = store.acquire_topology_lease_fenced("old", 1_000, 10).unwrap().unwrap();
    assert!(!store.heartbeat_topology_lease(&old, 1_010, 10).unwrap(), "expiry == now is expired");
    let current = store.acquire_topology_lease_fenced("new", 1_011, 10).unwrap().unwrap();
    assert!(!store.heartbeat_topology_lease(&old, 1_012, 10).unwrap());
    store.release_topology_lease_fenced(&old).unwrap();
    assert!(!store.acquire_topology_lease_fenced("third", 1_012, 10).unwrap().is_some(), "old release must not remove current holder");
    assert!(store.heartbeat_topology_lease(&current, 1_012, 10).unwrap());
    store.release_topology_lease_fenced(&current).unwrap();
    let later = store.acquire_topology_lease_fenced("later", 1_013, 10).unwrap().unwrap();
    assert!(later.generation > current.generation, "generation must not reset after release");
}

#[test]
fn same_origin_checkouts_and_linked_worktree_have_distinct_scopes_and_persist_together() {
    use std::collections::BTreeSet;
    use tune_core::topology::adapters::{assemble_capture, capture_local};

    let base = std::env::temp_dir().join(format!("topology-checkout-scope-{}", tune_core::db::now_ms()));
    let root = base.join("allowed");
    let first = root.join("first");
    let second = root.join("second");
    let linked = root.join("linked");
    let origin = "https://example.com/group/project.git";
    fixture_repo(&first, origin);
    fixture_repo(&second, origin);
    for path in [&first, &second] {
        std::fs::write(path.join("Cargo.toml"), "[package]\nname = 'same'\nversion = '0.1.0'\n").unwrap();
    }
    let committed = Command::new("git")
        .args(["-C", first.to_str().unwrap(), "-c", "user.name=fixture", "-c", "user.email=fixture@example.com", "add", "Cargo.toml"])
        .output()
        .unwrap();
    assert!(committed.status.success());
    let committed = Command::new("git")
        .args(["-C", first.to_str().unwrap(), "-c", "user.name=fixture", "-c", "user.email=fixture@example.com", "commit", "-qm", "fixture"])
        .output()
        .unwrap();
    assert!(committed.status.success());
    let worktree = Command::new("git").args(["-C", first.to_str().unwrap(), "worktree", "add", "--detach", linked.to_str().unwrap(), "HEAD"]).output().unwrap();
    assert!(worktree.status.success(), "{}", String::from_utf8_lossy(&worktree.stderr));

    let capture = capture_local("fixture-node:native", std::slice::from_ref(&root), &[], &TopologyLimits::default());
    let snapshots = assemble_capture(&capture);
    assert_eq!(snapshots.len(), 3);
    let scope_ids = snapshots.iter().map(|snapshot| snapshot.scope_id.clone()).collect::<BTreeSet<_>>();
    assert_eq!(scope_ids.len(), 3, "each checkout needs a separate persistence scope");
    let repository_ids =
        snapshots.iter().map(|snapshot| snapshot.entities.iter().find(|entity| entity.kind == EntityKind::Repository).unwrap()).collect::<Vec<_>>();
    assert!(repository_ids.windows(2).all(|pair| pair[0].id == pair[1].id));
    assert!(repository_ids.iter().all(|repository| repository.attributes.get("head").is_none()));
    let checkout_ids =
        snapshots.iter().map(|snapshot| snapshot.entities.iter().find(|entity| entity.kind == EntityKind::Checkout).unwrap()).collect::<Vec<_>>();
    assert_eq!(checkout_ids.iter().map(|checkout| checkout.id.clone()).collect::<BTreeSet<_>>().len(), 3);
    assert!(checkout_ids.iter().all(|checkout| checkout.attributes.get("revision").is_some()));
    let package_ids = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.entities.iter())
        .filter(|entity| entity.kind == EntityKind::Package && entity.label == "same")
        .map(|entity| entity.id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(package_ids.len(), 3, "packages are observed in a checkout namespace");

    let store = tune_core::db::Store::open_in_memory().unwrap();
    for snapshot in &snapshots {
        store.record_topology("same-run", snapshot, 1_000).unwrap();
    }
    assert_eq!(store.topology_view().unwrap()["scopes"].as_array().unwrap().len(), 3);
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn resolver_only_links_observed_git_workspace_path_and_reusable_workflow_targets() {
    use tune_core::topology::adapters::{assemble_capture, capture_local};

    let base = std::env::temp_dir().join(format!("topology-resolver-{}", tune_core::db::now_ms()));
    let root = base.join("allowed");
    let app = root.join("app");
    let flow = root.join("flow");
    fixture_repo(&app, "https://github.com/acme/app.git");
    fixture_repo(&flow, "https://github.com/acme/flow.git");
    std::fs::create_dir_all(app.join("member")).unwrap();
    std::fs::create_dir_all(app.join("shared")).unwrap();
    std::fs::create_dir_all(app.join("go-app")).unwrap();
    std::fs::create_dir_all(app.join("go-shared")).unwrap();
    std::fs::create_dir_all(app.join(".github/workflows")).unwrap();
    std::fs::create_dir_all(flow.join(".github/workflows")).unwrap();
    std::fs::write(
        app.join("Cargo.toml"),
        "[workspace]\nmembers=['member','shared']\n[workspace.dependencies]\nshared={path='shared'}\n[package]\nname='app'\nversion='0.1.0'\n[dependencies]\nremote={package='flow',git='https://github.com/acme/flow.git'}\nmissing='1'\n",
    )
    .unwrap();
    std::fs::write(
        app.join("member/Cargo.toml"),
        "[package]\nname='member'\nversion='0.1.0'\n[dependencies]\nshared={workspace=true}\nlocal={package='shared',path='../shared'}\n",
    )
    .unwrap();
    std::fs::write(app.join("shared/Cargo.toml"), "[package]\nname='shared'\nversion='0.1.0'\n").unwrap();
    std::fs::write(
        app.join("go-app/go.mod"),
        "module example.com/go-app\nrequire example.com/go-shared v1.0.0\nreplace example.com/go-shared => ../go-shared\n",
    )
    .unwrap();
    std::fs::write(app.join("go-shared/go.mod"), "module example.com/go-shared\n").unwrap();
    std::fs::write(app.join(".github/workflows/call.yml"), "on: push\njobs:\n  call:\n    uses: acme/flow/.github/workflows/reuse.yml@v1\n").unwrap();
    std::fs::write(flow.join("Cargo.toml"), "[package]\nname='flow'\nversion='0.1.0'\n").unwrap();
    std::fs::write(flow.join(".github/workflows/reuse.yml"), "on: workflow_call\njobs:\n  check:\n    runs-on: ubuntu-latest\n    steps: []\n").unwrap();

    let snapshots = assemble_capture(&capture_local("fixture-node:native", &[root], &[], &TopologyLimits::default()));
    let app = snapshots
        .iter()
        .find(|snapshot| snapshot.entities.iter().any(|entity| entity.kind == EntityKind::Checkout && entity.attributes["relative_path"] == "app"))
        .unwrap();
    let resolved = app
        .entities
        .iter()
        .filter(|entity| entity.attributes["declared"] == true)
        .filter(|entity| entity.attributes["resolution"] == "resolved")
        .map(|entity| entity.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(resolved.len() >= 3, "git, workspace, and path declarations resolve only after targets are observed");
    assert!(app.edges.iter().filter(|edge| edge.kind == EdgeKind::ResolvesTo).all(|edge| resolved.contains(&edge.from)));
    let reusable =
        app.entities.iter().find(|entity| entity.kind == EntityKind::Workflow && entity.label == "acme/flow/.github/workflows/reuse.yml@v1").unwrap();
    assert_eq!(reusable.attributes["resolution"], "resolved");
    assert!(app.edges.iter().any(|edge| edge.kind == EdgeKind::ResolvesTo && edge.from == reusable.id));
    let unresolved = app.entities.iter().find(|entity| entity.kind == EntityKind::Package && entity.label == "missing").unwrap();
    assert_eq!(unresolved.attributes["resolution"], "unresolved", "labels alone never bind a target");
    assert_eq!(unresolved.attributes["unresolved_reason"], "target was not observed");
    let go_local = app
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Package
                && entity.label == "example.com/go-shared"
                && entity.attributes["declared"] == true
                && entity.attributes["source"] == "path"
        })
        .unwrap();
    assert_eq!(go_local.attributes["spec"], "path:../go-shared");
    assert_eq!(go_local.attributes["resolution"], "resolved", "the Go replacement path resolves against the go.mod directory");
    assert!(app.edges.iter().any(|edge| edge.kind == EdgeKind::ResolvesTo && edge.from == go_local.id));
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn scheduler_master_disabled_skips_topology_collection() {
    use std::sync::Arc;
    let base = std::env::temp_dir().join(format!("topology-schedule-disabled-{}", tune_core::db::now_ms()));
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("package.json"), "{\"name\":\"fixture\"}").unwrap();
    let host = tune_core::nodes::local_host();
    let config = format!(
        r#"{{"topology":{{"enabled":true,"owners":[]}},"schedule":{{"enabled":false,"topology_minutes":73}},"nodes":[{{"id":"fixture","alias":"fixture","os":"macos","local_hostname":"{host}","topology":{{"environments":[{{"id":"native","transport":"node","roots":[{}]}}]}}}}]}}"#,
        serde_json::to_string(&root.to_string_lossy()).unwrap()
    );
    let config_path = base.join("nodes.json");
    std::fs::write(&config_path, config).unwrap();
    let engine = Arc::new(tune_core::engine::Engine::open(config_path, base.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap());
    assert_eq!(engine.config().schedule.as_ref().unwrap()["topology_minutes"], 73, "the disabled scheduler still parsed its non-default topology interval");
    assert_eq!(engine.config_error(), None, "the Windows-safe JSON path fixture must parse");
    engine.tick().await;
    assert!(engine.topology_view().unwrap()["scopes"].as_array().unwrap().is_empty());
    assert_eq!(engine.with_db(|db| db.get_meta("lastTopologyAt")).unwrap(), None);
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn local_refresh_persists_fenced_cursor_after_successful_scope_save() {
    use std::sync::Arc;
    let base = std::env::temp_dir().join(format!("topology-local-engine-{}", tune_core::db::now_ms()));
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("package.json"), "{\"name\":\"fixture\"}").unwrap();
    let host = tune_core::nodes::local_host();
    let config_path = base.join("nodes.json");
    std::fs::write(
        &config_path,
        format!(
            r#"{{"topology":{{"enabled":true,"owners":[],"github":{{"enabled":false}}}},"nodes":[{{"id":"fixture","alias":"fixture","os":"macos","local_hostname":"{host}","topology":{{"environments":[{{"id":"native","transport":"node","roots":[{}]}}]}}}}]}}"#,
            serde_json::to_string(&root.to_string_lossy()).unwrap()
        ),
    )
    .unwrap();
    let engine = Arc::new(tune_core::engine::Engine::open(config_path, base.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap());
    let missing = engine.topology_refresh_nodes("test", &["missing".into()]).await;
    assert_eq!(missing["status"], "failed");
    assert_eq!(missing["failure"], "unknown_node");
    assert!(engine.topology_view().unwrap()["scopes"].as_array().unwrap().is_empty());
    let receipt = engine.topology_refresh_nodes("test", &["fixture".into()]).await;
    assert!(receipt["scopes"].as_array().unwrap().iter().any(|scope| scope["status"] == "complete"));
    assert_eq!(engine.with_db(|db| db.topology_progress("fixture:native")).unwrap().unwrap()["frames"], serde_json::json!([]));
    assert!(!engine.topology_view().unwrap()["entities"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn complete_same_generation_marks_missing_checkout_unobserved_without_deleting_graph() {
    use std::sync::Arc;
    let base = std::env::temp_dir().join(format!("topology-unobserved-checkout-{}", tune_core::db::now_ms()));
    let root = base.join("root");
    let first = root.join("first");
    let second = root.join("second");
    fixture_repo(&first, "https://example.com/group/first.git");
    fixture_repo(&second, "https://example.com/group/second.git");
    std::fs::write(first.join("package.json"), r#"{"name":"first"}"#).unwrap();
    std::fs::write(second.join("package.json"), r#"{"name":"second"}"#).unwrap();
    let host = tune_core::nodes::local_host();
    let config_path = base.join("nodes.json");
    std::fs::write(
        &config_path,
        format!(
            r#"{{"topology":{{"enabled":true,"owners":[],"github":{{"enabled":false}}}},"nodes":[{{"id":"fixture","alias":"fixture","os":"macos","local_hostname":"{host}","topology":{{"environments":[{{"id":"native","transport":"node","roots":[{}]}}]}}}}]}}"#,
            serde_json::to_string(&root.to_string_lossy()).unwrap()
        ),
    )
    .unwrap();
    let engine = Arc::new(tune_core::engine::Engine::open(config_path, base.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap());
    let first_receipt = engine.topology_refresh_nodes("test", &["fixture".into()]).await;
    assert_eq!(first_receipt["status"], "complete");
    std::fs::remove_dir_all(&second).unwrap();
    let second_receipt = engine.topology_refresh_nodes("test", &["fixture".into()]).await;
    assert_eq!(second_receipt["status"], "partial");
    let view = engine.topology_view().unwrap();
    let unobserved = view["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["status"] == "partial" && scope["failures"].to_string().contains("checkout was not observed"))
        .expect("missing checkout receives a current partial receipt");
    assert!(unobserved["scope_id"].as_str().unwrap().starts_with("fixture:native:checkout:"));
    assert!(
        view["entities"].as_array().unwrap().iter().any(|entity| entity["label"] == "second"),
        "last complete graph remains available as historical evidence"
    );
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn changed_local_generation_never_infers_checkout_absence() {
    use std::sync::Arc;
    let base = std::env::temp_dir().join(format!("topology-generation-change-{}", tune_core::db::now_ms()));
    let root = base.join("root");
    let first = root.join("first");
    let second = root.join("second");
    fixture_repo(&first, "https://example.com/group/first.git");
    fixture_repo(&second, "https://example.com/group/second.git");
    std::fs::write(first.join("package.json"), r#"{"name":"first"}"#).unwrap();
    std::fs::write(second.join("package.json"), r#"{"name":"second"}"#).unwrap();
    let host = tune_core::nodes::local_host();
    let config_path = base.join("nodes.json");
    let config = |entries: Option<usize>| {
        let limits = entries.map(|value| format!(r#","limits":{{"entries_per_environment":{value}}}"#)).unwrap_or_default();
        format!(
            r#"{{"topology":{{"enabled":true,"owners":[],"github":{{"enabled":false}}{limits}}},"nodes":[{{"id":"fixture","alias":"fixture","os":"macos","local_hostname":"{host}","topology":{{"environments":[{{"id":"native","transport":"node","roots":[{}]}}]}}}}]}}"#,
            serde_json::to_string(&root.to_string_lossy()).unwrap()
        )
    };
    std::fs::write(&config_path, config(None)).unwrap();
    let engine = Arc::new(tune_core::engine::Engine::open(config_path.clone(), base.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap());
    assert_eq!(engine.topology_refresh_nodes("test", &["fixture".into()]).await["status"], "complete");
    std::fs::remove_dir_all(&second).unwrap();
    std::fs::write(&config_path, config(Some(19_999))).unwrap();
    let receipt = engine.topology_refresh_nodes("test", &["fixture".into()]).await;
    assert_ne!(receipt["status"], "failed");
    let view = engine.topology_view().unwrap();
    assert!(view["scopes"].as_array().unwrap().iter().all(|scope| !scope["failures"].to_string().contains("checkout was not observed")));
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn disabled_remote_environment_is_receipted_without_transport() {
    use std::sync::Arc;
    let base = std::env::temp_dir().join(format!("topology-disabled-remote-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    let config_path = base.join("nodes.json");
    std::fs::write(
        &config_path,
        r#"{"topology":{"enabled":true,"owners":[]},"nodes":[{"id":"fixture","alias":"unreachable-alias","os":"macos","topology":{"environments":[{"id":"excluded","enabled":false,"transport":"ssh","roots":[]}]}}]}"#,
    )
    .unwrap();
    let engine = Arc::new(tune_core::engine::Engine::open(config_path, base.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap());
    let receipt = engine.topology_refresh("test").await;
    assert_eq!(receipt["status"], "partial", "excluded-only refresh is never a complete collection");
    assert_eq!(receipt["scopes"][0]["status"], "excluded");
    assert_eq!(engine.topology_view().unwrap()["scopes"][0]["status"], "excluded");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn dependency_declarations_keep_kind_target_and_alias_after_sqlite_reopen() {
    use tune_core::{
        db::Store,
        topology::adapters::{assemble_capture, capture_local},
    };
    let base = std::env::temp_dir().join(format!("tune-topology-declarations-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("Cargo.toml"), "[package]\nname='app'\n[dependencies]\nfirst={package='actual',git='https://example.com/actual.git',rev='one'}\nserde='1'\n[dev-dependencies]\nserde='1'\n[target.'cfg(unix)'.dependencies]\nserde='1'\n[build-dependencies]\nsecond={package='actual',git='https://example.com/actual.git',rev='two'}\n").unwrap();
    let snapshots = assemble_capture(&capture_local("fixture:native", std::slice::from_ref(&base), &[], &TopologyLimits::default()));
    let snapshot = snapshots.iter().find(|snapshot| snapshot.scope_id.contains(":checkout:")).unwrap();
    let declarations = snapshot.entities.iter().filter(|entity| entity.attributes["source"].is_string()).collect::<Vec<_>>();
    assert_eq!(declarations.len(), 5);
    assert_eq!(declarations.iter().filter(|entity| entity.attributes["name"] == "serde").count(), 3);
    assert!(declarations.iter().any(|entity| entity.attributes["alias"] == "first" && entity.attributes["name"] == "actual"));
    assert!(declarations.iter().any(|entity| entity.attributes["alias"] == "second" && entity.attributes["name"] == "actual"));
    let directory = base.join("db");
    let db = Store::open(&directory).unwrap();
    db.record_topology("run", snapshot, 1).unwrap();
    drop(db);
    let reopened = Store::open(&directory).unwrap();
    let stored = reopened.topology_entities(&snapshot.scope_id).unwrap();
    assert_eq!(
        stored
            .iter()
            .filter(|entity| entity["attributes"]
                .as_str()
                .and_then(|attributes| serde_json::from_str::<serde_json::Value>(attributes).ok())
                .is_some_and(|attributes| attributes["source"].is_string()))
            .count(),
        5
    );
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn workflow_job_needs_becomes_observed_cicd_edge() {
    use tune_core::topology::{
        EdgeKind,
        adapters::{assemble_capture, capture_local},
    };
    let base = std::env::temp_dir().join(format!("tune-topology-needs-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(base.join(".github/workflows")).unwrap();
    std::fs::write(
        base.join(".github/workflows/check.yml"),
        "on:\n  push:\n    branches: [main]\njobs:\n  build:\n    runs-on: ubuntu\n    steps: []\n  test:\n    needs: build\n    steps: []\n",
    )
    .unwrap();
    let snapshots = assemble_capture(&capture_local("fixture:native", std::slice::from_ref(&base), &[], &TopologyLimits::default()));
    let snapshot = snapshots.iter().find(|snapshot| snapshot.scope_id.contains(":checkout:")).unwrap();
    assert!(snapshot.edges.iter().any(|edge| edge.kind == EdgeKind::NeedsJob));
    let workflow = snapshot.entities.iter().find(|entity| entity.kind == EntityKind::Workflow).unwrap();
    assert_eq!(workflow.attributes["trigger_filters"]["push"]["branches"][0], "main");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn local_directory_cursor_advances_through_low_entry_budget() {
    use tune_core::topology::adapters::capture_local_with_progress;
    let base = std::env::temp_dir().join(format!("tune-topology-progress-{}", tune_core::db::now_ms()));
    for name in ["one", "two", "three"] {
        let checkout = base.join(name);
        fixture_repo(&checkout, &format!("https://example.com/group/{name}.git"));
        std::fs::write(checkout.join("package.json"), format!(r#"{{"name":"{name}"}}"#)).unwrap();
    }
    let limits = TopologyLimits { entries_per_environment: 3, ..TopologyLimits::default() };
    let (first, cursor) = capture_local_with_progress("fixture:native", std::slice::from_ref(&base), &[], &limits, None);
    assert!(first.failures.iter().any(|failure| failure.code == FailureCode::LimitReached));
    assert_eq!(cursor["next_root"], 0);
    assert!(cursor["frames"].as_array().is_some_and(|frames| !frames.is_empty()));
    assert!(first.repositories.iter().flat_map(|repo| &repo.manifests).any(|manifest| manifest.content.contains("one")));

    let (second, cursor) = capture_local_with_progress("fixture:native", std::slice::from_ref(&base), &[], &limits, Some(&cursor));
    assert!(second.repositories.iter().flat_map(|repo| &repo.manifests).any(|manifest| manifest.content.contains("three")));
    let (third, cursor) = capture_local_with_progress("fixture:native", std::slice::from_ref(&base), &[], &limits, Some(&cursor));
    assert!(third.repositories.iter().flat_map(|repo| &repo.manifests).any(|manifest| manifest.content.contains("two")));
    assert_eq!(cursor["frames"], serde_json::json!([]));
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn local_root_budget_rotates_all_roots_and_reports_partial_coverage() {
    use tune_core::topology::adapters::capture_local_with_progress;
    let base = std::env::temp_dir().join(format!("tune-topology-root-rotation-{}", tune_core::db::now_ms()));
    let mut roots = Vec::new();
    for name in ["one", "two", "three"] {
        let root = base.join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package.json"), format!(r#"{{"name":"{name}"}}"#)).unwrap();
        roots.push(root);
    }
    let limits = TopologyLimits { roots_per_environment: 1, ..TopologyLimits::default() };
    let mut cursor = None;
    let mut observed = std::collections::BTreeSet::new();
    for _ in 0..3 {
        let (capture, next) = capture_local_with_progress("fixture:native", &roots, &[], &limits, cursor.as_ref());
        assert!(capture.failures.iter().any(|failure| failure.code == FailureCode::LimitReached && failure.message == "configured root budget reached"));
        assert_eq!(capture.failures.iter().filter(|failure| failure.message == "configured root budget reached").count(), 1);
        observed.extend(
            capture
                .repositories
                .iter()
                .flat_map(|repository| &repository.manifests)
                .filter_map(|manifest| serde_json::from_str::<serde_json::Value>(&manifest.content).ok())
                .filter_map(|manifest| manifest["name"].as_str().map(str::to_owned)),
        );
        cursor = Some(next);
    }
    assert_eq!(observed, ["one".to_string(), "three".to_string(), "two".to_string()].into_iter().collect());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn local_repository_budget_is_enforced_without_turning_coverage_complete() {
    use tune_core::topology::adapters::capture_local;
    let base = std::env::temp_dir().join(format!("tune-topology-repository-budget-{}", tune_core::db::now_ms()));
    for name in ["one", "two"] {
        let repository = base.join(name);
        fixture_repo(&repository, &format!("https://example.com/group/{name}.git"));
        std::fs::write(repository.join("package.json"), format!(r#"{{"name":"{name}"}}"#)).unwrap();
    }
    let capture =
        capture_local("fixture:native", std::slice::from_ref(&base), &[], &TopologyLimits { repositories_per_environment: 1, ..TopologyLimits::default() });
    assert_eq!(capture.counters.repositories, 1);
    assert!(capture.failures.iter().any(|failure| failure.code == FailureCode::LimitReached && failure.message == "repository budget exceeded"));
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn local_configuration_is_regular_bounded_and_counted() {
    use serde_json::json;
    use tune_core::topology::adapters::capture_local;
    let base = std::env::temp_dir().join(format!("tune-topology-config-bounds-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    let config = base.join("config.json");
    std::fs::write(&config, r#"{"value":1}"#).unwrap();
    let selection = json!({"path":config,"format":"json","keys":[{"pointer":"/value","mode":"hash"}]});
    let capture = capture_local("fixture:native", std::slice::from_ref(&base), std::slice::from_ref(&selection), &TopologyLimits::default());
    assert_eq!(capture.counters.config_files, 1);
    assert_eq!(capture.counters.config_keys, 1);
    assert_eq!(capture.counters.files, 1);
    assert_eq!(capture.counters.repositories, 1);
    assert_eq!(capture.counters.bytes, r#"{"value":1}"#.len());

    let config_budget = capture_local(
        "fixture:native",
        std::slice::from_ref(&base),
        &[selection.clone(), selection.clone()],
        &TopologyLimits { manifests_per_environment: 1, ..TopologyLimits::default() },
    );
    assert_eq!(config_budget.counters.config_files, 1);
    assert!(config_budget.failures.iter().any(|failure| failure.code == FailureCode::LimitReached && failure.message == "configuration file budget exceeded"));

    let missing = base.join("missing.json");
    let unreadable_budget = capture_local(
        "fixture:native",
        std::slice::from_ref(&base),
        &[json!({"path":missing,"format":"json","keys":[]}), selection.clone()],
        &TopologyLimits { manifests_per_environment: 1, ..TopologyLimits::default() },
    );
    assert_eq!(unreadable_budget.counters.config_files, 0, "a failed open is not a successful read");
    assert_eq!(unreadable_budget.counters.repositories, 1, "the failed source remains a scoped receipt");
    assert!(!unreadable_budget.repositories[0].config_file_read);
    assert!(!unreadable_budget.repositories[0].failures.is_empty());
    assert!(
        unreadable_budget.failures.iter().any(|failure| failure.code == FailureCode::LimitReached && failure.message == "configuration file budget exceeded")
    );

    let second = base.join("second.json");
    std::fs::write(&second, r#"{"one":1,"two":2}"#).unwrap();
    let key_budget = capture_local(
        "fixture:native",
        std::slice::from_ref(&base),
        &[selection.clone(), json!({"path":second,"format":"json","keys":[{"pointer":"/one","mode":"hash"},{"pointer":"/two","mode":"hash"}]})],
        &TopologyLimits { manifests_per_environment: 2, ..TopologyLimits::default() },
    );
    assert_eq!(key_budget.counters.config_files, 2);
    assert_eq!(key_budget.counters.config_keys, 2);
    assert!(key_budget.failures.iter().any(|failure| failure.code == FailureCode::LimitReached && failure.message == "configuration key budget exceeded"));
    assert!(key_budget.repositories.last().unwrap().failures.iter().any(|failure| failure.code == FailureCode::LimitReached));

    let oversized = capture_local(
        "fixture:native",
        std::slice::from_ref(&base),
        &[json!({"path":config,"format":"json","keys":[]})],
        &TopologyLimits { file_bytes: 4, ..TopologyLimits::default() },
    );
    assert!(oversized.failures.iter().any(|failure| failure.code == FailureCode::LimitReached && failure.message == "configuration byte budget exceeded"));

    #[cfg(unix)]
    {
        let fifo = base.join("config.fifo");
        assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let fifo_capture =
            capture_local("fixture:native", std::slice::from_ref(&base), &[json!({"path":fifo,"format":"json","keys":[]})], &TopologyLimits::default());
        assert!(
            fifo_capture.failures.iter().any(|failure| failure.code == FailureCode::Unsupported && failure.message == "configuration must be a regular file")
        );
        assert_eq!(fifo_capture.counters.config_files, 0, "a FIFO is rejected before open");
        assert_eq!(fifo_capture.counters.repositories, 1);
        assert!(!fifo_capture.repositories[0].config_file_read && !fifo_capture.repositories[0].failures.is_empty());

        let link = base.join("config-link.json");
        std::os::unix::fs::symlink(&config, &link).unwrap();
        let link_capture =
            capture_local("fixture:native", std::slice::from_ref(&base), &[json!({"path":link,"format":"json","keys":[]})], &TopologyLimits::default());
        assert!(
            link_capture.failures.iter().any(|failure| failure.code == FailureCode::Unsupported && failure.message == "configuration must be a regular file")
        );
        assert_eq!(link_capture.counters.config_files, 0, "a symlink is rejected before the path is opened");
        assert_eq!(link_capture.counters.repositories, 1);
        assert!(!link_capture.repositories[0].config_file_read && !link_capture.repositories[0].failures.is_empty());
    }
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn configuration_pointer_failure_makes_its_scope_partial() {
    use serde_json::json;
    use tune_core::topology::adapters::{assemble_capture, capture_local};
    let base = std::env::temp_dir().join(format!("tune-topology-config-missing-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    let config = base.join("config.json");
    std::fs::write(&config, r#"{"present":true}"#).unwrap();
    let capture = capture_local(
        "fixture:native",
        std::slice::from_ref(&base),
        &[json!({"path":config,"format":"json","keys":[{"pointer":"/missing","mode":"hash"}]})],
        &TopologyLimits::default(),
    );
    let snapshots = assemble_capture(&capture);
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.status == ScopeStatus::Partial && snapshot.failures.iter().any(|failure| failure.message == "configuration key is missing")
    }));
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn failed_configuration_selection_preserves_the_last_complete_scope() {
    use std::sync::Arc;
    let base = std::env::temp_dir().join(format!("tune-topology-config-retain-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    let selected = base.join("selected.json");
    std::fs::write(&selected, r#"{"present":true}"#).unwrap();
    let config_path = base.join("nodes.json");
    let host = tune_core::nodes::local_host();
    let config = |pointer: &str| {
        format!(
            r#"{{"topology":{{"enabled":true,"owners":[],"github":{{"enabled":false}}}},"nodes":[{{"id":"fixture","alias":"fixture","os":"macos","local_hostname":"{host}","topology":{{"environments":[{{"id":"native","transport":"node","roots":[{}],"configs":[{{"path":{},"format":"json","keys":[{{"pointer":"{pointer}","mode":"hash"}}]}}]}}]}}}}]}}"#,
            serde_json::to_string(&base.to_string_lossy()).unwrap(),
            serde_json::to_string(&selected.to_string_lossy()).unwrap(),
        )
    };
    std::fs::write(&config_path, config("/present")).unwrap();
    let engine = Arc::new(tune_core::engine::Engine::open(config_path.clone(), base.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap());
    assert_eq!(engine.topology_refresh_nodes("test", &["fixture".into()]).await["status"], "complete");
    let before = engine.topology_view().unwrap();
    assert!(before["entities"].as_array().unwrap().iter().any(|entity| entity["kind"] == "configuration"));

    std::fs::write(&config_path, config("/missing")).unwrap();
    let receipt = engine.topology_refresh_nodes("test", &["fixture".into()]).await;
    assert_eq!(receipt["status"], "partial");
    let after = engine.topology_view().unwrap();
    assert!(after["entities"].as_array().unwrap().iter().any(|entity| entity["kind"] == "configuration"));
    assert!(
        after["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|scope| scope["status"] == "partial" && scope["failures"].to_string().contains("configuration key is missing"))
    );
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn local_children_counts_git_process_attempts() {
    use tune_core::topology::adapters::capture_local;
    let base = std::env::temp_dir().join(format!("tune-topology-git-processes-{}", tune_core::db::now_ms()));
    fixture_repo(&base, "https://example.com/group/fixture.git");
    std::fs::write(base.join("package.json"), r#"{"name":"fixture"}"#).unwrap();
    let capture = capture_local("fixture:native", std::slice::from_ref(&base), &[], &TopologyLimits::default());
    assert_eq!(capture.counters.children, 5, "one discovered checkout performs five read-only git process attempts");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn local_configurations_are_independent_and_hash_json_canonically() {
    use serde_json::json;
    use tune_core::topology::adapters::{assemble_capture, capture_local};
    let base = std::env::temp_dir().join(format!("tune-topology-config-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    let first = base.join("first.json");
    let second = base.join("second.json");
    std::fs::write(&first, r#"{"model":{"name":"first","limits":{"b":2,"a":1}}}"#).unwrap();
    std::fs::write(&second, r#"{"model":{"name":"second","limits":{"a":1,"b":2}}}"#).unwrap();
    let configs = vec![
        json!({"path":first,"format":"json","keys":[{"pointer":"/model/limits","mode":"hash"}]}),
        json!({"path":second,"format":"json","keys":[{"pointer":"/model/limits","mode":"hash"}]}),
    ];
    let root_a = base.join("root-a");
    let root_b = base.join("root-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let capture = capture_local("fixture:native", &[root_a.clone(), root_b.clone()], &configs, &TopologyLimits::default());
    assert_eq!(capture.repositories.len(), 2);
    assert_ne!(capture.repositories[0].relative_path, capture.repositories[1].relative_path);
    assert_eq!(capture.repositories[0].configs[0].key, capture.repositories[1].configs[0].key);
    assert_eq!(capture.repositories[0].configs[0].value_hash, capture.repositories[1].configs[0].value_hash);
    let snapshots = assemble_capture(&capture);
    let config_ids = snapshots
        .iter()
        .flat_map(|snapshot| &snapshot.entities)
        .filter(|entity| entity.kind == EntityKind::Configuration)
        .map(|entity| entity.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(config_ids.len(), 2);
    let reversed = capture_local("fixture:native", &[root_b, root_a], &configs, &TopologyLimits::default());
    let reversed_ids = assemble_capture(&reversed)
        .iter()
        .flat_map(|snapshot| &snapshot.entities)
        .filter(|entity| entity.kind == EntityKind::Configuration)
        .map(|entity| entity.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(config_ids, reversed_ids, "configuration identity must not depend on root order");
    let db_dir = base.join("topology-db");
    let store = tune_core::db::Store::open(&db_dir).unwrap();
    for snapshot in &snapshots {
        store.record_topology("configs", snapshot, 1).unwrap();
    }
    drop(store);
    let reopened = tune_core::db::Store::open(&db_dir).unwrap();
    let stored_ids = reopened.topology_view().unwrap()["entities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entity| entity["kind"] == "configuration")
        .map(|entity| entity["id"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(stored_ids, config_ids, "both independent configuration entities survive SQLite reopen");
    drop(reopened);
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn typed_source_facts_reuse_matching_content_hash_without_reparse_output_changes() {
    use std::collections::HashMap;
    use tune_core::topology::adapters::{assemble_capture_cached, capture_local, capture_source_cache_keys};
    let base = std::env::temp_dir().join(format!("tune-topology-cache-{}", tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("package.json"), r#"{"name":"fixture","dependencies":{"left":"1"}}"#).unwrap();
    let capture = capture_local("fixture:native", std::slice::from_ref(&base), &[], &TopologyLimits::default());
    let (first, entries) = assemble_capture_cached(&capture, &HashMap::new());
    assert_eq!(entries.len(), capture_source_cache_keys(&capture).len());
    let cache = entries.into_iter().map(|entry| (entry.key, entry.facts)).collect::<HashMap<_, _>>();
    let (second, misses) = assemble_capture_cached(&capture, &cache);
    assert!(misses.is_empty());
    assert_eq!(first, second);
    let _ = std::fs::remove_dir_all(base);
}
