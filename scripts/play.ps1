# Starts the title with the settings this fork is played with (README.md, "How to run it").
#   scripts\play.ps1 [switches] -Exe <path of your own copy's executable>
#   scripts\play.ps1 -WarmupOnly [the same switches as the runs it is for]
# -Safe         without the newer, faster ways (the first thing to try when something is wrong)
# -RayTracing   the title's own ray-traced indirect light: lit interiors, about a third slower
# -Fast         the README's fastest setup: Series S profile, renderer patches 1, 2, 5, 6, 7 (VICTORY_PORT_*);
#               with -Fsr4K only the patches (FSR at 4K needs the Series X profile)
# -Fsr4K        PC FidelityFX 4.1.2 at 3840x2160 (quality, balanced or performance)
#               (with a FidelityFX folder set, VICTORY_FIDELITYFX_DIR or victory.toml, the PC's FSR 4.1.2
#               also replaces the console's on every profile, -Fast's Series S too; VICTORY_FSR_HOST=0: not)
# -Console      series-s or series-x on its own, without -Fast's patches (-Fast alone picks series-s)
# -WarmupOnly   do not start the title: translate every shader of the game into the cache, then make the
#               pipelines earlier runs wrote down (their pipeline book). Give it the same switches as the runs
#               it is for: some of them (-Fast's VICTORY_PORT_CBV, -More) are part of the shader cache's key.
#               After a first run on an empty cache, run it again to make that run's pipelines too.
# -Build        target\diag (the default, the build all measurements are of) or target\release
# -More         other settings: -More @{VICTORY_PANEL='1'}
# -Log          also write everything the runtime logs (its frame-rate and A/B lines) to that file: -Log ab-1.log
# Every run writes down the pipelines the title makes (VICTORY_PIPELINE_BOOK=1; -More @{VICTORY_PIPELINE_BOOK='0'}: not).
# Run it from a normal window, not as administrator: the driver then keeps its shader cache elsewhere.
param(
    [string]$Exe,
    [switch]$Safe,
    [switch]$RayTracing,
    [switch]$Fast,
    [ValidateSet('quality', 'balanced', 'performance')][string]$Fsr4K,
    [ValidateSet('diag', 'release')][string]$Build = 'diag',
    [hashtable]$More = @{},
    [ValidateSet('series-s', 'series-x')][string]$Console,
    [switch]$WarmupOnly,
    [string]$Log
)
if (-not $WarmupOnly -and -not $Exe) { Write-Host "-Exe <path of your own copy's executable> is needed to run the title."; exit 1 }
$settings = @{
    VICTORY_TEXTURE_NO_WAIT = '1'; VICTORY_RELAY = '2'; VICTORY_WIDE_BACK = '1'; VICTORY_CARD_REHASH_RUNS = '1'
    VICTORY_JOIN_COPIES = '1'; VICTORY_INDIRECT_WINDOW = '1'; VICTORY_RELAY_COPY = '1'; VICTORY_CARD_STEPS = '2'
    VICTORY_INDIRECT_BATCH = '2'; VICTORY_MAP_THREADS = '1'; VICTORY_TITLE_PRIORITIES = '0'; VICTORY_CARD_AWAKE = '1'
    VICTORY_SHOW_AFTER = '1'; VICTORY_AMBIENT = '0.8'; VICTORY_D3D12_DRED = 'light'
    VICTORY_PIPELINE_BOOK = '1'
}
if ($Safe) {
    $settings.VICTORY_UNDER_WIDE = '0'; $settings.VICTORY_LAND_ECHO = '0'
} else {
    $settings.VICTORY_THIN = '1'; $settings.VICTORY_JOIN_PARTS = '8'; $settings.VICTORY_UPLOAD_AHEAD = '1'
}
if ($RayTracing) { $settings.VICTORY_BVH_BUILDER = '1' }
if ($Fast) {
    foreach ($k in 'CONSTANTS', 'DESCRIPTORS', 'CBV', 'GUARD', 'JOURNAL') { $settings["VICTORY_PORT_$k"] = '1' }
}
if ($Fsr4K) { $settings.VICTORY_FSR_4K = $Fsr4K.ToLowerInvariant() }
foreach ($k in $More.Keys) { $settings[$k] = $More[$k] }

$victory = Join-Path $PSScriptRoot "..\target\$Build\victory.exe"
if (-not (Test-Path $victory)) { Write-Host "No $victory. Build it first (README.md)."; exit 1 }
if (-not $WarmupOnly -and -not (Test-Path $Exe)) { Write-Host "No $Exe."; exit 1 }
# (Other settings left over in this window go; where your files are stays.)
$paths = 'VICTORY_CONFIG', 'VICTORY_GAME_DIR', 'VICTORY_CACHE_DIR', 'VICTORY_DATA_DIR', 'VICTORY_FIDELITYFX_DIR'
$inherited = @(Get-ChildItem Env:VICTORY_* | Where-Object { $_.Name -notin $paths })
foreach ($entry in $inherited) { Remove-Item "Env:$($entry.Name)" }
foreach ($k in $settings.Keys) { Set-Item "Env:$k" $settings[$k] }
$discarded = $inherited | Where-Object {
    $_.Name -notin $More.Keys -and -not $_.Value.Equals([Environment]::GetEnvironmentVariable($_.Name))
} | Sort-Object Name
if ($discarded) {
    Write-Host "Discarded inherited values for $($discarded.Name -join ', '). Use -More to pass them explicitly."
}
# (Relative to where it was started from, not the folder the run goes to.)
if ($Log) { $Log = [IO.Path]::GetFullPath([IO.Path]::Combine((Get-Location).Path, $Log)); $env:NO_COLOR = '1' }
# Each step's output, also to the log when there is one: the runtime logs to stderr.
function Invoke-Victory([string[]]$Arguments) {
    if ($Log) {
        & $victory @Arguments 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath $Log -Append
    } else {
        & $victory @Arguments
    }
}
Push-Location (Join-Path $PSScriptRoot '..')   # victory.toml is looked for here
try {
    if ($Log -and (Test-Path $Log)) { Remove-Item $Log }
    if ($WarmupOnly) {
        # Where both steps read and write: the cache folder this checkout's victory.toml names.
        Invoke-Victory @('config')
        Write-Host "Warm-up 1/2: every shader of the game into the shader cache (victory shaders)."
        Invoke-Victory @('shaders')
        if ($LASTEXITCODE) { Write-Host "victory shaders failed ($LASTEXITCODE)."; exit $LASTEXITCODE }
        Write-Host "Warm-up 2/2: the pipelines earlier runs wrote down (victory warm)."
        Invoke-Victory @('warm')
        exit $LASTEXITCODE
    }
    $Exe = (Resolve-Path $Exe).Path
    $runArgs = @('run', $Exe, '--execute')
    if ($Console) {
        $runArgs += '--console', $Console
    } elseif ($Fast -and $Fsr4K) {
        Write-Host "-Fast with -Fsr4K: renderer patches on, but the Series X profile stays (FSR at 4K needs it)."
    } elseif ($Fast) {
        $runArgs += '--console', 'series-s'
    }
    Invoke-Victory $runArgs
} finally { Pop-Location }
