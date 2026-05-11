//! `harness validate` orchestrator. Implements the pipeline in
//! `docs/PLAN.md` §4.1.
//!
//! Wired together by [`run`]: bootstrap state → discover manifests +
//! extensions → schema-validate `with` payloads → compute per-check
//! scope hashes (effective scope: files under the manifest folder minus
//! files under any deeper same-capability manifest) → determine dirty
//! checks → fan out to extensions → persist tasks + notes → build the
//! [`ValidateReport`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use harness_protocol::{
    ResolveCheck, ResolveRequest, ResolveResponse, SnapshotHandle, PROTOCOL_VERSION,
};
use rusqlite::params;

use crate::bootstrap::StateSession;
use crate::builtin;
use crate::error::{Error, Result};
use crate::extension::descriptor::{discover_descriptors, Capability, ExtensionDescriptor};
use crate::extension::runtime::{ExtensionRunner, RuntimeOptions};
use crate::manifest::{
    check_id, discover as discover_manifests, parse as parse_manifest, scope_path_for,
    validate_with_payload, Manifest, ManifestEntry, ROOT_SCOPE,
};
use crate::report::{CapabilityNote, ValidateReport};
use crate::snapshot::{
    compute_scope_hash, hash_bytes, hash_file, is_case_insensitive_fs, normalise_rel_path,
    FileFingerprint, ScopeInput,
};

/// Configuration for one validate run. Built by the CLI layer; tests
/// pass an explicit value.
pub struct ValidateOptions {
    pub workspace_root: PathBuf,
    pub runtime_options: RuntimeOptions,
}

/// Compute the current `scope_hash` for every applicable check in the
/// workspace. Used by `harness evidence` to detect drift between when a
/// task was issued and when evidence is recorded (the `task_snapshot_hash`
/// staleness check in PLAN.md §4.2).
///
/// Side-effects: refreshes the file fingerprint cache, just like
/// [`run`]. Does **not** call extensions or write tasks/notes.
pub fn compute_current_scope_hashes(
    session: &mut StateSession,
    workspace_root: &Path,
) -> Result<BTreeMap<String, String>> {
    let now_unix = unix_seconds_now();
    let case_insensitive = is_case_insensitive_fs(&session.harness_dir);

    let manifest_entries = discover_manifests(workspace_root)?;
    let manifests = load_manifests(workspace_root, &manifest_entries)?;
    let descriptors = discover_descriptors(workspace_root)?;
    let ext_descriptor_hash = aggregate_descriptor_hash(&descriptors);
    let scope_files = compute_scope_file_inputs(
        workspace_root,
        &manifests,
        session,
        case_insensitive,
        now_unix,
    )?;
    let mut out = BTreeMap::new();
    for m in &manifests {
        let scope = scope_path_for(&m.entry.rel_path);
        let manifest_bytes = std::fs::read(&m.entry.abs_path).map_err(|source| Error::Io {
            path: m.entry.abs_path.clone(),
            source,
        })?;
        let manifest_hash = hash_bytes(&manifest_bytes);
        for (local_id, check) in &m.parsed.checks {
            let cid = check_id(&scope, local_id);
            let descendant_paths =
                descendant_same_capability_manifest_paths(&manifests, m, &check.uses);
            let effective_files = effective_files_for(
                &scope_files,
                &normalise_prefix(&m.scope_prefix, case_insensitive),
                &descendant_prefixes(workspace_root, &manifests, m, &check.uses, case_insensitive),
            );
            let scope_hash = compute_scope_hash(&ScopeInput {
                files: effective_files,
                manifest_hash: manifest_hash.clone(),
                ext_descriptor_hash: ext_descriptor_hash.clone(),
                descendant_manifest_paths: descendant_paths,
            });
            out.insert(cid, scope_hash);
        }
    }
    Ok(out)
}

