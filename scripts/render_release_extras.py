#!/usr/bin/env python3
"""Write installers and package-index files from release SHA-256 sidecars.

Reads wiremux-<target>.tar.gz.sha256 and wiremux-<target>.zip.sha256
anywhere under --assets (download-artifact nests files). Output is
stable for one version and hash set, so a later push can no-op.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

DESCRIPTION = (
    "Wiremux maps Chat Completions, Messages, Responses, Gemini, "
    "and Converse through one IR."
)
# Homebrew rejects a desc that starts with the formula name, ends
# with a period, or is 80 characters or longer.
HOMEBREW_DESC = (
    "Maps Chat Completions, Messages, Responses, Gemini, "
    "and Converse through one IR"
)
REPO_RE = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
VERSION_RE = re.compile(r"\d+\.\d+\.\d+")
HEX_RE = re.compile(r"[0-9a-fA-F]{64}")

REQUIRED_TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
)
OPTIONAL_TARGETS = (
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
)
ALL_TARGETS = REQUIRED_TARGETS + OPTIONAL_TARGETS


def normalize_version(raw: str) -> str:
    version = raw.strip()
    if version.startswith("v"):
        version = version[1:]
    if not VERSION_RE.fullmatch(version):
        raise SystemExit(f"bad version: {raw}")
    return version


def archive_filename(target: str) -> str:
    if "windows" in target:
        return f"wiremux-{target}.zip"
    return f"wiremux-{target}.tar.gz"


def asset_url(repo: str, version: str, filename: str) -> str:
    return (
        f"https://github.com/{repo}/releases/download/v{version}/{filename}"
    )


def parse_sha256(text: str, path: Path) -> str:
    token = text.strip().split()[0] if text.strip() else ""
    if not HEX_RE.fullmatch(token):
        raise SystemExit(f"bad sha256 sidecar: {path}")
    return token.lower()


def load_hashes(assets: Path) -> dict[str, str]:
    found: dict[str, str] = {}
    for path in sorted(assets.rglob("*.sha256")):
        name = path.name[: -len(".sha256")]
        for target in ALL_TARGETS:
            if name != archive_filename(target):
                continue
            if target in found:
                raise SystemExit(f"duplicate sidecar for {target}")
            found[target] = parse_sha256(path.read_text(encoding="utf-8"), path)
    missing = [target for target in REQUIRED_TARGETS if target not in found]
    if missing:
        raise SystemExit("missing sha256 for " + ", ".join(missing))
    return found


def _ruby_pair(indent: str, url: str, digest: str) -> list[str]:
    return [f'{indent}url "{url}"', f'{indent}sha256 "{digest}"']


def _os_block(
    os_name: str,
    pairs: list[tuple[str, str, str]],
) -> list[str]:
    """pairs are (brew_arch, url, digest) in emit order."""
    if not pairs:
        return []
    lines = [f"  on_{os_name} do"]
    for arch, url, digest in pairs:
        lines.append(f"    on_{arch} do")
        lines.extend(_ruby_pair("      ", url, digest))
        lines.append("    end")
    lines.append("  end")
    lines.append("")
    return lines


def homebrew_formula(repo: str, version: str, hashes: dict[str, str]) -> str:
    lines = [
        "class Wiremux < Formula",
        f'  desc "{HOMEBREW_DESC}"',
        f'  homepage "https://github.com/{repo}"',
        '  license any_of: ["MIT", "Apache-2.0"]',
        "",
    ]
    macos: list[tuple[str, str, str]] = []
    if "aarch64-apple-darwin" in hashes:
        name = archive_filename("aarch64-apple-darwin")
        macos.append(
            ("arm", asset_url(repo, version, name), hashes["aarch64-apple-darwin"])
        )
    if "x86_64-apple-darwin" in hashes:
        name = archive_filename("x86_64-apple-darwin")
        macos.append(
            ("intel", asset_url(repo, version, name), hashes["x86_64-apple-darwin"])
        )
    linux: list[tuple[str, str, str]] = []
    if "x86_64-unknown-linux-gnu" in hashes:
        name = archive_filename("x86_64-unknown-linux-gnu")
        linux.append(
            (
                "intel",
                asset_url(repo, version, name),
                hashes["x86_64-unknown-linux-gnu"],
            )
        )
    if "aarch64-unknown-linux-gnu" in hashes:
        name = archive_filename("aarch64-unknown-linux-gnu")
        linux.append(
            (
                "arm",
                asset_url(repo, version, name),
                hashes["aarch64-unknown-linux-gnu"],
            )
        )
    lines.extend(_os_block("macos", macos))
    lines.extend(_os_block("linux", linux))
    lines.extend(
        [
            "  def install",
            '    bin.install "wiremux"',
            "  end",
            "",
            "  test do",
            '    assert_match version.to_s, shell_output("#{bin}/wiremux --version")',
            "  end",
            "end",
            "",
        ]
    )
    return "\n".join(lines)


def scoop_manifest(repo: str, version: str, hashes: dict[str, str]) -> str:
    target = "x86_64-pc-windows-msvc"
    filename = archive_filename(target)
    url = asset_url(repo, version, filename)
    manifest = {
        "version": version,
        "description": DESCRIPTION,
        "homepage": f"https://github.com/{repo}",
        "license": "MIT|Apache-2.0",
        "architecture": {
            "64bit": {
                "url": url,
                "hash": hashes[target],
            }
        },
        "bin": "wiremux.exe",
        "checkver": "github",
        "autoupdate": {
            "architecture": {
                "64bit": {
                    "url": (
                        f"https://github.com/{repo}/releases/download/"
                        f"v$version/{filename}"
                    ),
                    "hash": {
                        "url": "$url.sha256",
                        "regex": "^([a-fA-F0-9]{64})",
                    },
                }
            }
        },
    }
    return json.dumps(manifest, indent=4) + "\n"


def shell_installer(repo: str, version: str, hashes: dict[str, str]) -> str:
    arms = []
    for target in ALL_TARGETS:
        if "windows" in target or target not in hashes:
            continue
        arms.append(f'  {target}) want="{hashes[target]}" ;;')
    if not arms:
        raise SystemExit("no unix archive for the shell installer")
    cases = "\n".join(arms)
    return f"""#!/bin/sh
