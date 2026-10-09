<#
.SYNOPSIS
  memprobe.ps1 — memory gate for the Lancat desktop app on Windows (WebView2 process tree).
  Same states and gates as scripts/memprobe.sh. TODO-verify: not run on Windows yet.

.DESCRIPTION
  Starts Lancat with a throw-away data dir and samples the WHOLE process tree:
  lancat.exe + every msedgewebview2.exe it spawned (browser, renderer, GPU, utility/network,
  crashpad). Child processes are found through Win32_Process.ParentProcessId, recursively.

  States:
    idle    panel collapsed, after -Settle seconds
    open    panel expanded (second launch with --open-panel), after -OpenWait seconds
    closed  collapsed again (second launch with --hide-panel), after -After seconds

  Per process: PrivateMemorySize64 ("Private Bytes" = commit charge, what the gate uses),
  WorkingSet64, and the private working set (WorkingSetPrivate perf counter, ≈ USS).

  Gates (MB):
    A  idle / closed : sum(Private Bytes) over the tree <= -MaxIdle   (default 30)
    B  open          : sum(Private Bytes) over the tree <= -MaxOpen   (default 180)

  Output: table on the console, JSON to -Json (default: stdout). Exit 1 on a failed gate, 2 on setup errors.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\memprobe.ps1 -Bin target\release\lancat.exe
  powershell -File scripts\memprobe.ps1 -Settle 25 -After 75 -MaxIdle 30 -MaxOpen 180 -Json probe.json
