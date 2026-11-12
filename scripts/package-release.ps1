# Packs a release for GitHub: Excavate-<version>-windows-x64.zip (the programs and the libraries beside them), its
# signature (.zip.sig: ECDSA P-256 over the zip's SHA-256, as hex, with the key scripts/release-key.ps1 made) and
# SHA256SUMS.txt. Attach all three to a GitHub release tagged v<version>; Excavate's updates (app/src/update.rs) take
# the latest release, check its signature, and use it at the next start. Build the release first (cargo build
# --release -p excavate -p indexer). Nothing is uploaded here.
param(
  [string]$Version = "",
  [string]$From = "$PSScriptRoot\..\target\release",
  [string]$Out = "$PSScriptRoot\..\dist",
  [string]$Key = "$env:USERPROFILE\.excavate\release-signing.key"
)
$ErrorActionPreference = "Stop"
if (-not $Version) {
  $Version = (Select-String -Path "$PSScriptRoot\..\Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
}
$files = "excavate.exe", "excavate-indexer.exe", "excavate-helper.exe", "pdfium.dll", "onnxruntime.dll", "DirectML.dll"
# Microsoft's C++ runtime, beside the programs (a fresh Windows may not have it; Microsoft allows this copy): from
# Visual Studio's redistributable folder, the newest.
$runtime = "msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll", "vcruntime140_1.dll"
$crt = Get-ChildItem "${env:ProgramFiles}\Microsoft Visual Studio\2022\*\VC\Redist\MSVC\*\x64\Microsoft.VC143.CRT" -Directory |
  Sort-Object { [version]($_.Parent.Parent.Name) } | Select-Object -Last 1
if (-not $crt) { throw "No Visual C++ redistributable folder (Visual Studio 2022's C++ tools)" }
$stage = Join-Path $Out "Excavate-$Version"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
foreach ($file in $runtime) { Copy-Item (Join-Path $crt.FullName $file) (Join-Path $stage $file) }
# And its OpenMP library, beside it in the same folder (the listener's whisper.cpp needs it).
Copy-Item (Join-Path $crt.Parent.FullName "Microsoft.VC143.OpenMP\vcomp140.dll") (Join-Path $stage "vcomp140.dll")
foreach ($file in $files) {
  $source = Get-Item (Join-Path $From $file)
  # (A link in target is copied as the file it points at.)
  $real = if ($source.LinkType) { $source.Target | Select-Object -First 1 } else { $source.FullName }
  Copy-Item $real (Join-Path $stage $file)
}
$zip = Join-Path $Out "Excavate-$Version-windows-x64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
# (A virus scanner may still be reading the files just copied: tried again for a few seconds.)
for ($try = 1; ; $try++) {
  try { Compress-Archive -Path "$stage\*" -DestinationPath $zip -ErrorAction Stop; break }
  catch { if ($try -ge 10) { throw }; if (Test-Path $zip) { Remove-Item -Force $zip }; Start-Sleep -Seconds 2 }
}
Remove-Item -Recurse -Force $stage
$bytes = [IO.File]::ReadAllBytes($zip)
Add-Type -AssemblyName System.Core
$blob = [IO.File]::ReadAllBytes($Key)
$cng = [System.Security.Cryptography.CngKey]::Import($blob, [System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
[Array]::Clear($blob, 0, $blob.Length)
$ecdsa = New-Object System.Security.Cryptography.ECDsaCng($cng)
$hash = [System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes)
$signature = $ecdsa.SignHash($hash)
$ecdsa.Dispose(); $cng.Dispose()
$hex = ($signature | ForEach-Object { "{0:x2}" -f $_ }) -join ""
[IO.File]::WriteAllText("$zip.sig", $hex)
$sum = ($hash | ForEach-Object { "{0:x2}" -f $_ }) -join ""
[IO.File]::WriteAllText((Join-Path $Out "SHA256SUMS.txt"), "$sum  $(Split-Path $zip -Leaf)`n")
"$zip ($([math]::Round($bytes.Length / 1MB, 1)) MB), signed; SHA-256 $sum"
