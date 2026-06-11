# Dateizuordnung für Editron unter Windows (nutzerlokal, HKCU — kein Admin):
#   .etron  →  Editron.Project  →  "editron.exe" "%1"
#
# Aufruf (PowerShell):
#   .\register-file-association.ps1 -ExePath "C:\Pfad\zu\editron.exe"
# Ohne Parameter wird der Release-Build relativ zum Repo angenommen.
param(
    [string]$ExePath = (Join-Path $PSScriptRoot "..\..\target\release\editron.exe")
)

$ExePath = [System.IO.Path]::GetFullPath($ExePath)
if (-not (Test-Path $ExePath)) {
    Write-Error "Binary nicht gefunden: $ExePath (erst 'cargo build --release' ausführen oder -ExePath angeben)"
    exit 1
}

$classes = "HKCU:\Software\Classes"
New-Item -Path "$classes\.etron" -Force | Out-Null
Set-ItemProperty -Path "$classes\.etron" -Name "(Default)" -Value "Editron.Project"
Set-ItemProperty -Path "$classes\.etron" -Name "Content Type" -Value "application/x-editron-project"

New-Item -Path "$classes\Editron.Project" -Force | Out-Null
Set-ItemProperty -Path "$classes\Editron.Project" -Name "(Default)" -Value "Editron-Projekt"
New-Item -Path "$classes\Editron.Project\DefaultIcon" -Force | Out-Null
Set-ItemProperty -Path "$classes\Editron.Project\DefaultIcon" -Name "(Default)" -Value "`"$ExePath`",0"
New-Item -Path "$classes\Editron.Project\shell\open\command" -Force | Out-Null
Set-ItemProperty -Path "$classes\Editron.Project\shell\open\command" -Name "(Default)" -Value "`"$ExePath`" `"%1`""

# Explorer über die Änderung informieren (sonst erst nach Ab-/Anmelden sichtbar).
$signature = '[System.Runtime.InteropServices.DllImport("shell32.dll")] public static extern void SHChangeNotify(int wEventId, int uFlags, System.IntPtr dwItem1, System.IntPtr dwItem2);'
$shell = Add-Type -MemberDefinition $signature -Name Shell32Notify -Namespace Editron -PassThru
$shell::SHChangeNotify(0x08000000, 0x0000, [System.IntPtr]::Zero, [System.IntPtr]::Zero)

Write-Host "Fertig: .etron-Dateien öffnen jetzt $ExePath"
