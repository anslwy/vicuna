param(
  [string]$Tag = $env:VICUNA_VERSION
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = if ($env:VICUNA_REPO) { $env:VICUNA_REPO } else { 'anslwy/vicuna' }
$Headers = @{ 'User-Agent' = 'vicuna-installer' }

function Find-SetupUrl {
  param([string]$ApiUrl)
  try {
    $resp = Invoke-RestMethod -Uri $ApiUrl -Headers $Headers
    $releases = if ($resp -is [array]) { $resp } else { @($resp) }
    foreach ($rel in $releases) {
      foreach ($asset in $rel.assets) {
        if ($asset.browser_download_url -match '_x64-setup\.exe$') {
          return $asset.browser_download_url
        }
      }
    }
  } catch {
    return $null
  }
  return $null
}

$SetupUrl = $null
if ($Tag) {
  $SetupUrl = Find-SetupUrl "https://api.github.com/repos/$Repo/releases/tags/$Tag"
} else {
  $SetupUrl = Find-SetupUrl "https://api.github.com/repos/$Repo/releases/latest"
  if (-not $SetupUrl) {
    $SetupUrl = Find-SetupUrl "https://api.github.com/repos/$Repo/releases?per_page=5"
  }
}

if (-not $SetupUrl) {
  $suffix = if ($Tag) { " ($Tag)" } else { '' }
  throw "Could not find a Windows x64 installer asset for $Repo$suffix."
}

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("vicuna-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $TempDir | Out-Null
$InstallerPath = Join-Path $TempDir 'Vicuna-setup.exe'

try {
  Write-Host "Downloading $SetupUrl..."
  Invoke-WebRequest -Uri $SetupUrl -OutFile $InstallerPath -Headers $Headers

  $InstallerArgs = @()
  if ($env:VICUNA_SILENT -eq '1') {
    $InstallerArgs += '/S'
  }

  Write-Host 'Running Vicuna installer...'
  $proc = Start-Process -FilePath $InstallerPath -ArgumentList $InstallerArgs -Wait -PassThru
  if ($proc.ExitCode -ne 0) {
    throw "Installer exited with code $($proc.ExitCode)."
  }
  Write-Host 'Vicuna installation finished.'
} finally {
  Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
}
