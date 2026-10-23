; Windows installer for AeternaVault (Inno Setup 6).
;
;   iscc /DAppVersion=0.5.0 installer\windows\AeternaVault.iss
;
; Installs into Program Files, adds aeternavault-cli to PATH and the PowerShell
; module to the machine-wide module folder. Settings live in the user profile
; (%APPDATA%\AeternaVault), so installing a newer version over an older one
; keeps them. Uninstalling removes program files, shortcuts, PATH entry,
; autostart and all registry entries; settings are removed only on request.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif
#define RepoRoot "..\.."

[Setup]
AppId={{3E0B6C1D-7A4F-4C2E-9B8D-5F1A2C3D4E5F}
AppName=AeternaVault
AppVersion={#AppVersion}
AppVerName=AeternaVault {#AppVersion}
AppPublisher=AeternaVault
AppPublisherURL=https://github.com/Benett53/AeternaVault
AppSupportURL=https://github.com/Benett53/AeternaVault/issues
AppUpdatesURL=https://github.com/Benett53/AeternaVault/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\AeternaVault
DefaultGroupName=AeternaVault
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
ChangesEnvironment=yes
SetupIconFile={#RepoRoot}\assets\icon\aeternavault.ico
UninstallDisplayIcon={app}\aeternavault.exe
UninstallDisplayName=AeternaVault
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=AeternaVault-{#AppVersion}-setup-x64
; A window that waits in the notification area is closed with
; `aeternavault --quit` (see PrepareToInstall), not by Restart Manager.
CloseApplications=no
RestartApplications=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"

[CustomMessages]
en.DesktopIcon=Create a desktop shortcut
de.DesktopIcon=Verknüpfung auf dem Desktop erstellen
en.StillRunning=AeternaVault is still running. Quit it from the icon in the notification area, then try again.
de.StillRunning=AeternaVault läuft noch. Bitte über das Symbol im Infobereich beenden und dann erneut versuchen.
en.RemoveSettings=Also remove the settings, history and the key remembered on this computer?%n%nYour backups are not touched.
de.RemoveSettings=Auch Einstellungen, Verlauf und den auf diesem Computer gespeicherten Schlüssel entfernen?%n%nDeine Sicherungen bleiben unverändert.

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopIcon}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\aeternavault.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\aeternavault-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\docs\ENCRYPTION.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\tools\aeterna-decrypt.py"; DestDir: "{app}\tools"; Flags: ignoreversion
Source: "{#RepoRoot}\powershell\AeternaVault\*"; DestDir: "{commonpf64}\WindowsPowerShell\Modules\AeternaVault"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\AeternaVault"; Filename: "{app}\aeternavault.exe"
Name: "{autodesktop}\AeternaVault"; Filename: "{app}\aeternavault.exe"; Tasks: desktopicon

[Registry]
; aeternavault-cli on PATH for every user (removed again on uninstall).
Root: HKLM; Subkey: "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"; \
    ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
    Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\aeternavault.exe"; Description: "{cm:LaunchProgram,AeternaVault}"; \
    Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
; Autostart, Explorer menu, double-click association (all per user, HKCU).
Filename: "{app}\aeternavault-cli.exe"; Parameters: "system cleanup"; \
    Flags: runhidden waituntilterminated; RunOnceId: "SystemCleanup"

[UninstallDelete]
Type: filesandordirs; Name: "{app}\logs"
Type: dirifempty; Name: "{app}"

[Code]
const
  EnvKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';

function NeedsAddPath(Dir: string): Boolean;
var
  Path: string;
begin
  if not RegQueryStringValue(HKLM, EnvKey, 'Path', Path) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

procedure RemovePath(Dir: string);
var
  Path: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKLM, EnvKey, 'Path', Path) then
    exit;
  Path := ';' + Path + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Path));
  if P = 0 then
    exit;
  Delete(Path, P, Length(Dir) + 1);
  Path := Copy(Path, 2, Length(Path) - 2);
  RegWriteExpandStringValue(HKLM, EnvKey, 'Path', Path);
end;

{ Asks running windows to quit. Returns False if one is still running. }
function QuitRunning(Exe: string): Boolean;
var
  Code: Integer;
begin
  Result := True;
  if not FileExists(Exe) then
    exit;
  if Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Result := Code = 0;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  { An older version in the same folder; the new one has not been copied yet. }
  if not QuitRunning(ExpandConstant('{app}\aeternavault.exe')) then
    Result := CustomMessage('StillRunning');
end;

function InitializeUninstall(): Boolean;
begin
  Result := QuitRunning(ExpandConstant('{app}\aeternavault.exe'));
  if not Result then
    SuppressibleMsgBox(CustomMessage('StillRunning'), mbError, MB_OK, IDOK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemovePath(ExpandConstant('{app}'));
    DelTree(ExpandConstant('{commonpf64}\WindowsPowerShell\Modules\AeternaVault'), True, True, True);
    if not UninstallSilent() then
      if MsgBox(CustomMessage('RemoveSettings'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      begin
        DelTree(ExpandConstant('{userappdata}\AeternaVault'), True, True, True);
        DelTree(ExpandConstant('{localappdata}\AeternaVault'), True, True, True);
      end;
  end;
end;
