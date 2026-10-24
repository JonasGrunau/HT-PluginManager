; HurenTohn Plugin Manager installer (Inno Setup 6). release.yml calls:
;   ISCC.exe /DAppVersion=<version> /DExe=<path to hurentohn.exe> /O<out dir> packaging\windows\HurenTohn.iss
; It installs per user (no admin prompt); the app asks for admin rights only
; when it installs a plugin into Common Files\VST3.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef Exe
  #define Exe "..\..\target\release\hurentohn.exe"
#endif
#define AppName "HurenTohn Plugin Manager"

[Setup]
AppId={{448ECD19-DA35-4734-9402-B2A99D8A17C9}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Jonas Grunau
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
DisableDirPage=yes
OutputBaseFilename=HurenTohn-Plugin-Manager-{#AppVersion}-Windows-Setup
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
SetupIconFile=..\..\assets\AppIcon.ico
UninstallDisplayIcon={app}\{#AppName}.exe
UninstallDisplayName={#AppName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Exe}"; DestDir: "{app}"; DestName: "{#AppName}.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppName}.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppName}.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppName}.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
