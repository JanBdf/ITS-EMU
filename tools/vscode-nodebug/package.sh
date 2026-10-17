#!/bin/sh
# Packs the extension into a .vsix without vsce.
set -e
cd "$(dirname "$0")"
out=$(pwd)/gdbtarget-nodebug.vsix
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/extension"
cp package.json extension.js README.md "$tmp/extension/"
cat > "$tmp/[Content_Types].xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension=".json" ContentType="application/json"/><Default Extension=".js" ContentType="application/javascript"/><Default Extension=".md" ContentType="text/markdown"/><Default Extension=".vsixmanifest" ContentType="text/xml"/></Types>
XML
cat > "$tmp/extension.vsixmanifest" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
  <Metadata>
    <Identity Language="en-US" Id="gdbtarget-nodebug" Version="0.1.0" Publisher="its-emu"/>
    <DisplayName>gdbtarget: Run Without Debugging</DisplayName>
    <Description xml:space="preserve">Run Without Debugging for gdbtarget launch configurations.</Description>
    <Categories>Debuggers</Categories>
    <Properties><Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.80.0"/></Properties>
  </Metadata>
  <Installation><InstallationTarget Id="Microsoft.VisualStudio.Code"/></Installation>
  <Dependencies/>
  <Assets><Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/></Assets>
</PackageManifest>
XML
rm -f "$out"
(cd "$tmp" && zip -qr "$out" '[Content_Types].xml' extension.vsixmanifest extension)
echo "$out"
