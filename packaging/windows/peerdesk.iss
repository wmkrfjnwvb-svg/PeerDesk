; PeerDesk 测试版的安装程序（Inno Setup 6）。按用户安装，不需要管理员权限。
; 由 scripts/package-windows.sh 调用：iscc /DAppVersion=... /DSourceDir=... /DOutputDir=... /DOutputName=...

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6F1E2B54-3C1D-4D1B-9B7A-5E4C2A9D7F10}
AppName=PeerDesk
AppVersion={#AppVersion}
AppVerName=PeerDesk {#AppVersion} (test)
AppPublisher=PeerDesk
DefaultDirName={autopf}\PeerDesk
DefaultGroupName=PeerDesk
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
Compression=lzma2
SolidCompression=yes
UninstallDisplayIcon={app}\peerdesk.exe
WizardStyle=modern

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\peerdesk.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\peerdesk-server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\测试说明.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Icons]
Name: "{group}\PeerDesk"; Filename: "{app}\peerdesk.exe"
Name: "{group}\测试说明 (README)"; Filename: "{app}\测试说明.txt"
Name: "{group}\{cm:UninstallProgram,PeerDesk}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\PeerDesk"; Filename: "{app}\peerdesk.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\peerdesk.exe"; Description: "{cm:LaunchProgram,PeerDesk}"; Flags: nowait postinstall skipifsilent