# Install the wiremux {version} archive into a prefix.
set -eu

version="{version}"
repo="{repo}"
prefix="${{WIREMUX_PREFIX:-$HOME/.local}}"

os=$(uname -s)
arch=$(uname -m)
case "$os:$arch" in
  Darwin:arm64) target=aarch64-apple-darwin ;;
  Darwin:x86_64) target=x86_64-apple-darwin ;;
  Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-gnu ;;
  Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-gnu ;;
  *)
    echo "FAIL: unsupported ${{os}} ${{arch}}" >&2
    exit 1
    ;;
esac

want=""
case "$target" in
{cases}
  *)
    echo "FAIL: no archive for ${{target}}" >&2
    exit 1
    ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
name="wiremux-${{target}}.tar.gz"
url="https://github.com/${{repo}}/releases/download/v${{version}}/${{name}}"
curl -fsSL "$url" -o "$tmp/$name"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$name" | awk '{{print $1}}')
else
  actual=$(shasum -a 256 "$tmp/$name" | awk '{{print $1}}')
fi
if [ "${{#actual}}" -ne 64 ] || [ "$actual" != "$want" ]; then
  echo "FAIL: sha256 mismatch for ${{name}}" >&2
  exit 1
fi
tar -xzf "$tmp/$name" -C "$tmp"
mkdir -p "$prefix/bin"
install -m 0755 "$tmp/wiremux" "$prefix/bin/wiremux"
echo "OK: ${{prefix}}/bin/wiremux"
"""


def powershell_installer(repo: str, version: str, hashes: dict[str, str]) -> str:
    target = "x86_64-pc-windows-msvc"
    filename = archive_filename(target)
    return f"""$ErrorActionPreference = 'Stop'
$Version = '{version}'
$Repo = '{repo}'
$Want = '{hashes[target]}'
if ($env:WIREMUX_PREFIX) {{
  $Prefix = $env:WIREMUX_PREFIX
}} else {{
  $Prefix = Join-Path $env:LOCALAPPDATA 'wiremux'
}}
$Name = '{filename}'
$Url = "https://github.com/$Repo/releases/download/v$Version/$Name"
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {{
  $Zip = Join-Path $Tmp $Name
  Invoke-WebRequest -Uri $Url -OutFile $Zip
  $Got = (Get-FileHash -Algorithm SHA256 -Path $Zip).Hash.ToLowerInvariant()
  if ($Got -ne $Want) {{
    throw "sha256 mismatch for $Name"
  }}
  Expand-Archive -Path $Zip -DestinationPath $Tmp -Force
  $Dest = Join-Path $Prefix 'bin'
  New-Item -ItemType Directory -Force -Path $Dest | Out-Null
  Copy-Item -Force (Join-Path $Tmp 'wiremux.exe') (Join-Path $Dest 'wiremux.exe')
  Write-Output "OK: $(Join-Path $Dest 'wiremux.exe')"
}} finally {{
  Remove-Item -Recurse -Force $Tmp
}}
"""


def winget_manifests(repo: str, version: str, hashes: dict[str, str]) -> dict[str, str]:
    target = "x86_64-pc-windows-msvc"
    filename = archive_filename(target)
    url = asset_url(repo, version, filename)
    digest = hashes[target].upper()
    version_yaml = f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.1.12.0.schema.json

PackageIdentifier: Wiremux.Wiremux
PackageVersion: {version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.12.0
"""
    installer_yaml = f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.1.12.0.schema.json

PackageIdentifier: Wiremux.Wiremux
PackageVersion: {version}
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: wiremux.exe
  PortableCommandAlias: wiremux
