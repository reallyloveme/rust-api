# 先结束占用端口的进程，再启动转发服务
# 用法: .\run.ps1
# 可选: .\run.ps1 -Port 8787

param(
    [int]$Port = 8787
)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

function Stop-PortListeners {
    param([int]$ListenPort)

    $pids = @()

    # 优先用 Get-NetTCPConnection
    try {
        $pids = @(
            Get-NetTCPConnection -LocalPort $ListenPort -State Listen -ErrorAction SilentlyContinue |
                Select-Object -ExpandProperty OwningProcess -Unique
        )
    } catch {}

    # 兜底：netstat
    if (-not $pids -or $pids.Count -eq 0) {
        $lines = netstat -ano | Select-String ":$ListenPort\s+.*LISTENING\s+(\d+)$"
        foreach ($m in $lines) {
            if ($m.Matches.Count -gt 0) {
                $pids += [int]$m.Matches[0].Groups[1].Value
            }
        }
        $pids = $pids | Sort-Object -Unique
    }

    # 再清残留的 rust-api
    Get-Process -Name "rust-api" -ErrorAction SilentlyContinue | ForEach-Object {
        $pids += $_.Id
    }
    $pids = $pids | Where-Object { $_ -gt 0 } | Sort-Object -Unique

    if (-not $pids -or $pids.Count -eq 0) {
        Write-Host "[run] 端口 $ListenPort 空闲"
        return
    }

    foreach ($procId in $pids) {
        $name = (Get-Process -Id $procId -ErrorAction SilentlyContinue)?.ProcessName
        Write-Host "[run] 结束进程 PID=$procId Name=$name"
        Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue
    }

    Start-Sleep -Seconds 1
}

# 保证 cargo / mingw 可用（若已在 PATH 可忽略）
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path $cargoBin) {
    $env:Path = "$cargoBin;$env:Path"
}

Stop-PortListeners -ListenPort $Port
Write-Host "[run] 启动 cargo run ..."
cargo run
