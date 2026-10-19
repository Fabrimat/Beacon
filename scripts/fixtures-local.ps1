<#
.SYNOPSIS
    Maintainer-only local production-equivalence check for `beacon migrate`. Never run in CI.

.DESCRIPTION
    For a real (private) Regolith project it:
      1. copies the project to %TEMP%\beacon-mig\<name> (with .regolith/ and
         filters_data/node_modules); the source is never modified;
      2. appends a `beacon_oracle` profile to the COPY's config.json (the packaging profile's
         filters, nested profiles flattened, `exact` export to ./.beacon-oracle/{BP,RP}) and runs
         `regolith run beacon_oracle`;
      3. for system_template projects also runs `beacon_eval` = [subfunctions, system_template,
         shapescape_custom_blocks_generator, system_template_esbuild] exact-exported to
         ./.beacon-eval/ (with the retained tmp `data/` and system_template_log.json);
      4. runs `beacon migrate <copy> --out <tmp>/migrated --oracle ... --oracle-profile <profile> [--evaluated ...]
         --trust --json` and prints the summary and the report paths.
    Nothing is committed and nothing is written inside the repo. `-WhatIf` only prints the steps.

.PARAMETER Project
    Regolith project dir (local path).
.PARAMETER PackProfile
    Profile whose filters are the oracle (alias -Profile). Default: packaging.
.PARAMETER EnvVar
    K=V pairs for the legacy run and `beacon migrate --env` (alias -Env).
.PARAMETER Name
    Work dir name under %TEMP%\beacon-mig (default: the project dir name).

.EXAMPLE
    pwsh scripts/fixtures-local.ps1 D:\Repos\SomeProject\regolith -Profile packaging -WhatIf
#>
param(
    [Parameter(Mandatory = $true, Position = 0)][string]$Project,
    [Alias('Profile')][string]$PackProfile = 'packaging',
    [Alias('Env')][string[]]$EnvVar = @(),
    [string]$Name,
    # project pre-build step (e.g. 'cd filters_data && npm ci && node preregolith.js'), run in the copy
    [string]$Pre,
    [switch]$WhatIf
)

$ErrorActionPreference = 'Stop'
$src = (Resolve-Path -LiteralPath $Project).Path
if (-not $Name) { $Name = Split-Path -Leaf $src }
if ($Name -match '[\\/:]|^\.+$') { throw "-Name must not contain path separators: $Name" }
$tmp = Join-Path ([IO.Path]::GetTempPath()) "beacon-mig\$Name"
$copy = Join-Path $tmp 'project'
$out = Join-Path $tmp 'migrated'
$repo = Split-Path -Parent $PSScriptRoot
if ($WhatIf -eq $false -and ($tmp.StartsWith($repo, 'OrdinalIgnoreCase'))) { throw "work dir $tmp is inside the repo" }

function Step([string]$what, [scriptblock]$do) {
    Write-Host "==> $what"
    if (-not $WhatIf) { & $do }
}

$cfgPath = Join-Path $src 'config.json'
$cfg = Get-Content -Raw -LiteralPath $cfgPath | ConvertFrom-Json
$profiles = $cfg.regolith.profiles
function Flatten($name) {
    foreach ($f in $profiles.$name.filters) {
        if ($f.profile) { Flatten $f.profile } else { $f }
    }
}
$filters = @(Flatten $PackProfile)
if (-not $filters) { throw "profile '$PackProfile' not found or empty in $cfgPath" }
$isST = [bool]($filters | Where-Object { $_.filter -eq 'system_template' })

Step "copy $src -> $copy (never touches the source)" {
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    New-Item -ItemType Directory -Force $tmp | Out-Null
    Copy-Item -Recurse -LiteralPath $src -Destination $copy
}

