## [0.1.6] - 2026-01-01

### ⚙️ Miscellaneous Tasks

- Skip ci on release commits
## [0.1.5] - 2026-01-01

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.5
- Release remark version 0.1.5
## [0.1.4] - 2026-01-01

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.4
- Release remark version 0.1.4
## [0.1.3] - 2026-01-01

### 📚 Documentation

- Clarify release workflow and token

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.3
- Release remark version 0.1.3
## [0.1.2] - 2026-01-01

### 🐛 Bug Fixes

- Drop tag prefix for cargo-release

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.2
- Release remark version 0.1.2
## [0.1.1] - 2026-01-01

### 🚀 Features

- Switch to file-based reviews with JSON notes
- Add syntax highlighting support
- Support file-level comments
- Add per-file notes and resolve comments
- Improve diff scrolling and headless ops
- Add reload key and Wayland clipboard
- Improve file list UI
- Restructure prompt output by file
- Add file comments and ctrl-u/ctrl-d paging
- Improve review UX and persistence
- Add adjustable diff context
- Add review session commands
- Remove prompt copy flag

### 🐛 Bug Fixes

- Address clippy field_reassign_with_default
- Prune stale line comments
- Appease clippy collapsible if
- Simplify conditional check for removing empty file comments in Review struct
- Improve language detection for TypeScript
- Scroll pane under mouse cursor
- Allow scrolling past selection
- Read Cargo.toml with tomllib
- Write GITHUB_OUTPUT correctly
- Allow release on main

### 🚜 Refactor

- Simplify prompt output

### 🧪 Testing

- Lock in review note and diff behavior

### ⚙️ Miscellaneous Tasks

- Initial commit
- Add cargo-dist release pipeline
- Add release-plz automation
- Add mimalloc dependency and libmimalloc-sys package to Cargo.toml and Cargo.lock; create AGENTS.md with repository guidelines and project structure.
- Regenerate release-plz workflow
- Gate release-plz on CI success
- Fallback to github token for release-plz
- Switch to cargo-release and git-cliff
- Use cargo-binstall main
- Update changelog for v0.1.1
- Release remark version 0.1.1
