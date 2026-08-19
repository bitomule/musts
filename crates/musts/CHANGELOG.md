# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Pre-1.0:** minor versions may include breaking changes. Patch versions are
> bug-fix only.

## [0.2.0] - 2026-08-19

### Added

- Add musts lint ([#75](https://github.com/bitomule/musts/pull/75))
- Implement bazel/test as a built-in capability ([#74](https://github.com/bitomule/musts/pull/74))
- Warn when the ledger is gitignored or absent ([#72](https://github.com/bitomule/musts/pull/72))
- Add musts stats ([#63](https://github.com/bitomule/musts/pull/63))

### Fixed

- Only invalidate a check by what it actually depends on ([#73](https://github.com/bitomule/musts/pull/73))
- Warn on unknown manifest keys instead of ignoring them ([#71](https://github.com/bitomule/musts/pull/71))

### Internal

- Teach the manifest-authoring rules the audit produced ([#76](https://github.com/bitomule/musts/pull/76))

## [0.1.10] - 2026-07-31

### Fixed

- Guard the commit's own repo, and make the ledger survive merges ([#60](https://github.com/bitomule/musts/pull/60))

## [0.1.9] - 2026-07-04

### Added

- Add musts run, tighten evidence, drop the evidence archive ([#55](https://github.com/bitomule/musts/pull/55))

## [0.1.8] - 2026-07-04

### Added

- Validate before git commit instead of on Stop, add exclude_paths ([#53](https://github.com/bitomule/musts/pull/53))

## [0.1.7] - 2026-05-23

### Internal

- Remove irrelevant README comparison
- Improve README onboarding narrative

## [0.1.6] - 2026-05-20

### Added

- Add Claude Code plugin (skill + Stop hook) ([#42](https://github.com/bitomule/musts/pull/42))

## [0.1.5] - 2026-05-20

### Fixed

- Compact validate task batches

### Internal

- Fix Boxy URL and drop Nokoru from Used at ([#40](https://github.com/bitomule/musts/pull/40))
- OSS launch prep — README revamp, assets, agents.md, issue templates ([#39](https://github.com/bitomule/musts/pull/39))
- Fix README accuracy and switch logo to alpha-transparent ([#37](https://github.com/bitomule/musts/pull/37))

## [0.1.4] - 2026-05-15

### Internal

- Updated the following local packages: musts-core

## [0.1.3] - 2026-05-14

### Added

- Bundle cargo/bazel/mav reference extensions as built-in capabilities ([#29](https://github.com/bitomule/musts/pull/29))

## [0.1.2] - 2026-05-13

### Added

- Add `.mustsignore` to exclude files from scope hash ([#26](https://github.com/bitomule/musts/pull/26))

## [0.1.1] - 2026-05-13

### Internal

- Updated the following local packages: musts-core
