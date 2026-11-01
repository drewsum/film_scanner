# Capture the V500's USB traffic while a command runs. Needs an elevated shell and USBPcap.
#
#   .\docs\epson-v500\capture.ps1 -Out target\captures\wia_preview.pcap -Run { cargo test ... }
#   .\docs\epson-v500\capture.ps1 -Out target\captures\manual.pcap -Seconds 120
#
# Without -Run, it captures for -Seconds (or until Enter is pressed) so you can drive
# Epson Scan or power-cycle the scanner by hand.
param(
    [Parameter(Mandatory)][string]$Out,
    [scriptblock]$Run,
    [int]$Seconds = 0,
    [string]$Match = "V500"
)
$ErrorActionPreference = "Stop"
$exe = "C:\Program Files\USBPcap\USBPcapCMD.exe"

# USBPcapCMD writes to the console directly, so collect its output through a file.
function Invoke-UsbPcap([string]$ArgLine) {
    $tmp = [IO.Path]::GetTempFileName()
    cmd /c "`"$exe`" $ArgLine > `"$tmp`" 2>&1"
    Get-Content $tmp
    Remove-Item $tmp
}

# Find the root hub and device address of the scanner from USBPcap's device tree.
$iface = $null; $address = $null
foreach ($line in (Invoke-UsbPcap "--extcap-interfaces")) {
    if ($line -notmatch "value=(\\\\\.\\USBPcap\d+)") { continue }
    $candidate = $Matches[1]
    foreach ($dev in (Invoke-UsbPcap "--extcap-interface $candidate --extcap-config")) {
        if ($dev -match "\{value=(\d+)\}\{display=\[\d+\] [^}]*$Match") {
            $iface = $candidate; $address = $Matches[1]; break
        }
    }
    if ($iface) { break }
}
if (-not $iface) { throw "No USB device matching '$Match' found by USBPcap" }

New-Item -ItemType Directory -Force (Split-Path -Parent $Out) | Out-Null
$full = [IO.Path]::GetFullPath($Out)
Write-Host "Capturing device $address on $iface to $full"
$proc = Start-Process $exe -ArgumentList "-d", $iface, "--devices", $address, "-o", "`"$full`"", "-s", "65535", "-b", "16777216" -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 1500
try {
    if ($Run) { & $Run }
    elseif ($Seconds -gt 0) { Start-Sleep -Seconds $Seconds }
    else { Read-Host "Capturing; press Enter to stop" | Out-Null }
} finally {
    Start-Sleep -Milliseconds 1000
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Write-Host ("Wrote {0:N0} bytes" -f (Get-Item $full).Length)
}
