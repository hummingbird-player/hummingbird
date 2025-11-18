[Setup]
AppName=Hummingbird
AppPublisher=William Whittaker
AppPublisherURL=https://mailliw.org
AppVersion=0.1.0
WizardStyle=modern dynamic windows11
DefaultDirName={autopf}\Hummingbird
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\Hummingbird.exe
Compression=lzma2
SolidCompression=yes
OutputDir=Output
OutputBaseFilename=HummingbirdSetup_aarch64
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
WizardSmallImageFile=SmallImage.png
WizardSmallImageFileDynamicDark=SmallImage.png
WizardImageFile=LargeImage.png
WizardImageFileDynamicDark=LargeImage.png
LicenseFile=LICENSE.txt
DisableWelcomePage=no

[Files]
Source: "..\..\target\bundle\aarch64-pc-windows-msvc\release-distro\Hummingbird.exe"; DestDir: "{app}"

[Icons]
Name: "{autoprograms}\Hummingbird"; Filename: "{app}\Hummingbird.exe"
