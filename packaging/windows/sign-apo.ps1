# Self-sign the ADtune APO DLL to allow loading in audiodg.exe without disabling protected paths.
#
# ⚠ SECURITY NOTICE — local testing only, NOT for distribution.
# This script adds a self-signed certificate to the machine's Trusted Root and
# Trusted Publisher stores. A cert in LocalMachine\Root is a machine-wide trust
# anchor: anything it signs is trusted to run on this machine. That is an
# acceptable trade-off on a personal test box, but it WEAKENS the machine's code
# trust and must never be run on machines you don't own. The private key is
# created NON-exportable to limit the blast radius if the box is compromised.
# For real distribution, sign with a genuine code-signing certificate from a CA
# instead and delete this script's trust-store steps.
#
# Usage (run PowerShell as Administrator):
#   .\packaging\windows\sign-apo.ps1

$ErrorActionPreference = "Stop"

# 1. Verify administrative privileges
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "This script must be run as Administrator."
}

# 2. Locate the DLL
$root = Resolve-Path "$PSScriptRoot\..\.."
$dllPath = "$root\target\release\adtune_apo.dll"
if (-not (Test-Path $dllPath)) {
    $dllPath = "${env:ProgramFiles}\ADtune\adtune_apo.dll"
}
if (-not (Test-Path $dllPath)) {
    throw "adtune_apo.dll not found in target\release or C:\Program Files\ADtune."
}

Write-Host "Found DLL at $dllPath" -ForegroundColor Cyan

# 3. Reuse the existing signing certificate, creating it only on the first run
#    (a fresh cert per run would pile duplicates into the machine trust stores).
#    The key is NON-exportable (the default when -KeyExportPolicy is omitted) so
#    it can't be lifted off the machine and reused as a signing oracle.
$subject = "CN=ADtune-APO-Local-Sign"
$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert | Where-Object { $_.Subject -eq $subject } | Select-Object -First 1
if ($cert) {
    Write-Host "Reusing existing code signing certificate $subject" -ForegroundColor Cyan
} else {
    Write-Host "Creating self-signed code signing certificate (non-exportable key)..." -ForegroundColor Cyan
    $cert = New-SelfSignedCertificate -Type CodeSigning -CertStoreLocation Cert:\CurrentUser\My -Subject $subject -FriendlyName "ADtune APO Local Sign Cert" -KeyExportPolicy NonExportable
}

# 4. Import the certificate to the LocalMachine Trusted Root and Trusted Publisher
#    stores (X509Store.Add is a no-op if it is already present).
Write-Host "Importing certificate to local trust stores..." -ForegroundColor Cyan
$rootStore = New-Object System.Security.Cryptography.X509Certificates.X509Store("Root", "LocalMachine")
$rootStore.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
$rootStore.Add($cert)
$rootStore.Close()

$pubStore = New-Object System.Security.Cryptography.X509Certificates.X509Store("TrustedPublisher", "LocalMachine")
$pubStore.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
$pubStore.Add($cert)
$pubStore.Close()

# 5. Sign the DLL using PowerShell's built-in Authenticode signing
Write-Host "Signing the DLL..." -ForegroundColor Cyan
Set-AuthenticodeSignature -FilePath $dllPath -Certificate $cert

Write-Host "Success: adtune_apo.dll is now signed and trusted locally!" -ForegroundColor Green
