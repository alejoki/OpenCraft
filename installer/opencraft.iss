; -- OpenCraft per-user Windows installer (D290) -------------------------
;
; Built by .github/workflows/release.yml after scripts/package_release.sh
; has staged dist/opencraft-<version>-x86_64-pc-windows-msvc/. ISCC must be
; given three defines (all values are Windows paths):
;
;   ISCC.exe /DMyAppVersion=0.2.2 \
;            /DStageDir=D:\...\dist\opencraft-0.2.2-x86_64-pc-windows-msvc \
;            /DOutDir=D:\...\dist \
;            installer\opencraft.iss
;
; PrivilegesRequired=lowest keeps the install in %LOCALAPPDATA%\OpenCraft:
; the game resolves config/, worldgen/, packs/, saves/ and settings.toml
; from its working directory (D269), so a Program Files install would make
; player state unwritable for a non-elevated process. The shortcut therefore
; sets WorkingDir={app} and no per-machine install mode is offered.
;
; Requires Inno Setup 6.3+ (ArchitecturesAllowed=x64compatible).

#ifndef MyAppVersion
  # error MyAppVersion is not defined; pass /DMyAppVersion=<x.y.z>
#endif
#ifndef StageDir
  # error StageDir is not defined; pass /DStageDir=<staged folder>
#endif
#ifndef OutDir
  # error OutDir is not defined; pass /DOutDir=<dist folder>
#endif

#define MyAppName "OpenCraft"
#define MyAppPublisher "OpenCraft contributors"
#define MyAppURL "https://github.com/alejoki/OpenCraft"
#define MyAppExeName "opencraft.exe"

[Setup]
AppId={{691D4DC0-B315-4F29-B491-BF497D34F2DD}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={localappdata}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#OutDir}
OutputBaseFilename=opencraft-{#MyAppVersion}-x86_64-pc-windows-msvc-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
UninstallDisplayIcon={app}\{#MyAppExeName}
CloseApplications=yes
VersionInfoVersion={#MyAppVersion}.0
VersionInfoProductName={#MyAppName}
VersionInfoProductVersion={#MyAppVersion}
VersionInfoDescription={#MyAppName} {#MyAppVersion} installer
VersionInfoCompany={#MyAppPublisher}
LicenseFile={#StageDir}\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#StageDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\config\*"; DestDir: "{app}\config"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#StageDir}\worldgen\*"; DestDir: "{app}\worldgen"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#StageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
