# Build pluginů z workspace linuxfs-plugins a příprava instalačních balíčků do dist\.
#   .\build.ps1                    -> všechny pluginy
#   .\build.ps1 -Plugin wcx        -> jen Total Commander  (dist\linuxfs_wcx.wcx64 + dist\linuxfs_wcx.zip)
#   .\build.ps1 -Plugin spl        -> jen Salamander       (dist\linuxfs.spl)
#   .\build.ps1 -Native            -> optimalizace pro tento CPU (nepřenosné na starší CPU)
param(
    [ValidateSet('all', 'wcx', 'spl')][string]$Plugin = 'all',
    [switch]$Native
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$target = 'x86_64-pc-windows-msvc'

if ($Native) { $env:RUSTFLAGS = '-C target-cpu=native' }

rustup target add $target | Out-Null
cargo test --workspace --release --target $target
if ($LASTEXITCODE -ne 0) { throw 'tests failed' }
# vendor\btrfs-disk není členem workspace (lokální patche), jeho vlastní testy se tu nepouští

$packages = @()
if ($Plugin -in 'all', 'wcx') { $packages += '-p', 'linuxfs-wcx' }
if ($Plugin -in 'all', 'spl') { $packages += '-p', 'linuxfs-spl' }
cargo build --release --target $target @packages
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

$rel  = "target\$target\release"
$dist = Join-Path $PSScriptRoot 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null

if ($Plugin -in 'all', 'wcx') {
    Copy-Item "$rel\linuxfs_wcx.dll" (Join-Path $dist 'linuxfs_wcx.wcx64') -Force
    Copy-Item 'crates\linuxfs-wcx\pluginst.inf' $dist -Force
    # Instalační balíček: Total Commander nabídne instalaci po otevření zipu.
    Compress-Archive -Force -DestinationPath (Join-Path $dist 'linuxfs_wcx.zip') `
        -Path (Join-Path $dist 'linuxfs_wcx.wcx64'), (Join-Path $dist 'pluginst.inf')
    Write-Host "Done: $dist\linuxfs_wcx.wcx64"
}
if ($Plugin -in 'all', 'spl') {
    Copy-Item "$rel\linuxfs_spl.dll" (Join-Path $dist 'linuxfs.spl') -Force
    Write-Host "Done: $dist\linuxfs.spl"
    Write-Host 'Zkopíruj ho do <Salamander>\plugins\linuxfs\ a přidej v Plugins > Plugins Manager.'
}
