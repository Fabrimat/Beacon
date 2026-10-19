# Signing hook (OFF by default). desktop.yml merges src-tauri/tauri.sign.conf.json only when the
# repository variable BEACON_SIGN is 1 and the secrets exist; Tauri then calls this per binary.
# Unverified until a certificate exists (phase 5 decision: releases are unsigned for the alpha).
param([Parameter(Mandatory)][string]$File)
$ErrorActionPreference = 'Stop'
if (-not $env:BEACON_SIGN_PFX_BASE64 -or -not $env:BEACON_SIGN_PFX_PASSWORD) {
  throw 'BEACON_SIGN_PFX_BASE64 / BEACON_SIGN_PFX_PASSWORD are not set'
}
$pfx = Join-Path $env:RUNNER_TEMP 'beacon-sign.pfx'
if (-not (Test-Path $pfx)) {
  [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:BEACON_SIGN_PFX_BASE64))
}
$signtool = (Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" | Sort-Object FullName | Select-Object -Last 1).FullName
& $signtool sign /fd sha256 /td sha256 /tr http://timestamp.digicert.com /f $pfx /p $env:BEACON_SIGN_PFX_PASSWORD $File
if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
