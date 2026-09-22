param(
    [ValidateSet("lint", "check")]
    [string]$Mode = "check"
)

$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param([string]$Command, [string[]]$Arguments)

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $Command $($Arguments -join ' ')"
    }
}

function Invoke-CargoWithZigCacheRecovery {
    param([string[]]$Arguments)

    & cargo @Arguments
    if ($LASTEXITCODE -eq 0) {
        return
    }

    Write-Warning "cargo compile failed; clearing Zig build caches and retrying once"
    Remove-Item -Recurse -Force .zig-cache -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force vendor/libghostty-vt/.zig-cache -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force vendor/libghostty-vt/zig-out -ErrorAction SilentlyContinue
    Invoke-Checked cargo $Arguments
}

function Invoke-CargoTestFilter {
    param(
        [Parameter(Mandatory)]
        [string]$Filter,
        [switch]$Exact
    )

    $commonArguments = @(
        "test",
        "--locked",
        "--bin",
        "herdr",
        $Filter
    )
    $harnessArguments = @("--list")
    if ($Exact) {
        $harnessArguments += "--exact"
    }

    $listArguments = $commonArguments + @("--") + $harnessArguments
    $listOutput = @(& cargo @listArguments)
    if ($LASTEXITCODE -ne 0) {
        throw "could not enumerate tests for filter '$Filter': $($listOutput -join [Environment]::NewLine)"
    }

    $testNames = @(
        foreach ($line in $listOutput) {
            $match = [regex]::Match([string]$line, '^\s*(\S+): test\s*$')
            if ($match.Success) {
                $match.Groups[1].Value
            }
        }
    )
    if ($testNames.Count -eq 0) {
        throw "test filter '$Filter' selected zero tests"
    }

    Write-Host "Running $($testNames.Count) test(s) for '$Filter'"
    $runArguments = $commonArguments
    if ($Exact) {
        $runArguments += @("--", "--exact")
    }
    Invoke-Checked cargo $runArguments
}

Invoke-Checked cargo @("fmt", "--check")
Invoke-CargoWithZigCacheRecovery @(
    "clippy",
    "--all-targets",
    "--locked",
    "--",
    "-D",
    "warnings"
)

if ($Mode -eq "lint") {
    return
}

# Upstream runs its whole suite on Windows (#3660). The fork carries ~6,000 tests
# on top of that and they do not finish inside any sane Windows CI budget, so keep
# the fork's targeted Windows scope: the platform-specific tests plus the two areas
# that have regressed on Windows before.
Invoke-CargoTestFilter "windows_"
Invoke-CargoTestFilter "server::client_transport::tests"
Invoke-CargoTestFilter "app::tests::native_repeats_and_releases_follow_the_pressed_pane" -Exact
Invoke-Checked cargo @("build", "--locked")
