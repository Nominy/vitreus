; Vitreus Setup for Windows (Inno Setup 6). Built by scripts/package.sh:
;
;   iscc /DAppVersion=0.2.0 /DBinary=path\to\vitreus.exe /DRoot=repo /O<out-dir> assets\windows\vitreus.iss
;
; Installs per user without admin by default (same folder as install.ps1:
; %LOCALAPPDATA%\Programs\Vitreus); the privileges dialog or /ALLUSERS installs
; for everyone under Program Files. File types, "Open with" and Default Apps
; come from `vitreus --register`, like every other install method.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif
#ifndef Binary
  #error Pass /DBinary=path\to\vitreus.exe
#endif
#ifndef Root
  #define Root "..\.."
#endif

[Setup]
AppId={{8EF3EEE6-0F4E-4D9D-AC9C-A9893BCE66D6}
AppName=Vitreus
AppVersion={#AppVersion}
AppVerName=Vitreus {#AppVersion}
AppPublisher=Nominy
AppPublisherURL=https://github.com/Nominy/vitreus
AppSupportURL=https://github.com/Nominy/vitreus/issues
AppUpdatesURL=https://github.com/Nominy/vitreus/releases
AppCopyright=MIT License
VersionInfoVersion={#AppVersion}
VersionInfoDescription=Vitreus Setup
DefaultDirName={autopf}\Vitreus
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableReadyPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
WizardStyle=modern
WizardSizePercent=100
SetupIconFile={#Root}\assets\app\vitreus.ico
WizardImageFile={#Root}\assets\windows\setup-large.bmp,{#Root}\assets\windows\setup-large-200.bmp
WizardSmallImageFile={#Root}\assets\windows\setup-small.bmp,{#Root}\assets\windows\setup-small-200.bmp
UninstallDisplayIcon={app}\vitreus.exe
UninstallDisplayName=Vitreus
OutputBaseFilename=vitreus-setup-x64
Compression=lzma2/ultra64
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes
ChangesEnvironment=yes

[Tasks]
Name: "setdefault"; Description: "Make Vitreus my default photo viewer (opens Windows Settings)"
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked
Name: "addtopath"; Description: "Add &vitreus to PATH for the terminal"; Flags: unchecked

[Files]
Source: "{#Binary}"; DestDir: "{app}"; DestName: "vitreus.exe"; Flags: ignoreversion
Source: "{#Root}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Vitreus"; Filename: "{app}\vitreus.exe"
Name: "{autodesktop}\Vitreus"; Filename: "{app}\vitreus.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\vitreus.exe"; Parameters: "--register {code:MachineFlag}"; Flags: runhidden waituntilterminated; StatusMsg: "Registering photo types..."
Filename: "ms-settings:defaultapps?{code:RegisteredApp}=Vitreus"; Flags: shellexec nowait skipifsilent; Tasks: setdefault

[UninstallRun]
Filename: "{app}\vitreus.exe"; Parameters: "--unregister {code:MachineFlag}"; Flags: runhidden waituntilterminated; RunOnceId: "Unregister"

[Code]
const
  ScriptUninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\Vitreus';

function MachineFlag(Param: String): String;
begin
  if IsAdminInstallMode then Result := '--machine' else Result := '';
end;

function RegisteredApp(Param: String): String;
begin
  if IsAdminInstallMode then Result := 'registeredAppMachine' else Result := 'registeredAppUser';
end;

function EnvironmentKey(var Root: Integer): String;
begin
  if IsAdminInstallMode then begin
    Root := HKEY_LOCAL_MACHINE;
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  end else begin
    Root := HKEY_CURRENT_USER;
    Result := 'Environment';
  end;
end;

{ Adds or removes the install folder in PATH, leaving every other entry alone. }
procedure SetPathEntry(Present: Boolean);
var
  Root, I: Integer;
  Key, Current, Entry, Rest, Updated, Dir: String;
begin
  Key := EnvironmentKey(Root);
  if not RegQueryStringValue(Root, Key, 'Path', Current) then Current := '';
  Dir := ExpandConstant('{app}');
  Updated := '';
  Rest := Current;
  while Rest <> '' do begin
    I := Pos(';', Rest);
    if I = 0 then begin Entry := Rest; Rest := ''; end
    else begin Entry := Copy(Rest, 1, I - 1); Delete(Rest, 1, I); end;
    if (Entry <> '') and (CompareText(RemoveBackslashUnlessRoot(Entry), Dir) <> 0) then begin
      if Updated <> '' then Updated := Updated + ';';
      Updated := Updated + Entry;
    end;
  end;
  if Present then begin
    if Updated <> '' then Updated := Updated + ';';
    Updated := Updated + Dir;
  end;
  if Updated <> Current then RegWriteExpandStringValue(Root, Key, 'Path', Updated);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ScriptLocation: String;
begin
  if CurStep = ssPostInstall then begin
    if WizardIsTaskSelected('addtopath') then SetPathEntry(True);
    { Installed over an install.ps1 copy in the same folder: Setup owns it now,
      so drop that "Apps & features" entry and its uninstaller script. }
    if RegQueryStringValue(HKA, ScriptUninstallKey, 'InstallLocation', ScriptLocation) and
       (CompareText(RemoveBackslashUnlessRoot(ScriptLocation), ExpandConstant('{app}')) = 0) then begin
      RegDeleteKeyIncludingSubkeys(HKA, ScriptUninstallKey);
      DeleteFile(ExpandConstant('{app}\install.ps1'));
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then SetPathEntry(False);
end;
