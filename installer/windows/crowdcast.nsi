; CrowdCast NSIS Installer Script
; Builds a Windows installer for CrowdCast Agent

!include "MUI2.nsh"
!include "FileFunc.nsh"

; Application metadata
!define APPNAME "CrowdCast"
!define COMPANYNAME "CrowdCast"
!define DESCRIPTION "Paired screencast and input capture agent"
!define VERSIONMAJOR 0
!define VERSIONMINOR 1
!define VERSIONBUILD 0
!define HELPURL "https://github.com/crowdcast/crowd-cast"
!define UPDATEURL "https://github.com/crowdcast/crowd-cast/releases"
!define ABOUTURL "https://crowdcast.dev"

; Installer attributes
Name "${APPNAME}"
OutFile "CrowdCast-Setup.exe"
InstallDir "$PROGRAMFILES64\${APPNAME}"
InstallDirRegKey HKLM "Software\${APPNAME}" "Install_Dir"
RequestExecutionLevel admin

; Modern UI settings
!define MUI_ABORTWARNING
!define MUI_ICON "..\..\resources\icons\crowdcast.ico"
!define MUI_UNICON "..\..\resources\icons\crowdcast.ico"

; Pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\..\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; Languages
!insertmacro MUI_LANGUAGE "English"

; Installer Section
Section "Install"
    SetOutPath $INSTDIR
    
    ; Install main executable
    File "..\..\agent\target\release\crowdcast-agent.exe"
    
    ; Install OBS plugin
    File "..\..\obs-crowdcast-plugin\build\obs-crowdcast.dll"
    
    ; Install resources
    SetOutPath "$INSTDIR\data\locale"
    File "..\..\obs-crowdcast-plugin\data\locale\en-US.ini"
    
    ; Create uninstaller
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    
    ; Create Start Menu shortcuts
    CreateDirectory "$SMPROGRAMS\${APPNAME}"
    CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\crowdcast-agent.exe"
    CreateShortcut "$SMPROGRAMS\${APPNAME}\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
    
    ; Registry entries for Add/Remove Programs
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "DisplayName" "${APPNAME}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "QuietUninstallString" "$\"$INSTDIR\Uninstall.exe$\" /S"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "InstallLocation" "$\"$INSTDIR$\""
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "Publisher" "${COMPANYNAME}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "HelpLink" "${HELPURL}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "URLUpdateInfo" "${UPDATEURL}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "URLInfoAbout" "${ABOUTURL}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                     "DisplayVersion" "${VERSIONMAJOR}.${VERSIONMINOR}.${VERSIONBUILD}"
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                      "VersionMajor" ${VERSIONMAJOR}
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                      "VersionMinor" ${VERSIONMINOR}
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                      "NoModify" 1
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                      "NoRepair" 1
    
    ; Calculate and store installed size
    ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
    IntFmt $0 "0x%08X" $0
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" \
                      "EstimatedSize" "$0"
    
    ; Install OBS plugin to user's OBS plugins directory
    SetOutPath "$APPDATA\obs-studio\obs-plugins\64bit"
    File "..\..\obs-crowdcast-plugin\build\obs-crowdcast.dll"
    
    SetOutPath "$APPDATA\obs-studio\data\obs-plugins\obs-crowdcast\locale"
    File "..\..\obs-crowdcast-plugin\data\locale\en-US.ini"
    
SectionEnd

; Post-install: Run setup wizard
Section "Run Setup"
    ExecWait '"$INSTDIR\crowdcast-agent.exe" --setup --non-interactive'
SectionEnd

; Uninstaller Section
Section "Uninstall"
    ; Remove files
    Delete "$INSTDIR\crowdcast-agent.exe"
    Delete "$INSTDIR\obs-crowdcast.dll"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir /r "$INSTDIR\data"
    RMDir "$INSTDIR"
    
    ; Remove Start Menu items
    Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
    Delete "$SMPROGRAMS\${APPNAME}\Uninstall.lnk"
    RMDir "$SMPROGRAMS\${APPNAME}"
    
    ; Remove registry entries
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"
    DeleteRegKey HKLM "Software\${APPNAME}"
    
    ; Remove autostart entry
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${APPNAME}"
    
    ; Remove OBS plugin
    Delete "$APPDATA\obs-studio\obs-plugins\64bit\obs-crowdcast.dll"
    RMDir /r "$APPDATA\obs-studio\data\obs-plugins\obs-crowdcast"
    
SectionEnd
