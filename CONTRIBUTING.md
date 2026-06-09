# Contributing

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org): `<type>(<scope>): <subject>`.
Common types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `chore`.
Keep commits atomic; each one compiles and its tests pass.

## Versioning

catapulte is a **self-hosted application**, not a published library. Its version
tracks the **operator contract**, not the Rust API. The release version is a
single number for the whole workspace (`[workspace.package].version`), exposed
as the binary version and the release tag `vX.Y.Z`.

The next version is computed from commit messages by `git-cliff`
(see [`cliff.toml`](./cliff.toml)):

| Bump      | Trigger                                   | Meaning for operators |
| --------- | ----------------------------------------- | --------------------- |
| **major** | a commit marked breaking (`type!:` or a `BREAKING CHANGE:` footer) | upgrading requires action |
| **minor** | `feat:`                                   | new backward-compatible capability |
| **patch** | `fix:` and everything else                | safe to upgrade as-is |

### What counts as breaking

Mark a commit breaking **only** when it changes the operator-facing contract:

- **Configuration** — an environment variable is removed, renamed, or becomes
  required; a default value changes in a way that alters behavior.
- **Behavior** — the HTTP or NATS request/response contract; lifecycle event
  names or payloads; the on-disk or database format in an incompatible way.

Do **not** mark a commit breaking for changes operators never see: internal
refactors, changes to the `catapulte` crate's library API (it exists only for
the integration tests), dependency bumps, or test/CI changes. The version is
not gated on `cargo-semver-checks` precisely because a Rust API break is not an
operator break.

When a change is breaking, explain the migration in a `BREAKING CHANGE:` footer
so it lands in the changelog and release notes.
