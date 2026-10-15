$ErrorActionPreference = "Stop"

cargo build --release
elf2uf2-rs .\target\thumbv8m.main-none-eabihf\release\lumentra .\lumentra.uf2

$bytes = [System.IO.File]::ReadAllBytes(".\lumentra.uf2")
$familyValue = [Convert]::ToUInt32("E48BFF59", 16)
$familyBytes = [BitConverter]::GetBytes($familyValue)

for ($i = 0; $i -lt $bytes.Length; $i += 512) {
    $magic0 = [BitConverter]::ToUInt32($bytes, $i)
    $magic1 = [BitConverter]::ToUInt32($bytes, $i + 4)

    if ($magic0 -eq 0x0A324655 -and $magic1 -eq [Convert]::ToUInt32("9E5D5157", 16)) {
        $flags = [BitConverter]::ToUInt32($bytes, $i + 8) -bor 0x2000
        [BitConverter]::GetBytes([uint32]$flags).CopyTo($bytes, $i + 8)
        $familyBytes.CopyTo($bytes, $i + 28)
    }
}

[System.IO.File]::WriteAllBytes(".\lumentra-rp2350.uf2", $bytes)
Write-Host "Created lumentra-rp2350.uf2"
