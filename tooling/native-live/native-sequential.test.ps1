#requires -Version 7.0
# 此测试只装载被测函数并运行短子进程，不启动桌面、访问模型或读取真实配置。
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$tokens = $null
$parseErrors = $null
$source = Join-Path $PSScriptRoot 'native-sequential.ps1'
$ast = [Management.Automation.Language.Parser]::ParseFile($source, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw '批量入口存在语法错误' }
foreach ($name in @('Get-TimeoutValues', 'Assert-TimeoutValues', 'Start-NativePlan')) {
  $function = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true)
  if ($null -eq $function) { throw "缺少函数 $name" }
  . ([scriptblock]::Create($function.Extent.Text))
}

# null 表示使用供应商默认超时，不能在预检时被强制转换成 0。
Assert-TimeoutValues ([pscustomobject]@{ steps = @([pscustomobject]@{ requestTimeoutMs = $null }, [pscustomobject]@{ requestTimeoutMs = 1 }) })
foreach ($invalid in @(0, -1, 300001, 1.5, $true)) {
  $rejected = $false
  try { Assert-TimeoutValues ([pscustomobject]@{ requestTimeoutMs = $invalid }) } catch { $rejected = $true }
  if (-not $rejected) { throw '非法超时未被拒绝' }
}

$root = [IO.Path]::GetTempPath()
$taskDirectory = Join-Path $root ('keencode-batch-regression-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($taskDirectory) | Out-Null
$NodePath = (Get-Command node -ErrorAction Stop).Source
$runner = Join-Path $taskDirectory 'exit-fixture.cjs'
$providerConfigPath = Join-Path $taskDirectory 'unused-config.json'
$binaryPath = Join-Path $taskDirectory 'unused-binary.exe'
try {
  [IO.File]::WriteAllText($runner, "process.stdout.write('stdout-ok'); process.stderr.write('stderr-ok'); process.exit(Number(process.argv[3]));")
  foreach ($expected in @(0, 7)) {
    $scopeOutput = Join-Path $taskDirectory ([string]$expected)
    [IO.Directory]::CreateDirectory($scopeOutput) | Out-Null
    $actual = @(Start-NativePlan ([string]$expected) $scopeOutput 9236)
    # CopyToAsync 的 VoidTaskResult 不能泄漏到 PowerShell 管道，否则成功退出会变成数组。
    if ($actual.Count -ne 1 -or $actual[0] -isnot [int] -or $actual[0] -ne $expected) { throw '子进程退出码不再是唯一整数' }
    if ([IO.File]::ReadAllText((Join-Path $scopeOutput 'runner.stdout.log')) -ne 'stdout-ok' -or
        [IO.File]::ReadAllText((Join-Path $scopeOutput 'runner.stderr.log')) -ne 'stderr-ok') { throw '子进程输出未完整落盘' }
  }
  Write-Output 'PASS: null/default timeout, invalid timeout rejection, scalar exit codes, stdout/stderr persistence'
} finally {
  # 仅删除本测试新建的、已确认位于系统临时目录下的随机目录。
  $resolved = [IO.Path]::GetFullPath($taskDirectory)
  if (-not $resolved.StartsWith([IO.Path]::GetFullPath($root), [StringComparison]::OrdinalIgnoreCase)) { throw '测试清理路径越界' }
  Remove-Item -LiteralPath $resolved -Recurse -Force
}
