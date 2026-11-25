!define SPOTIFINE_INSTALL_KEY "Software\Spotifine\Spotifine"
!define SPOTIFINE_RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Var SpotifineStartupCommand

; Tauri's interactive upgrade can run the old uninstaller before PREINSTALL.
; That uninstaller removes the Run value, so capture it before the first page.
!define MUI_CUSTOMFUNCTION_GUIINIT SpotifineRememberStartup
Function SpotifineRememberStartup
  Push $0
  Push $1
  Push $2
  ReadRegStr $0 HKCU "${SPOTIFINE_INSTALL_KEY}" ""
  ${If} $0 != ""
    ReadRegStr $1 HKCU "${SPOTIFINE_RUN_KEY}" "Spotifine"
    StrCpy $2 "$0\Spotifine.exe"
    ${If} $1 == '"$2"'
    ${OrIf} $1 == '"$2" '
    ${OrIf} $1 == "$2"
    ${OrIf} $1 == "$2 "
      StrCpy $SpotifineStartupCommand $1
    ${EndIf}
  ${EndIf}
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

!macro NSIS_HOOK_POSTINSTALL
  ${If} $SpotifineStartupCommand != ""
    Push $0
    ReadRegStr $0 HKCU "${SPOTIFINE_RUN_KEY}" "Spotifine"
    ${If} $0 == ""
    ${OrIf} $0 == $SpotifineStartupCommand
      ; Restore or re-point only the existing registration. Windows can still
      ; block it via StartupApproved, which we deliberately never change.
      WriteRegStr HKCU "${SPOTIFINE_RUN_KEY}" "Spotifine" '"$INSTDIR\Spotifine.exe"'
    ${EndIf}
    Pop $0
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; After a successful launch the updater exits the shell. Its engine can
  ; still be draining on pipe EOF. Wait on the installed image's actual write
  ; lock, not on every process named PlaybackEngine.exe on this machine.
  ${If} $UpdateMode == 1
    StrCpy $1 50
    ${Do}
      IfFileExists "$INSTDIR\PlaybackEngine.exe" 0 spotifine_engine_released
      System::Call 'kernel32::CreateFileW(w "$INSTDIR\PlaybackEngine.exe", i 0x40000000, i 7, p 0, i 3, i 128, p 0) p.r0'
      ${If} $0 != -1
        System::Call 'kernel32::CloseHandle(p r0)'
        ${ExitDo}
      ${EndIf}
      IntOp $1 $1 - 1
      ${If} $1 == 0
        Abort "Could not replace the Spotifine playback engine. Close Spotifine and try again."
      ${EndIf}
      Sleep 200
    ${Loop}
  ${EndIf}
  spotifine_engine_released:
!macroend
