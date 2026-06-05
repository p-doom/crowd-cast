; Inno Setup script for the crowd-cast Windows agent.
;
; Per-user install (no admin / UAC): the agent stores its config under %APPDATA%,
; autostarts via the HKCU Run key, and fetches OBS at runtime, so nothing needs
; machine-wide privileges.
;
; Build with scripts/build-windows-installer.ps1, which compiles the release
; binary and passes the version + source path in via /D defines:
;   ISCC /DAppVersion=1.0.3 /DAppVersionInfo=1.0.3.0 /DSourceExe=<path> crowd-cast.iss

#ifndef AppVersion
#define AppVersion "0.0.0-dev"
#endif
#ifndef AppVersionInfo
#define AppVersionInfo "0.0.0.0"
#endif
#ifndef SourceDir
#define SourceDir "..\..\target\release"
#endif

#define AppName "crowd-cast"
#define AppExe "crowd-cast-agent.exe"
#define AppPublisher "p-doom"
#define AppUrl "https://github.com/p-doom/crowd-cast"
; Must match APP_AUMID in src/ui/aumid_windows.rs so toast notifications are
; branded by this shortcut.
#define AppUserModelId "dev.crowd-cast.agent"

[Setup]
; Stable across versions — do not change (identifies the app for upgrades/uninstall).
AppId={{30A7FFF9-6F71-4B85-B83D-97C8C31D5E33}
AppName={#AppName}
AppVersion={#AppVersion}
VersionInfoVersion={#AppVersionInfo}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Gracefully close a running agent (Restart Manager) before replacing its files.
CloseApplications=yes
RestartApplications=no
OutputDir=..\..\dist
OutputBaseFilename=crowd-cast-setup-{#AppVersion}
SetupIconFile=..\..\resources\windows\crowd-cast.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2
SolidCompression=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; obs.dll is the loader the agent links against and must be present for the
; process to start; the rest of the OBS runtime (data\, obs-plugins\, codec
; DLLs) is downloaded into {app} on first launch by the bootstrapper.
Source: "{#SourceDir}\obs.dll"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Start Menu shortcut placed directly under Programs at the SAME path the agent
; uses for its runtime fallback (src/ui/aumid_windows.rs), so the running app
; finds it and does not create a duplicate. The AppUserModelID makes toast
; notifications resolve to this shortcut (name + icon = crowd-cast).
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; \
    IconFilename: "{app}\{#AppExe}"; AppUserModelID: "{#AppUserModelId}"

[Registry]
; The autostart Run value is created by the app (wizard "start at login"); make
; sure uninstall removes it. dontcreatekey + ValueType none = no-op on install.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
    ValueType: none; ValueName: "{#AppName}"; Flags: dontcreatekey uninsdeletevalue

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName}"; \
    Flags: nowait postinstall skipifsilent

[UninstallRun]
; Stop a running agent so its files can be removed.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#AppExe}"; \
    Flags: runhidden; RunOnceId: "StopAgent"

[UninstallDelete]
; The OBS runtime is downloaded into {app} on first launch (not tracked by the
; installer), so remove the whole install directory on uninstall.
Type: filesandordirs; Name: "{app}"
