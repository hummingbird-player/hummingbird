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
OutputBaseFilename=HummingbirdSetup_x86_64
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardSmallImageFile=SmallImage.png
WizardSmallImageFileDynamicDark=SmallImage.png
WizardImageFile=LargeImage.png
WizardImageFileDynamicDark=LargeImage.png
LicenseFile=LICENSE.txt
DisableWelcomePage=no

[Files]
Source: "..\..\target\bundle\x86_64-pc-windows-msvc\release-distro\Hummingbird.exe"; DestDir: "{app}"

[Icons]
Name: "{autoprograms}\Hummingbird"; Filename: "{app}\Hummingbird.exe"
