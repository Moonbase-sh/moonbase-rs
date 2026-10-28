#!/usr/bin/env bash
#
# Write a release's version everywhere it lives, for semantic-release's prepare
# step (.releaserc.json). Every file written here must also be listed in
# .releaserc.json's git assets, or the rewrite happens in the release workflow's
# working tree and is thrown away. Each write is checked, so a moved or
# reformatted line fails the release rather than publishing a stale version.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: bump-version.sh <new-version>" >&2
    exit 2
fi

new_version="$1"

if [[ ! "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "bump-version.sh: refusing to write non-semver value '$new_version'" >&2
    exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

require_file() {
    if [[ ! -f "$repo_root/$1" ]]; then
        echo "bump-version.sh: $1 not found" >&2
        exit 1
    fi
}

# Portable in-place sed (works on both BSD/macOS and GNU/Linux).
edit() {
    local file="$repo_root/$1"
    shift
    sed -E -i.bak "$@" "$file"
    rm -f "${file}.bak"
}

check() {
    local rel="$1" pattern="$2" what="$3"
    if ! grep -Eq "$pattern" "$repo_root/$rel"; then
        echo "bump-version.sh: failed to update $what in $rel to $new_version" >&2
        exit 1
    fi
    echo "bumped $what in $rel to $new_version"
}

# Both crates take the workspace's version, and depend on each other through the
# workspace's dependency table, which names it too.
require_file Cargo.toml
edit Cargo.toml \
    -e "s/^version = \"[0-9]+\.[0-9]+\.[0-9]+\"/version = \"${new_version}\"/" \
    -e "s/^(moonbase = \{ version = \")[0-9]+\.[0-9]+\.[0-9]+/\1${new_version}/" \
    -e "s/^(tauri-plugin-moonbase = \{ version = \")[0-9]+\.[0-9]+\.[0-9]+/\1${new_version}/"
check Cargo.toml "^version = \"${new_version}\"$" "the workspace version"
check Cargo.toml "^moonbase = \{ version = \"${new_version}\"" "the moonbase dependency"
check Cargo.toml "^tauri-plugin-moonbase = \{ version = \"${new_version}\"" "the tauri-plugin-moonbase dependency"

# The lock file records the workspace crates' versions too; keep it in step so a
# locked build of the release commit doesn't rewrite it.
require_file Cargo.lock
edit Cargo.lock "/^name = \"(moonbase|tauri-plugin-moonbase)\"$/{n;s/^version = \"[^\"]+\"/version = \"${new_version}\"/;}"
for crate in moonbase tauri-plugin-moonbase; do
    if ! grep -A1 -E "^name = \"${crate}\"$" "$repo_root/Cargo.lock" | grep -Fqx "version = \"${new_version}\""; then
        echo "bump-version.sh: failed to update $crate in Cargo.lock to $new_version" >&2
        exit 1
    fi
done
echo "bumped the workspace crates in Cargo.lock to $new_version"

# The plugin's frontend bindings ship with it.
package_json="crates/tauri-plugin-moonbase/guest-js/package.json"
require_file "$package_json"
edit "$package_json" "s/^(  \"version\": \")[0-9]+\.[0-9]+\.[0-9]+(\")/\1${new_version}\2/"
check "$package_json" "^  \"version\": \"${new_version}\"" "the package version"

# The release tag the install snippets pin.
pinned_files=(
    "README.md"
    "docs/core-sdk.md"
    "docs/tauri.md"
)
for rel in "${pinned_files[@]}"; do
    require_file "$rel"
    edit "$rel" "s|(github\.com/Moonbase-sh/moonbase-rs\", tag = \"v)[0-9]+\.[0-9]+\.[0-9]+|\1${new_version}|"
    check "$rel" "moonbase-rs\", tag = \"v${new_version}\"" "the pinned tag"
done
