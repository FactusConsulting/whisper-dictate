; whisper-dictate — Inno Setup installer script
; Build:  iscc /DVERSION=0.3.10 packaging\windows\inno\whisper-dictate.iss
; Output: whisper-dictate-windows-setup-{VERSION}.exe

#ifndef VERSION
  #define VERSION "0.0.0"
#endif
#ifndef VERSION_INFO
  #define VERSION_INFO VERSION
#endif

[Setup]
AppId={{7B3F8A2C-4E1D-4F9A-B5C6-D2E8F0A1C3B7}
AppName=whisper-dictate
AppVersion={#VERSION}
AppPublisher=FactusConsulting
AppPublisherURL=https://github.com/FactusConsulting/whisper-dictate
AppSupportURL=https://github.com/FactusConsulting/whisper-dictate/issues
AppUpdatesURL=https://github.com/FactusConsulting/whisper-dictate/releases
VersionInfoVersion={#VERSION_INFO}
DefaultDirName={localappdata}\Programs\WhisperDictate
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputBaseFilename=whisper-dictate-windows-setup-{#VERSION}
Compression=lzma2/ultra64
SolidCompression=yes
SetupIconFile=..\..\..\assets\whisper-dictate.ico
WizardStyle=modern
UninstallDisplayName=whisper-dictate
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "..\..\..\src\python\whisper_dictate\*.py"; DestDir: "{app}\src\python\whisper_dictate"; Flags: ignoreversion
Source: "..\..\..\src\python\whisper_dictate\*.json"; DestDir: "{app}\src\python\whisper_dictate"; Flags: ignoreversion
Source: "..\..\..\target\release\whisper-dictate.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\..\assets\whisper-dictate.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\..\README.md";          DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\..\docs\*.md";          DestDir: "{app}\docs"; Flags: ignoreversion
Source: "..\..\..\docs\examples\dictionary.example.json"; DestDir: "{app}\docs\examples"; Flags: ignoreversion
Source: "..\..\..\requirements\*.txt"; DestDir: "{app}\requirements"; Flags: ignoreversion
Source: "..\..\..\VERSION";            DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\..\..\scripts\dev\inject-smoke.py"; DestDir: "{app}\scripts"; Flags: ignoreversion

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

function PowerShellQuote(S: String): String;
var
  Quoted: String;
begin
  Quoted := S;
  StringChangeEx(Quoted, '''', '''''', True);
  Result := '''' + Quoted + '''';
end;

function CommandLineQuote(S: String): String;
var
  Quoted: String;
begin
  Quoted := S;
  StringChangeEx(Quoted, '"', '\"', True);
  Result := '"' + Quoted + '"';
end;

function RunPowerShellScript(Script: String; var ResultCode: Integer): Boolean;
var
  ScriptPath: String;
begin
  ScriptPath := ExpandConstant('{tmp}\whisper-dictate-installer.ps1');
  SaveStringToFile(ScriptPath, Script, False);
  Result := Exec(
    'powershell.exe',
    '-NoProfile -ExecutionPolicy Bypass -File ' + CommandLineQuote(ScriptPath),
    '',
    SW_HIDE,
    ewWaitUntilTerminated,
    ResultCode);
end;

function IsWhisperDictateRunning(): Boolean;
var
  Script, AppExe: String;
  ResultCode: Integer;
begin
  AppExe := ExpandConstant('{app}\whisper-dictate.exe');
  Script :=
    '$ErrorActionPreference = "SilentlyContinue"' + #13#10 +
    '$appExe = ' + PowerShellQuote(AppExe) + #13#10 +
    '$running = Get-CimInstance Win32_Process -Filter "name = ''whisper-dictate.exe''" | Where-Object { $_.ExecutablePath -eq $appExe }' + #13#10 +
    'if ($running) { exit 1 }' + #13#10 +
    'exit 0' + #13#10;

  if not RunPowerShellScript(Script, ResultCode) then
  begin
    Result := True;
    Exit;
  end;
  Result := ResultCode <> 0;
end;

function StopRunningWhisperDictate(): String;
var
  Script, AppExe, AppRoot: String;
  ResultCode: Integer;
begin
  AppExe := ExpandConstant('{app}\whisper-dictate.exe');
  AppRoot := ExpandConstant('{app}');
  Script :=
    '$ErrorActionPreference = "SilentlyContinue"' + #13#10 +
    '$appExe = ' + PowerShellQuote(AppExe) + #13#10 +
    '$appRoot = ' + PowerShellQuote(AppRoot) + #13#10 +
    '$currentPid = $PID' + #13#10 +
    '$desktop = Get-CimInstance Win32_Process -Filter "name = ''whisper-dictate.exe''" | Where-Object { $_.ProcessId -ne $currentPid -and $_.ExecutablePath -eq $appExe }' + #13#10 +
    'foreach ($process in $desktop) {' + #13#10 +
    '  $p = Get-Process -Id $process.ProcessId -ErrorAction SilentlyContinue' + #13#10 +
    '  if ($p -and $p.MainWindowHandle -ne 0) { [void]$p.CloseMainWindow() }' + #13#10 +
    '}' + #13#10 +
    '$deadline = (Get-Date).AddSeconds(8)' + #13#10 +
    'do {' + #13#10 +
    '  Start-Sleep -Milliseconds 250' + #13#10 +
    '  $desktop = Get-CimInstance Win32_Process -Filter "name = ''whisper-dictate.exe''" | Where-Object { $_.ProcessId -ne $currentPid -and $_.ExecutablePath -eq $appExe }' + #13#10 +
    '} while ($desktop -and (Get-Date) -lt $deadline)' + #13#10 +
    '$desktop | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }' + #13#10 +
    '$workers = Get-CimInstance Win32_Process | Where-Object { ($_.Name -like ''python*.exe'' -or $_.Name -eq ''py.exe'') -and $_.CommandLine -like ''*whisper_dictate.runtime*'' -and $_.CommandLine -like (''*'' + $appRoot + ''*'') }' + #13#10 +
    '$workers | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }' + #13#10 +
    '$deadline = (Get-Date).AddSeconds(10)' + #13#10 +
    'do {' + #13#10 +
    '  Start-Sleep -Milliseconds 250' + #13#10 +
    '  $remaining = Get-CimInstance Win32_Process -Filter "name = ''whisper-dictate.exe''" | Where-Object { $_.ProcessId -ne $currentPid -and $_.ExecutablePath -eq $appExe }' + #13#10 +
    '} while ($remaining -and (Get-Date) -lt $deadline)' + #13#10 +
    '$remaining = Get-CimInstance Win32_Process -Filter "name = ''whisper-dictate.exe''" | Where-Object { $_.ProcessId -ne $currentPid -and $_.ExecutablePath -eq $appExe }' + #13#10 +
    'if ($remaining) { exit 2 }' + #13#10 +
    'exit 0' + #13#10;

  if not RunPowerShellScript(Script, ResultCode) then
  begin
    Result := 'Could not run PowerShell to close the running whisper-dictate app.';
    Exit;
  end;
  if ResultCode <> 0 then
  begin
    Result := 'Close whisper-dictate and run the installer again.';
    Exit;
  end;
  Result := '';
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
var
  StopError: String;
begin
  if IsWhisperDictateRunning() then
  begin
    if MsgBox(
      'whisper-dictate is currently running.' + #13#10#13#10 +
        'Close it now so setup can continue?',
      mbConfirmation,
      MB_YESNO) <> IDYES then
    begin
      Result := 'Installation cancelled because whisper-dictate is still running.';
      Exit;
    end;
  end;

  while True do
  begin
    StopError := StopRunningWhisperDictate();
    if StopError = '' then
      Break;

    if MsgBox(
      StopError + #13#10#13#10 +
        'Close whisper-dictate, then click Retry to continue.',
      mbError,
      MB_RETRYCANCEL) <> IDRETRY then
    begin
      Result := 'Installation cancelled because whisper-dictate is still running.';
      Exit;
    end;
  end;

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
