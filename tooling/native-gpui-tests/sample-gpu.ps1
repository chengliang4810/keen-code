[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 2147483647)]
    [int]$TargetPid,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath,

    [ValidateRange(1, 3600)]
    [int]$Samples = 55
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$taskCounterPaths = @(
    "\GPU Engine(pid_${TargetPid}*)\Utilization Percentage",
    "\GPU Process Memory(pid_${TargetPid}*)\Dedicated Usage",
    "\GPU Process Memory(pid_${TargetPid}*)\Shared Usage"
)
$taskInstancePattern = "(^|_)pid_${TargetPid}(_|$)"
$taskOutputPath = [IO.Path]::GetFullPath($OutputPath)
$taskOutputParent = Split-Path -Parent $taskOutputPath
if (-not [string]::IsNullOrWhiteSpace($taskOutputParent)) {
    [IO.Directory]::CreateDirectory($taskOutputParent) | Out-Null
}
if (Test-Path -LiteralPath $taskOutputPath) {
    throw "禁止覆盖已有 GPU 采样证据：$taskOutputPath"
}

function Get-UtcText {
    return [DateTimeOffset]::UtcNow.ToString('o')
}

function ConvertTo-FiniteDouble {
    param([object]$Value)
    if ($null -eq $Value) {
        return $null
    }
    try {
        $taskNumber = [double]$Value
        if ([double]::IsNaN($taskNumber) -or [double]::IsInfinity($taskNumber)) {
            return $null
        }
        return $taskNumber
    } catch {
        return $null
    }
}

function Get-CounterKind {
    param([string]$Path)
    if ($Path -match '(?i)GPU Engine.*Utilization Percentage') {
        return 'gpuEngineUtilizationPercentage'
    }
    if ($Path -match '(?i)GPU Process Memory.*Dedicated Usage') {
        return 'dedicatedGpuBytes'
    }
    if ($Path -match '(?i)GPU Process Memory.*Shared Usage') {
        return 'sharedGpuBytes'
    }
    return 'unknown'
}

function Test-CounterSuccess {
    param([string]$Status)
    return $Status -eq 'Success' -or $Status -eq '0'
}