/// Best-effort GC of `.harness/evidence/<task>/submission-NNN/` directories
/// per `docs/PLAN.md` §4.4.1:
///
/// - missing `evidence.json` → aborted submission, delete.
/// - present `evidence.json` but no matching `evidence_records` row → the
///   ledger transaction never committed, delete.
///
/// Submissions whose ledger row exists are kept as history.
fn gc_orphan_submissions(session: &StateSession) {
    let evidence_root = session.harness_dir.join("evidence");
    let Ok(read) = std::fs::read_dir(&evidence_root) else {
        return;
    };
    for task_entry in read.flatten() {
        if !task_entry.path().is_dir() {
            continue;
        }
        let Ok(submissions) = std::fs::read_dir(task_entry.path()) else {
            continue;
        };
        for sub in submissions.flatten() {
            let sub_path = sub.path();
            if !sub_path.is_dir() {
                continue;
            }
            let evidence_json = sub_path.join("evidence.json");
            let task_id = task_entry.file_name().to_string_lossy().to_string();
            let submission_id = sub_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            let keep = if evidence_json.is_file() {
                ledger_has_submission(&session.db, &task_id, &submission_id).unwrap_or(false)
            } else {
                false
            };
            if !keep {
                if let Err(err) = std::fs::remove_dir_all(&sub_path) {
                    tracing::warn!(path = ?sub_path, %err, "could not GC orphan submission");
                }
            }
        }
    }
}

fn ledger_has_submission(
    db: &crate::state::Db,
    task_id: &str,
    submission_id: &str,
) -> Result<bool> {
    let mut stmt = db.conn().prepare(
        "SELECT 1 FROM evidence_records WHERE task_id = ?1 AND submission_id = ?2 LIMIT 1",
    )?;
    let exists = stmt.exists(params![task_id, submission_id])?;
    Ok(exists)
}

