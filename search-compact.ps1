param(
    [Parameter(Mandatory=$true)][string]$Root,
    [Parameter(Mandatory=$true)][string[]]$Pattern,
    [string]$Filter = '*.rs'
)

$files = Get-ChildItem -LiteralPath $Root -Recurse -File -Filter $Filter |
    Where-Object { $_.FullName -notmatch '\\(target|test|tests|fixtures)\\' }

$matches = $files | Select-String -Pattern $Pattern

foreach ($group in ($matches | Group-Object Path)) {
    "`n$($group.Name)"
    foreach ($match in $group.Group) {
        '  {0}: {1}' -f $match.LineNumber, $match.Line.Trim()
    }
}
