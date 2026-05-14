# `.musts/` (sub-workspace marker)

Intentionally tracked even though it carries no runtime config — the parent
`crates/musts-core/src/manifest/discovery.rs::is_sub_workspace` rule treats
any directory under the workspace root that owns a `.musts/` directory as a
**sub-workspace** and skips it when walking manifests from the root.

Without this marker, a `musts validate` run at the repo root would discover
`fixtures/login-app/MUSTS.yml` and emit `bazel/build` + `mav/expect` tasks
against the fixture, which is not what the dogfood loop is meant to cover
(the fixture is self-contained and exercised by `phase6_e2e` instead).

If you ever need to actually validate the fixture by hand, run `musts
validate --workspace fixtures/login-app` from a fresh checkout — the
sub-workspace rule only fires when the directory is *below* the root that
`musts` is invoked with.
