#!/usr/bin/env bash
# Maintainer-only: prints the `pin!(...)` lines for crates/api/src/lib.rs `PINS`.
# Usage: scripts/api-pins.sh [@minecraft/]<pkg>@<version> ...
#   scripts/api-pins.sh server@2.10.0 server@2.11.0-beta.1.26.50-stable \
#     server-ui@2.2.0 server-ui@2.3.0-beta.1.26.50-stable \
#     vanilla-data@1.26.50 bedrock-schemas@1.26.50
# Pick per module the latest stable and the latest beta for the game version
# (`npm view @minecraft/server versions --json`). Needs npm, curl and b3sum
# (`cargo install b3sum`). Size and blake3 are computed from the bytes YOU download; the
# registry's own `integrity`/`shasum` are never used.
set -euo pipefail
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for spec in "$@"; do
  spec=${spec#@minecraft/}
  pkg=${spec%@*}
  ver=${spec##*@}
  url=$(npm view "@minecraft/$pkg@$ver" dist.tarball)
  curl -fsSL "$url" -o "$tmp/f.tgz"
  lic=$(npm view "@minecraft/$pkg@$ver" license)
  [ "$lic" = "MIT" ] || { echo "$pkg@$ver: license is '$lic', not MIT" >&2; exit 1; }
  size=$(wc -c < "$tmp/f.tgz" | tr -d ' ')
  hash=$(b3sum --no-names "$tmp/f.tgz")
  printf '        pin!(\n            "%s",\n            "%s",\n            "%s",\n            %s,\n            "%s"\n        ),\n' \
    "$pkg" "$pkg" "$ver" "$size" "$hash"
done