/// Run the orchestrator and return the rendered report. Persists tasks
/// + notes into the state DB held by `session`.
pub fn run(session: &mut StateSession, opts: &ValidateOptions) -> Result<ValidateReport> {
    let workspace_root = &opts.workspace_root;
    let now_unix = unix_seconds_now();
    let case_insensitive = is_case_insensitive_fs(&session.harness_dir);

    // 0. Best-effort cleanup of orphan submission dirs from interrupted
    //    earlier evidence calls (PLAN.md §4.4.1).
    gc_orphan_submissions(session);

    // 1. Discover manifests and parse each one.
    let manifest_entries = discover_manifests(workspace_root)?;
    let manifests = load_manifests(workspace_root, &manifest_entries)?;

    // 2. Discover extensions.
    let descriptors = discover_descriptors(workspace_root)?;
    let ext_descriptor_hash = aggregate_descriptor_hash(&descriptors);
    let cap_index = build_capability_index(&descriptors);

    // 3. Schema-validate every `with` payload (manifest-error path).
    //    Built-in capabilities (registered in `crate::builtin`) win
    //    when no external descriptor provides the capability; if neither
    //    has it we surface MissingExtension.
    for m in &manifests {
        for (local_id, check) in &m.parsed.checks {
            let scope = scope_path_for(&m.entry.rel_path);
            let cid = check_id(&scope, local_id);
            if !capability_implemented(&cap_index, &check.uses) {
                return Err(Error::MissingExtension {
                    manifest_path: m.entry.rel_path.clone(),
                    check_id: cid.clone(),
                    capability: check.uses.clone(),
                });
            }
            let schema = capability_schema(&cap_index, &check.uses);
            validate_with_payload(
                &m.entry.rel_path,
                &cid,
                &check.uses,
                schema,
                &check.with_payload,
            )?;
        }
    }

    // 4. Compute the per-check effective scope and scope hash, plus the
    //    list of dirty checks (Phase 3: a check is dirty iff no green
    //    ledger row for the current scope_hash; Phase 4 adds the writes).
    let scope_files = compute_scope_file_inputs(
        workspace_root,
        &manifests,
        session,
        case_insensitive,
        now_unix,
    )?;
    let mut per_check = Vec::new();
    for m in &manifests {
        let scope = scope_path_for(&m.entry.rel_path);
        let manifest_bytes = std::fs::read(&m.entry.abs_path).map_err(|source| Error::Io {
            path: m.entry.abs_path.clone(),
            source,
        })?;
        let manifest_hash = hash_bytes(&manifest_bytes);
        for (local_id, check) in &m.parsed.checks {
            let cid = check_id(&scope, local_id);
            let descendant_paths =
                descendant_same_capability_manifest_paths(&manifests, m, &check.uses);
            let effective_files = effective_files_for(
                &scope_files,
                &normalise_prefix(&m.scope_prefix, case_insensitive),
                &descendant_prefixes(workspace_root, &manifests, m, &check.uses, case_insensitive),
            );
            let effective_file_paths: Vec<String> =
                effective_files.iter().map(|(p, _)| p.clone()).collect();
            let scope_hash = compute_scope_hash(&ScopeInput {
                files: effective_files,
                manifest_hash: manifest_hash.clone(),
                ext_descriptor_hash: ext_descriptor_hash.clone(),
                descendant_manifest_paths: descendant_paths,
            });
            persist_scope_snapshot(&mut session.db, &cid, &scope_hash, now_unix)?;
            let already_green = check_has_green_evidence(&session.db, &cid, &scope_hash)?;
            per_check.push(PreparedCheck {
                check_id: cid,
                local_id: local_id.clone(),
                manifest_rel: m.entry.rel_path.clone(),
                scope_path: scope.clone(),
                depth: scope_depth(&scope),
                capability: check.uses.clone(),
                with_payload: check.with_payload.clone(),
                scope_hash,
                dirty: !already_green,
                effective_files: effective_file_paths,
            });
        }
    }

    // 5. Group dirty checks by capability and fan out to extensions.
    let mut by_capability: BTreeMap<String, Vec<&PreparedCheck>> = BTreeMap::new();
    for c in &per_check {
        if c.dirty {
            by_capability
                .entry(c.capability.clone())
                .or_default()
                .push(c);
        }
    }

    let mut tasks = Vec::new();
    let mut ignored_checks = Vec::new();
    let mut notes = Vec::new();
    let mut tasks_to_persist = Vec::new();
    // Per PLAN.md §7.3 scenario 9: a failing capability does not abort
    // the rest. We collect failures, surface the first one after every
    // capability has been attempted, and keep the partial report
    // observable via the persisted tasks for the surviving ones.
    let mut first_error: Option<Error> = None;

    for (capability, checks_for_cap) in &by_capability {
        let request = build_resolve_request(workspace_root, capability, &per_check, checks_for_cap);
        let outcome: Result<harness_protocol::ResolveResponse> =
            if let Some(builtin) = builtin::lookup(capability) {
                (builtin.resolve)(&request)
            } else {
                let (descriptor, cap) = cap_index
                    .get(capability.as_str())
                    .copied()
                    .expect("validated above");
                let runner = ExtensionRunner {
                    capability: capability.clone(),
                    descriptor_root: &descriptor.root,
                    options: opts.runtime_options.clone(),
                };
                runner.resolve(&cap.resolve, &request)
            };
        match outcome {
            Ok(response) => {
                ingest_resolve_response(
                    response,
                    capability,
                    checks_for_cap,
                    &mut tasks,
                    &mut ignored_checks,
                    &mut notes,
                    &mut tasks_to_persist,
                );
            }
            Err(err) => {
                tracing::warn!(capability = %capability, error = %err, "extension resolve failed; continuing with other capabilities");
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }

    persist_tasks(&mut session.db, &tasks_to_persist, &notes, now_unix)?;

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(ValidateReport {
        workspace_root: workspace_root.display().to_string(),
        tasks,
        ignored_checks,
        notes,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct LoadedManifest {
    entry: ManifestEntry,
    parsed: Manifest,
    /// Workspace-relative prefix of files belonging to this manifest's
    /// folder. Always uses `/` separators.
    scope_prefix: String,
}

struct PreparedCheck {
    check_id: String,
    #[allow(dead_code)]
    local_id: String,
    manifest_rel: PathBuf,
    scope_path: String,
    depth: u32,
    capability: String,
    with_payload: serde_json::Value,
    scope_hash: String,
    dirty: bool,
    /// Normalised relative paths of every file in this check's effective
    /// scope. Populated when the check is prepared; surfaced verbatim
    /// in the resolve request's `changed_files` field.
    effective_files: Vec<String>,
}

fn load_manifests(workspace_root: &Path, entries: &[ManifestEntry]) -> Result<Vec<LoadedManifest>> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let bytes = std::fs::read(&entry.abs_path).map_err(|source| Error::Io {
            path: entry.abs_path.clone(),
            source,
        })?;
        let parsed = parse_manifest(&entry.rel_path, &bytes)?;
        let scope_prefix = scope_prefix_for(&entry.rel_path);
        out.push(LoadedManifest {
            entry: entry.clone(),
            parsed,
            scope_prefix,
        });
        // workspace_root is borrowed but not used per-iteration; the
        // canonical workspace path is captured by the orchestrator.
        let _ = workspace_root;
    }
    Ok(out)
}

/// Returns the workspace-relative folder prefix for a manifest. The root
/// manifest has prefix `""` so every file matches `startswith("")`.
fn scope_prefix_for(manifest_rel: &Path) -> String {
    match manifest_rel.parent() {
        Some(p) if !p.as_os_str().is_empty() => p
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/"),
        _ => String::new(),
    }
}

fn scope_depth(scope_path: &str) -> u32 {
    if scope_path == ROOT_SCOPE {
        0
    } else {
        (scope_path.split('/').count()) as u32
    }
}

fn aggregate_descriptor_hash(descriptors: &[ExtensionDescriptor]) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut sorted: Vec<&ExtensionDescriptor> = descriptors.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for d in sorted {
        hasher.update(d.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(&d.descriptor_bytes);
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

type CapabilityIndex<'a> = BTreeMap<&'a str, (&'a ExtensionDescriptor, &'a Capability)>;

fn build_capability_index(descriptors: &[ExtensionDescriptor]) -> CapabilityIndex<'_> {
    let mut idx = BTreeMap::new();
    for d in descriptors {
        for cap in d.capabilities.values() {
            idx.insert(cap.uses.as_str(), (d, cap));
        }
    }
    idx
}

/// True when `uses` is implemented either by an installed external
/// extension or by a [`crate::builtin`] capability.
fn capability_implemented(index: &CapabilityIndex<'_>, uses: &str) -> bool {
    index.contains_key(uses) || builtin::lookup(uses).is_some()
}

/// Resolve the JSON Schema for a capability. External descriptors win
/// when present (so a workspace can override a built-in by shipping
/// its own extension); built-ins are consulted on miss. Returns `None`
/// when the capability provides no schema at all.
fn capability_schema<'a>(
    index: &'a CapabilityIndex<'_>,
    uses: &str,
) -> Option<&'a serde_json::Value> {
    if let Some((_, cap)) = index.get(uses) {
        return cap.schema.as_ref();
    }
    builtin::lookup(uses).map(|b| (b.schema)())
}

/// Map of `workspace-relative rel_path → content_hash` for every file in
/// **any** manifest's scope. Phase 1's [`hash_file`] is used; results
/// are cached in the `file_fingerprints` table.
fn compute_scope_file_inputs(
    workspace_root: &Path,
    manifests: &[LoadedManifest],
    session: &mut StateSession,
    case_insensitive: bool,
    now_unix: i64,
) -> Result<BTreeMap<String, String>> {
    // Walk every manifest's folder once, deduplicate by absolute path.
    let mut files: BTreeMap<PathBuf, ()> = BTreeMap::new();
    for m in manifests {
        let scope_dir = m
            .entry
            .abs_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| workspace_root.to_path_buf());
        for entry in ignore::WalkBuilder::new(&scope_dir)
            .standard_filters(true)
            .git_ignore(true)
            .git_exclude(true)
            .require_git(false)
            .hidden(false)
            .follow_links(false)
            .filter_entry(skip_built_in_ignores)
            .build()
        {
            let entry = entry.map_err(|err| Error::Io {
                path: scope_dir.clone(),
                source: err
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("scope walk error")),
            })?;
            if entry.file_type().is_some_and(|t| t.is_file()) {
                files.insert(entry.path().to_path_buf(), ());
            }
        }
    }

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for abs_path in files.keys() {
        let rel = abs_path
            .strip_prefix(workspace_root)
            .unwrap_or(abs_path)
            .to_path_buf();
        let normalised = normalise_rel_path(&rel, case_insensitive);
        let raw_rel = rel
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/");
        let metadata = std::fs::metadata(abs_path).map_err(|source| Error::Io {
            path: abs_path.clone(),
            source,
        })?;
        let size_bytes = metadata.len();
        let mtime_ns = mtime_nanos(&metadata);
        let hash = match session.db.fingerprint_for(&raw_rel)? {
            Some((cached_mtime, cached_size, cached_hash)) => {
                let fp = FileFingerprint {
                    size_bytes: cached_size,
                    mtime_ns: cached_mtime,
                    content_hash: cached_hash.clone(),
                };
                FileFingerprint::cached_hash(&fp, size_bytes, mtime_ns).unwrap_or_else(|| {
                    // Refresh on miss.
                    hash_file(abs_path)
                        .map(|f| f.content_hash)
                        .unwrap_or_default()
                })
            }
            None => hash_file(abs_path)?.content_hash,
        };
        session
            .db
            .upsert_fingerprint(&raw_rel, mtime_ns, size_bytes, &hash, now_unix)?;
        out.insert(normalised, hash);
    }
    Ok(out)
}

