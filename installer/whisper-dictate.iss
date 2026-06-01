; whisper-dictate — Inno Setup installer script
; Build:  iscc /DVERSION=0.2.21 /DVARIANT=cpu whisper-dictate.iss
; Output: whisper-dictate-windows-{VARIANT}-setup.exe

#ifndef VERSION
  #define VERSION "0.0.0"
#endif
#ifndef VARIANT
  #define VARIANT "cpu"
#endif

; Map installer variant names to actual requirements files:
;   nvidia  → requirements-gpu.txt  (CUDA wheels)
;   cpu/amd → requirements-cpu.txt  (CPU-only, AMD has no CUDA path)
#if VARIANT == "nvidia"
  #define REQFILE "requirements-gpu.txt"
#else
  #define REQFILE "requirements-cpu.txt"
#endif

[Setup]
AppId={{7B3F8A2C-4E1D-4F9A-B5C6-D2E8F0A1C3B7}
AppName=whisper-dictate
AppVersion={#VERSION}
AppPublisher=FactusConsulting
AppPublisherURL=https://github.com/FactusConsulting/whisper-dictate
AppSupportURL=https://github.com/FactusConsulting/whisper-dictate/issues
AppUpdatesURL=https://github.com/FactusConsulting/whisper-dictate/releases
VersionInfoVersion={#VERSION}
DefaultDirName={localappdata}\Programs\WhisperDictate
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputBaseFilename=whisper-dictate-windows-{#VARIANT}-setup-{#VERSION}
Compression=lzma2/ultra64
SolidCompression=yes
SetupIconFile=..\assets\whisper-dictate.ico
WizardStyle=modern
UninstallDisplayName=whisper-dictate
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "..\voice_pi.py";        DestDir: "{app}"; Flags: ignoreversion
Source: "..\vp_*.py";            DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\whisper-dictate.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\whisper-dictate.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md";          DestDir: "{app}"; Flags: ignoreversion
Source: "..\TECHNICAL.md";       DestDir: "{app}"; Flags: ignoreversion
Source: "..\dictionary.example.json"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\requirements-parakeet.txt"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\VERSION";            DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\scripts\inject-smoke.py"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "..\{#REQFILE}";               DestDir: "{app}"; DestName: "requirements.txt"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\whisper-dictate\whisper-dictate";    Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"; WorkingDir: "{app}"; IconFilename: "{app}\whisper-dictate.ico"
Name: "{userprograms}\whisper-dictate\Uninstall";          Filename: "{uninstallexe}"
Name: "{userdesktop}\whisper-dictate";                     Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"; WorkingDir: "{app}"; IconFilename: "{app}\whisper-dictate.ico"

[Run]
Filename: "{app}\whisper-dictate.exe"; Parameters: "ui"; Description: "Launch whisper-dictate now"; \
  Flags: postinstall nowait skipifsilent unchecked

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
const
  UninstKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{7B3F8A2C-4E1D-4F9A-B5C6-D2E8F0A1C3B7}_is1';

function GetUninstallString(): String;
var
  S: String;
begin
  S := '';
  if not RegQueryStringValue(HKCU, UninstKey, 'UninstallString', S) then
    RegQueryStringValue(HKLM, UninstKey, 'UninstallString', S);
  Result := S;
end;

procedure UninstallPrevious();
var
  UnStr: String;
  ResultCode, I: Integer;
begin
  UnStr := RemoveQuotes(GetUninstallString());
  if UnStr = '' then
    Exit;
  if Exec(UnStr, '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART', '',
          SW_HIDE, ewWaitUntilTerminated, ResultCode) then
  begin
    // The Inno uninstaller relaunches a temp copy and returns early; wait
    // until the uninstall registry key is gone (max ~60 s) so the freshly
    // installed files are not deleted by the in-progress old uninstaller.
    for I := 1 to 120 do
    begin
      if GetUninstallString() = '' then
        Break;
      Sleep(500);
    end;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  // On upgrade, fully remove the previous version first so no orphaned
  // files survive. The venv (%USERPROFILE%\voice-pi-venv) and the model
  // cache live outside {app}, so they are preserved across upgrades.
  UninstallPrevious();
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Path, NewPath: string;
  Paths: TStringList;
  i: Integer;
  Found: Boolean;
begin
  if CurStep = ssPostInstall then
  begin
    // Add install dir to user PATH so 'whisper-dictate' is runnable from anywhere
    RegQueryStringValue(HKCU, 'Environment', 'PATH', Path);
    NewPath := ExpandConstant('{app}');
    Paths := TStringList.Create;
    try
      Paths.Delimiter := ';';
      Paths.StrictDelimiter := True;
      Paths.DelimitedText := Path;
      Found := False;
      for i := 0 to Paths.Count - 1 do
        if CompareText(Paths[i], NewPath) = 0 then
        begin
          Found := True;
          Break;
        end;
      if not Found then
      begin
        if Path <> '' then Path := Path + ';';
        Path := Path + NewPath;
        RegWriteStringValue(HKCU, 'Environment', 'PATH', Path);
      end;
    finally
      Paths.Free;
    end;
  end;
end;
