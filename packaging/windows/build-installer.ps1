# Build ADtune (release) and package the Windows installer.
#
# Prerequisites:
#   - Rust toolchain (https://rustup.rs) with the MSVC target
#   - Inno Setup 6 (https://jrsoftware.org/isdl.php); `iscc` on PATH or at the
#     default install location.
#
# Usage (from a PowerShell prompt):
#   .\packaging\windows\build-installer.ps1             # release build, UNSIGNED
#   .\packaging\windows\build-installer.ps1 -SelfSign   # local testing only
#
# Release builds ship UNSIGNED: no certificate is created and no trust stores
# are touched. Self-signing gives end users nothing (their machines don't
# trust the cert), so -SelfSign exists only for local testing on a box you
# own: it creates a self-signed cert, adds it to THIS machine's Trusted Root
# and Trusted Publisher stores (a machine-wide trust anchor — never do this on
# a machine you don't own), and signs the binaries and installer with it. For
# real distribution, Authenticode-sign adtune_apo.dll, adtune-ui.exe, and the
# packaged installer with a CA-issued code-signing certificate instead.
param(
    [switch]$SelfSign
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\..\.."

# Derive the version from the workspace manifest (mirrors build-deb.sh) so the
# installer version and file name can never drift from the crate version.
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { $version = "0.0.0" }
$installerPath = "$root\target\installer\ADtune-Setup-$version.exe"

Write-Host "Building ADtune app + APO (release)" -ForegroundColor Cyan
Push-Location $root
try {
    cargo build --release --locked --target x86_64-pc-windows-msvc -p adtune-ui -p adtune-apo
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

# Copy target\x86_64-pc-windows-msvc\release to target\release for Inno Setup
$msvcReleaseDir = "$root\target\x86_64-pc-windows-msvc\release"
$releaseDir = "$root\target\release"
if (Test-Path $msvcReleaseDir) {
    if (-not (Test-Path $releaseDir)) {
        New-Item -ItemType Directory -Path $releaseDir -Force | Out-Null
    }
    Copy-Item -Path "$msvcReleaseDir\adtune-ui.exe" -Destination "$releaseDir\adtune-ui.exe" -Force -ErrorAction SilentlyContinue
    Copy-Item -Path "$msvcReleaseDir\adtune_apo.dll" -Destination "$releaseDir\adtune_apo.dll" -Force -ErrorAction SilentlyContinue
}

function Sign-File {
    param (
        [string]$FilePath,
        $Certificate
    )
    if (-not (Test-Path $FilePath)) { return }
    Write-Host "Signing $FilePath ..." -ForegroundColor Cyan
    # Try signing with timestamp first
    $status = Set-AuthenticodeSignature -FilePath $FilePath -Certificate $Certificate -TimestampServer "http://timestamp.digicert.com" -ErrorAction SilentlyContinue
    if (-not $status -or $status.Status -ne "Valid") {
        # Fallback to signing without timestamp
        $status = Set-AuthenticodeSignature -FilePath $FilePath -Certificate $Certificate
    }

    # Self-signed certificates can report "UnknownError" or "NotTrusted" because
    # they terminate in an untrusted root; the signature block is still applied.
    if ($status.Status -eq "NotSigned") {
        Write-Warning "Signing failed for $FilePath`: $($status.StatusMessage)"
    } elseif ($status.Status -eq "Valid") {
        Write-Host "Successfully signed and verified $FilePath" -ForegroundColor Green
    } else {
        Write-Host "Signed $FilePath (signature applied, but status is untrusted: $($status.Status))" -ForegroundColor Yellow
    }
}

$cert = $null
if ($SelfSign) {
    Write-Warning "-SelfSign is for LOCAL TESTING ONLY: it trusts a self-signed cert machine-wide. Do not distribute these artifacts."

    # Reuse the existing signing certificate, creating it only on the first run
    # (a fresh cert per run would pile duplicates into the trust stores). The
    # key is NON-exportable so it can't be lifted off the machine and reused
    # as a signing oracle.
    $subject = "CN=ADtune-Local-Sign"
    $cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert | Where-Object { $_.Subject -eq $subject } | Select-Object -First 1
    if ($cert) {
        Write-Host "Reusing existing code signing certificate $subject" -ForegroundColor Cyan
    } else {
        Write-Host "Creating self-signed code signing certificate $subject (non-exportable key)..." -ForegroundColor Cyan
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject -FriendlyName "ADtune Local Sign Cert" -KeyExportPolicy NonExportable -KeyUsage DigitalSignature -CertStoreLocation Cert:\CurrentUser\My
    }

    # Trust the certificate (idempotent; LocalMachine needs an elevated prompt,
    # CurrentUser is the fallback).
    try {
        foreach ($storeName in @("Root", "TrustedPublisher")) {
            $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($storeName, "LocalMachine")
            $store.Open("ReadWrite")
            $store.Add($cert)
            $store.Close()
        }
        Write-Host "Certificate trusted in Root and TrustedPublisher stores (Machine)." -ForegroundColor Green
    } catch {
        try {
            foreach ($storeName in @("Root", "TrustedPublisher")) {
                $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($storeName, "CurrentUser")
                $store.Open("ReadWrite")
                $store.Add($cert)
                $store.Close()
            }
            Write-Host "Certificate trusted in Root and TrustedPublisher stores (User fallback)." -ForegroundColor Green
        } catch {
            Write-Warning "Could not add certificate to Root/TrustedPublisher stores: $_"
            Write-Warning "Run this script as Administrator once to trust the certificate."
        }
    }

    # Sign the built binaries before packaging
    foreach ($dir in @("target\release", "target\x86_64-pc-windows-msvc\release")) {
        foreach ($name in @("adtune_apo.dll", "adtune-ui.exe")) {
            Sign-File -FilePath "$root\$dir\$name" -Certificate $cert
        }
    }
}

# Locate the Inno Setup compiler.
$iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    foreach ($p in @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe"
    )) { if (Test-Path $p) { $iscc = $p; break } }
}
if (-not $iscc) { throw "Inno Setup (iscc.exe) not found. Install Inno Setup 6." }

Write-Host "Packaging installer with $iscc …" -ForegroundColor Cyan
& $iscc "/DAppVersion=$version" "$PSScriptRoot\adtune.iss"
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }

if ($SelfSign) {
    Sign-File -FilePath $installerPath -Certificate $cert
}

Write-Host "Done: $installerPath" -ForegroundColor Green
