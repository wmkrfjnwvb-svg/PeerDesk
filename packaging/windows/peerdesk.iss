; PeerDesk 测试版的安装程序（Inno Setup 6）。默认按用户安装，不需要管理员权限；
; 选“为所有用户安装”（管理员）时可以勾选无人值守：注册系统服务，锁屏和登录界面也能远程。
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
; 安装程序自己的图标（exe 里的图标由 crates/client/build.rs 编进去，快捷方式和卸载列表用它）
SetupIconFile=..\icons\peerdesk.ico
WizardStyle=modern

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "unattended"; Description: "无人值守：注册系统服务，锁屏、登录界面和管理员提示也能远程"; Check: IsAdminInstallMode

[Files]
Source: "{#SourceDir}\peerdesk.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\测试说明.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Icons]
Name: "{group}\PeerDesk"; Filename: "{app}\peerdesk.exe"
Name: "{group}\测试说明 (README)"; Filename: "{app}\测试说明.txt"
Name: "{group}\{cm:UninstallProgram,PeerDesk}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\PeerDesk"; Filename: "{app}\peerdesk.exe"; Tasks: desktopicon

[Run]
; 先存密码再装服务（参数里的密码只有管理员能在进程列表里看到，安装后立即结束）
Filename: "{app}\peerdesk.exe"; Parameters: "service password ""{code:UnattendedPassword}"""; Tasks: unattended; Flags: runhidden waituntilterminated
Filename: "{app}\peerdesk.exe"; Parameters: "service install"; Tasks: unattended; StatusMsg: "正在注册无人值守服务…"; Flags: runhidden waituntilterminated
Filename: "{app}\peerdesk.exe"; Description: "{cm:LaunchProgram,PeerDesk}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; 没装服务时失败也没关系
Filename: "{app}\peerdesk.exe"; Parameters: "service uninstall"; RunOnceId: "PeerDeskService"; Flags: runhidden waituntilterminated

[Code]
var
  PasswordPage: TInputQueryWizardPage;

procedure InitializeWizard;
begin
  PasswordPage := CreateInputQueryPage(wpSelectTasks,
    '无人值守密码', '别人连接这台电脑（包括锁屏时）要输入这个密码',
    '至少 8 位，不能含双引号。以后可以用管理员身份运行 peerdesk service password 新密码 修改。');
  PasswordPage.Add('密码：', True);
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := (PageID = PasswordPage.ID) and not WizardIsTaskSelected('unattended');
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID = PasswordPage.ID then
  begin
    if (Length(PasswordPage.Values[0]) < 8) or (Pos('"', PasswordPage.Values[0]) > 0) then
    begin
      MsgBox('密码至少 8 位，不能含双引号。', mbError, MB_OK);
      Result := False;
    end;
  end;
end;

function UnattendedPassword(Param: String): String;
begin
  Result := PasswordPage.Values[0];
end;
