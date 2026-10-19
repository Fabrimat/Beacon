# Maintainer-only: prints the `pin!(...)` lines for crates/api/src/lib.rs `PINS`.
# Usage: scripts/api-pins.ps1 server@2.10.0 server@2.11.0-beta.1.26.50-stable `
#          server-ui@2.2.0 server-ui@2.3.0-beta.1.26.50-stable `
#          vanilla-data@1.26.50 bedrock-schemas@1.26.50
# Pick per module the latest stable and the latest beta for the game version
# (`npm view @minecraft/server versions --json`). Needs npm, curl and b3sum
# (`cargo install b3sum`). Size and blake3 are computed from the bytes YOU download; the
# registry's own `integrity`/`shasum` are never used.
$ErrorActionPreference = 'Stop'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  foreach ($spec in $args) {
    $spec = $spec -replace '^@minecraft/', ''
    $i = $spec.LastIndexOf('@')
    $pkg = $spec.Substring(0, $i); $ver = $spec.Substring($i + 1)
    $url = (npm view "@minecraft/$pkg@$ver" dist.tarball).Trim()
    $f = Join-Path $tmp 'f.tgz'
    curl.exe -fsSL $url -o $f
    $lic = (npm view "@minecraft/$pkg@$ver" license).Trim()
    if ($lic -ne 'MIT') { throw "$pkg@${ver}: license is '$lic', not MIT" }
    $size = (Get-Item $f).Length
    $hash = (b3sum --no-names $f).Trim()
    "        pin!(`n            `"$pkg`",`n            `"$pkg`",`n            `"$ver`",`n            $size,`n            `"$hash`"`n        ),"
  }
} finally { Remove-Item -Recurse -Force $tmp }