ArchiveBinariesDependOnPath: true
Commands:
- wiremux
Installers:
- Architecture: x64
  InstallerUrl: {url}
  InstallerSha256: {digest}
  Dependencies:
    PackageDependencies:
    - PackageIdentifier: Microsoft.VCRedist.2015+.x64
ManifestType: installer
ManifestVersion: 1.12.0
"""
    locale_yaml = f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.1.12.0.schema.json

PackageIdentifier: Wiremux.Wiremux
PackageVersion: {version}
PackageLocale: en-US
Publisher: Wiremux
PublisherUrl: https://github.com/{repo}
PackageName: Wiremux
PackageUrl: https://github.com/{repo}
License: MIT OR Apache-2.0
LicenseUrl: https://github.com/{repo}/blob/v{version}/LICENSE
ShortDescription: {DESCRIPTION}
Moniker: wiremux
ManifestType: defaultLocale
ManifestVersion: 1.12.0
"""
    return {
        "winget/Wiremux.Wiremux.yaml": version_yaml,
        "winget/Wiremux.Wiremux.installer.yaml": installer_yaml,
        "winget/Wiremux.Wiremux.locale.en-US.yaml": locale_yaml,
    }


def chocolatey_files(repo: str, version: str, hashes: dict[str, str]) -> dict[str, bytes]:
    target = "x86_64-pc-windows-msvc"
    filename = archive_filename(target)
    url = asset_url(repo, version, filename)
    digest = hashes[target]
    nuspec = f"""<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2015/06/nuspec.xsd">
  <metadata>
    <id>wiremux</id>
    <version>{version}</version>
    <title>Wiremux</title>
    <authors>wiremuxhq</authors>
    <owners>wiremuxhq</owners>
    <description>{DESCRIPTION}</description>
    <projectUrl>https://github.com/{repo}/releases</projectUrl>
    <projectSourceUrl>https://github.com/{repo}</projectSourceUrl>
    <packageSourceUrl>https://github.com/{repo}</packageSourceUrl>
    <licenseUrl>https://github.com/{repo}/blob/v{version}/LICENSE</licenseUrl>
    <requireLicenseAcceptance>false</requireLicenseAcceptance>
    <releaseNotes>https://github.com/{repo}/releases/tag/v{version}</releaseNotes>
    <tags>wiremux cli</tags>
  </metadata>
  <files>
    <file src="tools\\**" target="tools" />
  </files>
</package>
"""
    install = f"""$ErrorActionPreference = 'Stop'
$packageName = $env:ChocolateyPackageName
$toolsDir = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"
$url64 = '{url}'
$checksum64 = '{digest}'
Install-ChocolateyZipPackage -PackageName $packageName -Url64bit $url64 -Checksum64 $checksum64 -ChecksumType64 'sha256' -UnzipLocation $toolsDir
"""
    return {
        "chocolatey/wiremux.nuspec": nuspec.encode("utf-8"),
        "chocolatey/tools/chocolateyInstall.ps1": b"\xef\xbb\xbf" + install.encode("utf-8"),
    }


def render(version: str, assets: Path, repo: str) -> dict[str, bytes]:
    if not REPO_RE.fullmatch(repo):
        raise SystemExit(f"bad repo: {repo}")
    version = normalize_version(version)
    hashes = load_hashes(assets)
    files: dict[str, bytes] = {
        "wiremux-installer.sh": shell_installer(repo, version, hashes).encode("utf-8"),
        "wiremux-installer.ps1": powershell_installer(repo, version, hashes).encode(
            "utf-8"
        ),
        "Formula/wiremux.rb": homebrew_formula(repo, version, hashes).encode("utf-8"),
        "bucket/wiremux.json": scoop_manifest(repo, version, hashes).encode("utf-8"),
    }
    for rel, text in winget_manifests(repo, version, hashes).items():
        files[rel] = text.encode("utf-8")
    files.update(chocolatey_files(repo, version, hashes))
    return files


def write_files(out: Path, files: dict[str, bytes]) -> None:
    for rel in sorted(files):
        path = out / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(files[rel])
        if rel.endswith(".sh"):
            path.chmod(0o755)


def check_files(out: Path, files: dict[str, bytes]) -> None:
    stale = []
    for rel, data in sorted(files.items()):
        path = out / rel
        if not path.is_file() or path.read_bytes() != data:
            stale.append(rel)
    if stale:
        raise SystemExit("stale: " + ", ".join(stale))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--repo", default="wiremuxhq/wiremux")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if not args.assets.is_dir():
        raise SystemExit(f"assets dir missing: {args.assets}")
    files = render(args.version, args.assets, args.repo)
    if args.check:
        check_files(args.out, files)
        print("OK: current")
        return
    write_files(args.out, files)
    for rel in sorted(files):
        print(f"OK: {rel}")


if __name__ == "__main__":
    main()
