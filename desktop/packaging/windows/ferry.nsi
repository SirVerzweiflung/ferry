; Ferry installer for Windows 10/11 - built on Linux with makensis (see package-windows.sh).
; Per-user install, no admin rights needed, except one UAC prompt for the firewall rule.
;
;   makensis -DVERSION=0.1.0 -DSRC=<dir with ferry.exe, ferryd.exe> -DOUTFILE=FerrySetup.exe ferry.nsi

Unicode true
!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef SRC
  !define SRC "."
!endif
!ifndef OUTFILE
  !define OUTFILE "FerrySetup.exe"
!endif

!define APP "Ferry"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ferry"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Ferry"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "Ferry ${VERSION}"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Ferry"
VIAddVersionKey "FileDescription" "Ferry - share files and clipboard with your phone"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT License"

!include "MUI2.nsh"
!include "LogicLib.nsh"

!define MUI_ICON "ferry.ico"
!define MUI_UNICON "ferry.ico"
!define MUI_ABORTWARNING

!define MUI_WELCOMEPAGE_TEXT "Ferry sends files and shares the clipboard between this PC and your Android phone.$\r$\n$\r$\nIt runs quietly in the system tray and starts with Windows.$\r$\n$\r$\nAt the end, Windows asks once for permission to let the phone connect through the firewall."
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TITLE "Ferry is installed"
!define MUI_FINISHPAGE_TEXT "Click the Ferry icon in the system tray (maybe hidden under ^) and choose $\"Pair new phone...$\".$\r$\n$\r$\nTo send files: right-click them in Explorer > Send to > Phone (Ferry).$\r$\n$\r$\nImportant: your Wi-Fi must be set to $\"Private network$\" in Windows settings."
!define MUI_FINISHPAGE_RUN "$INSTDIR\ferryd.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start Ferry now"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "Ferry" SecMain
  ; replace a running copy (updates)
  nsExec::Exec 'taskkill /F /IM ferryd.exe'
  Pop $0
  Sleep 400

  SetOutPath "$INSTDIR"
  File "${SRC}/ferry.exe"
  File "${SRC}/ferryd.exe"
  File "ferry.ico"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; start at login
  WriteRegStr HKCU "${RUN_KEY}" "Ferry" '"$INSTDIR\ferryd.exe"'

  ; Start menu + Explorer "Send to"
  CreateShortcut "$SMPROGRAMS\Ferry.lnk" "$INSTDIR\ferryd.exe" "" "$INSTDIR\ferry.ico" 0
  CreateShortcut "$SENDTO\Phone (Ferry).lnk" "$INSTDIR\ferryd.exe" "send" "$INSTDIR\ferry.ico" 0

  ; "ferry" command for terminals (user PATH)
  nsExec::Exec `powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$d='$INSTDIR'; $$p=[Environment]::GetEnvironmentVariable('Path','User'); if(-not $$p){$$p=''}; if(($$p -split ';') -notcontains $$d){[Environment]::SetEnvironmentVariable('Path',($$p.TrimEnd(';')+';'+$$d).TrimStart(';'),'User')}"`
  Pop $0

  ; Settings > Apps entry
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "Ferry"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "Ferry"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\ferry.ico"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1

  ; Firewall: let the phone connect (private/domain networks only). Needs admin -> one UAC prompt.
  nsExec::Exec 'netsh advfirewall firewall show rule name="Ferry"'
  Pop $0
  ${If} $0 != 0
    DetailPrint "Allowing Ferry through the Windows firewall..."
    ExecShellWait "runas" "netsh.exe" 'advfirewall firewall add rule name="Ferry" dir=in action=allow profile=private,domain program="$INSTDIR\ferryd.exe" enable=yes' SW_HIDE
    nsExec::Exec 'netsh advfirewall firewall show rule name="Ferry"'
    Pop $0
    ${If} $0 != 0
      DetailPrint "Firewall rule not added - Windows will ask on first use; allow $\"Private networks$\"."
    ${EndIf}
  ${EndIf}
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /F /IM ferryd.exe'
  Pop $0
  Sleep 400

  Delete "$INSTDIR\ferry.exe"
  Delete "$INSTDIR\ferryd.exe"
  Delete "$INSTDIR\ferry.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\Ferry.lnk"
  Delete "$SENDTO\Phone (Ferry).lnk"
  DeleteRegValue HKCU "${RUN_KEY}" "Ferry"
  DeleteRegKey HKCU "${UNINST_KEY}"

  nsExec::Exec `powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$d='$INSTDIR'; $$p=[Environment]::GetEnvironmentVariable('Path','User'); if($$p){[Environment]::SetEnvironmentVariable('Path',((($$p -split ';') | Where-Object { $$_ -and $$_ -ne $$d }) -join ';'),'User')}"`
  Pop $0

  nsExec::Exec 'netsh advfirewall firewall show rule name="Ferry"'
  Pop $0
  ${If} $0 == 0
    ExecShellWait "runas" "netsh.exe" 'advfirewall firewall delete rule name="Ferry"' SW_HIDE
  ${EndIf}
  ; Settings and pairings in %APPDATA%\Ferry are kept on purpose.
SectionEnd
