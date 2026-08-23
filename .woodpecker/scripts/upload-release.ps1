$ErrorActionPreference = "Stop"

$api = "https://git.mailliw.org/api/v1/repos/hummingbird/hummingbird"
$tag = if ($env:RELEASE_TAG) { $env:RELEASE_TAG } else { "latest" }

if ($args.Count -eq 0) {
    Write-Error "Usage: upload-release.ps1 <asset> [asset...]"
    exit 1
}

if (-not $env:FORGEJO_TOKEN) {
    Write-Error "FORGEJO_TOKEN is required"
    exit 1
}

foreach ($asset in $args) {
    if (-not (Test-Path -LiteralPath $asset -PathType Leaf)) {
        Write-Error "Missing release asset: $asset"
        exit 1
    }
}

$headers = @{ Authorization = "token $env:FORGEJO_TOKEN" }
$release = Invoke-RestMethod -Headers $headers -Uri "$api/releases/tags/$tag"
$releaseId = $release.id

if (-not $releaseId) {
    Write-Error "Release '$tag' does not exist"
    exit 1
}

$currentAssets = Invoke-RestMethod -Headers $headers -Uri "$api/releases/$releaseId/assets"

foreach ($asset in $args) {
    $name = Split-Path -Leaf $asset
    $matchingAssets = $currentAssets | Where-Object { $_.name -eq $name }

    foreach ($matchingAsset in $matchingAssets) {
        if ($matchingAsset.id) {
            Invoke-RestMethod `
                -Method Delete `
                -Headers $headers `
                -Uri "$api/releases/$releaseId/assets/$($matchingAsset.id)" `
                | Out-Null
        }
    }
}

foreach ($asset in $args) {
    $name = Split-Path -Leaf $asset
    $encodedName = [uri]::EscapeDataString($name)

    Invoke-RestMethod `
        -Method Post `
        -Headers $headers `
        -Form @{ attachment = (Get-Item -LiteralPath $asset) } `
        -Uri "$api/releases/$releaseId/assets?name=$encodedName" `
        | Out-Null
    Write-Output "Uploaded $name"
}
