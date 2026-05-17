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
SetupIconFile=
WizardStyle=modern
UninstallDisplayName=whisper-dictate
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "..\voice_pi.py";        DestDir: "{app}"; Flags: ignoreversion
Source: "..\setup.ps1";          DestDir: "{app}"; Flags: ignoreversion
Source: "..\setup.cmd";          DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md";          DestDir: "{app}"; Flags: ignoreversion
Source: "..\TECHNICAL.md";       DestDir: "{app}"; Flags: ignoreversion
Source: "..\{#REQFILE}";               DestDir: "{app}"; DestName: "requirements.txt"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\whisper-dictate\whisper-dictate";    Filename: "{app}\setup.cmd"
Name: "{userprograms}\whisper-dictate\Uninstall";          Filename: "{uninstallexe}"

[Run]
Filename: "{app}\setup.cmd"; Description: "Run first-time setup now (downloads ~1.5 GB model)"; \
  Flags: postinstall nowait skipifsilent unchecked

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
procedure CurStepChanged(CurStep: TSetupStep);
var
  Path, NewPath: string;
  Paths: TStringList;
  i: Integer;
  Found: Boolean;
begin
  if CurStep = ssPostInstall then
  begin
    // Add install dir to user PATH so 'setup.cmd' is runnable from anywhere
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
