param(
  [Parameter(Mandatory = $true)]
  [string]$IsolatedProject
)

# This fixture is deliberately stdio-only: every response is a single JSON-RPC line.
Set-StrictMode -Version Latest
$root = [IO.Path]::GetFullPath($IsolatedProject)
if (-not (Test-Path -LiteralPath $root -PathType Container)) {
  throw "isolated project does not exist"
}
$rootPrefix = if ($root.EndsWith([IO.Path]::DirectorySeparatorChar) -or $root.EndsWith([IO.Path]::AltDirectorySeparatorChar)) {
  $root
} else {
  $root + [IO.Path]::DirectorySeparatorChar
}
$evidence = [IO.Path]::GetFullPath((Join-Path $root "evidence.txt"))
if (-not $evidence.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
  throw "evidence path escapes isolated project"
}

function Write-RpcResponse([object]$Response) {
  $json = $Response | ConvertTo-Json -Depth 12 -Compress
  [Console]::Out.WriteLine($json)
  [Console]::Out.Flush()
}

function New-RpcError([object]$Id, [int]$Code, [string]$Message) {
  return [ordered]@{
    jsonrpc = "2.0"
    id = $Id
    error = [ordered]@{ code = $Code; message = $Message }
  }
}

function New-ToolsListResult {
  return [ordered]@{
    tools = @(
      [ordered]@{
        name = "read_evidence"
        description = "Read the evidence.txt file inside the isolated project."
        inputSchema = [ordered]@{
          type = "object"
          properties = [ordered]@{}
          required = @()
          additionalProperties = $false
        }
        annotations = [ordered]@{ readOnlyHint = $true }
      }
    )
  }
}

while ($null -ne ($line = [Console]::In.ReadLine())) {
  if ([string]::IsNullOrWhiteSpace($line)) { continue }
  $request = $null
  $id = $null
  try {
    $request = $line | ConvertFrom-Json
    $hasId = $request.PSObject.Properties.Name -contains "id"
    if ($hasId) { $id = $request.id }
    $method = [string]$request.method
    if (-not $hasId) {
      # Notifications, including notifications/initialized, have no response.
      continue
    }

    $result = switch ($method) {
      "initialize" {
        [ordered]@{
          protocolVersion = "2025-11-25"
          capabilities = [ordered]@{ tools = [ordered]@{} }
          serverInfo = [ordered]@{ name = "keencode-native-fixture"; version = "1.0.0" }
        }
        break
      }
      "ping" {
        [ordered]@{}
        break
      }
      "tools/list" {
        New-ToolsListResult
        break
      }
      "tools/call" {
        $params = $request.params
        if ($null -eq $params -or [string]$params.name -ne "read_evidence") {
          throw [System.ArgumentException]::new("unknown MCP tool")
        }
        $arguments = $params.arguments
        if ($null -ne $arguments -and @($arguments.PSObject.Properties).Count -gt 0) {
          throw [System.ArgumentException]::new("read_evidence does not accept a path or other arguments")
        }
        $text = [IO.File]::ReadAllText($evidence)
        [ordered]@{
          content = @([ordered]@{ type = "text"; text = $text })
          isError = $false
        }
        break
      }
      default {
        throw [System.NotSupportedException]::new("unsupported MCP method")
      }
    }
    Write-RpcResponse ([ordered]@{ jsonrpc = "2.0"; id = $id; result = $result })
  } catch [System.ArgumentException] {
    Write-RpcResponse (New-RpcError $id -32602 $_.Exception.Message)
  } catch [System.NotSupportedException] {
    Write-RpcResponse (New-RpcError $id -32601 $_.Exception.Message)
  } catch {
    Write-RpcResponse (New-RpcError $id -32603 "MCP fixture request failed")
  }
}
