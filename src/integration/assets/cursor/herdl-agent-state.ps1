# managed by herdl; reinstalling the integration replaces this file.
# HERDL_INTEGRATION_ID=cursor
# HERDL_INTEGRATION_VERSION=2

param([string]$Action = "")

function Exit-Hook {
    Write-Output "{}"
    exit 0
}

if ($Action -ne "session") { Exit-Hook }
if ($env:HERDL_ENV -ne "1") { Exit-Hook }
$targetId = if (![string]::IsNullOrWhiteSpace($env:HERDL_TAB_ID)) { $env:HERDL_TAB_ID } else { $env:HERDL_PANE_ID }
if ([string]::IsNullOrWhiteSpace($targetId)) { Exit-Hook }

$inputText = [Console]::In.ReadToEnd()
$jsonStart = $inputText.IndexOf("{")
if ($jsonStart -gt 0) {
    $inputText = $inputText.Substring($jsonStart)
}
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    Exit-Hook
}

if ($null -eq $payload) { Exit-Hook }
$event = if ($payload.hook_event_name -is [string]) { $payload.hook_event_name } else { $payload.hookEventName }
if (-not [string]::IsNullOrWhiteSpace($event) -and $event -ne "sessionStart") { Exit-Hook }

$sessionId = $null
foreach ($name in @("session_id", "sessionId", "conversation_id", "conversationId")) {
    $value = $payload.$name
    if ($value -is [string] -and -not [string]::IsNullOrWhiteSpace($value)) {
        $sessionId = $value
        break
    }
}
if ([string]::IsNullOrWhiteSpace($sessionId)) { Exit-Hook }

$seq = [DateTime]::UtcNow.Ticks
$herdl = if ([string]::IsNullOrWhiteSpace($env:HERDL_BIN_PATH)) { "herdl" } else { $env:HERDL_BIN_PATH }
try {
    & $herdl pane report-agent-session $targetId --source herdl:cursor --agent cursor --seq $seq --agent-session-id $sessionId 2>$null | Out-Null
} catch {
}

Exit-Hook
