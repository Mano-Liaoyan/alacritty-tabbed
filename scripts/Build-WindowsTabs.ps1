param([switch]$Test, [switch]$Check)
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$localRust = Join-Path $projectRoot '.tools\rust'
$oldCargoHome = $env:CARGO_HOME
$oldRustupHome = $env:RUSTUP_HOME
$oldPath = $env:PATH
Push-Location $projectRoot
try {
    $bundledCargo = Join-Path $localRust 'cargo\bin\cargo.exe'
    if (Test-Path -LiteralPath $bundledCargo) {
        $env:CARGO_HOME = Join-Path $localRust 'cargo'
        $env:RUSTUP_HOME = Join-Path $localRust 'rustup'
        $env:PATH = (Join-Path $env:CARGO_HOME 'bin') + ';' + $env:PATH
        $cargo = $bundledCargo
    } else {
        $cargo = (Get-Command cargo.exe -ErrorAction Stop).Source
    }
    & $cargo build --release --locked --no-default-features -p alacritty
    if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }
    if ($Test) {
        & $cargo test --release --locked --no-default-features -p alacritty
        if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
    }
    if ($Check) {
        & $cargo clippy --release --locked --no-default-features -p alacritty -- -D warnings
        if ($LASTEXITCODE -ne 0) { throw 'Static checks failed.' }
    }
    Write-Output "Built: $projectRoot\target\release\alacritty.exe"
} finally {
    Pop-Location
    $env:CARGO_HOME = $oldCargoHome
    $env:RUSTUP_HOME = $oldRustupHome
    $env:PATH = $oldPath
}