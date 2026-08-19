//! `musts validate` orchestrator. Implements the pipeline in
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

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use musts_protocol::{
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
    validate_with_payload, Check, Manifest, ManifestEntry, ROOT_SCOPE,
};
use crate::report::{CapabilityNote, ManifestIssue, ValidateReport};
use crate::snapshot::{
    compute_scope_hash, hash_bytes, hash_file, normalise_rel_path, FileFingerprint, ScopeInput,
};

/// Configuration for one validate run. Built by the CLI layer; tests
/// pass an explicit value.
pub struct ValidateOptions {
    pub workspace_root: PathBuf,
    pub runtime_options: RuntimeOptions,
}

/// Compute the current `scope_hash` for every applicable check in the
/// workspace. Used by `musts evidence` to detect drift between when a
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

    let manifest_entries = discover_manifests(workspace_root)?;
    let manifests = load_manifests(workspace_root, &manifest_entries)?;
    let descriptors = discover_descriptors(workspace_root)?;
    let ext_descriptor_hash = aggregate_descriptor_hash(&descriptors);
    let scope_files = compute_scope_file_inputs(workspace_root, &manifests, session, now_unix)?;
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
            let path_filter = compile_path_filter(&m.entry.rel_path, check)?;
            let effective_files = filter_effective_files(
                effective_files_for(
                    &scope_files,
                    &normalise_prefix(&m.scope_prefix),
                    &descendant_prefixes(workspace_root, &manifests, m, &check.uses),
                ),
                &path_filter,
            );
            // A check with an explicit `paths:`/`exclude_paths:` filter
            // that currently matches nothing is "not applicable" — it has
            // no effective scope to validate, so don't record a hash for
            // it. The task list excludes it the same way; if files appear
            // later the next `validate` will pick it up.
            if path_filter.is_active() && effective_files.is_empty() {
                continue;
            }
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

