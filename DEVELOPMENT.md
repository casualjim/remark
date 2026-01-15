# Development Guide

## Overview

`remark` is a Rust TUI for writing code review notes in Git repositories. This document covers local development and the automated release process.

## Requirements

- Rust stable toolchain (see `rust-toolchain.toml`)
- Git

## Build and Run

- Build (debug): `cargo build`
- Build (release): `cargo build --release`
- Run locally: `cargo run -- <args>`

Example:

```sh
cargo run -- prompt --filter base
```

## Formatting, Linting, and Tests

- Format: `cargo fmt --all`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`
- Test: `cargo test --all`

## Release Process

Releases are fully automated after CI passes on `main`. The release bump level is controlled by tokens in the PR title/body.

### Bump Tokens

Add one of the following to the PR title or body:

- `bump:major`
- `bump:minor`
- `bump:patch`

If no token is present, the release defaults to `bump:patch`. If multiple tokens are present, the highest wins (major > minor > patch).

### What Happens in CI

1. The `CI` workflow runs on `main`.
2. The `Cut Release` workflow runs after `CI` succeeds.
3. `Cut Release` determines the bump level from the PR (or commit message fallback).
4. It computes the next version from `Cargo.toml`, updates `CHANGELOG.md` via `git-cliff`, and runs `cargo-release` to create the release tag.
5. The tag push triggers the `Release` workflow, which builds and publishes artifacts with `cargo-dist`.

### Manual Release

You can also run the `Cut Release` workflow via `workflow_dispatch` in GitHub Actions. The bump token rules still apply (PR title/body or commit message fallback).

