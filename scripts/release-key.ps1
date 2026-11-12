# Makes Excavate's release-signing key, once. The private key stays in your profile, readable by your account alone:
# back it up somewhere safe and offline. Whoever has it can publish an update every Excavate with automatic updates on
# will install, and without it no update can be published to the copies already out. It's never printed, and never
# goes in the repository. The public key is printed, for PUBLIC_KEY in app/src/update.rs.
param([string]$Out = "$env:USERPROFILE\.excavate\release-signing.key")
if (Test-Path $Out) { "There's a key already: $Out (move it away first to make a new one)"; exit 1 }
Add-Type -AssemblyName System.Core
$parameters = New-Object System.Security.Cryptography.CngKeyCreationParameters
$parameters.ExportPolicy = [System.Security.Cryptography.CngExportPolicies]::AllowPlaintextExport
$key = [System.Security.Cryptography.CngKey]::Create([System.Security.Cryptography.CngAlgorithm]::ECDsaP256, $null, $parameters)
$private = $key.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
$public = $key.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob)
$key.Dispose()
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
[IO.File]::WriteAllBytes($Out, $private)
[Array]::Clear($private, 0, $private.Length)
# Readable by this account only.
$me = [Security.Principal.WindowsIdentity]::GetCurrent().Name
icacls $Out /inheritance:r /grant:r "${me}:F" | Out-Null
"Saved the private key: $Out"
"Public key, for PUBLIC_KEY in app/src/update.rs:"
"[" + (($public | ForEach-Object { "0x{0:x2}" -f $_ }) -join ", ") + "]"
