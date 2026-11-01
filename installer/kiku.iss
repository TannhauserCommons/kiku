; Kiku's installer: per user, no admin, into %LOCALAPPDATA%\Programs\Kiku with a Start menu shortcut.
; It packs what scripts\build-release.ps1 staged, built for x64 (the default) or ARM64:
;   iscc /DVersion=0.1.0 installer\kiku.iss                -> target\dist\Kiku-0.1.0-x64-setup.exe
;   iscc /DVersion=0.1.0 /DArch=arm64 installer\kiku.iss   -> target\dist\Kiku-0.1.0-arm64-setup.exe
; Updates run it with /SILENT /SUPPRESSMSGBOXES /NORESTART: it waits for Kiku to quit, then opens it again.

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
Source: "{#Staged}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{userprograms}\Kiku"; Filename: "{app}\kiku.exe"; WorkingDir: "{app}"

[InstallDelete]
; Copies of programs that were still running at the last update (see CurStepChanged).
Type: files; Name: "{app}\*.old"

[UninstallDelete]
; The GPU pack and its downloads, which Kiku puts next to itself.
Type: filesandordirs; Name: "{app}"

[Run]
Filename: "{app}\kiku.exe"; Description: "Start Kiku"; Flags: nowait postinstall skipifsilent
; An update, run silently by Kiku: Kiku quit for it, so it opens again.
Filename: "{app}\kiku.exe"; Flags: nowait; Check: WizardSilent

[Code]
// Kiku starts an update and then quits. Silently, AppMutex would give up at once, so wait for Kiku first.
function InitializeSetup(): Boolean;
var
  Waited: Integer;
begin
  Waited := 0;
  while WizardSilent and CheckForMutexes('{#Mutex}') and (Waited < {#QuitWait}) do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;
  Result := True;
end;

// An assistant using it can keep the MCP server running, and the GPU helper may outlive Kiku for a moment. A running
// program can't be replaced but can be renamed; [InstallDelete] removes the copy once nothing runs it.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Name, Path: String;
  I: Integer;
begin
  if CurStep <> ssInstall then
    Exit;
  for I := 0 to 1 do
  begin
    if I = 0 then Name := 'kiku-mcp' else Name := 'kiku-engine';
    Path := ExpandConstant('{app}\') + Name;
    if FileExists(Path + '.exe') then
      RenameFile(Path + '.exe', Path + '.' + GetDateTimeString('yyyymmddhhnnsszzz', #0, #0) + '.old');
  end;
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