function Exact($dir) { @{ target = 'exact'; bpPath = "./$dir/BP"; rpPath = "./$dir/RP"; readOnly = $false } }
Step "append profile beacon_oracle ($($filters.Count) filters of '$PackProfile') to the copy's config.json" {
    $c = Get-Content -Raw -LiteralPath "$copy\config.json" | ConvertFrom-Json
    $c.regolith.profiles | Add-Member -NotePropertyName beacon_oracle -NotePropertyValue @{ filters = $filters; export = (Exact '.beacon-oracle') }
    if ($isST) {
        $names = 'subfunctions', 'system_template', 'shapescape_custom_blocks_generator', 'system_template_esbuild'
        $c.regolith.profiles | Add-Member -NotePropertyName beacon_eval -NotePropertyValue @{
            filters = @($names | ForEach-Object { $n = $_; $filters | Where-Object { $_.filter -eq $n } | Select-Object -First 1 } | Where-Object { $_ })
            export  = (Exact '.beacon-eval')
        }
    }
    $c | ConvertTo-Json -Depth 50 | Set-Content -Encoding utf8 "$copy\config.json"
}

$envs = @{}
foreach ($kv in $EnvVar) { $k, $v = $kv -split '=', 2; $envs[$k] = $v }
function Regolith([string]$profile, [string[]]$more) {
    Push-Location $copy
    try {
        foreach ($k in $envs.Keys) { Set-Item "env:$k" $envs[$k] }
        regolith run $profile @more
        if ($LASTEXITCODE) { throw "regolith run $profile failed ($LASTEXITCODE)" }
    } finally { Pop-Location }
}
if ($Pre) {
    Step "pre-build step in the copy: $Pre" {
        Push-Location $copy
        try { & $env:ComSpec /c $Pre; if ($LASTEXITCODE) { throw "pre-build step failed ($LASTEXITCODE)" } }
        finally { Pop-Location }
    }
}
if (-not (Test-Path "$copy\.regolith\cache\filters")) {
    Step "regolith install-all (filter cache missing in the copy)" {
        Push-Location $copy
        try { regolith install-all; if ($LASTEXITCODE) { throw "regolith install-all failed ($LASTEXITCODE)" } }
        finally { Pop-Location }
    }
}
Step "regolith run beacon_oracle (in the copy)" { Regolith 'beacon_oracle' @() }
if ($isST) {
    Step "regolith run beacon_eval --debug; keep tmp data/ and system_template_log.json in .beacon-eval" {
        Regolith 'beacon_eval' @('--debug')
        $ev = "$copy\.beacon-eval"
        if (Test-Path "$copy\.regolith\tmp\data") { Copy-Item -Recurse "$copy\.regolith\tmp\data" "$ev\data" }
        if (Test-Path "$copy\system_template_log.json") { Copy-Item "$copy\system_template_log.json" $ev }
    }
}

$beacon = if (Get-Command beacon -ErrorAction SilentlyContinue) { 'beacon' } elseif ($env:BEACON_BIN) { $env:BEACON_BIN } else { Join-Path $repo 'target\debug\beacon.exe' }
$margs = @('migrate', $copy, '--out', $out, '--oracle', "$copy\.beacon-oracle", '--oracle-profile', $PackProfile)
if ($isST) { $margs += @('--evaluated', "$copy\.beacon-eval") }
foreach ($kv in $EnvVar) { $margs += @('--env', $kv) }
$margs += @('--trust', '--json')
Step "$beacon $($margs -join ' ')  > $tmp\migrate.json" {
    & $beacon @margs | Set-Content -Encoding utf8 "$tmp\migrate.json"
    $code = $LASTEXITCODE
    Write-Host "equivalence summary (exit $code; non-zero = differences):"
    $j = Get-Content -Raw "$tmp\migrate.json" | ConvertFrom-Json
    $j.PSObject.Properties | Where-Object { $_.Name -match 'equiv|diff|summary|oracle' } | ForEach-Object { "  $($_.Name): $($_.Value | ConvertTo-Json -Compress -Depth 4)" }
    Write-Host "MIGRATION.md: $out\MIGRATION.md"
    Write-Host "diff list (full JSON report): $tmp\migrate.json"
    exit $code
}
if ($WhatIf) { Write-Host "(dry run: nothing was copied, run or written; would report $out\MIGRATION.md and $tmp\migrate.json)" }