function Get-TargetProcessSnapshot {
    param(
        [long]$ExpectedStartUtcTicks,
        [string]$ExpectedExecutablePath = $null
    )

    try {
        $taskCurrent = Get-Process -Id $TargetPid -ErrorAction Stop
    } catch {
        $taskMessage = $_.Exception.Message
        $taskStatus = if ($taskMessage -match '(?i)cannot find|no process|not found|找不到|不存在') {
            'exited'
        } else {
            'unavailable'
        }
        return [pscustomobject]@{
            status = $taskStatus
            pid = $TargetPid
            processStartUtc = $null
            processStartUtcTicks = $null
            privateBytes = $null
            workingSetBytes = $null
            userCpuMs = $null
            kernelCpuMs = $null
            cpuMs = $null
            error = $taskMessage
        }
    }

    try {
        $taskStartUtc = $taskCurrent.StartTime.ToUniversalTime()
        $taskStartTicks = $taskStartUtc.Ticks
        $taskStartText = $taskStartUtc.ToString('o')
    } catch {
        return [pscustomobject]@{
            status = 'unavailable'
            pid = $TargetPid
            processStartUtc = $null
            processStartUtcTicks = $null
            privateBytes = $null
            workingSetBytes = $null
            userCpuMs = $null
            kernelCpuMs = $null
            cpuMs = $null
            error = "读取进程启动时间失败：$($_.Exception.Message)"
        }
    }

    if ($taskStartTicks -ne $ExpectedStartUtcTicks) {
        return [pscustomobject]@{
            status = 'pid_reused'
            pid = $TargetPid
            processStartUtc = $taskStartText
            processStartUtcTicks = $taskStartTicks
            privateBytes = $null
            workingSetBytes = $null
            userCpuMs = $null
            kernelCpuMs = $null
            cpuMs = $null
            error = "PID $TargetPid 的启动时间发生变化，拒绝混入其他进程样本"
        }
    }

    if (-not [string]::IsNullOrWhiteSpace($ExpectedExecutablePath)) {
        try {
            $taskCurrentPath = [IO.Path]::GetFullPath([string]$taskCurrent.Path)
            $taskExpectedPath = [IO.Path]::GetFullPath($ExpectedExecutablePath)
            if (-not [string]::Equals($taskCurrentPath, $taskExpectedPath, [StringComparison]::OrdinalIgnoreCase)) {
                return [pscustomobject]@{
                    status = 'pid_reused'
                    pid = $TargetPid
                    processStartUtc = $taskStartText
                    processStartUtcTicks = $taskStartTicks
                    privateBytes = $null
                    workingSetBytes = $null
                    userCpuMs = $null
                    kernelCpuMs = $null
                    cpuMs = $null
                    error = "PID $TargetPid 的可执行文件路径发生变化，拒绝混入其他进程样本"
                }
            }
        } catch {
            return [pscustomobject]@{
                status = 'unavailable'
                pid = $TargetPid
                processStartUtc = $taskStartText
                processStartUtcTicks = $taskStartTicks
                privateBytes = $null
                workingSetBytes = $null
                userCpuMs = $null
                kernelCpuMs = $null
                cpuMs = $null
                error = "读取目标进程可执行文件路径失败：$($_.Exception.Message)"
            }
        }
    }

    try {
        $taskUserCpuMs = [int64][math]::Round($taskCurrent.UserProcessorTime.TotalMilliseconds)
        $taskKernelCpuMs = [int64][math]::Round($taskCurrent.PrivilegedProcessorTime.TotalMilliseconds)
        return [pscustomobject]@{
            status = 'running'
            pid = $TargetPid
            processStartUtc = $taskStartText
            processStartUtcTicks = $taskStartTicks
            privateBytes = [int64]$taskCurrent.PrivateMemorySize64
            workingSetBytes = [int64]$taskCurrent.WorkingSet64
            userCpuMs = $taskUserCpuMs
            kernelCpuMs = $taskKernelCpuMs
            cpuMs = $taskUserCpuMs + $taskKernelCpuMs
            error = $null
        }
    } catch {
        return [pscustomobject]@{
            status = 'unavailable'
            pid = $TargetPid
            processStartUtc = $taskStartText
            processStartUtcTicks = $taskStartTicks
            privateBytes = $null
            workingSetBytes = $null
            userCpuMs = $null
            kernelCpuMs = $null
            cpuMs = $null
            error = "读取进程资源指标失败：$($_.Exception.Message)"
        }
    }
}

function New-CounterAggregate {
    param(
        [object[]]$Rows,
        [string]$ValueLabel
    )

    $taskRows = @($Rows)
    $taskValidRows = @($taskRows | Where-Object {
        $_.statusOk -and $null -ne $_.value
    })
    $taskInvalidRows = @($taskRows | Where-Object {
        -not $_.statusOk -or $null -eq $_.value
    })
    $taskValues = @($taskValidRows | ForEach-Object { [double]$_.value })
    $taskStatus = if ($taskValidRows.Count -gt 0) {
        if ($taskInvalidRows.Count -gt 0) { 'partial' } else { 'ok' }
    } else {
        'unavailable'
    }
    $taskSum = $null
    $taskMax = $null
    if ($taskValues.Count -gt 0) {
        $taskSum = ($taskValues | Measure-Object -Sum).Sum
        $taskMax = ($taskValues | Measure-Object -Maximum).Maximum
    }

    return [ordered]@{
        status = $taskStatus
        value = $ValueLabel
        instanceCount = $taskRows.Count
        successfulInstanceCount = $taskValidRows.Count
        invalidInstanceCount = $taskInvalidRows.Count
        sum = $taskSum
        max = $taskMax
    }
}