#>
param(
  [string]$Bin = "target\release\lancat.exe",
  [int]$Settle = 25,
  [int]$OpenWait = 6,
  [int]$After = 75,
  [double]$MaxIdle = 30,
  [double]$MaxOpen = 180,
  [string]$Json = "",
  [string]$Label = "",
  [switch]$NoGate
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

if (-not (Test-Path $Bin)) { Write-Error "no binary at $Bin (build first)"; exit 2 }
$Bin = (Resolve-Path $Bin).Path

# The single-instance lock is keyed on the app identifier, so --open-panel/--hide-panel from a
# second launch reach the probed instance. That means the probe must use the default data dir
# (LANCAT_DATA_DIR disables the lock): make sure no other Lancat is running first.
if (Get-Process -Name lancat -ErrorAction SilentlyContinue) {
  Write-Error "another lancat.exe is running; quit it first (the probe drives the app through the single-instance lock)"
  exit 2
}

$env:LANCAT_PORT = "" + (48000 + (Get-Random -Maximum 1000))
$env:LANCAT_UDP_PORT = $env:LANCAT_PORT
$env:LANCAT_NO_MDNS = "1"
$env:LANCAT_NAME = "memprobe"
$env:RUST_LOG = "info"
$log = Join-Path $env:TEMP "lancat-memprobe-$PID.log"
$app = Start-Process -FilePath $Bin -PassThru -RedirectStandardError $log -WindowStyle Hidden
Start-Sleep -Seconds 2
if ($app.HasExited) { Get-Content $log; Write-Error "app exited early"; exit 2 }

function Get-Tree([int]$root) {
  $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
  $ids = New-Object System.Collections.Generic.List[int]
  $todo = New-Object System.Collections.Generic.Queue[int]
  $todo.Enqueue($root)
  while ($todo.Count -gt 0) {
    $p = $todo.Dequeue(); $ids.Add($p)
    $all | Where-Object { $_.ParentProcessId -eq $p -and $_.ProcessId -ne $p } | ForEach-Object { $todo.Enqueue([int]$_.ProcessId) }
  }
  return $ids
}

function Get-Sample([string]$state) {
  $procs = @()
  foreach ($id in (Get-Tree $app.Id)) {
    $p = Get-Process -Id $id -ErrorAction SilentlyContinue
    if (-not $p) { continue }
    $wsPriv = $null
    try {
      $wsPriv = (Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$id").WorkingSetPrivate
    } catch {}
    $kind = ""
    try {
      $cmd = (Get-CimInstance Win32_Process -Filter "ProcessId=$id").CommandLine
      if ($cmd -match "--type=([a-z\-]+)") { $kind = $Matches[1] } elseif ($p.ProcessName -eq "msedgewebview2") { $kind = "browser" }
      if ($cmd -match "--utility-sub-type=([\w\.]+)") { $kind += ":" + $Matches[1] }
    } catch {}
    $procs += [pscustomobject]@{
      pid = $id; name = $p.ProcessName; kind = $kind; threads = $p.Threads.Count
      private_mb = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
      ws_mb = [math]::Round($p.WorkingSet64 / 1MB, 1)
      ws_private_mb = if ($wsPriv) { [math]::Round($wsPriv / 1MB, 1) } else { $null }
    }
  }
  $tot = [pscustomobject]@{
    private_mb = [math]::Round(($procs | Measure-Object private_mb -Sum).Sum, 1)
    ws_mb = [math]::Round(($procs | Measure-Object ws_mb -Sum).Sum, 1)
    ws_private_mb = [math]::Round(($procs | Measure-Object ws_private_mb -Sum).Sum, 1)
  }
  return [pscustomobject]@{ state = $state; procs = $procs; total = $tot }
}

function Show-Sample($s) {
  Write-Host "`n[$($s.state)]"
  $s.procs | Sort-Object private_mb -Descending | Format-Table pid, name, kind, threads, private_mb, ws_private_mb, ws_mb -AutoSize | Out-String | Write-Host
  Write-Host ("  TOTAL  private {0} MB | private WS {1} MB | WS {2} MB" -f $s.total.private_mb, $s.total.ws_private_mb, $s.total.ws_mb)
}

try {
  Write-Host "settling ${Settle}s (collapsed)..."
  Start-Sleep -Seconds $Settle
  $idle = Get-Sample "idle"
  Start-Process -FilePath $Bin -ArgumentList "--open-panel" -WindowStyle Hidden | Out-Null
  Start-Sleep -Seconds $OpenWait
  $open = Get-Sample "open"
  Start-Process -FilePath $Bin -ArgumentList "--hide-panel" -WindowStyle Hidden | Out-Null
  Write-Host "collapsed, waiting ${After}s..."
  Start-Sleep -Seconds $After
  $closed = Get-Sample "closed"
  if ($app.HasExited) { Get-Content $log -Tail 30; Write-Error "app died during the probe"; exit 2 }

  $lat = $null
  $m = Select-String -Path $log -Pattern "panel shown (\d+) ms" | Select-Object -First 1
  if ($m) { $lat = [int]$m.Matches[0].Groups[1].Value }

  Write-Host "`n== Lancat memprobe (Windows) $Label"
  Show-Sample $idle; Show-Sample $open; Show-Sample $closed
  $fail = @()
  if ($idle.total.private_mb -gt $MaxIdle) { $fail += "A idle $($idle.total.private_mb)MB > ${MaxIdle}MB" }
  if ($closed.total.private_mb -gt $MaxIdle) { $fail += "A closed $($closed.total.private_mb)MB > ${MaxIdle}MB" }
  if ($open.total.private_mb -gt $MaxOpen) { $fail += "B open $($open.total.private_mb)MB > ${MaxOpen}MB" }
  $res = [pscustomobject]@{
    label = $Label; binary_bytes = (Get-Item $Bin).Length; open_latency_ms = $lat
    samples = @($idle, $open, $closed)
    gates = [pscustomobject]@{ A_idle_private_mb = $idle.total.private_mb; A_closed_private_mb = $closed.total.private_mb; B_open_private_mb = $open.total.private_mb; max_idle_mb = $MaxIdle; max_open_mb = $MaxOpen }
    failures = $fail
  }
  Write-Host ("`nA idle {0}MB | A closed {1}MB | B open {2}MB | open latency {3} ms" -f $idle.total.private_mb, $closed.total.private_mb, $open.total.private_mb, $lat)
  if ($fail.Count) { Write-Host ("GATE: FAIL - " + ($fail -join "; ")) } else { Write-Host "GATE: PASS" }
  $out = $res | ConvertTo-Json -Depth 6
  if ($Json) { $out | Set-Content -Encoding UTF8 $Json } else { $out }
  if ($fail.Count -and -not $NoGate) { exit 1 } else { exit 0 }
}
finally {
  foreach ($id in (Get-Tree $app.Id)) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
  Remove-Item $log -ErrorAction SilentlyContinue
  Remove-Item Env:LANCAT_PORT, Env:LANCAT_UDP_PORT, Env:LANCAT_NO_MDNS, Env:LANCAT_NAME -ErrorAction SilentlyContinue
}
