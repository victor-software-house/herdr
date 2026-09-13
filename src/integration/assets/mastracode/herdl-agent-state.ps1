# installed by herdl
# managed by herdl; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDL_INTEGRATION_ID=mastracode
# HERDL_INTEGRATION_VERSION=3

param([string]$Action = "")

if ($Action -notin @("session", "working", "idle", "blocked")) { exit 0 }
if ($env:HERDL_ENV -ne "1") { exit 0 }
$targetId = if (![string]::IsNullOrWhiteSpace($env:HERDL_TAB_ID)) { $env:HERDL_TAB_ID } else { $env:HERDL_PANE_ID }
if ([string]::IsNullOrWhiteSpace($targetId)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

$sessionId = if ($null -ne $payload -and $payload.session_id -is [string]) { $payload.session_id } else { $null }
$seq = [DateTime]::UtcNow.Ticks
$herdl = if ([string]::IsNullOrWhiteSpace($env:HERDL_BIN_PATH)) { "herdl" } else { $env:HERDL_BIN_PATH }
try {
    if ($Action -eq "session") {
        if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }
        & $herdl pane report-agent-session $targetId --source herdl:mastracode --agent mastracode --seq $seq --session-start-source startup --agent-session-id $sessionId 2>$null | Out-Null
    } else {
        $args = @("pane", "report-agent", $targetId, "--source", "herdl:mastracode", "--agent", "mastracode", "--state", $Action, "--seq", "$seq")
        if (-not [string]::IsNullOrWhiteSpace($sessionId)) {
            $args += @("--agent-session-id", $sessionId)
        }
        & $herdl @args 2>$null | Out-Null
    }
} catch {
}
