#!/usr/bin/env bash
#
# Publish the workspace's crates to crates.io at the release's version, for
# semantic-release's publish step (.releaserc.json). `cargo publish --workspace`
# puts out the core before the plugin that depends on it, and skips the example
# app, which has `publish = false`.
#
# The token comes from crates.io's trusted publishing, or a CARGO_REGISTRY_TOKEN
# secret (see .github/workflows/release.yml). Without one the release is cut, and
# tagged, without publishing, and the run carries a warning saying so: a release that
# is green but never reached crates.io is easy to miss otherwise.

set -euo pipefail

if [[ -z "${CARGO_REGISTRY_TOKEN:-}" ]]; then
    echo "::warning title=crates.io::No crates.io token, so the crates were not published (see Publishing to crates.io in CONTRIBUTING.md)"
    exit 0
fi

# Not rebuilt here: CI's dry run builds both crates from their packaged sources on
# every pull request, on a runner with the webview libraries the plugin needs.
cargo publish --workspace --locked --no-verify
