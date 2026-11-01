; Inno Setup script for sonar-rollerbar.
; Produces a per-user installer: no UAC prompt and nothing written outside the user's profile.
; Output choices are stored as shortcut arguments. The optional features go in config.ini, which
; the tray menu edits too, as it manages the same sign-in shortcut as the "startup" task.
;
; Build the release executable first, then:
;   iscc /DAppVersion=0.1.1 installer\sonar-rollerbar.iss
; CI passes the version from Cargo.toml.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#define AppName      "sonar-rollerbar"
#define AppExeName   "sonar-rollerbar.exe"
#define AppPublisher "Bastien Poupelin"
#define AppURL       "https://github.com/BastienPoP/sonar-rollerbar"
#define ConfigDir    "{localappdata}\" + AppName
#define ConfigFile   ConfigDir + "\config.ini"

[Setup]
; Identifies the app for upgrades and must never change.
AppId={{00479F5A-6B3E-4460-BBA4-CA4548625923}
AppName={#AppName}
AppVersion={#AppVersion}
VersionInfoVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases

PrivilegesRequired=lowest
DefaultDirName={userpf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE

; The program has no window for the Restart Manager to close: [Code] and [UninstallRun] stop it.
CloseApplications=no
RestartApplications=no

OutputDir=..\dist
OutputBaseFilename=sonar-rollerbar-setup-v{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
MinVersion=10.0

ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

SetupIconFile=..\assets\sonar-rollerbar.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}

[Files]
Source: "..\target\x86_64-pc-windows-gnu\release\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md";   DestDir: "{app}"; Flags: ignoreversion
Source: "..\SECURITY.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE";     DestDir: "{app}"; Flags: ignoreversion
Source: "mixer-preview.png"; Flags: dontcopy

[Dirs]
Name: "{#ConfigDir}"

[INI]
Filename: "{#ConfigFile}"; Section: "options"; Key: "oled"; String: "{code:OptionValue|oled}"
Filename: "{#ConfigFile}"; Section: "options"; Key: "sonar_mixer"; String: "{code:OptionValue|sonar_mixer}"
Filename: "{#ConfigFile}"; Section: "options"; Key: "follow_output"; String: "{code:OptionValue|follow_output}"
Filename: "{#ConfigFile}"; Section: "options"; Key: "wheel_select"; String: "{code:OptionValue|wheel_select}"
Filename: "{#ConfigFile}"; Section: "options"; Key: "wheel_master"; String: "{code:OptionValue|wheel_master}"

[Tasks]
Name: "startup"; Description: "Start {#AppName} automatically when I sign in"; Flags: unchecked

[InstallDelete]
; Removes the sign-in shortcut on upgrades if the option is no longer ticked.
Type: files; Name: "{userstartup}\{#AppName}.lnk"; Tasks: not startup

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Parameters: "{code:GetAppArguments}"
Name: "{userstartup}\{#AppName}";  Filename: "{app}\{#AppExeName}"; Parameters: "{code:GetAppArguments}"; Tasks: startup

[Run]
Filename: "{app}\{#AppExeName}"; Parameters: "{code:GetAppArguments}"; \
    Description: "Start {#AppName} now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM {#AppExeName} /F"; \
    RunOnceId: "StopApp"; Flags: runhidden

[UninstallDelete]
; The tray menu can create this shortcut, so Inno Setup may not know about it.
Type: files; Name: "{userstartup}\{#AppName}.lnk"
Type: filesandordirs; Name: "{#ConfigDir}"

[Code]
const
  AutoChoice = 'Automatic (first physical output)';
  NoFallback = '(none)';
  // `--list-devices` prints "<marker> <kind padded to 10 characters> <name>".
  PhysicalKind = '[physical]';
  KindColumn = 3;
  NameColumn = 14;
  MixerPreview = 'mixer-preview.png';
  // The size of that file, and the gap that keeps the option texts clear of it.
  PreviewWidth = 170;
  PreviewHeight = 232;
  PreviewGap = 14;

var
  OutputPageId: Integer;
  PrimaryCombo: TNewComboBox;
  FallbackCombo: TNewComboBox;
  OledCheck: TNewCheckBox;
  MixerCheck: TNewCheckBox;
  FollowCheck: TNewCheckBox;
  WheelCheck: TNewCheckBox;
  MasterCheck: TNewCheckBox;

function PhysicalOutputs: TStringList;
var
  ResultCode, Index: Integer;
  Output: TExecOutput;
  Line: String;
begin
  Result := TStringList.Create;
  try
    ExtractTemporaryFile('{#AppExeName}');
    if ExecAndCaptureOutput(ExpandConstant('{tmp}\{#AppExeName}'), '--list-devices', '', SW_HIDE,
      ewWaitUntilTerminated, ResultCode, Output) then
    begin
      for Index := 0 to GetArrayLength(Output.StdOut) - 1 do
      begin
        Line := Output.StdOut[Index];
        if Pos(PhysicalKind, Line) = KindColumn then
          Result.Add(Trim(Copy(Line, NameColumn, Length(Line))));
      end;
    end;
  except
    // The lists stay editable, so names can still be typed by hand.
    Log(GetExceptionMessage);
  end;
end;

function AddLabel(Page: TWizardPage; const Caption: String; Top: Integer): TNewStaticText;
begin
  Result := TNewStaticText.Create(Page);
  Result.Parent := Page.Surface;
  Result.Caption := Caption;
  Result.Top := Top;
end;

function AddCombo(Page: TWizardPage; Top: Integer): TNewComboBox;
begin
  Result := TNewComboBox.Create(Page);
  Result.Parent := Page.Surface;
  Result.Style := csDropDown;
  Result.Top := Top;
  Result.Width := Page.SurfaceWidth;
end;

function AddText(Page: TWizardPage; const Caption: String;
  Left, Top, Width: Integer): TNewStaticText;
begin
  Result := TNewStaticText.Create(Page);
  Result.AutoSize := False;
  Result.WordWrap := True;
  Result.Left := Left;
  Result.Top := Top;
  Result.Width := Width;
  Result.Caption := Caption;
  Result.Parent := Page.Surface;
  Result.AdjustHeight;
end;

// Adds a tick box and what it does, and moves Top below them.
function AddOption(Page: TWizardPage; const Key, Caption, Description: String;
  Left, Width: Integer; var Top: Integer): TNewCheckBox;
var
  Text: TNewStaticText;
begin
  Result := TNewCheckBox.Create(Page);
  Result.Left := Left;
  Result.Top := Top;
  Result.Width := Width - Left;
  Result.Height := ScaleY(17);
  Result.Caption := Caption;
  // Reads the file rather than earlier choices: the tray menu may have changed it since.
  Result.Checked := GetIniBool('options', Key, False, ExpandConstant('{#ConfigFile}'));
  Result.Parent := Page.Surface;
  Top := Top + Result.Height;
  if Description <> '' then
  begin
    Text := AddText(Page, Description, Left + ScaleX(18), Top, Width - Left - ScaleX(18));
    Top := Top + Text.Height + ScaleY(6);
  end;
end;

procedure WheelCheckClick(Sender: TObject);
begin
  MasterCheck.Enabled := WheelCheck.Checked;
end;

procedure AddOptionsPage;
var
  Page: TWizardPage;
  Preview: TBitmapImage;
  Note: TNewStaticText;
  Top, Width: Integer;
begin
  Page := CreateCustomPage(OutputPageId, 'Optional features',
    'Your wheel already controls your output. Each option adds something on top.');

  Width := Page.SurfaceWidth - ScaleX(PreviewWidth + PreviewGap);
  Note := AddText(Page,
    'They need SteelSeries GG and stay off unless you tick them. ' +
    'You can change them later from the tray icon''s Options menu.', 0, 0, Width);

  Top := Note.Height + ScaleY(8);
  OledCheck := AddOption(Page, 'oled', 'Show the volume on the keyboard OLED',
    'The keyboard''s screen shows what changed and its level for 3 seconds.', 0, Width, Top);
  MixerCheck := AddOption(Page, 'sonar_mixer', 'Sonar mixer',
    'A window, opened from the tray icon, with sliders and mute buttons for your output, ' +
    'Sonar''s channels, the Mic and ChatMix.', 0, Width, Top);
  FollowCheck := AddOption(Page, 'follow_output', 'Follow Sonar''s output',
    'When you change the output in GG, the wheel moves to it.', 0, Width, Top);
  WheelCheck := AddOption(Page, 'wheel_select', 'Double-click the wheel to choose its target',
    'Points the wheel at ChatMix, Game, Chat, Media, Aux or the Mic for 10 seconds. ' +
    'A single click then takes a third of a second.', 0, Width, Top);
  MasterCheck := AddOption(Page, 'wheel_master', 'Include Master in the double click', '',
    ScaleX(18), Width, Top);
  WheelCheck.OnClick := @WheelCheckClick;
  WheelCheckClick(WheelCheck);

  ExtractTemporaryFile(MixerPreview);
  Preview := TBitmapImage.Create(Page);
  Preview.Stretch := True;
  Preview.Left := Page.SurfaceWidth - ScaleX(PreviewWidth);
  Preview.Width := ScaleX(PreviewWidth);
  Preview.Height := ScaleY(PreviewHeight);
  Preview.PngImage.LoadFromFile(ExpandConstant('{tmp}\') + MixerPreview);
  Preview.Parent := Page.Surface;

  Note := AddText(Page, 'The Sonar mixer', Preview.Left, Preview.Height + ScaleY(4),
    Preview.Width);
  Note.Alignment := taCenter;
end;

procedure InitializeWizard;
var
  Page: TWizardPage;
  Outputs: TStringList;
  Index: Integer;
begin
  Page := CreateCustomPage(wpSelectDir, 'Audio outputs',
    'Choose which output the volume wheel controls. This is all the program needs.');
  OutputPageId := Page.ID;

  AddLabel(Page, 'Main output:', 0);
  PrimaryCombo := AddCombo(Page, ScaleY(18));
  AddLabel(Page, 'Fallback output, used while the main output is disconnected:', ScaleY(52));
  FallbackCombo := AddCombo(Page, ScaleY(70));

  PrimaryCombo.Items.Add(AutoChoice);
  FallbackCombo.Items.Add(NoFallback);
  FallbackCombo.Items.Add(AutoChoice);
  Outputs := PhysicalOutputs;
  for Index := 0 to Outputs.Count - 1 do
  begin
    PrimaryCombo.Items.Add(Outputs[Index]);
    FallbackCombo.Items.Add(Outputs[Index]);
  end;
  Outputs.Free;

  PrimaryCombo.Text := GetPreviousData('PrimaryOutput', '');
  FallbackCombo.Text := GetPreviousData('FallbackOutput', NoFallback);

  AddOptionsPage;
end;

function OptionValue(Param: String): String;
var
  Check: TNewCheckBox;
begin
  Check := OledCheck;
  if Param = 'sonar_mixer' then
    Check := MixerCheck
  else if Param = 'follow_output' then
    Check := FollowCheck
  else if Param = 'wheel_select' then
    Check := WheelCheck
  else if Param = 'wheel_master' then
    Check := MasterCheck;
  Result := '0';
  if Check.Checked then
    Result := '1';
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  // The tray menu may have added the shortcut: an upgrade must not remove it.
  if (CurPageID = wpSelectTasks) and FileExists(ExpandConstant('{userstartup}\{#AppName}.lnk')) then
    WizardSelectTasks('startup');
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID = OutputPageId) and (Trim(PrimaryCombo.Text) = '') then
  begin
    MsgBox('Choose a main output.', mbError, MB_OK);
    Result := False;
  end;
end;

function OutputArgument(Choice: String): String;
begin
  Result := '';
  StringChangeEx(Choice, '"', '', True);
  Choice := Trim(Choice);
  if Choice = AutoChoice then
    Result := '--auto'
  else if (Choice <> '') and (Choice <> NoFallback) then
    Result := '--device "' + Choice + '"';
end;

function GetAppArguments(Param: String): String;
var
  Fallback: String;
begin
  Result := OutputArgument(PrimaryCombo.Text);
  // `--auto` always ranks after named outputs, so a named fallback would take priority over it.
  if Result <> '--auto' then
  begin
    Fallback := OutputArgument(FallbackCombo.Text);
    if Fallback <> Result then
      Result := Trim(Result + ' ' + Fallback);
  end;
end;

procedure RegisterPreviousData(PreviousDataKey: Integer);
begin
  SetPreviousData(PreviousDataKey, 'PrimaryOutput', PrimaryCombo.Text);
  SetPreviousData(PreviousDataKey, 'FallbackOutput', FallbackCombo.Text);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  // A running copy would keep the executable locked during an upgrade.
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM {#AppExeName} /F', '', SW_HIDE,
    ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
