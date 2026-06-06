# Git workflow & contribution guidelines

How we structure source control for Nyx. Small project, small team — the rule is
**keep it simple and keep `main` releasable**. No GitFlow, no ceremony.

## Branching

- **Trunk-based.** `main` is always in a releasable state. Everything else is a
  short-lived branch off `main`.
- Branch naming: `<type>/<short-desc>` — e.g. `feat/sftp-rename`,
  `fix/keychain-lookup`, `docs/git-workflow`.
- Open a **PR** for every change, even solo work. It gives CI a gate and leaves a
  reviewable record. Don't commit straight to `main`.
- **Squash-merge** PRs so `main` reads as one logical change per feature. Delete
  the branch after merge.
- Protect `main`: require a passing CI check, disallow force-push.

## Commits

- **[Conventional Commits](https://www.conventionalcommits.org/).** Format:
  `<type>: <summary>` (imperative, lower-case, no trailing period).
- Types we use: `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `chore`,
  `build`, `ci`.
- Keep commits **atomic** — one logical change each. A commit should compile and
  pass clippy on its own.
- Breaking changes: add a `!` (`feat!: …`) or a `BREAKING CHANGE:` footer.

This isn't bureaucracy — it's what lets us generate changelogs and pick semver
bumps automatically (see Releases).

## Versioning & releases

- **[SemVer](https://semver.org/)** while pre-1.0 (`0.x.y`): minor = features,
  patch = fixes. Breaking changes are expected and only bump the minor.
- **Tag releases** on `main`: `v0.2.0`. Annotated tags.
- Maintain `CHANGELOG.md` in [Keep a Changelog](https://keepachangelog.com/)
  format, or generate it from conventional commits with
  [`git-cliff`](https://git-cliff.org/).
- [`cargo-release`](https://github.com/crate-ci/cargo-release) automates the
  version bump + tag + (later) publish step.
- Cut a GitHub Release tied to each tag.

## `Cargo.lock`

**Commit it.** Nyx ships a binary (`nyx`), and GPUI is pinned to a git rev — a
committed lockfile is what makes the frozen dependency tree reproducible across
machines and CI. This is consistent with the "don't bump GPUI casually" rule in
[`../CLAUDE.md`](../CLAUDE.md).

## CI gates

Every PR must be green before merge. CI runs exactly what an agent runs locally
(see the build-verification loop in [`../CLAUDE.md`](../CLAUDE.md)):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
cargo build
```

macOS runner first (the Metal backend needs full Xcode, not just the CLT — set
`DEVELOPER_DIR` to the Xcode toolchain). Add a Linux runner once the Linux GPUI
features land.

## Repo hygiene files

The community-standard set, kept light for a small project:

| File | Purpose |
|---|---|
| `README.md` | Pitch + install/build/run. Keep commands in sync with `CLAUDE.md`. |
| `LICENSE` | Apache-2.0 (or dual MIT OR Apache-2.0 — the Rust norm). |
| `CONTRIBUTING.md` | How to build, test, and submit a PR. Links here. |
| `CODE_OF_CONDUCT.md` | Contributor Covenant. |
| `CHANGELOG.md` | Keep a Changelog, or `git-cliff`-generated. |
| `.github/workflows/ci.yml` | The CI gates above. |
| `.github/PULL_REQUEST_TEMPLATE.md` | PR checklist (fmt/clippy/test, changelog). |
| `.github/ISSUE_TEMPLATE/` | Bug + feature templates. |
| `.github/dependabot.yml` | Keep deps fresh (mind the pinned GPUI rev). |

## PR checklist (for authors & reviewers)

- [ ] Conventional-commit title; atomic, focused diff
- [ ] `cargo fmt`, `clippy -D warnings`, `cargo test` all clean
- [ ] No credentials in logs or committed files (keychain only)
- [ ] `nyx-ui` still depends on no `nyx-*` crate, no domain types in component
      signatures (see [`plans/plan-02-nyx-ui-flint.md`](plans/plan-02-nyx-ui-flint.md))
- [ ] Docs/CHANGELOG updated if behavior or commands changed