/// Run the orchestrator and return the rendered report. Persists tasks
/// + notes into the state DB held by `session`.
pub fn run(session: &mut StateSession, opts: &ValidateOptions) -> Result<ValidateReport> {
    let workspace_root = &opts.workspace_root;
    let now_unix = unix_seconds_now();

    // 0b. Load the portable, repo-committed ledger lock. Empty when the
    //    file doesn't exist (fresh workspace) — the local
    //    `evidence_records` table still answers in that case.
    let ledger_lock = crate::state::lock::load(&session.musts_dir)?;

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
                    available: available_capabilities(&cap_index),
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
    let scope_files = compute_scope_file_inputs(workspace_root, &manifests, session, now_unix)?;
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
            let path_filter = compile_path_filter(&m.entry.rel_path, check)?;
            let effective_files = filter_effective_files(
                effective_files_for(
                    &scope_files,
                    &normalise_prefix(&m.scope_prefix),
                    &descendant_prefixes(workspace_root, &manifests, m, &check.uses),
                ),
                &path_filter,
            );
            // Skip checks whose `paths:`/`exclude_paths:` filter matches
            // no current files: there is nothing for the extension to
            // validate and emitting a task would dead-end. The check
            // rejoins the loop automatically when a matching file appears.
            if path_filter.is_active() && effective_files.is_empty() {
                continue;
            }
            let effective_file_paths: Vec<String> =
                effective_files.iter().map(|(p, _)| p.clone()).collect();
            let scope_hash = compute_scope_hash(&ScopeInput {
                files: effective_files,
                manifest_hash: manifest_hash.clone(),
                ext_descriptor_hash: ext_descriptor_hash.clone(),
                descendant_manifest_paths: descendant_paths,
            });
            persist_scope_snapshot(&mut session.db, &cid, &scope_hash, now_unix)?;
            let already_green = check_has_green_evidence(&session.db, &cid, &scope_hash)?
                || ledger_lock.contains(&cid, &scope_hash);
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
        // External descriptors win over built-ins (mirrors
        // `capability_schema` and `evidence::submit`): a workspace can
        // override or replace a built-in capability by shipping its
        // own `.musts/extensions/<name>/extension.yml`. Only when no
        // external implementor is installed do we fall back to the
        // built-in registry.
        // A built-in resolve is the only trusted source of a runnable
        // `command`; an external descriptor's tasks have their command
        // stripped at ingest so `musts run` never executes extension-
        // supplied argv.
        let command_trusted = !cap_index.contains_key(capability.as_str());
        let outcome: Result<musts_protocol::ResolveResponse> =
            if let Some((descriptor, cap)) = cap_index.get(capability.as_str()).copied() {
                let runner = ExtensionRunner {
                    capability: capability.clone(),
                    descriptor_root: &descriptor.root,
                    options: opts.runtime_options.clone(),
                };
                runner.resolve(&cap.resolve, &request)
            } else {
                let builtin = builtin::lookup(capability).expect("validated above");
                (builtin.resolve)(&request)
            };
        match outcome {
            Ok(response) => {
                ingest_resolve_response(
                    response,
                    capability,
                    command_trusted,
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

    // Which tasks are byte-identical requests to the previous validate run
    // (same id + same task_snapshot_hash)? Load the prior snapshot hashes
    // BEFORE persist_tasks overwrites the table so the renderer can print
    // unchanged tasks compactly instead of re-injecting the full body.
    let prior = load_prior_task_snapshot_hashes(&session.db)?;
    let repeated_task_ids: Vec<String> = tasks_to_persist
        .iter()
        .filter(|t| prior.get(&t.id).is_some_and(|h| h == &t.task_snapshot_hash))
        .map(|t| t.id.clone())
        .collect();

    persist_tasks(&mut session.db, &tasks_to_persist, &notes, now_unix)?;

    if let Some(err) = first_error {
        return Err(err);
    }

    let mut warnings = crate::diagnose::workspace_warnings(
        workspace_root,
        &session.musts_dir,
        &ledger_lock,
        !tasks.is_empty(),
    );
    warnings.extend(manifest_warnings(&manifests));

    Ok(ValidateReport {
        workspace_root: workspace_root.display().to_string(),
        tasks,
        ignored_checks,
        notes,
        warnings,
        repeated_task_ids,
    })
}

/// Every capability a check could legally name right now, sorted, with
/// descriptor-backed and built-in ones in one list — the distinction does
/// not matter to someone fixing a `uses:` line.
fn available_capabilities(index: &CapabilityIndex<'_>) -> String {
    let mut all: BTreeSet<String> = index.keys().map(|k| (*k).to_string()).collect();
    all.extend(builtin::registered_capabilities().map(str::to_string));
    all.into_iter().collect::<Vec<_>>().join(", ")
}

/// Flatten every manifest's parse warnings, tagged with the file they
/// came from.
fn manifest_warnings(manifests: &[LoadedManifest]) -> Vec<ManifestIssue> {
    manifests
        .iter()
        .flat_map(|m| {
            m.parsed.warnings.iter().map(|w| ManifestIssue {
                manifest: m.entry.rel_path.display().to_string(),
                message: w.to_string(),
            })
        })
        .collect()
}

/// Load `task_id → task_snapshot_hash` for the tasks persisted by the
/// previous `musts validate`. Used to flag unchanged tasks for compact
/// rendering. Returns an empty map on a fresh workspace.
fn load_prior_task_snapshot_hashes(db: &crate::state::Db) -> Result<BTreeMap<String, String>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT task_id, task_snapshot_hash FROM tasks")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = BTreeMap::new();
    for r in rows {
        let (id, hash) = r?;
        out.insert(id, hash);
    }
    Ok(out)
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
            // `.mustsignore` keeps matched files out of the scope hash so
            // edits to local logs / scratch artefacts don't re-invalidate
            // checks. Same syntax + precedence as `.gitignore`. See
            // discovery::discover for the matching wiring on manifest walk.
            .add_custom_ignore_filename(".mustsignore")
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

    // The scope hash always treats rel-paths as their lowercase NFC form so
    // a lock generated on macOS APFS (case-insensitive) matches the hash
    // computed on Linux ext4 (case-sensitive) for the same repo contents.
    // The cost: on case-sensitive filesystems two files like `Foo.txt` and
    // `foo.txt` can coexist and collide here. Detect that and refuse rather
    // than silently fold them into a single hash key.
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut origin_of_normalised: BTreeMap<String, String> = BTreeMap::new();
    for abs_path in files.keys() {
        let rel = abs_path
            .strip_prefix(workspace_root)
            .unwrap_or(abs_path)
            .to_path_buf();
        let raw_rel = rel
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/");
        let normalised = normalise_rel_path(&rel);
        if let Some(prior_raw) = origin_of_normalised.get(&normalised) {
            if prior_raw != &raw_rel {
                return Err(Error::CasePathCollision {
                    workspace_root: workspace_root.to_path_buf(),
                    first: prior_raw.clone(),
                    second: raw_rel,
                });
            }
        }
        origin_of_normalised.insert(normalised.clone(), raw_rel.clone());
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
        ".git" | ".musts" | "node_modules" | "target" | "DerivedData" | "xcuserdata"
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
/// `scope_files` are (NFC + always-lowercase) so the comparison can match
/// regardless of the host filesystem's case sensitivity.
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
        if !prefix_is_strict_ancestor(&current.scope_prefix, &other.scope_prefix) {
            continue;
        }
        if other.parsed.checks.values().any(|c| c.uses == capability) {
            out.push(normalise_prefix(&other.scope_prefix));
        }
    }
    out
}

fn normalise_prefix(prefix: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    prefix.nfc().collect::<String>().to_lowercase()
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

/// A check's compiled effective-scope filter: an optional `paths:`
/// include set and an optional `exclude_paths:` subtract set. Both are
/// `None` when the corresponding manifest field is empty.
#[derive(Default)]
struct PathFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl PathFilter {
    /// True when the check declares any `paths:` or `exclude_paths:`
    /// filtering at all. A check with an active filter that matches no
    /// files is treated as "not applicable" and dropped from the run.
    fn is_active(&self) -> bool {
        self.include.is_some() || self.exclude.is_some()
    }
}

/// Compile the check's `paths:` and `exclude_paths:` patterns into
/// `GlobSet`s for fast matching. Each field is `None` when empty (the
/// legacy "apply to everything in scope" path for `paths`; "subtract
/// nothing" for `exclude_paths`). Returns an error when a pattern fails
/// to compile here — the parser already validates each pattern
/// individually, so this only fires on a pathological
/// `GlobSetBuilder::build` failure.
///
/// Globs are compiled case-insensitively because `normalise_rel_path`
/// always lowercases the scope-file map keys for OS-portable scope
/// hashes. Writing `**/Tracking*.swift` keeps matching regardless of
/// the file's on-disk case.
fn compile_path_filter(manifest_rel: &std::path::Path, check: &Check) -> Result<PathFilter> {
    Ok(PathFilter {
        include: compile_glob_set(manifest_rel, check, "paths", &check.paths)?,
        exclude: compile_glob_set(manifest_rel, check, "exclude_paths", &check.exclude_paths)?,
    })
}

/// Build a case-insensitive `GlobSet` from `patterns`, or `None` when
/// there are none. `field` is used only for error messages.
fn compile_glob_set(
    manifest_rel: &std::path::Path,
    check: &Check,
    field: &str,
    patterns: &[String],
) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let glob = GlobBuilder::new(pat)
            .case_insensitive(true)
            .build()
            .map_err(|err| Error::Manifest {
                path: manifest_rel.to_path_buf(),
                message: format!(
                    "check `{}`: `{}`: invalid glob `{}`: {}",
                    check.local_id, field, pat, err
                ),
            })?;
        builder.add(glob);
    }
    let set = builder.build().map_err(|err| Error::Manifest {
        path: manifest_rel.to_path_buf(),
        message: format!(
            "check `{}`: `{}`: could not build glob set: {}",
            check.local_id, field, err
        ),
    })?;
    Ok(Some(set))
}

/// Narrow `files` to the check's effective scope: keep entries matching
/// the `include` set (or all when `include` is `None`), then drop any
/// entry matching the `exclude` set. Matching is against the
/// workspace-relative path string, so a pattern like `**/Tracking*.swift`
/// works regardless of how deep the file is.
fn filter_effective_files(
    files: Vec<(String, String)>,
    filter: &PathFilter,
) -> Vec<(String, String)> {
    files
        .into_iter()
        .filter(|(rel, _)| {
            let included = filter.include.as_ref().is_none_or(|set| set.is_match(rel));
            let excluded = filter.exclude.as_ref().is_some_and(|set| set.is_match(rel));
            included && !excluded
        })
        .collect()
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

#[allow(clippy::too_many_arguments)]
fn ingest_resolve_response(
    response: ResolveResponse,
    capability: &str,
    command_trusted: bool,
    dirty: &[&PreparedCheck],
    tasks: &mut Vec<musts_protocol::Task>,
    ignored_checks: &mut Vec<musts_protocol::IgnoredCheck>,
    notes: &mut Vec<CapabilityNote>,
    tasks_to_persist: &mut Vec<PersistedTask>,
) {
    let by_id: BTreeMap<&str, &PreparedCheck> =
        dirty.iter().map(|c| (c.check_id.as_str(), *c)).collect();
    for mut task in response.tasks {
        // `musts run` executes a task's `command`. Only trust it when the
        // task came from a built-in capability — an external extension
        // (even one claiming a built-in's name) could otherwise inject an
        // arbitrary argv that survives in the persisted payload and runs
        // later, after the extension is gone. Strip it so a `command` in
        // the ledger is always built-in-authored.
        if !command_trusted {
            task.command = None;
        }
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

    fn prepared(check_id: &str) -> PreparedCheck {
        PreparedCheck {
            check_id: check_id.into(),
            local_id: "x".into(),
            manifest_rel: PathBuf::from("MUSTS.yml"),
            scope_path: "root".into(),
            depth: 0,
            capability: "cargo/test".into(),
            with_payload: serde_json::json!({}),
            scope_hash: "h".into(),
            dirty: true,
            effective_files: vec![],
        }
    }

    fn task_with_command(id: &str, satisfies: &str) -> musts_protocol::Task {
        musts_protocol::Task {
            id: id.into(),
            extension: "cargo/test".into(),
            title: "t".into(),
            satisfies: vec![satisfies.into()],
            parallelizable: true,
            command: Some(vec!["cargo".into(), "test".into()]),
            instructions: vec![],
            evidence_contract: musts_protocol::EvidenceContract {
                text: musts_protocol::TextContract {
                    required: true,
                    description: None,
                },
                assets: vec![],
            },
        }
    }

    #[test]
    fn ingest_strips_command_when_untrusted() {
        // A task whose command came from an external extension must not
        // survive into the report or the persisted payload — otherwise
        // `musts run` could execute extension-supplied argv.
        let dirty = prepared("root/test");
        let dirty_refs = [&dirty];
        let resp = ResolveResponse {
            protocol_version: PROTOCOL_VERSION,
            tasks: vec![task_with_command("evil", "root/test")],
            ignored_checks: vec![],
            notes: vec![],
        };
        let mut tasks = Vec::new();
        let mut ignored = Vec::new();
        let mut notes = Vec::new();
        let mut persist = Vec::new();
        ingest_resolve_response(
            resp,
            "cargo/test",
            false, // untrusted (external)
            &dirty_refs,
            &mut tasks,
            &mut ignored,
            &mut notes,
            &mut persist,
        );
        assert_eq!(tasks[0].command, None, "untrusted command must be stripped");
        assert!(
            !persist[0].payload_json.contains("command"),
            "stripped command must not persist in the payload"
        );
    }

    #[test]
    fn ingest_keeps_command_when_trusted() {
        let dirty = prepared("root/test");
        let dirty_refs = [&dirty];
        let resp = ResolveResponse {
            protocol_version: PROTOCOL_VERSION,
            tasks: vec![task_with_command("cargo-test-root", "root/test")],
            ignored_checks: vec![],
            notes: vec![],
        };
        let mut tasks = Vec::new();
        let mut ignored = Vec::new();
        let mut notes = Vec::new();
        let mut persist = Vec::new();
        ingest_resolve_response(
            resp,
            "cargo/test",
            true, // trusted (built-in)
            &dirty_refs,
            &mut tasks,
            &mut ignored,
            &mut notes,
            &mut persist,
        );
        assert_eq!(tasks[0].command, Some(vec!["cargo".into(), "test".into()]));
    }

    fn make_check(local_id: &str, paths: Vec<&str>) -> Check {
        make_check_ex(local_id, paths, vec![])
    }

    fn make_check_ex(local_id: &str, paths: Vec<&str>, exclude_paths: Vec<&str>) -> Check {
        Check {
            local_id: local_id.into(),
            uses: "cargo/test".into(),
            with_payload: serde_json::Value::Object(Default::default()),
            paths: paths.into_iter().map(String::from).collect(),
            exclude_paths: exclude_paths.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn filter_effective_files_no_filter_is_passthrough() {
        let files = vec![
            ("a.rs".into(), "h1".into()),
            ("b.swift".into(), "h2".into()),
        ];
        let out = filter_effective_files(files.clone(), &PathFilter::default());
        assert_eq!(out, files);
    }

    #[test]
    fn filter_effective_files_keeps_matches() {
        let check = make_check("tracking", vec!["**/Tracking*.swift"]);
        let filter = compile_path_filter(std::path::Path::new("MUSTS.yml"), &check).unwrap();
        let files = vec![
            ("App/TrackingEvents.swift".into(), "h1".into()),
            ("App/OtherFile.swift".into(), "h2".into()),
            ("Tests/TrackingEventsTests.swift".into(), "h3".into()),
        ];
        let out = filter_effective_files(files, &filter);
        assert_eq!(
            out,
            vec![
                ("App/TrackingEvents.swift".to_string(), "h1".to_string()),
                (
                    "Tests/TrackingEventsTests.swift".to_string(),
                    "h3".to_string(),
                ),
            ]
        );
    }

    #[test]
    fn filter_effective_files_supports_multiple_patterns() {
        let check = make_check("multi", vec!["**/*.json", "tests/**"]);
        let filter = compile_path_filter(std::path::Path::new("MUSTS.yml"), &check).unwrap();
        let files = vec![
            ("fixtures/data.json".into(), "h1".into()),
            ("tests/it.rs".into(), "h2".into()),
            ("src/main.rs".into(), "h3".into()),
        ];
        let out = filter_effective_files(files, &filter);
        assert_eq!(
            out,
            vec![
                ("fixtures/data.json".to_string(), "h1".to_string()),
                ("tests/it.rs".to_string(), "h2".to_string()),
            ]
        );
    }

    #[test]
    fn filter_effective_files_returns_empty_when_no_matches() {
        let check = make_check("none", vec!["**/Tracking*.swift"]);
        let filter = compile_path_filter(std::path::Path::new("MUSTS.yml"), &check).unwrap();
        let files = vec![
            ("App/A.swift".into(), "h1".into()),
            ("App/B.swift".into(), "h2".into()),
        ];
        let out = filter_effective_files(files, &filter);
        assert!(out.is_empty());
    }

    #[test]
    fn filter_effective_files_is_case_insensitive() {
        // `normalise_rel_path` always lowercases scope-file keys for
        // OS-portable scope hashes, so a glob written with mixed case
        // ("Tracking*") must still match the lowercased entry.
        let check = make_check("tracking", vec!["**/Tracking*.swift"]);
        let filter = compile_path_filter(std::path::Path::new("MUSTS.yml"), &check).unwrap();
        let files = vec![
            ("app/trackingevents.swift".into(), "h1".into()),
            ("app/other.swift".into(), "h2".into()),
        ];
        let out = filter_effective_files(files, &filter);
        assert_eq!(
            out,
            vec![("app/trackingevents.swift".to_string(), "h1".to_string())]
        );
    }

    #[test]
    fn exclude_paths_subtracts_from_all_files_when_no_include() {
        // No `paths:` → start from every file, then drop excludes.
        let check = make_check_ex("build", vec![], vec!["tools/config.bzl"]);
        let filter = compile_path_filter(std::path::Path::new("MUSTS.yml"), &check).unwrap();
        assert!(filter.is_active());
        let files = vec![
            ("app/main.swift".into(), "h1".into()),
            ("tools/config.bzl".into(), "h2".into()),
        ];
        let out = filter_effective_files(files, &filter);
        assert_eq!(out, vec![("app/main.swift".to_string(), "h1".to_string())]);
    }

    #[test]
    fn exclude_paths_applies_after_include() {
        // Include the swift files, then carve out the generated one.
        let check = make_check_ex("unit", vec!["**/*.swift"], vec!["**/*.generated.swift"]);
        let filter = compile_path_filter(std::path::Path::new("MUSTS.yml"), &check).unwrap();
        let files = vec![
            ("app/a.swift".into(), "h1".into()),
            ("app/b.generated.swift".into(), "h2".into()),
            ("app/notes.md".into(), "h3".into()),
        ];
        let out = filter_effective_files(files, &filter);
        assert_eq!(out, vec![("app/a.swift".to_string(), "h1".to_string())]);
    }
}
