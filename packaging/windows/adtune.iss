; Inno Setup script for ADtune (Windows).
; Installs ADtune and registers its own Audio Processing Object (APO) as a
; loadable COM server — nothing else is required. Enabling the APO on a specific
; output is done in-app (the Calibration switch, behind a UAC prompt), not here.
; Build with packaging\windows\build-installer.ps1.

#define AppName "ADtune"
; AppVersion is injected by build-installer.ps1 (/DAppVersion=<version>), which
; derives it from the workspace Cargo.toml so the installer version and file
; name can never drift from the crate version. 0.0.0 signals a direct ISCC run.
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define ApoClsid "{{7F4A1E02-9C3B-4D5A-8E21-AD7C0C0FFEE1}"

[Setup]
AppName={#AppName}
AppVersion={#AppVersion}
; The version resource of the setup exe itself; without these Inno stamps
; 0.0.0.0. VersionInfoVersion is the binary form (Windows pads it to four
; parts, 1.0.0.0); VersionInfoTextVersion is the string Explorer displays —
; kept at the plain crate version so it reads "1.0.0" like the app binaries.
VersionInfoVersion={#AppVersion}
VersionInfoTextVersion={#AppVersion}
AppPublisher=Antonio DEDEJ
AppCopyright=Copyright (c) 2026 Antonio DEDEJ
DefaultDirName={autopf}\ADtune
DefaultGroupName=ADtune
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\adtune.exe
OutputDir=..\..\target\installer
OutputBaseFilename=ADtune-Setup-{#AppVersion}
SetupIconFile=..\..\packaging\windows\adtune.ico
; The app icon in the wizard's top-right corner (SetupIconFile only covers the
; setup exe's file icon). Two DPI variants; Inno picks the best match.
WizardSmallImageFile=wizard-small-100.bmp,wizard-small-200.bmp
; The tall banner on the welcome/finish pages, rendered from wizard-large.svg
; (same tile + curve artwork as the app icon). Two DPI variants.
WizardImageFile=wizard-large-100.bmp,wizard-large-200.bmp
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
; The APO is registered machine-wide, so elevation is required.
PrivilegesRequired=admin
LicenseFile=..\..\LICENSE
; Close a running ADtune during install/uninstall so adtune.exe isn't locked.
CloseApplications=yes

[Files]
Source: "..\..\target\release\adtune-ui.exe"; DestDir: "{app}"; DestName: "adtune.exe"; Flags: ignoreversion
Source: "..\..\target\release\adtune_apo.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\packaging\windows\adtune.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\NOTICE"; DestDir: "{app}"; Flags: ignoreversion

[Dirs]
; Where the APO reads its live correction. The unprivileged app writes
; config.txt here and audiodg.exe reads it, so interactive users need Modify —
; but scope it to Users (not Everyone, which includes Guests/anonymous) so the
; low-integrity → protected-process input channel is as narrow as possible.
; config.txt only ever carries clamped EQ parameters (parsed by the hardened
; adtune-core parser — a trust boundary), never paths or commands.
Name: "{commonappdata}\ADtune"; Permissions: users-modify

[Registry]
; --- COM in-process server for the ADtune APO ---
Root: HKLM; Subkey: "SOFTWARE\Classes\CLSID\{#ApoClsid}"; Flags: uninsdeletekey
Root: HKLM; Subkey: "SOFTWARE\Classes\CLSID\{#ApoClsid}"; ValueType: string; ValueName: ""; ValueData: "ADtune APO"
Root: HKLM; Subkey: "SOFTWARE\Classes\CLSID\{#ApoClsid}\InprocServer32"; ValueType: string; ValueName: ""; ValueData: "{app}\adtune_apo.dll"
Root: HKLM; Subkey: "SOFTWARE\Classes\CLSID\{#ApoClsid}\InprocServer32"; ValueType: string; ValueName: "ThreadingModel"; ValueData: "Both"
; --- Register it as an Audio Processing Object ---
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; Flags: uninsdeletekey
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: string; ValueName: "FriendlyName"; ValueData: "ADtune APO"
; APOInterface0 declares the FUNCTIONAL interface the engine drives the APO
; through: IAudioProcessingObject ({FD7F2B29-...}), exactly what every in-box
; Windows system-effect APO declares. Declaring IAudioSystemEffects here instead
; makes the engine probe-and-abandon the endpoint graph (no audio). deletevalue
; prunes older ADtune installs.
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: string; ValueName: "APOInterface0"; ValueData: "{{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: none; ValueName: "APOInterface1"; Flags: deletevalue
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: none; ValueName: "APOInterface2"; Flags: deletevalue
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "NumAPOInterfaces"; ValueData: "1"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: string; ValueName: "Copyright"; ValueData: "Copyright (c) 2026 Antonio DEDEJ"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "MajorVersion"; ValueData: "1"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "MinorVersion"; ValueData: "0"
; 0xD = INPLACE | FRAMESPERSECOND_MUST_MATCH | BITSPERSAMPLE_MUST_MATCH — the
; exact flags the in-box WM-audio system-effect APOs register with.
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "Flags"; ValueData: "$0000000D"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "MinInputConnections"; ValueData: "1"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "MaxInputConnections"; ValueData: "1"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "MinOutputConnections"; ValueData: "1"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "MaxOutputConnections"; ValueData: "1"
Root: HKLM; Subkey: "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{#ApoClsid}"; ValueType: dword; ValueName: "MaxInstances"; ValueData: "$FFFFFFFF"
; --- Allow the (currently unsigned) APO to load in the protected audio path ---
; uninsdeletevalue removes just this value on uninstall (not the shared Audio key).
; Caveat: it's a machine-wide switch that other third-party unsigned-APO tools
; may also rely on — deleting it restores the default protected path but would
; disable such a tool until it re-sets it.
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows\CurrentVersion\Audio"; ValueType: dword; ValueName: "DisableProtectedAudioDG"; ValueData: "$00000001"; Flags: uninsdeletevalue

[Icons]
Name: "{group}\ADtune"; Filename: "{app}\adtune.exe"; IconFilename: "{app}\adtune.ico"
Name: "{group}\Uninstall ADtune"; Filename: "{uninstallexe}"
Name: "{autodesktop}\ADtune"; Filename: "{app}\adtune.exe"; IconFilename: "{app}\adtune.ico"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional icons:"

[Run]
; The APO's COM/AudioProcessingObjects registration above makes it loadable; the
; app enables it on a chosen device on demand (its Calibration switch → a UAC
; prompt), so the installer no longer enables it or restarts the audio engine.
Filename: "{app}\adtune.exe"; Description: "Launch ADtune"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Remove the APO from every device (and reload the audio engine, which the exe
; now does itself) before the files go away.
Filename: "{app}\adtune.exe"; Parameters: "--disable-apo"; RunOnceId: "DisableApo"; Flags: runhidden

[UninstallDelete]
; The app writes its config/state here at runtime (config.txt, state.json,
; last-op.txt), so [Dirs] can't auto-remove the non-empty folder — wipe it here.
; The user's saved EQ profiles under {userappdata}\ADtune are removed only if the
; uninstaller's "remove all my data" checkbox is ticked (see [Code]).
Type: filesandordirs; Name: "{commonappdata}\ADtune"

[Code]
var
  RemoveProfiles: Boolean;

// Close a running ADtune so its executable isn't locked during removal
// (CloseApplications only covers Setup, not the uninstaller). Ask nicely
// first — taskkill without /F posts a close request the app honours — then
// force after a grace period. Runs before [UninstallRun], whose fresh
// `adtune.exe --disable-apo` helper process is unaffected.
procedure CloseRunningApp();
var
  R: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM adtune.exe', '', SW_HIDE,
       ewWaitUntilTerminated, R);
  Sleep(1500);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM adtune.exe', '', SW_HIDE,
       ewWaitUntilTerminated, R);
end;

// One native Yes/No question inside the uninstall flow (right after the
// uninstaller's own confirmation): also delete the saved EQ profiles?
// Defaults to No — keeping the library is the safe choice. Machine
// config/state under {commonappdata}\ADtune is always removed (see
// [UninstallDelete]); only the per-user profile library is optional.
procedure CurUninstallStepChanged(CurStep: TUninstallStep);
begin
  if CurStep = usUninstall then
  begin
    RemoveProfiles :=
      MsgBox('Do you also want to delete your saved EQ profiles?' #13#10 #13#10
             'Choose No to keep them for a future installation.',
             mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;
    CloseRunningApp();
  end;
  if (CurStep = usPostUninstall) and RemoveProfiles then
    DelTree(ExpandConstant('{userappdata}\ADtune'), True, True, True);
end;
