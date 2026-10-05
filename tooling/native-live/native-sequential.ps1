#requires -Version 7.0

# 需要 PowerShell 7+（pwsh）；ProcessStartInfo.ArgumentList 不在 Windows PowerShell 5.1 中可用。
param(
  [Parameter(Mandatory = $true)]
  [string]$ProviderConfig,

  [string]$Binary = (Join-Path $PSScriptRoot "..\..\target\debug\keencode-desktop.exe"),

  [string]$NodePath = "node",

  [int]$BasePort = 9236,

  [string]$BuildLabel = "native",

  [string]$AttemptDirectory,

  # 完整验收将跨 Runtime/Journal 的主流程加入同一二进制批次，仍按独立隔离目录串行执行。
  [switch]$FullAcceptance
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($PSVersionTable.PSEdition -ne "Core" -or $PSVersionTable.PSVersion.Major -lt 7) {
  throw "native-sequential.ps1 需要 PowerShell 7+（pwsh）"
}

# 标签只用于验收批次身份与目录名，限制为短的小写 slug，避免报告出现歧义或路径注入。
if ($BuildLabel.Length -lt 1 -or $BuildLabel.Length -gt 32 -or
    -not [regex]::IsMatch($BuildLabel, '^[a-z0-9]+(?:-[a-z0-9]+)*$', [Text.RegularExpressions.RegexOptions]::CultureInvariant)) {
  throw "BuildLabel 必须是 1..32 个小写字母、数字或单连字符分隔的短 slug"
}

$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$runner = Join-Path $root "tooling\scripts\native-live-e2e.mjs"
$providerConfigPath = [IO.Path]::GetFullPath($ProviderConfig)
$binaryPath = [IO.Path]::GetFullPath($Binary)

$planNames = @(
  "browser-only-local-features-plan.json",
  "native-editor-plan.json",
  "goal-subagents-plan.json",
  "project-groups-plan.json",
  "local-resources-crud-plan.json",
  "selection-side-rewind-plan.json",
  "session-reconnect-plan.json",
  "error-retry-hook-plan.json",
  "assistant-feedback-plan.json",
  "attachment-send-plan.json",
  "desktop-controls-plan.json",
  "general-settings-plan.json",
  "hook-trust-plan.json",
  "model-settings-plan.json",
  "permission-mode-plan.json",
  "task-menu-unread-plan.json"
)
if ($FullAcceptance) {
  $planNames = @(
    "zcode-desktop-plan.json",
    "native-worktree-gui-plan.json",
    "workflow-controls-plan.json",
    "local-mcp-skills-memory-plan.json",
    "local-automation-plan.json",
    "scheduled-automation-plan.json",
    "ui-layout-final-plan.json"
  ) + $planNames + @(
    "exit-confirmation-plan.json",
    "native-pdf-export-plan.json"
  )
}

function Read-JsonFile([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "JSON 文件不存在"
  }
  return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}

function Get-TimeoutValues([object]$Value) {
  if ($null -eq $Value) {
    return
  }
  if ($Value -is [System.Management.Automation.PSCustomObject]) {
    foreach ($property in $Value.PSObject.Properties) {
      if ($property.Name -eq "requestTimeoutMs") {
        Write-Output $property.Value
      }
      if ($null -ne $property.Value) {
        Get-TimeoutValues $property.Value
      }
    }
    return
  }
  if ($Value -is [System.Collections.IEnumerable] -and $Value -isnot [string]) {
    foreach ($item in $Value) {
      Get-TimeoutValues $item
    }
  }
}

function Assert-TimeoutValues([object]$Plan) {
  foreach ($value in @(Get-TimeoutValues $Plan)) {
    if ($null -eq $value) { continue }
    if ($value -is [bool]) {
      throw "requestTimeoutMs 必须是整数或 null"
    }
    try {
      $number = [double]$value
    } catch {
      throw "requestTimeoutMs 必须是整数或 null"
    }
    if ([double]::IsNaN($number) -or [double]::IsInfinity($number) -or
        [math]::Truncate($number) -ne $number -or $number -lt 1 -or $number -gt 300000) {
      throw "requestTimeoutMs 必须是 1..300000 毫秒整数"
    }
  }
}

function Assert-ProviderSelection([object]$Plan, [object]$Config) {
  $providerId = [string]$Plan.providerId
  $model = [string]$Plan.model
  if ([string]::IsNullOrWhiteSpace($providerId) -or [string]::IsNullOrWhiteSpace($model)) {
    throw "计划缺少 providerId 或 model"
  }
  $provider = @($Config.providers | Where-Object { [string]$_.id -eq $providerId }) | Select-Object -First 1
  if ($null -eq $provider) {
    throw "计划 provider 未在私有配置中找到"
  }
  if ([string]$provider.apiBackend -ne "chat_completions") {
    throw "计划 provider 不是 Chat Completions"
  }
  if (@($provider.models | ForEach-Object { [string]$_ }) -notcontains $model) {
    throw "计划 model 不在 provider catalog 中"
  }
  if ([string]::IsNullOrWhiteSpace([string]$provider.apiKey)) {
    throw "计划 provider 缺少凭据"
  }
}

function Assert-Plan([string]$PlanPath, [object]$Config) {
  $plan = Read-JsonFile $PlanPath
  if (-not @($plan.steps).Count) {
    throw "计划没有步骤"
  }
  Assert-ProviderSelection $plan $Config
  Assert-TimeoutValues $plan
  if ([IO.Path]::GetFileName($PlanPath) -eq "error-retry-hook-plan.json") {
    $retrySteps = @($plan.steps | Where-Object { [string]$_.action -eq "restart" })
    if (-not ($retrySteps | Where-Object {
      $_.PSObject.Properties.Name -contains "requestTimeoutMs" -and [double]$_.requestTimeoutMs -eq 1
    })) {
      throw "error-retry-hook 计划必须保留 restart requestTimeoutMs=1"
    }
  }
  return $plan
}

function Start-NativePlan(
  [string]$PlanPath,
  [string]$ScopeOutput,
  [int]$Port
) {
  $stdoutPath = Join-Path $ScopeOutput "runner.stdout.log"
  $stderrPath = Join-Path $ScopeOutput "runner.stderr.log"
  $startInfo = [Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $NodePath
  $startInfo.WorkingDirectory = $root
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.RedirectStandardOutput = $true
  $startInfo.RedirectStandardError = $true
  foreach ($argument in @(
      $runner,
      "--plan", $PlanPath,
      "--provider-config", $providerConfigPath,
      "--binary", $binaryPath,
      "--output", $ScopeOutput,
      "--port", [string]$Port
    )) {
    [void]$startInfo.ArgumentList.Add($argument)
  }

  $process = [Diagnostics.Process]::new()
  $process.StartInfo = $startInfo
  $hasStarted = $false
  $stdoutStream = $null
  $stderrStream = $null
  try {
    if (-not $process.Start()) {
      throw "无法启动 native-live runner"
    }
    $hasStarted = $true
    # 持续落盘而非等整个范围结束后才保存，便于查看长流程当前步骤，并保留异常退出证据。
    $stdoutStream = [IO.FileStream]::new($stdoutPath, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::ReadWrite, 1, $true)
    $stderrStream = [IO.FileStream]::new($stderrPath, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::ReadWrite, 1, $true)
    $stdoutTask = $process.StandardOutput.BaseStream.CopyToAsync($stdoutStream)
    $stderrTask = $process.StandardError.BaseStream.CopyToAsync($stderrStream)
    $process.WaitForExit()
    $null = $stdoutTask.GetAwaiter().GetResult()
    $null = $stderrTask.GetAwaiter().GetResult()
    return $process.ExitCode
  } finally {
    # 日志写入等外层失败也必须收回本函数创建的测试进程，不能留下持有原生验收租约的宿主。
    if ($hasStarted -and -not $process.HasExited) {
      $process.Kill($true)
      $process.WaitForExit()
    }
    if ($null -ne $stdoutStream) { $stdoutStream.Dispose() }
    if ($null -ne $stderrStream) { $stderrStream.Dispose() }
    $process.Dispose()
  }
}

if (-not (Test-Path -LiteralPath $runner -PathType Leaf)) {
  throw "native-live runner 不存在"
}
if (-not (Test-Path -LiteralPath $providerConfigPath -PathType Leaf)) {
  throw "Provider 配置文件不存在"
}
if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
  throw "桌面 binary 不存在"
}

$binaryItem = Get-Item -LiteralPath $binaryPath
$binarySha256 = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash.ToLowerInvariant()
$binarySizeBytes = [int64]$binaryItem.Length
if ($BasePort -lt 1024 -or $BasePort -gt 65500) {
  throw "BasePort 必须在 1024..65500"
}
if ($BasePort + $planNames.Count - 1 -gt 65535) {
  throw "BasePort 无法为整批计划分配有效端口"
}

$config = Read-JsonFile $providerConfigPath
$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$attemptRoot = if ([string]::IsNullOrWhiteSpace($AttemptDirectory)) {
  Join-Path $root ("out\native-live\{0}-attempt-{1}" -f $BuildLabel, $timestamp)
} else {
  [IO.Path]::GetFullPath($AttemptDirectory)
}
[IO.Directory]::CreateDirectory($attemptRoot) | Out-Null

$summary = [System.Collections.Generic.List[object]]::new()
for ($index = 0; $index -lt $planNames.Count; $index++) {
  $planName = $planNames[$index]
  $planPath = Join-Path $root "tooling\native-live\$planName"
  $scope = [IO.Path]::GetFileNameWithoutExtension($planName) -replace "-plan$", ""
  $scopeOutput = Join-Path $attemptRoot (("{0:D2}-{1}" -f ($index + 1), $scope))
  [IO.Directory]::CreateDirectory($scopeOutput) | Out-Null
  $started = Get-Date
  $entry = [ordered]@{
    index = $index + 1
    buildLabel = $BuildLabel
    binarySha256 = $binarySha256
    binarySizeBytes = $binarySizeBytes
    plan = $planName
    scope = $scope
    output = $scopeOutput
    passed = $false
    exitCode = $null
    passedSteps = 0
    executedSteps = 0
    totalSteps = 0
    preflight = $false
    error = $null
  }
  try {
    $plan = Assert-Plan $planPath $config
    $entry.totalSteps = @($plan.steps).Count
    $entry.preflight = $true
    $entry.exitCode = Start-NativePlan $planPath $scopeOutput ($BasePort + $index)
    $reportPath = Join-Path $scopeOutput "report.json"
    if (Test-Path -LiteralPath $reportPath -PathType Leaf) {
      $report = Read-JsonFile $reportPath
      $entry.passedSteps = @($report.results | Where-Object { $_.passed -eq $true }).Count
      # 失败提前退出时仍保留计划总数，避免把尚未执行的验收项隐藏在分母之外。
      $entry.executedSteps = @($report.results | Where-Object {
        $_.PSObject.Properties.Name -contains "index" -and $null -ne $_.index
      }).Count
      $entry.passed = [bool]$report.passed -and $entry.exitCode -eq 0
    } else {
      $entry.error = "runner 未生成 report.json"
    }
    if (-not $entry.passed -and $null -eq $entry.error) {
      $entry.error = "范围验收失败"
    }
  } catch {
    $entry.error = "预检或启动失败"
    if ($null -eq $entry.exitCode) {
      $entry.exitCode = 2
    }
  }
  $entry.elapsedMs = [int64]((Get-Date) - $started).TotalMilliseconds
  $summary.Add([pscustomobject]$entry)
  $entry | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $scopeOutput "scope-summary.json") -Encoding utf8
  $status = if ($entry.passed) { "PASS" } else { "FAIL" }
  Write-Output ("[{0}] {1} {2}/{3}" -f $status, $scope, $entry.passedSteps, $entry.totalSteps)
}

$summaryDocument = [ordered]@{
  batch = "$BuildLabel-sequential"
  buildLabel = $BuildLabel
  binarySha256 = $binarySha256
  binarySizeBytes = $binarySizeBytes
  sourceBaseline = "29628c9acdb81b703bbd4080c207a0e7ce5e276e"
  attemptDirectory = $attemptRoot
  planCount = $planNames.Count
  completedCount = @($summary | Where-Object { $_.passed }).Count
  failedCount = @($summary | Where-Object { -not $_.passed }).Count
  scopes = @($summary)
}
$summaryPath = Join-Path $attemptRoot "batch-summary.json"
$summaryDocument | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $summaryPath -Encoding utf8

if (@($summary | Where-Object { -not $_.passed }).Count -gt 0) {
  exit 1
}
exit 0