function New-UnavailableSample {
    param(
        [int]$Sequence,
        [string]$ErrorText,
        [object]$ProcessSnapshot,
        [string[]]$CounterErrors = @(),
        [string]$GpuReason = $null
    )
    if ($null -eq $ProcessSnapshot) {
        $ProcessSnapshot = Get-TargetProcessSnapshot `
            -ExpectedStartUtcTicks $taskProcessStartUtcTicks `
            -ExpectedExecutablePath $taskExpectedExecutablePath
    }
    if ([string]::IsNullOrWhiteSpace($GpuReason)) {
        $GpuReason = $ErrorText
    }
    return [pscustomobject]@{
        sequence = $Sequence
        counterTimestampUtc = $null
        observedAtUtc = Get-UtcText
        process = $ProcessSnapshot
        gpu = [ordered]@{
            status = 'unavailable'
            reason = $GpuReason
            instanceCount = 0
            engine = New-CounterAggregate -Rows @() -ValueLabel 'utilizationPercentage'
            dedicatedMemory = New-CounterAggregate -Rows @() -ValueLabel 'bytes'
            sharedMemory = New-CounterAggregate -Rows @() -ValueLabel 'bytes'
            counterErrors = @($CounterErrors)
        }
        rawCounters = @()
        error = $ErrorText
    }
}

function New-CounterSampleRecord {
    param(
        [object[]]$CounterSets,
        [int]$Sequence,
        [object]$ProcessSnapshot,
        [string[]]$CounterErrors = @()
    )

    $taskCounterTimestampUtc = $null
    $taskRawRows = [System.Collections.Generic.List[object]]::new()
    foreach ($taskCounterSet in @($CounterSets)) {
        try {
            if ($null -eq $taskCounterTimestampUtc -and $null -ne $taskCounterSet.Timestamp) {
                $taskCounterTimestampUtc = $taskCounterSet.Timestamp.ToUniversalTime().ToString('o')
            }
        } catch {
            # PDH 可能只返回 CounterSamples 而无法读取统一时间戳，样本仍可使用。
            $taskCounterTimestampUtc = $null
        }
        foreach ($taskCounter in @($taskCounterSet.CounterSamples)) {
            if ($null -eq $taskCounter -or [string]::IsNullOrWhiteSpace([string]$taskCounter.InstanceName)) {
                continue
            }
            if ([string]$taskCounter.InstanceName -notmatch $taskInstancePattern) {
                continue
            }
            $taskPath = [string]$taskCounter.Path
            $taskStatus = [string]$taskCounter.Status
            $taskRawRows.Add([pscustomobject]@{
                kind = Get-CounterKind -Path $taskPath
                path = $taskPath
                instance = [string]$taskCounter.InstanceName
                value = ConvertTo-FiniteDouble -Value $taskCounter.CookedValue
                status = $taskStatus
                statusOk = Test-CounterSuccess -Status $taskStatus
                timestampUtc = $taskCounterTimestampUtc
            })
        }
    }

    $taskRows = @($taskRawRows.ToArray())
    $taskEngineRows = @($taskRows | Where-Object kind -eq 'gpuEngineUtilizationPercentage')
    $taskDedicatedRows = @($taskRows | Where-Object kind -eq 'dedicatedGpuBytes')
    $taskSharedRows = @($taskRows | Where-Object kind -eq 'sharedGpuBytes')
    $taskEngine = New-CounterAggregate -Rows $taskEngineRows -ValueLabel 'utilizationPercentage'
    $taskDedicated = New-CounterAggregate -Rows $taskDedicatedRows -ValueLabel 'bytes'
    $taskShared = New-CounterAggregate -Rows $taskSharedRows -ValueLabel 'bytes'
    $taskAggregates = @($taskEngine, $taskDedicated, $taskShared)
    $taskGpuStatus = if ($taskAggregates.Count -gt 0 -and @($taskAggregates | Where-Object status -eq 'unavailable').Count -eq 0) {
        if (@($taskAggregates | Where-Object status -eq 'partial').Count -gt 0) {
            'partial'
        } else {
            'ok'
        }
    } elseif (@($taskAggregates | Where-Object status -ne 'unavailable').Count -gt 0) {
        'partial'
    } else {
        'unavailable'
    }
    $taskGpuReason = $null
    if ($taskGpuStatus -ne 'ok') {
        $taskGpuReason = if ($CounterErrors.Count -gt 0) {
            @($CounterErrors) -join ' | '
        } elseif ($taskRows.Count -eq 0) {
            '未找到绑定目标 PID 的 GPU counter 实例'
        } else {
            '目标 PID 的部分 GPU counter 实例无效或缺失'
        }
    }

    return [pscustomobject]@{
        sequence = $Sequence
        counterTimestampUtc = $taskCounterTimestampUtc
        observedAtUtc = Get-UtcText
        process = $ProcessSnapshot
        gpu = [ordered]@{
            status = $taskGpuStatus
            reason = $taskGpuReason
            instanceCount = $taskRows.Count
            engine = $taskEngine
            dedicatedMemory = $taskDedicated
            sharedMemory = $taskShared
            counterErrors = @($CounterErrors)
        }
        rawCounters = $taskRows
        error = if ($ProcessSnapshot.status -eq 'running') { $null } else { $ProcessSnapshot.error }
    }
}

