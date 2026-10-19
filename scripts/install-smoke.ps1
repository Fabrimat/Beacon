<#
.SYNOPSIS
  NOTE: installs to the real %LOCALAPPDATA%\Programs\Beacon, so a developer's own install is
  replaced and then removed (the Beacon data dir is untouched; BEACON_HOME is a temp dir).
  Install smoke for exit criterion 1 (docs/phase5-plan.md 3B): silent per-user install ->
  `<app>.exe --smoke` (first launch, create a project, Play into a temp com.mojang, packs
  landed) -> reinstall while the Play daemon is alive -> uninstall -> data dir untouched.
  Every step is timestamped; a JSON table and a summary are printed and written to -OutJson.
  Exit 1 on any failed check or when install start -> packs deployed exceeds -MaxMinutes.

  Does NOT cover launching Minecraft (needs the game and a signed-in account): that half is the
  human test (docs/alpha-test.md).

  "No other installs": PATH is scrubbed for the smoke of every directory holding node, python,
  git, ... (process level: a CI image has them installed, the smoke just cannot see them), and
  the scrub is verified. For a truly clean machine use Windows Sandbox
  (scripts/sandbox/install-smoke.wsb).
.EXAMPLE
  pwsh scripts/install-smoke.ps1 -Installer .\Beacon_0.0.0_x64-setup.exe
