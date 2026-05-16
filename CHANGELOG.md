## [0.5.1] - 2026-05-16

### 🐛 Bug Fixes

- *(tui)* Force full redraw when switching files or diff views
## [0.5.0] - 2026-05-16

### 🐛 Bug Fixes

- *(deps)* Upgrade ratatui 0.30, rename syntastica to verdant, fix stale TUI

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.5.0 [ci skip]
- Release remark version 0.5.0
## [0.4.3] - 2026-02-23

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.4.3 [ci skip]
- Release remark version 0.4.3
## [0.4.2] - 2026-01-25

### 🐛 Bug Fixes

- Preserve bold styling for files with unresolved comments when reviewed

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.4.2 [ci skip]
- Release remark version 0.4.2
## [0.4.1] - 2026-01-24

### 🐛 Bug Fixes

- Handle empty repos and improve file tree navigation
- Improve diff rendering with syntax highlighting and inline emphasis

### 🎨 Styling

- Apply formatting fixes

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.4.1 [ci skip]
- Release remark version 0.4.1
## [0.4.0] - 2026-01-23

### 🚀 Features

- Add decorated view mode with git decorations
- Add diff popup with unified hunk display

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.4.0 [ci skip]
- Release remark version 0.4.0
## [0.3.3] - 2026-01-15

### ⚙️ Miscellaneous Tasks

- Update release bump flow and docs
- Update changelog for v0.3.3 [ci skip]
- Release remark version 0.3.3
## [0.3.2] - 2026-01-15

### ⚙️ Miscellaneous Tasks

- Configure homebrew tap publishing
- Update changelog for v0.3.2 [ci skip]
- Release remark version 0.3.2
## [0.3.1] - 2026-01-15

### 🚀 Features

- [**breaking**] Use colored line numbers instead of +/- prefixes in diffs (#10)

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.3.1 [ci skip]
- Release remark version 0.3.1
## [0.2.7] - 2026-01-14

### 🚀 Features

- Only tint diff gutter, preserve syntax highlighting in content (#9)

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.2.7 [ci skip]
- Release remark version 0.2.7
## [0.2.6] - 2026-01-06

### 🐛 Bug Fixes

- Skip fetching missing notes ref

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.2.6 [ci skip]
- Release remark version 0.2.6
## [0.2.5] - 2026-01-06

### 🚀 Features

- Add vscode comment threads

### 🐛 Bug Fixes

- Optimize should_sync_path to eliminate expensive tree walks
- Collapse nested if statements to satisfy clippy

### ⚙️ Miscellaneous Tasks

- Fix clippy lints in lsp.rs
- Update changelog for v0.2.5 [ci skip]
- Release remark version 0.2.5
## [0.2.4] - 2026-01-05

### 🐛 Bug Fixes

- Restore draft sync and draft actions

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.2.4 [ci skip]
- Release remark version 0.2.4
## [0.2.3] - 2026-01-05

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.2.3 [ci skip]
- Release remark version 0.2.3
## [0.2.2] - 2026-01-05

### 🐛 Bug Fixes

- Restore draft actions and diff scroll

### 🧪 Testing

- Cover diff wrapping regression

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.2.2 [ci skip]
- Release remark version 0.2.2
## [0.2.1] - 2026-01-05

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.2.1 [ci skip]
- Release remark version 0.2.1
## [0.2.0] - 2026-01-05

### 🚀 Features

- Add draft workflow for helix
- Improve draft sync and editor integration

### 🚜 Refactor

- Move cli parsing to clap/confique and add notes auto-fetch

### 📚 Documentation

- Adjust helix draft bindings
- Improve helix integration instructions
- Add vscode integration notes

### 🧪 Testing

- Add lsp integration coverage
- Set git identity for repo init

### ⚙️ Miscellaneous Tasks

- Improve visible hints for lsp
- Update changelog for v0.1.16 [ci skip]
- Release remark version 0.1.16
- Release v0.2.0
## [0.1.15] - 2026-01-03

### 🚀 Features

- Include code snippets in prompt output

### 🐛 Bug Fixes

- Satisfy clippy

### ⚙️ Miscellaneous Tasks

- Fmt
- Update changelog for v0.1.15 [ci skip]
- Release remark version 0.1.15
## [0.1.14] - 2026-01-02

### 🚀 Features

- Add comment list modal

### 🐛 Bug Fixes

- Replace editor and invalidate reviewed state
- Clippy needless as_bytes

### ⚙️ Miscellaneous Tasks

- Format
- Update changelog for v0.1.14 [ci skip]
- Release remark version 0.1.14
## [0.1.13] - 2026-01-02

### ⚙️ Miscellaneous Tasks

- Regenerate cargo-dist release workflow
- Update changelog for v0.1.13 [ci skip]
- Release remark version 0.1.13
## [0.1.12] - 2026-01-02

### 🐛 Bug Fixes

- Make windows dist builds succeed

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.12 [ci skip]
- Release remark version 0.1.12
## [0.1.11] - 2026-01-02

### ⚙️ Miscellaneous Tasks

- Update cargo-dist release workflow
- Update changelog for v0.1.11 [ci skip]
- Release remark version 0.1.11
## [0.1.10] - 2026-01-02

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.10 [ci skip]
- Release remark version 0.1.10
## [0.1.9] - 2026-01-02

### 🐛 Bug Fixes

- Avoid credential override when pushing tags

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.9 [ci skip]
- Release remark version 0.1.9
## [0.1.8] - 2026-01-02

### 🐛 Bug Fixes

- Split release pushes by token

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.8 [ci skip]
- Release remark version 0.1.8
## [0.1.7] - 2026-01-01

### 🐛 Bug Fixes

- Correct release workflow triggers

### ⚙️ Miscellaneous Tasks

- Update changelog for v0.1.7 [ci skip]
- Release remark version 0.1.7 [ci skip]
## [0.1.6] - 2026-01-01

### ⚙️ Miscellaneous Tasks

- Skip ci on release commits
- Update changelog for v0.1.6 [ci skip]
- Release remark version 0.1.6 [ci skip]
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
