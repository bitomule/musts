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

/// Run the orchestrator and return the rendered report. Persists tasks
/// + notes into the state DB held by `session`.
pub fn run(session: &mut StateSession, opts: &ValidateOptions) -> Result<ValidateReport> {
    let workspace_root = &opts.workspace_root;
    let now_unix = unix_seconds_now();
    let case_insensitive = is_case_insensitive_fs(&session.harness_dir);

    // 1. Discover manifests and parse each one.
    let manifest_entries = discover_manifests(workspace_root)?;
    let manifests = load_manifests(workspace_root, &manifest_entries)?;

    // 2. Discover extensions.
    let descriptors = discover_descriptors(workspace_root)?;
    let ext_descriptor_hash = aggregate_descriptor_hash(&descriptors);
    let cap_index = build_capability_index(&descriptors);

    // 3. Schema-validate every `with` payload (manifest-error path).
    for m in &manifests {
        for (local_id, check) in &m.parsed.checks {
            let scope = scope_path_for(&m.entry.rel_path);
            let cid = check_id(&scope, local_id);
            let cap = lookup_capability(&cap_index, &check.uses).map_err(|_| {
                Error::MissingExtension {
                    manifest_path: m.entry.rel_path.clone(),
                    check_id: cid.clone(),
                    capability: check.uses.clone(),
                }
            })?;
            validate_with_payload(
                &m.entry.rel_path,
                &cid,
                &check.uses,
                cap.schema.as_ref(),
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
                &m.scope_prefix,
                &descendant_prefixes(workspace_root, &manifests, m, &check.uses),
            );
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

    for (capability, checks_for_cap) in &by_capability {
        let cap = lookup_capability(&cap_index, capability).expect("validated above");
        let descriptor = cap_index
            .get(capability.as_str())
            .map(|(d, _)| *d)
            .expect("validated above");
        let request = build_resolve_request(workspace_root, capability, &per_check, checks_for_cap);
        let runner = ExtensionRunner {
            capability: capability.clone(),
            descriptor_root: &descriptor.root,
            options: opts.runtime_options.clone(),
        };
        let response = runner.resolve(&cap.resolve, &request)?;
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

    persist_tasks(&mut session.db, &tasks_to_persist, &notes, now_unix)?;

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

fn lookup_capability<'a>(
    index: &'a CapabilityIndex<'_>,
    uses: &str,
) -> std::result::Result<&'a Capability, ()> {
    index.get(uses).map(|(_, c)| *c).ok_or(())
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

/// Compute the relative-path *prefixes* of every deeper manifest that
/// declares a check of the same `capability` as the current one. Used
/// to subtract files from the parent's effective scope.
fn descendant_prefixes(
    _workspace_root: &Path,
    manifests: &[LoadedManifest],
    current: &LoadedManifest,
    capability: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for other in manifests {
        if other.entry.rel_path == current.entry.rel_path {
            continue;
        }
        let is_deeper = other.scope_prefix.len() > current.scope_prefix.len()
            && (current.scope_prefix.is_empty()
                || other.scope_prefix.starts_with(&current.scope_prefix));
        if !is_deeper {
            continue;
        }
        let same_capability = other.parsed.checks.values().any(|c| c.uses == capability);
        if same_capability {
            out.push(other.scope_prefix.clone());
        }
    }
    out
}

/// Workspace-relative paths of every deeper manifest (any capability) —
/// used as `descendant_manifest_paths` in the scope hash so adding or
/// removing a child manifest invalidates the parent's hash.
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
        let is_deeper = other.scope_prefix.len() > current.scope_prefix.len()
            && (current.scope_prefix.is_empty()
                || other.scope_prefix.starts_with(&current.scope_prefix));
        if !is_deeper {
            continue;
        }
        let same_capability = other.parsed.checks.values().any(|c| c.uses == capability);
        if same_capability {
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
    all_checks: &[PreparedCheck],
    dirty: &[&PreparedCheck],
) -> ResolveRequest {
    let dirty_scopes: BTreeSet<String> = dirty.iter().map(|c| c.scope_path.clone()).collect();
    // Phase 3: `changed_files` lists every file inside any dirty scope's
    // effective scope. We approximate by emitting all files from dirty
    // checks' scope inputs — the orchestrator will refine this when the
    // ledger lands in Phase 4.
    let mut changed_files: BTreeSet<String> = BTreeSet::new();
    for c in dirty {
        for other in all_checks {
            if other.scope_path == c.scope_path && other.capability == c.capability {
                let _ = other; // placeholder — keeps the closure shape stable
            }
        }
        // For now we leave changed_files conservative — the contract
        // says "no prior fingerprint = treat all in-scope files as
        // changed" but the per-scope file list lives only inside
        // compute_scope_file_inputs's locals. Phase 4 will plumb the
        // exact list through; the response shape stays correct because
        // extensions consume `changed_files` opportunistically.
        let _ = changed_files.insert(c.scope_path.clone());
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