fn mtime_nanos(metadata: &std::fs::Metadata) -> i128 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

fn skip_built_in_ignores(entry: &ignore::DirEntry) -> bool {
    let Some(name) = entry.file_name().to_str() else {
        return true;
    };
    let exact = matches!(
        name,
        ".git" | ".harness" | "node_modules" | "target" | "DerivedData" | "xcuserdata"
    );
    !(exact || name.starts_with("bazel-"))
}

/// Is `child` strictly nested inside the directory denoted by `parent`?
/// Both arguments are workspace-relative path prefixes joined by `/`,
/// using `""` for the root scope. The comparison is segment-aware so
/// `App/LoginExtra` is **not** considered a child of `App/Login`.
fn prefix_is_strict_ancestor(parent: &str, child: &str) -> bool {
    if child == parent {
        return false;
    }
    if parent.is_empty() {
        // Any non-empty child is nested inside the root.
        return !child.is_empty();
    }
    child.len() > parent.len()
        && child.starts_with(parent)
        && child.as_bytes().get(parent.len()) == Some(&b'/')
}

/// Compute the relative-path *prefixes* of every deeper manifest that
/// declares a check of the same `capability` as the current one. Used
/// to subtract files from the parent's effective scope.
///
/// The returned prefixes are normalised the **same way** the keys in
/// `scope_files` are (NFC + optional lowercase) so a case-insensitive
/// filesystem doesn't accidentally desync the comparison and break the
/// carve-out.
fn descendant_prefixes(
    _workspace_root: &Path,
    manifests: &[LoadedManifest],
    current: &LoadedManifest,
    capability: &str,
    case_insensitive: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for other in manifests {
        if other.entry.rel_path == current.entry.rel_path {
            continue;
        }
        if !prefix_is_strict_ancestor(&current.scope_prefix, &other.scope_prefix) {
            continue;
        }
        if other.parsed.checks.values().any(|c| c.uses == capability) {
            out.push(normalise_prefix(&other.scope_prefix, case_insensitive));
        }
    }
    out
}

