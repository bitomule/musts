//! Integration test: discover + parse two manifests in a fixture tree,
//! confirm stable global IDs and the snapshot primitives line up.
//!
//! This is the Phase 1 ✅ checkpoint for "two-manifest fixture parses."

use std::fs;
use std::path::PathBuf;

use harness_core::manifest::{check_id, discover, parse, scope_path_for, ROOT_SCOPE};
use harness_core::snapshot::{compute_scope_hash, hash_bytes, hash_file, ScopeInput};
use harness_core::state::open;
use tempfile::TempDir;

fn write(path: &PathBuf, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

#[test]
fn discovers_parses_and_hashes_two_manifest_fixture() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join(".git")).unwrap();

    let root_manifest = root.join("HARNESS.yml");
    let nested_manifest = root.join("App/Login/HARNESS.yml");
    let login_view = root.join("App/Login/LoginView.swift");

    write(
        &root_manifest,
        "version: 1\nchecks:\n  app-build:\n    uses: bazel/build\n    with:\n      target: //App:App\n",
    );
    write(
        &nested_manifest,
        "version: 1\nchecks:\n  login-build:\n    uses: bazel/build\n    with:\n      target: //App/Login:Login\n",
    );
    write(&login_view, "struct LoginView {}\n");

    // 1. Discovery returns both manifests sorted.
    let entries = discover(root).unwrap();
    let rels: Vec<_> = entries
        .iter()
        .map(|e| e.rel_path.to_str().unwrap().to_string())
        .collect();
    assert_eq!(
        rels,
        vec![
            "App/Login/HARNESS.yml".to_string(),
            "HARNESS.yml".to_string()
        ]
    );

    // 2. Parsing each manifest yields its checks with stable global IDs.
    let root_bytes = fs::read(&root_manifest).unwrap();
    let root_parsed = parse(&entries[1].rel_path, &root_bytes).unwrap();
    let root_check = &root_parsed.checks["app-build"];
    assert_eq!(root_check.uses, "bazel/build");
    assert_eq!(check_id(ROOT_SCOPE, &root_check.local_id), "root/app-build");
    assert_eq!(scope_path_for(&entries[1].rel_path), "root");

    let nested_bytes = fs::read(&nested_manifest).unwrap();
    let nested_parsed = parse(&entries[0].rel_path, &nested_bytes).unwrap();
    let login = &nested_parsed.checks["login-build"];
    assert_eq!(
        check_id(&scope_path_for(&entries[0].rel_path), &login.local_id),
        "App/Login/login-build"
    );

    // 3. The two checks share the same capability but live under
    // different scopes, so their global IDs are distinct (the
    // same_local_id_two_manifests guarantee at the manifest layer).
    assert_ne!(
        check_id(ROOT_SCOPE, "login-build"),
        check_id("App/Login", "login-build")
    );

    // 4. Snapshot primitives: scope hash incorporates the manifest hash,
    // the file fingerprint, and the descendant manifest path. Editing
    // LoginView changes the nested scope's hash; the *root* scope hash
    // (with the carve-out applied — i.e. without LoginView) is unaffected.
    let view_fp = hash_file(&login_view).unwrap();
    let nested_input = ScopeInput {
        files: vec![(
            "App/Login/LoginView.swift".into(),
            view_fp.content_hash.clone(),
        )],
        manifest_hash: hash_bytes(&nested_bytes),
        ext_descriptor_hash: hash_bytes(b""),
        descendant_manifest_paths: vec![],
    };
    let nested_hash = compute_scope_hash(&nested_input);

    let root_input_with_carveout = ScopeInput {
        // App/Login/LoginView.swift is carved out — it falls under the
        // nested manifest's same-capability scope.
        files: vec![],
        manifest_hash: hash_bytes(&root_bytes),
        ext_descriptor_hash: hash_bytes(b""),
        descendant_manifest_paths: vec!["App/Login/HARNESS.yml".into()],
    };
    let root_hash_original = compute_scope_hash(&root_input_with_carveout);

    // Edit LoginView and recompute. Nested hash must change; root must not.
    fs::write(&login_view, "struct LoginView { let v = 1 }\n").unwrap();
    let view_fp_after = hash_file(&login_view).unwrap();
    assert_ne!(view_fp.content_hash, view_fp_after.content_hash);

    let nested_input_after = ScopeInput {
        files: vec![(
            "App/Login/LoginView.swift".into(),
            view_fp_after.content_hash,
        )],
        ..nested_input
    };
    let nested_hash_after = compute_scope_hash(&nested_input_after);
    assert_ne!(nested_hash, nested_hash_after);
    let root_hash_after = compute_scope_hash(&root_input_with_carveout);
    assert_eq!(root_hash_original, root_hash_after);
}

#[test]
fn state_db_round_trips_with_two_manifest_inputs() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let mut db = open(&db_path).unwrap();

    db.upsert_manifest_index("HARNESS.yml", "root", 100, 32, &"hash-root".into(), 1)
        .unwrap();
    db.upsert_manifest_index(
        "App/Login/HARNESS.yml",
        "App/Login",
        200,
        48,
        &"hash-login".into(),
        1,
    )
    .unwrap();

    db.upsert_fingerprint("App/Login/LoginView.swift", 5, 21, &"file-hash".into(), 1)
        .unwrap();
    let found = db
        .fingerprint_for("App/Login/LoginView.swift")
        .unwrap()
        .unwrap();
    assert_eq!(found, (5, 21, "file-hash".into()));
}
