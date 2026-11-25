; The dock (muelle.exe) is a bundled sidecar, not the Tauri main binary, so the
; installer stops and starts it itself.

; Quits the dock, gives the Windows taskbar back and waits for every
; muelle.exe (dock and watchdog) to exit, so its file can be replaced.
!macro MUELLE_STOP_DOCK
  ${If} ${FileExists} "$INSTDIR\muelle.exe"
    ExecWait '"$INSTDIR\muelle.exe" --restore'
    StrCpy $R8 0
    ${Do}
      nsis_tauri_utils::FindProcessCurrentUser "muelle.exe"
      Pop $R9
      ${If} $R9 <> 0
        ${Break}
      ${EndIf}
      ${If} $R8 >= 50
        ; Still running after 5 s: force it, then restore the taskbar the
        ; killed watchdog can no longer restore.
        nsis_tauri_utils::KillProcessCurrentUser "muelle.exe"
        Pop $R9
        Sleep 300
        ExecWait '"$INSTDIR\muelle.exe" --restore'
        ${Break}
      ${EndIf}
      Sleep 100
      IntOp $R8 $R8 + 1
    ${Loop}
  ${EndIf}
!macroend

; The previous fork installed as "Muelle", a different product, so this
; installer does not replace that entry by itself. Stop it, turn its
; autostart off, and uninstall it without deleting %APPDATA%\Muelle.
; The dock copies that folder to %APPDATA%\Nikao Dock on first launch.
!macro NIKAO_RETIRE_MUELLE
  SetShellVarContext current
  StrCpy $R7 "$LOCALAPPDATA\Programs\Muelle"
  ${If} ${FileExists} "$R7\muelle.exe"
    ExecWait '"$R7\muelle.exe" --restore'
    ExecWait '"$R7\muelle.exe" --autostart off'
  ${EndIf}
  StrCpy $R8 0
  ${Do}
    nsis_tauri_utils::FindProcessCurrentUser "muelle.exe"
    Pop $R9
    ${If} $R9 <> 0
      ${Break}
    ${EndIf}
    ${If} $R8 >= 50
      nsis_tauri_utils::KillProcessCurrentUser "muelle.exe"
      Pop $R9
      Sleep 300
      ${If} ${FileExists} "$R7\muelle.exe"
        ExecWait '"$R7\muelle.exe" --restore'
      ${EndIf}
      ${Break}
    ${EndIf}
    Sleep 100
    IntOp $R8 $R8 + 1
  ${Loop}
  ; Settings is muelle-settings.exe. Close it so the old uninstaller does not
  ; stop to ask. This installer is not that process.
  nsis_tauri_utils::KillProcessCurrentUser "muelle-settings.exe"
  Pop $R9
  ; /S leaves the delete-data checkbox unchecked, so pins stay put.
  ; _?= runs the uninstaller in place and makes ExecWait wait for it.
  ${If} ${FileExists} "$R7\uninstall.exe"
    ExecWait '"$R7\uninstall.exe" /S _?=$R7'
  ${EndIf}
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Muelle"
  Delete "$SMPROGRAMS\Muelle.lnk"
  Delete "$DESKTOP\Muelle.lnk"
!macroend

; The details list would otherwise show the internal file names (muelle.exe,
; the old Programs\Muelle folder) to the user. Hidden from the first hook to
; the last, so it also covers the file copy in between.
!macro NSIS_HOOK_PREINSTALL
  DetailPrint "${PRODUCTNAME}"
  SetDetailsPrint none
  !insertmacro MUELLE_STOP_DOCK
  !insertmacro NIKAO_RETIRE_MUELLE
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Updates keep whatever the user chose in Settings.
  ${If} $UpdateMode <> 1
    ExecWait '"$INSTDIR\muelle.exe" --autostart on'
  ${EndIf}
  ; Unelevated, even if the installer was run as administrator.
  nsis_tauri_utils::RunAsUser "$INSTDIR\muelle.exe" ""
  SetDetailsPrint both
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "${PRODUCTNAME}"
  SetDetailsPrint none
  !insertmacro MUELLE_STOP_DOCK
  ; Removes the logon task (and an older Run entry), which would otherwise
  ; keep pointing at the deleted muelle.exe. Updates keep it.
  ${If} $UpdateMode <> 1
  ${AndIf} ${FileExists} "$INSTDIR\muelle.exe"
    ExecWait '"$INSTDIR\muelle.exe" --autostart off'
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  SetDetailsPrint both
  ${If} $UpdateMode <> 1
    SetShellVarContext current
    ; The icon cache is always disposable.
    RmDir /r "$LOCALAPPDATA\Nikao Dock"
    ; Where Nikao Dock remembers that it starts with Windows.
    DeleteRegKey HKCU "Software\Nikao Dock"
    ${If} $DeleteAppDataCheckboxState = 1
      RmDir /r "$APPDATA\Nikao Dock"
    ${EndIf}
  ${EndIf}
!macroend