fn normalise_prefix(prefix: &str, case_insensitive: bool) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut s = prefix.nfc().collect::<String>();
    if case_insensitive {
        s = s.to_lowercase();
    }
    s
}

/// Workspace-relative paths of every deeper manifest that declares a
/// check of the same capability — used as `descendant_manifest_paths`
/// in the scope hash so adding/removing a same-capability child
/// manifest invalidates the parent's hash.
fn descendant_same_capability_manifest_paths(
    manifests: &[LoadedManifest],
    current: &LoadedManifest,
    capability: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for other in manifests {
        if other.entry.rel_path == current.entry.rel_path {
            continue;
        }
        if !prefix_is_strict_ancestor(&current.scope_prefix, &other.scope_prefix) {
            continue;
        }
        if other.parsed.checks.values().any(|c| c.uses == capability) {
            out.push(
                other
                    .entry
                    .rel_path
                    .components()
                    .filter_map(|c| c.as_os_str().to_str())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
    out
}

/// Filter the workspace-wide file map down to a check's effective scope.
fn effective_files_for(
    scope_files: &BTreeMap<String, String>,
    manifest_prefix: &str,
    descendant_prefixes: &[String],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (rel, hash) in scope_files {
        if !rel_starts_with_dir(rel, manifest_prefix) {
            continue;
        }
        if descendant_prefixes
            .iter()
            .any(|d| rel_starts_with_dir(rel, d))
        {
            continue;
        }
        out.push((rel.clone(), hash.clone()));
    }
    out
}

fn rel_starts_with_dir(rel: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    rel == prefix || rel.starts_with(&format!("{prefix}/"))
}

fn persist_scope_snapshot(
    db: &mut crate::state::Db,
    _check_id: &str,
    scope_hash: &str,
    now_unix: i64,
) -> Result<()> {
    db.conn().execute(
        r#"
        INSERT INTO scope_snapshots (scope_path, scope_hash, computed_at)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(scope_path) DO UPDATE SET
            scope_hash  = excluded.scope_hash,
            computed_at = excluded.computed_at
        "#,
        params![_check_id, scope_hash, now_unix],
    )?;
    Ok(())
}

fn check_has_green_evidence(db: &crate::state::Db, cid: &str, scope_hash: &str) -> Result<bool> {
    let mut stmt = db.conn().prepare(
        "SELECT 1 FROM evidence_records WHERE check_id = ?1 AND scope_hash = ?2 AND accepted = 1 LIMIT 1",
    )?;
    let exists = stmt.exists(params![cid, scope_hash])?;
    Ok(exists)
}

fn build_resolve_request(
    workspace_root: &Path,
    capability: &str,
    _all_checks: &[PreparedCheck],
    dirty: &[&PreparedCheck],
) -> ResolveRequest {
    let dirty_scopes: BTreeSet<String> = dirty.iter().map(|c| c.scope_path.clone()).collect();
    // `changed_files` is the deduplicated, sorted union of every file
    // in each dirty check's effective scope. Phase 4 will narrow this
    // to files whose fingerprint actually changed when a ledger row
    // already exists, but the wire shape is the same.
    let mut changed_files: BTreeSet<String> = BTreeSet::new();
    for c in dirty {
        for file in &c.effective_files {
            changed_files.insert(file.clone());
        }
    }
    let checks: Vec<ResolveCheck> = dirty
        .iter()
        .map(|c| ResolveCheck {
            id: c.check_id.clone(),
            local_id: c.local_id.clone(),
            manifest_path: c
                .manifest_rel
                .components()
                .filter_map(|comp| comp.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join("/"),
            scope_path: c.scope_path.clone(),
            depth: c.depth,
            with_payload: c.with_payload.clone(),
        })
        .collect();

    let snapshot = SnapshotHandle {
        // Opaque to the extension. We embed the capability so two
        // capability handles in one run are distinguishable.
        handle: format!("v1:{capability}"),
        dirty_scopes: dirty_scopes.into_iter().collect(),
    };

    ResolveRequest {
        protocol_version: PROTOCOL_VERSION,
        workspace_root: workspace_root.display().to_string(),
        capability: capability.to_string(),
        changed_files: changed_files.into_iter().collect(),
        checks,
        snapshot,
    }
}

fn ingest_resolve_response(
    response: ResolveResponse,
    capability: &str,
    dirty: &[&PreparedCheck],
    tasks: &mut Vec<harness_protocol::Task>,
    ignored_checks: &mut Vec<harness_protocol::IgnoredCheck>,
    notes: &mut Vec<CapabilityNote>,
    tasks_to_persist: &mut Vec<PersistedTask>,
) {
    let by_id: BTreeMap<&str, &PreparedCheck> =
        dirty.iter().map(|c| (c.check_id.as_str(), *c)).collect();
    for task in response.tasks {
        let mut scope_hashes: BTreeMap<String, String> = BTreeMap::new();
        for s in &task.satisfies {
            if let Some(c) = by_id.get(s.as_str()) {
                scope_hashes.insert(s.clone(), c.scope_hash.clone());
            }
        }
        let task_snapshot_hash = compute_task_snapshot_hash(&scope_hashes);
        tasks_to_persist.push(PersistedTask {
            id: task.id.clone(),
            capability: capability.to_string(),
            title: task.title.clone(),
            satisfies_json: serde_json::to_string(&task.satisfies).unwrap_or_else(|_| "[]".into()),
            scope_hashes_json: serde_json::to_string(&scope_hashes).unwrap_or_else(|_| "{}".into()),
            task_snapshot_hash,
            payload_json: serde_json::to_string(&task).unwrap_or_else(|_| "{}".into()),
        });
        tasks.push(task);
    }
    ignored_checks.extend(response.ignored_checks);
    for note in response.notes {
        notes.push(CapabilityNote {
            capability: capability.to_string(),
            note,
        });
    }
}

fn compute_task_snapshot_hash(scope_hashes: &BTreeMap<String, String>) -> String {
    let mut hasher = blake3::Hasher::new();
    for (cid, h) in scope_hashes {
        hasher.update(cid.as_bytes());
        hasher.update(b"\0");
        hasher.update(h.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

struct PersistedTask {
    id: String,
    capability: String,
    title: String,
    satisfies_json: String,
    scope_hashes_json: String,
    task_snapshot_hash: String,
    payload_json: String,
}

fn persist_tasks(
    db: &mut crate::state::Db,
    tasks: &[PersistedTask],
    notes: &[CapabilityNote],
    now_unix: i64,
) -> Result<()> {
    let tx = db.conn_mut().transaction()?;
    tx.execute("DELETE FROM tasks", [])?;
    tx.execute("DELETE FROM resolve_notes", [])?;
    for t in tasks {
        tx.execute(
            r#"
            INSERT INTO tasks
                (task_id, capability, title, satisfies_json, scope_hashes,
                 task_snapshot_hash, payload_json, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                t.id,
                t.capability,
                t.title,
                t.satisfies_json,
                t.scope_hashes_json,
                t.task_snapshot_hash,
                t.payload_json,
                now_unix,
            ],
        )?;
    }
    for n in notes {
        tx.execute(
            "INSERT INTO resolve_notes (capability, note, created_at) VALUES (?1, ?2, ?3)",
            params![n.capability, n.note, now_unix],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn unix_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_starts_with_dir_empty_matches_everything() {
        assert!(rel_starts_with_dir("anything/here", ""));
        assert!(rel_starts_with_dir("a", ""));
    }

    #[test]
    fn rel_starts_with_dir_requires_segment_boundary() {
        assert!(rel_starts_with_dir("App/Login/file.swift", "App/Login"));
        assert!(!rel_starts_with_dir(
            "App/LoginExtra/file.swift",
            "App/Login"
        ));
        assert!(rel_starts_with_dir("App/Login", "App/Login"));
    }

    #[test]
    fn scope_depth_works() {
        assert_eq!(scope_depth(ROOT_SCOPE), 0);
        assert_eq!(scope_depth("App"), 1);
        assert_eq!(scope_depth("App/Login"), 2);
    }
}
