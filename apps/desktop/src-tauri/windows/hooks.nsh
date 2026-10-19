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
!macroend
