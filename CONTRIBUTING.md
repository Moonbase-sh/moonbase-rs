# Contributing

## Layout

| Path | |
| --- | --- |
| `crates/moonbase` | The core SDK |
| `crates/tauri-plugin-moonbase` | The Tauri plugin and its activation window (`ui/`) |
| `examples/tauri-app` | The Aurora example app |
| `docs/` | Guides |
| `scripts/`, `.releaserc.json` | Release tooling (see [Releases](#releases)) |

## Build and test

```sh
cargo test                      # the core SDK (the default workspace member)
cargo test --workspace          # everything, Tauri included
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The Tauri crates need your platform's webview development libraries; on Debian or
Ubuntu, install `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`,
`librsvg2-dev` and `libxdo-dev`. The core SDK needs nothing beyond a Rust toolchain.

### Tests

- `crates/moonbase/tests/` covers the client API and every failure shape it maps, the
  grace period and throttling, revocation and offline licenses, stores and the
  cross-process lock (including a test that runs several processes against one license
  file), the device id resolvers, and the bundled transport against a local server.
- `crates/moonbase/tests/fingerprint_vectors.rs` and the vector tests in
  `crates/moonbase/tests/release_access.rs` run the cross-SDK conformance vectors.
- `crates/tauri-plugin-moonbase/tests/` drives the plugin's state machine on Tauri's
  mock runtime with a scripted Moonbase. `typescript.rs` there fails when
  `guest-js/index.d.ts`, which is written by hand, misses a field or value the plugin
  sends the frontend.
- `crates/moonbase/tests/live.rs` runs a whole activation against a real store. It is
  off unless `MOONBASE_LIVE_TESTS=1`, and uses the public demo store unless
  `MOONBASE_ENDPOINT`, `MOONBASE_PRODUCT_ID`, `MOONBASE_PUBLIC_KEY` and
  `MOONBASE_ACCOUNT_ID` say otherwise.

Test tokens are signed with the throwaway keys in `tests/fixtures/`. They sign nothing
outside the tests.

### Conformance vectors

`crates/moonbase/tests/vectors/` holds verbatim copies of two files shipped in
`@moonbase.sh/licensing`: `fingerprint-vectors.json`, which pins down the device id, and
`release-access-vectors.json`, which pins down which versions a license that covers
only some of them covers. Refresh them from there, never by hand:

```sh
npm pack @moonbase.sh/licensing
for file in fingerprint-vectors.json release-access-vectors.json; do
  tar -xOf moonbase.sh-licensing-*.tgz "package/$file" > "crates/moonbase/tests/vectors/$file"
done
```

If the fingerprint spec version changes, `vectors_pin_the_spec_version_this_sdk_implements`
fails first. The fingerprint-parity workflow checks both files against the published
package.

## The activation window

The plugin's window lives in `crates/tauri-plugin-moonbase/ui/`: plain HTML, CSS and
JavaScript with no build step, embedded into the crate with `include_str!` and served
over the plugin's `moonbase://` scheme by `src/ui.rs`. It follows the "Solstice
Activation" design that the JUCE module also implements; keep the two in step. Every
screen renders from the `LicenseState` the plugin pushes in, so you can iterate on it
in a browser by stubbing `window.__TAURI_INTERNALS__.invoke` and
`window.__MOONBASE_UI__`.

## Style

Format with the repository's `rustfmt.toml`. Comments explain why, not what. Keep the
behaviour, error mapping and file format in step with moonbase-cpp; where this SDK
departs from it on purpose, say so in `docs/core-sdk.md`.

## Releases

Releases are cut by [semantic-release](https://semantic-release.gitbook.io) on every push
to `main`, as in moonbase-cpp. Pull requests are squash-merged, and their title becomes
the commit it reads, so a title must be a
[Conventional Commit](https://www.conventionalcommits.org) (the PR Title check enforces
it): `feat: ...` releases a minor version, `fix: ...` or `perf: ...` a patch, and
`docs:`, `chore:`, `ci:`, `test:` or `refactor:` nothing.

Releases stay at 0.x while the API settles: a breaking change (a `BREAKING CHANGE:`
footer) bumps the minor version rather than the major. Going to 1.0 is a matter of
removing the `releaseRules` entry in `.releaserc.json`.

A release:

1. Works out the version from the commits since the last `v*` tag.
2. Writes `CHANGELOG.md`, and the version everywhere it lives
   (`scripts/bump-version.sh`): the workspace version and its dependency table in
   `Cargo.toml`, `Cargo.lock`, the frontend bindings' `package.json`, and the tag the
   install snippets in `README.md`, `docs/core-sdk.md` and `docs/tauri.md` pin.
3. Commits that as `chore(release): x.y.z [skip ci]`, tags it `vx.y.z`, and pushes both.
4. Publishes both crates to crates.io (`scripts/publish-crates.sh`), when the workflow
   has a token for it, and creates the GitHub release.

### Publishing to crates.io

The release workflow publishes with whichever token it has: one from crates.io's
trusted publishing when the repository variable `CRATES_IO_TRUSTED_PUBLISHING` is
`true`, else a `CARGO_REGISTRY_TOKEN` secret. With neither, releases are cut without
publishing. Trusted publishing is the one to keep, since nothing long-lived is stored,
but crates.io offers it only for a crate that already exists, so the first version goes
out with a token:

1. Sign in to [crates.io](https://crates.io) with GitHub, verify an email address, and
   create an API token (Account Settings, API Tokens) with the `publish-new` and
   `publish-update` scopes, limited to the crates `moonbase` and
   `tauri-plugin-moonbase`, expiring in a day or so.
2. Publish the first time, either way:
   - From the release tag, locally: `git checkout vX.Y.Z`, `cargo login`, then
     `cargo publish --workspace`.
   - Or let the release do it: add the token as the repository secret
     `CARGO_REGISTRY_TOKEN` before the release runs.
3. On each crate's page on crates.io (Settings, Trusted Publishing), add a GitHub
   publisher: owner `Moonbase-sh`, repository `moonbase-rs`, workflow `release.yml`.
4. Set the repository variable `CRATES_IO_TRUSTED_PUBLISHING` to `true`, then delete
   the `CARGO_REGISTRY_TOKEN` secret, if you added one, and revoke the token.
5. Add the other maintainers as owners (`cargo owner --add <github-login> moonbase`,
   and the same for `tauri-plugin-moonbase`), so the crates don't hang on one account.

CI's lint job runs `cargo publish --workspace --dry-run` on every pull request, which
builds both crates from their packaged sources alone, so a release can publish without
building them again.