#>
param(
  [Parameter(Mandatory)][string]$Installer,
  [string]$OutJson = 'install-smoke-timings.json',
  [double]$MaxMinutes = 5
)
$ErrorActionPreference = 'Stop'
$Installer = (Resolve-Path $Installer).Path
$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\Beacon'
# Isolation: a temp BEACON_HOME for the whole run (installer, uninstaller and both smoke runs
# inherit it), so the machine's real %LOCALAPPDATA%\Beacon (tools, trust.json, run dir) is never
# touched. It is also the "data dir" the uninstall must leave alone.
$origHome = $env:BEACON_HOME
$DataDir = Join-Path ([IO.Path]::GetTempPath()) "beacon-smoke-home-$PID"
New-Item -ItemType Directory -Force $DataDir | Out-Null
$env:BEACON_HOME = $DataDir
$tmp = [IO.Path]::GetTempPath()
$steps = [System.Collections.Generic.List[object]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
$startedAt = [DateTimeOffset]::UtcNow

function Step([string]$Name, [scriptblock]$Body) {
  $start = [DateTimeOffset]::UtcNow
  $ok = $true
  try { $detail = [string](& $Body) } catch { $ok = $false; $detail = $_.Exception.Message; $failures.Add("${Name}: $detail") }
  $end = [DateTimeOffset]::UtcNow
  $steps.Add([pscustomobject]@{ step = $Name; ok = $ok; start = $start.ToString('o'); end = $end.ToString('o'); seconds = [math]::Round(($end - $start).TotalSeconds, 2); detail = $detail })
  Write-Host ('[{0}] {1,-30} {2,-4} {3,7:N2}s  {4}' -f $end.ToString('HH:mm:ss.fff'), $Name, $(if ($ok) { 'ok' } else { 'FAIL' }), ($end - $start).TotalSeconds, $detail)
}

function Run-Installer {
  $p = Start-Process -FilePath $Installer -ArgumentList '/S' -PassThru -Wait
  if ($p.ExitCode -ne 0) { throw "installer exit code $($p.ExitCode)" }
}

function Daemons { Get-Process beacon -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir + '\', 'OrdinalIgnoreCase') } }

function Run-Smoke([string[]]$More) {
  $p = Start-Process -FilePath $mainExe.FullName -ArgumentList (@('--smoke') + $More) -PassThru -WindowStyle Hidden
  if (-not $p.WaitForExit(180000)) { $p.Kill(); throw 'smoke timed out (180 s)' }
  $p.ExitCode
}

function Data-Snapshot {
  if (-not (Test-Path $DataDir)) { return 'absent' }
  (Get-ChildItem $DataDir -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.FullName -notlike (Join-Path $DataDir 'run\*') } | Sort-Object FullName |
    ForEach-Object { '{0}|{1}|{2}' -f $_.FullName, $_.Length, $_.LastWriteTimeUtc.Ticks }) -join "`n"
}

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
Write-Host "installer: $Installer ($([math]::Round((Get-Item $Installer).Length / 1MB, 1)) MB); elevated shell: $isAdmin"

# --- PATH scrub (process level) and a clean env: nothing that could hide a missing dependency
$tools = 'node', 'npm', 'npx', 'pnpm', 'yarn', 'python', 'python3', 'py', 'pip', 'git', 'deno', 'bun'
$origPath = $env:PATH
Step 'scrub_path' {
  $kept = $origPath -split ';' | Where-Object {
    $dir = $_
    $dir -and -not ($tools | Where-Object { (Test-Path -LiteralPath (Join-Path $dir "$_.exe")) -or (Test-Path -LiteralPath (Join-Path $dir "$_.cmd")) })
  }
  $env:PATH = $kept -join ';'
  $left = Get-Command $tools -ErrorAction SilentlyContinue
  if ($left) { throw "still on PATH: $($left.Source -join ', ')" }
  foreach ($v in 'BEACON_ESBUILD', 'BEACON_COM_MOJANG', 'BEACON_SERVE_IDLE') { Remove-Item "Env:$v" -ErrorAction SilentlyContinue }
  "dropped $((($origPath -split ';').Count) - $kept.Count) PATH entries; none of $($tools -join ',') resolves"
}

$installStart = [DateTimeOffset]::UtcNow
Step 'install_silent' { Run-Installer; '/S, per-user' }

Step 'verify_install' {
  foreach ($f in 'beacon.exe', 'esbuild.exe') { if (-not (Test-Path (Join-Path $InstallDir $f))) { throw "$f not in $InstallDir" } }
  $names = { param($root) Get-ChildItem $root -ErrorAction SilentlyContinue | ForEach-Object { (Get-ItemProperty $_.PSPath).DisplayName } }
  if ('Beacon' -notin (& $names 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall')) { throw 'no per-user (HKCU) uninstall entry' }
  if ('Beacon' -in (& $names 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall')) { throw 'a per-machine (HKLM) entry exists: the installer elevated' }
  # An elevated shell (CI runners) cannot show a UAC prompt, so "no UAC" is only proven from a
  # normal shell; the per-user location + HKCU-only registration holds either way.
  "dir $InstallDir, HKCU entry only; " + $(if ($isAdmin) { 'shell elevated: UAC absence not provable here' } else { 'unelevated shell: no UAC' })
}

$mainExe = Get-ChildItem $InstallDir -Filter *.exe -ErrorAction SilentlyContinue | Where-Object { $_.Name -notin 'beacon.exe', 'esbuild.exe', 'uninstall.exe' } | Select-Object -First 1
$smokeOut = Join-Path $tmp "beacon-smoke-$PID.jsonl"
$smoke = @()
Step 'smoke' {
  if (-not $mainExe) { throw "no app exe in $InstallDir" }
  $code = Run-Smoke '--out', $smokeOut
  $script:smoke = @(Get-Content $smokeOut -ErrorAction SilentlyContinue | ForEach-Object { $_ | ConvertFrom-Json })
  if ($code -ne 0) { throw "smoke exit ${code}: $(($script:smoke | Where-Object { -not $_.ok } | ConvertTo-Json -Compress))" }
  ($script:smoke | ForEach-Object { '{0}@{1}ms' -f $_.step, $_.elapsedMs }) -join ' '
}

$deployed = $smoke | Where-Object { $_.step -eq 'packs_deployed' -and $_.ok } | Select-Object -First 1
$totalSeconds = $null
if ($deployed) {
  $totalSeconds = [math]::Round(([DateTimeOffset]::FromUnixTimeMilliseconds([int64]$deployed.unixMs) - $installStart).TotalSeconds, 2)
}
Step 'budget_install_to_packs' {
  if ($null -eq $totalSeconds) { throw 'packs were never deployed' }
  if ($totalSeconds -gt $MaxMinutes * 60) { throw "$totalSeconds s exceeds $MaxMinutes min" }
  "$totalSeconds s (limit $($MaxMinutes * 60) s)"
}

Step 'daemon_alive_after_play' {
  $d = @(Daemons)
  if (-not $d) { throw 'no beacon daemon from the install dir is running (expected the Play daemon, 60 s idle)' }
  "pid $($d.Id -join ',')"
}

# Reinstall while the daemon is alive: the PREINSTALL hook must stop it or the overwrite fails.
Step 'reinstall_with_daemon_alive' {
  Run-Installer
  if (-not (Test-Path (Join-Path $InstallDir 'beacon.exe'))) { throw 'beacon.exe missing after reinstall' }
  'ok'
}

# A daemon again, so the uninstall also runs with one alive (PREUNINSTALL hook).
Step 'daemon_alive_before_uninstall' {
  $code = Run-Smoke '--dir', (Join-Path $tmp "beacon-smoke-2-$PID")
  if ($code -ne 0) { throw "second smoke exit $code" }
  if (-not (Daemons)) { throw 'no daemon alive before uninstall' }
  'daemon alive'
}

$dataBefore = Data-Snapshot
Step 'uninstall_silent' {
  $u = Join-Path $InstallDir 'uninstall.exe'
  # `_?=` makes the NSIS uninstaller run in place and synchronously (otherwise it copies itself
  # to %TEMP% and returns at once); it then cannot delete itself.
  $p = Start-Process -FilePath $u -ArgumentList '/S', "_?=$InstallDir" -PassThru -Wait
  if ($p.ExitCode -ne 0) { throw "uninstaller exit code $($p.ExitCode)" }
  Remove-Item $u -Force -ErrorAction SilentlyContinue
  if (Test-Path (Join-Path $InstallDir 'beacon.exe')) { throw 'beacon.exe still present after uninstall' }
  Remove-Item $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
  'install dir removed'
}

Step 'verify_data_dir_untouched' {
  $after = Data-Snapshot
  if ($after -ne $dataBefore) { throw 'data dir changed by the uninstall' }
  "$DataDir unchanged ($(($after -split "`n").Count) entries)"
}

foreach ($d in "beacon-smoke-$PID", "beacon-smoke-2-$PID") { Remove-Item (Join-Path $tmp $d) -Recurse -Force -ErrorAction SilentlyContinue }
Remove-Item $smokeOut -Force -ErrorAction SilentlyContinue
$env:PATH = $origPath
if ($null -eq $origHome) { Remove-Item Env:BEACON_HOME -ErrorAction SilentlyContinue } else { $env:BEACON_HOME = $origHome }
Remove-Item $DataDir -Recurse -Force -ErrorAction SilentlyContinue

$pass = $failures.Count -eq 0
$result = [pscustomobject]@{
  pass = $pass; installer = $Installer; installerMB = [math]::Round((Get-Item $Installer).Length / 1MB, 2)
  installToPacksDeployedSeconds = $totalSeconds; limitSeconds = $MaxMinutes * 60; elevatedShell = $isAdmin
  startedAt = $startedAt.ToString('o'); finishedAt = [DateTimeOffset]::UtcNow.ToString('o'); steps = $steps; failures = $failures
}
$json = $result | ConvertTo-Json -Depth 5
Set-Content -Path $OutJson -Value $json -Encoding utf8
Write-Host "`n=== JSON ($OutJson) ===`n$json"
Write-Host "`n=== Summary ==="
($steps | Format-Table step, ok, seconds, detail -AutoSize -Wrap | Out-String -Width 200) | Write-Host
Write-Host ('install start -> packs deployed: {0} s (limit {1} s)   result: {2}' -f $totalSeconds, ($MaxMinutes * 60), $(if ($pass) { 'PASS' } else { 'FAIL' }))
exit $(if ($pass) { 0 } else { 1 })
