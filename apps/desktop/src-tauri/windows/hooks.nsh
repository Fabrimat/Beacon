; Stops what runs from $INSTDIR besides the app itself: a project daemon the app spawned
; ($INSTDIR\beacon.exe serve, 60 s idle) and the esbuild it may be running. A live image makes
; the file overwrite (reinstall) or delete (uninstall) fail. Chosen over a discovery-dir sweep
; with `serve --stop --force`: that needs the per-user run dir and the project path of every
; file, and still misses a daemon whose file is already gone; the image path is exact and only
; matches this install (a `beacon` from elsewhere, e.g. cargo's, is never touched). The path
; goes through an env var so quotes in a user name cannot break the PowerShell command. CIM (not
; Get-Process): the installer is a 32-bit process and cannot read the path of the 64-bit daemon.
!macro BeaconStopDaemons
  System::Call 'Kernel32::SetEnvironmentVariable(t "BEACON_INSTDIR", t "$INSTDIR")'
  nsExec::ExecToStack /TIMEOUT=20000 `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process | Where-Object { @('beacon.exe','esbuild.exe') -contains $$_.Name -and $$_.ExecutablePath -and $$_.ExecutablePath.StartsWith($$env:BEACON_INSTDIR + [char]92, 'OrdinalIgnoreCase') } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force; 'killed ' + $$_.ProcessId }; Start-Sleep -Milliseconds 500"`
  Pop $R8
  Pop $R6
!macroend

; Default per-user install dir: $LOCALAPPDATA\Programs\Beacon, so the install never shares a folder
; with Beacon's data dir ($LOCALAPPDATA\Beacon: tools, api, trust.json). Only when there is no
; previous install location (an upgrade keeps where it is) and the dir is still Tauri's default.
; NSIS_HOOK_PREINSTALL runs inside Section Install, after the template's own `SetOutPath
; $INSTDIR`, so the output path is set again here.
!macro NSIS_HOOK_PREINSTALL
  ReadRegStr $R9 SHCTX "${MANUPRODUCTKEY}" ""
  ${If} $R9 == ""
  ${AndIf} $INSTDIR == "$LOCALAPPDATA\${PRODUCTNAME}"
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${PRODUCTNAME}"
    SetOutPath $INSTDIR
  ${EndIf}
  !insertmacro BeaconStopDaemons
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro BeaconStopDaemons
!macroend
