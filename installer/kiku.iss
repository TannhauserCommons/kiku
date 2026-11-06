; Kiku's installer: per user, no admin, into %LOCALAPPDATA%\Programs\Kiku with a Start menu shortcut.
; It packs what scripts\build-release.ps1 staged, built for x64 (the default) or ARM64:
;   iscc /DVersion=0.1.0 installer\kiku.iss                -> target\dist\Kiku-0.1.0-x64-setup.exe
;   iscc /DVersion=0.1.0 /DArch=arm64 installer\kiku.iss   -> target\dist\Kiku-0.1.0-arm64-setup.exe
; Updates run it with /SILENT /SUPPRESSMSGBOXES /NORESTART: it waits for Kiku to quit, then opens it again,
; the old version if the update failed.

#ifndef Version
  #error Pass the version: iscc /DVersion=x.y.z installer\kiku.iss
#endif
#ifndef Arch
  #define Arch "x64"
#endif
; x64 also installs on ARM64 Windows 11, which runs it emulated.
#if Arch == "x64"
  #define Allowed "x64compatible"
#else
  #define Allowed Arch
#endif
; CI packs each architecture's build from a folder of its own.
#ifndef Staged
  #define Staged "..\target\dist\Kiku"
#endif
; Held by a running Kiku.
#define Mutex "Local\Kiku"
; How long an update waits for Kiku to quit, in ms.
#define QuitWait "30000"

[Setup]
AppId={{AB2AE4FA-B14C-4FE5-AC10-4F9A7FC76EC9}
AppName=Kiku
AppVersion={#Version}
AppVerName=Kiku {#Version}
AppPublisher=Kiku
AppCopyright=Copyright (c) 2026 Kiku contributors
VersionInfoVersion={#Version}
VersionInfoCompany=Kiku
VersionInfoProductName=Kiku
VersionInfoDescription=Kiku setup
VersionInfoCopyright=Copyright (c) 2026 Kiku contributors
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\Kiku
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
AppMutex={#Mutex}
CloseApplications=no
LicenseFile=..\LICENSE
SetupIconFile=..\assets\icon\kiku.ico
UninstallDisplayIcon={app}\kiku.exe
UninstallDisplayName=Kiku
WizardStyle=modern
ArchitecturesAllowed={#Allowed}
ArchitecturesInstallIn64BitMode={#Allowed}
MinVersion=10.0
OutputDir=..\target\dist
OutputBaseFilename=Kiku-{#Version}-{#Arch}-setup
Compression=lzma2/max
SolidCompression=yes

[Files]
Source: "{#Staged}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs; BeforeInstall: MoveAside

[Icons]
Name: "{userprograms}\Kiku"; Filename: "{app}\kiku.exe"; WorkingDir: "{app}"

[InstallDelete]
; Files that were still in use at the last update (see MoveAside).
Type: files; Name: "{app}\*.old"
Type: files; Name: "{app}\models\*.old"

[UninstallDelete]
; The GPU pack and its downloads, which Kiku puts next to itself.
Type: filesandordirs; Name: "{app}"

[Run]
Filename: "{app}\kiku.exe"; Description: "Start Kiku"; Flags: nowait postinstall skipifsilent
; An update, run silently by Kiku: Kiku quit for it, so it opens again.
Filename: "{app}\kiku.exe"; Flags: nowait; Check: WizardSilent

[Code]
var
  // The files moved aside for their new copies, and where each went.
  Moved, Aside: TStringList;
  Installed: Boolean;

// Kiku starts an update and then quits. Silently, AppMutex would give up at once, so wait for Kiku first.
function InitializeSetup(): Boolean;
var
  Waited: Integer;
begin
  Moved := TStringList.Create;
  Aside := TStringList.Create;
  Waited := 0;
  while WizardSilent and CheckForMutexes('{#Mutex}') and (Waited < {#QuitWait}) do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;
  Result := True;
end;

// A file being replaced may be in use: an assistant can keep the MCP server running (its exe and the Visual C++
// runtime), and the model's helper may outlive Kiku for a moment (its DLLs). Such a file can't be replaced but
// can be renamed, so each old file is moved aside just before its new copy is written.
procedure MoveAside();
var
  Path, OldCopy: String;
begin
  Path := ExpandConstant(CurrentFileName);
  if not FileExists(Path) then
    Exit;
  OldCopy := Path + '.' + GetDateTimeString('yyyymmddhhnnsszzz', #0, #0) + '.old';
  if RenameFile(Path, OldCopy) then
  begin
    Moved.Add(Path);
    Aside.Add(OldCopy);
  end;
end;

// Once installed, the old files go; those still in use stay until [InstallDelete] at the next update.
procedure CurStepChanged(CurStep: TSetupStep);
var
  I: Integer;
begin
  if CurStep <> ssPostInstall then
    Exit;
  Installed := True;
  for I := 0 to Aside.Count - 1 do
    DeleteFile(Aside[I]);
end;

// A failed install is rolled back, which removes the new files but knows nothing of the old ones moved aside:
// they go back, and an update opens the old version again.
procedure DeinitializeSetup();
var
  I, Code: Integer;
begin
  if Installed or (Moved = nil) or (Moved.Count = 0) then
    Exit;
  Log(Format('Putting back the %d files moved aside', [Moved.Count]));
  for I := Moved.Count - 1 downto 0 do
  begin
    DeleteFile(Moved[I]);
    if not RenameFile(Aside[I], Moved[I]) then
      Log('Couldn''t put back ' + Moved[I]);
  end;
  if WizardSilent and not ShellExec('', ExpandConstant('{app}\kiku.exe'), '', ExpandConstant('{app}'), SW_SHOWNORMAL, ewNoWait, Code) then
    Log('Couldn''t start Kiku again: ' + SysErrorMessage(Code));
end;

// Transcripts, settings and downloaded models stay unless the person removing Kiku says otherwise.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent then
    if MsgBox('Also delete your transcripts, settings and downloaded models?', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\kiku'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\kiku'), True, True, True);
    end;
end;
