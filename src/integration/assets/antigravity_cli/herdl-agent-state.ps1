# installed by herdl
# managed by herdl; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDL_INTEGRATION_ID=antigravity_cli
# HERDL_INTEGRATION_VERSION=3

# Session-only: this hook reports the Antigravity conversation so HerDL can
# resume the pane. Lifecycle state comes from HerDL's screen detection.

param([string]$Action = "")

# Antigravity CLI expects a JSON object on stdout and this hook never injects
# anything, so every exit path emits an empty object.
function Exit-Hook {
    Write-Output "{}"
    exit 0
}

if ($Action -ne "session") { Exit-Hook }
if ($env:HERDL_ENV -ne "1") { Exit-Hook }
$targetId = if (![string]::IsNullOrWhiteSpace($env:HERDL_TAB_ID)) { $env:HERDL_TAB_ID } else { $env:HERDL_PANE_ID }
if ([string]::IsNullOrWhiteSpace($targetId)) { Exit-Hook }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    Exit-Hook
}

if ($null -eq $payload) { Exit-Hook }

$conversationId = if ($payload.conversationId -is [string]) { $payload.conversationId } else { $null }
if ([string]::IsNullOrWhiteSpace($conversationId)) { Exit-Hook }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$herdl = if ([string]::IsNullOrWhiteSpace($env:HERDL_BIN_PATH)) { "herdl" } else { $env:HERDL_BIN_PATH }
try {
    $sessionArgs = @(
        "pane",
        "report-agent-session",
        $targetId,
        "--source",
        "herdl:antigravity_cli",
        "--agent",
        "agy",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$conversationId"
    )
    if ($payload.transcriptPath -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.transcriptPath)) {
        $sessionArgs += @("--agent-session-path", "$($payload.transcriptPath)")
    }
    & $herdl @sessionArgs 2>$null | Out-Null
} catch {
}

Exit-Hook