function Get-CounterErrorText {
    param([object]$ErrorRecord)

    if ($null -eq $ErrorRecord) {
        return $null
    }
    try {
        if ($null -ne $ErrorRecord.Exception) {
            return [string]$ErrorRecord.Exception.Message
        }
    } catch {
    }
    return [string]$ErrorRecord
}

function Get-TargetCounterSample {
    $taskCounterErrors = @()
    $taskCounterSets = @()
    try {
        # 只查询目标 PID 的实例；其他进程的 PDH 实例失效时不会使目标样本失败。
        $taskCounterSets = @(
            Get-Counter -Counter $taskCounterPaths -SampleInterval 1 -MaxSamples 1 `
                -ErrorAction Continue -ErrorVariable taskCounterErrors
        )
    } catch {
        $taskCounterErrors += $_
    }
    $taskErrors = @($taskCounterErrors | ForEach-Object {
        Get-CounterErrorText -ErrorRecord $_
    } | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique)
    return [pscustomobject]@{
        counterSets = $taskCounterSets
        errors = $taskErrors
    }
}

function Test-ProcessTerminal {
    param([string]$Status)

    return $Status -ne 'running'
}

function Get-StopReason {
    param([string]$ProcessStatus)

    switch ($ProcessStatus) {
        'exited' { return 'process_exited' }
        'pid_reused' { return 'pid_reused' }
        default { return 'process_unavailable' }
    }
}

function Add-CounterWarnings {
    param(
        [System.Collections.Generic.List[string]]$Warnings,
        [string[]]$Errors
    )

    foreach ($taskError in @($Errors)) {
        if (-not [string]::IsNullOrWhiteSpace($taskError) -and -not $Warnings.Contains($taskError)) {
            $Warnings.Add($taskError)
        }
    }
}

function New-ProcessTerminalSample {
    param(
        [int]$Sequence,
        [object]$ProcessSnapshot,
        [string]$Reason
    )

    return New-UnavailableSample `
        -Sequence $Sequence `
        -ErrorText $Reason `
        -ProcessSnapshot $ProcessSnapshot `
        -GpuReason $Reason
}

$taskRunStartedAtUtc = Get-UtcText
$taskProcessStartUtcTicks = 0
$taskExpectedExecutablePath = $null
$taskExpectedExecutableSha256 = $null
$taskSamples = [System.Collections.Generic.List[object]]::new()
$taskErrors = [System.Collections.Generic.List[string]]::new()
$taskCounterWarnings = [System.Collections.Generic.List[string]]::new()
$taskStopReason = $null
$taskResult = [ordered]@{
    schema = 'keencode/windows-gpu-counter-samples'
    schemaVersion = 3
    source = 'Windows GPU Engine and GPU Process Memory performance counters'
    pid = $TargetPid
    processStartUtc = $null
    processStartUtcTicks = $null
    executablePath = $null
    executableSha256 = $null
    identityStatus = 'unavailable'
    counterPaths = $taskCounterPaths
    targetInstancePattern = $taskInstancePattern
    sampleIntervalSeconds = 1
    requestedSamples = $Samples
    samplerStatus = 'initializing'
    gpuStatus = 'unavailable'
    stopReason = $null
    startedAtUtc = $taskRunStartedAtUtc
    finishedAtUtc = $null
    sampleCount = 0
    validSampleCount = 0
    partialSampleCount = 0
    missingSampleCount = $Samples
    complete = $false
    errors = @()
    counterWarnings = @()
    samples = @()
    exitCode = 2
}

try {
    $taskProcess = Get-Process -Id $TargetPid -ErrorAction Stop
    $taskProcessStartUtcValue = $taskProcess.StartTime.ToUniversalTime()
    $taskProcessStartUtcTicks = $taskProcessStartUtcValue.Ticks
    $taskExecutablePath = [string]$taskProcess.Path
    $taskResult.processStartUtc = $taskProcessStartUtcValue.ToString('o')
    $taskResult.processStartUtcTicks = $taskProcessStartUtcTicks
    $taskResult.executablePath = $taskExecutablePath
    if ([string]::IsNullOrWhiteSpace($taskExecutablePath)) {
        throw "无法读取 PID $TargetPid 的可执行文件路径，拒绝生成未绑定的 GPU 证据"
    }
    $taskExecutableSha256 = (Get-FileHash -LiteralPath $taskExecutablePath -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant()
    $taskResult.executableSha256 = $taskExecutableSha256
    $taskResult.identityStatus = 'bound'

    $taskExpectedExecutablePath = $taskExecutablePath
    $taskExpectedExecutableSha256 = $taskExecutableSha256
    $taskResult.identityBinding = [ordered]@{
        pid = $TargetPid
        processStartUtc = $taskResult.processStartUtc
        processStartUtcTicks = $taskResult.processStartUtcTicks
        executablePath = $taskResult.executablePath
        executableSha256 = $taskExpectedExecutableSha256
    }

    # 每次查询前后都验证 PID、启动时间和路径；目标退出只追加终止标记，不伪造完整窗口。
    for ($taskIndex = 0; $taskIndex -lt $Samples; $taskIndex++) {
        $taskSampleStartedAt = [DateTimeOffset]::UtcNow
        $taskBeforeProcess = Get-TargetProcessSnapshot `
            -ExpectedStartUtcTicks $taskProcessStartUtcTicks `
            -ExpectedExecutablePath $taskExpectedExecutablePath
        if (Test-ProcessTerminal -Status $taskBeforeProcess.status) {
            $taskStopReason = Get-StopReason -ProcessStatus $taskBeforeProcess.status
            $taskSamples.Add((New-ProcessTerminalSample `
                    -Sequence $taskSamples.Count `
                    -ProcessSnapshot $taskBeforeProcess `
                    -Reason $taskStopReason))
            break
        }

        $taskCounterResult = Get-TargetCounterSample
        $taskCounterSets = @($taskCounterResult.counterSets)
        $taskCounterErrors = @($taskCounterResult.errors)
        Add-CounterWarnings -Warnings $taskCounterWarnings -Errors $taskCounterErrors
        $taskAfterProcess = Get-TargetProcessSnapshot `
            -ExpectedStartUtcTicks $taskProcessStartUtcTicks `
            -ExpectedExecutablePath $taskExpectedExecutablePath

        if ($taskCounterSets.Count -gt 0) {
            $taskSample = New-CounterSampleRecord `
                -CounterSets $taskCounterSets `
                -Sequence $taskSamples.Count `
                -ProcessSnapshot $taskAfterProcess `
                -CounterErrors $taskCounterErrors
        } elseif (Test-ProcessTerminal -Status $taskAfterProcess.status) {
            $taskTerminalReason = Get-StopReason -ProcessStatus $taskAfterProcess.status
            $taskSample = New-ProcessTerminalSample `
                -Sequence $taskSamples.Count `
                -ProcessSnapshot $taskAfterProcess `
                -Reason $taskTerminalReason
        } else {
            $taskReason = if ($taskCounterErrors.Count -gt 0) {
                $taskCounterErrors -join ' | '
            } else {
                'Get-Counter 未返回目标 PID 的样本'
            }
            $taskSample = New-UnavailableSample `
                -Sequence $taskSamples.Count `
                -ErrorText $taskReason `
                -ProcessSnapshot $taskAfterProcess `
                -CounterErrors $taskCounterErrors `
                -GpuReason $taskReason
        }
        $taskSamples.Add($taskSample)

        if (Test-ProcessTerminal -Status $taskAfterProcess.status) {
            $taskStopReason = Get-StopReason -ProcessStatus $taskAfterProcess.status
            break
        }

        $taskElapsedMilliseconds = ([DateTimeOffset]::UtcNow - $taskSampleStartedAt).TotalMilliseconds
        $taskDelayMilliseconds = [int][math]::Floor(1000 - $taskElapsedMilliseconds)
        if ($taskDelayMilliseconds -gt 0) {
            Start-Sleep -Milliseconds $taskDelayMilliseconds
        }
    }
} catch {
    $taskResult.samplerStatus = 'identity_failed'
    $taskErrors.Add($_.Exception.Message)
}

