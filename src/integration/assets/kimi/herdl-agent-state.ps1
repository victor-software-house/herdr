# installed by herdl
# managed by herdl; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDL_INTEGRATION_ID=kimi
# HERDL_INTEGRATION_VERSION=8

param([string]$Action = "")

if (@("session", "working", "blocked", "idle") -notcontains $Action) { exit 0 }
if ($env:HERDL_ENV -ne "1") { exit 0 }
$targetId = if (![string]::IsNullOrWhiteSpace($env:HERDL_TAB_ID)) { $env:HERDL_TAB_ID } else { $env:HERDL_PANE_ID }
if ([string]::IsNullOrWhiteSpace($targetId)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$sessionId = if ($null -ne $payload -and -not [string]::IsNullOrWhiteSpace($payload.session_id)) { $payload.session_id } else { $null }
$herdl = if ([string]::IsNullOrWhiteSpace($env:HERDL_BIN_PATH)) { "herdl" } else { $env:HERDL_BIN_PATH }

try {
    if ($Action -eq "session") {
        if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }
        & $herdl pane report-agent-session $targetId --source herdl:kimi --agent kimi --agent-session-id $sessionId --session-start-source startup --seq $seq 2>$null | Out-Null
    } else {
        if ([string]::IsNullOrWhiteSpace($sessionId)) {
            & $herdl pane report-agent $targetId --source herdl:kimi --agent kimi --state $Action --seq $seq 2>$null | Out-Null
        } else {
            & $herdl pane report-agent $targetId --source herdl:kimi --agent kimi --state $Action --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
        }
    }
} catch {
}