$taskResult.finishedAtUtc = Get-UtcText
$taskResult.sampleCount = $taskSamples.Count
$taskResult.stopReason = $taskStopReason
$taskResult.errors = @($taskErrors.ToArray())
$taskResult.counterWarnings = @($taskCounterWarnings.ToArray())
$taskResult.samples = @($taskSamples.ToArray())
$taskResult.validSampleCount = @($taskSamples | Where-Object {
    $_.process.status -eq 'running' -and $_.gpu.status -eq 'ok'
}).Count
$taskResult.partialSampleCount = @($taskSamples | Where-Object {
    $_.gpu.status -eq 'partial'
}).Count
$taskResult.missingSampleCount = [math]::Max(0, $Samples - $taskSamples.Count)
$taskResult.samplerStatus = if ($taskResult.identityStatus -ne 'bound') {
    'identity_failed'
} elseif ($taskStopReason -ne $null) {
    $taskStopReason
} elseif ($taskSamples.Count -eq $Samples -and $taskResult.validSampleCount -eq $Samples) {
    'completed'
} else {
    'counter_incomplete'
}
$taskResult.complete = $taskResult.identityStatus -eq 'bound' `
    -and $taskResult.samplerStatus -eq 'completed' `
    -and $taskResult.validSampleCount -eq $Samples
$taskResult.gpuStatus = if ($taskResult.complete) {
    'ok'
} elseif ($taskResult.validSampleCount -gt 0 -or $taskResult.partialSampleCount -gt 0) {
    'partial'
} else {
    'unavailable'
}
$taskResult.exitCode = if ($taskResult.complete) { 0 } else { 2 }

$taskResult | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $taskOutputPath -Encoding utf8
Write-Output "GPU采样文件：$taskOutputPath；样本数：$($taskSamples.Count)；状态：$($taskResult.samplerStatus)；GPU：$($taskResult.gpuStatus)；完整：$($taskResult.complete)"
if ([int]$taskResult.exitCode -ne 0) {
    exit ([int]$taskResult.exitCode)
}
