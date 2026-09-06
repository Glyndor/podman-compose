#!/usr/bin/env python3
"""Assert that every file the install scripts fetch from a release exists on
disk, in the staging directory, immediately before the release is published.

This is the substance half of the installer contract. `installer-contract.yml`
answers a different question, "do `install.sh` and `release.yml` agree on the
asset *names*?", by reading both files. That is a cross-reference between two
declarations, and reading YAML is the right way to answer it. It cannot answer
"does the release actually produce these files", because nothing in a workflow
file is evidence that a step ran.

That distinction was not academic. Between v1.14.1 and this change the gate
carried a third check that tried to answer the substance question from the same
parsed set:

    published = set(re.findall(r"^\\s*-?\\s*asset:\\s*(\\S+)\\s*$", release_yml, re.M))
    if "SHA256SUMS" not in published: fail

`published` is the build matrix. `SHA256SUMS` is a manifest computed *from* the
matrix outputs in a later step and passed to `gh release create` as an argument,
so it can never appear as a matrix entry. The check was therefore unpassable by
construction: failing it was the normal state of a correct release workflow, and
the only way to pass was to declare a matrix entry named `SHA256SUMS` without
producing one, the exact lie the check existed to prevent. Measured on
2026-08-17: no repository in the org declared that asset, podup was the gate's
only consumer and was still pinned to a tag from before the clause, so the check
had never run green anywhere. Its first contact with a real repository (the pin
bump in podup#1425) failed a workflow that does produce and sign the manifest.

The fix is not a better regex over the same file. It is to ask the question at
the only moment it can be answered honestly: after the assets are built, signed
and checksummed, while they are sitting in the runner's working directory, and
before `gh release create` makes them immutable. At that point existence is a
stat call, not an inference.

What it checks is derived from the install script rather than hardcoded. A
release-fetching line looks like

    download "${BASE_URL}/SHA256SUMS" "${TMP_DIR}/SHA256SUMS"

so the set of names is read straight out of the contract the installer will
execute. Hardcoding `SHA256SUMS` here would have repeated the original mistake
one layer down: the old clause never checked `SHA256SUMS.sig`, which
`install.sh` also downloads and without which it aborts, so a release missing
only the signature would have passed a gate whose stated purpose was making
downloads verifiable.

Conscious non-goals: this does not verify signatures (release.yml already
re-verifies every `.sig` against the keys consumers embed), does not check file
contents, and does not talk to GitHub. It answers one question, whether the files
the installer will ask for are present, and fails closed on anything it cannot
parse, because an unparsed installer is not evidence of a good release.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# `download "${BASE_URL}/<name>" "<dest>"`, the only shape the install scripts
# use to pull a file out of the release. Anything fetched from another host
# (the apt keyring, for example) does not go through BASE_URL and is correctly
# invisible here.
DOWNLOAD_RE = re.compile(r'\$\{BASE_URL\}/([^"\']+)')

ARTIFACT_RE = re.compile(r'ARTIFACT="([^"]+)"')
ARTIFACT_TEMPLATE_RE = re.compile(r"([A-Za-z0-9_.-]+)-\$\{OS\}-\$\{ARCH\}")
OS_RE = re.compile(r'OS="([a-z0-9_]+)"')
ARCH_RE = re.compile(r'ARCH="([a-z0-9_]+)"')

PS_ARTIFACT_RE = re.compile(r'\$Artifact\s*=\s*"([^"]+)"')
PS_ARTIFACT_TEMPLATE_RE = re.compile(
    r"([A-Za-z0-9_.-]+)-(\$\{?OS\}?|[a-z0-9_]+)-\$\{?Arch\}?(\.[A-Za-z0-9]+)?"
)
PS_OS_RE = re.compile(r"\$OS\s*=\s*['\"]([a-z0-9_]+)['\"]")
PS_ARCH_RE = re.compile(r"\$Arch\s*=\s*['\"]([a-z0-9_]+)['\"]")


def fail(message: str) -> "NoReturn":  # noqa: F821
    sys.exit(f"verify-release-assets: {message}")


def artifact_names_sh(text: str, path: Path) -> set[str]:
    """Expand install.sh's ARTIFACT template over every OS/ARCH arm it handles."""
    template = ARTIFACT_RE.search(text)
    if not template:
        fail(f'no ARTIFACT="..." in {path}.')
    matched = ARTIFACT_TEMPLATE_RE.fullmatch(template.group(1))
    if not matched:
        fail(
            'ARTIFACT template must be "<name>-${OS}-${ARCH}"; got ' + template.group(1)
        )
    oses = set(OS_RE.findall(text))
    arches = set(ARCH_RE.findall(text))
    if not oses or not arches:
        fail(f"could not parse OS/ARCH arms from {path}.")
    return {f"{matched.group(1)}-{o}-{a}" for o in oses for a in arches}


def artifact_names_ps1(text: str, path: Path) -> set[str]:
    """Expand install.ps1's $Artifact template. The OS is usually a literal,
    there being only one Windows, but an interpolation is accepted too."""
    template = PS_ARTIFACT_RE.search(text)
    if not template:
        fail(f'no $Artifact = "..." in {path}.')
    matched = PS_ARTIFACT_TEMPLATE_RE.fullmatch(template.group(1))
    if not matched:
        fail(
            'the $Artifact template must be "<name>-<os>-$Arch[.ext]"; got '
            + template.group(1)
        )
    prefix, os_token, suffix = (
        matched.group(1),
        matched.group(2),
        matched.group(3) or "",
    )
    oses = (
        {os_token} if os_token not in ("$OS", "${OS}") else set(PS_OS_RE.findall(text))
    )
    arches = set(PS_ARCH_RE.findall(text))
    if not oses or not arches:
        fail(f"could not parse OS/ARCH arms from {path}.")
    return {f"{prefix}-{o}-{a}{suffix}" for o in oses for a in arches}


def release_fetches(text: str, path: Path) -> set[str]:
    """Every name install.sh pulls from the release, with the artifact
    placeholder left in for the caller to expand.

    Only the shell installer is parsed for this. install.ps1 expresses the same
    downloads through a helper (`Get-ReleaseFile 'SHA256SUMS'`) rather than an
    interpolated URL, and teaching this script a second shape keyed on a
    PowerShell function name would be exactly the brittle name-matching the
    gate is being fixed to stop doing. It is also unnecessary: the manifest and
    its signature are properties of the release, not of the installer that
    fetches them, so both installers must find the same ones. What genuinely
    differs between the two is the per-platform artifact, and that is read from
    each script separately below.
    """
    names = set(DOWNLOAD_RE.findall(text))
    if not names:
        fail(
            f"{path} fetches nothing from the release. Either the installer no "
            "longer downloads release assets, in which case this gate should be "
            "removed from the caller, or the ${BASE_URL}/... form changed and "
            "this script must be taught the new one."
        )
    return names


def expand(names: set[str], artifacts: set[str]) -> set[str]:
    """Replace the ARTIFACT placeholder with every concrete name it stands for."""
    expanded: set[str] = set()
    for name in names:
        if "${ARTIFACT}" in name:
            head, _, tail = name.partition("${ARTIFACT}")
            expanded.update(f"{head}{a}{tail}" for a in artifacts)
        else:
            expanded.add(name)
    return expanded


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--workdir",
        required=True,
        type=Path,
        help="directory holding the staged release assets",
    )
    parser.add_argument(
        "--install-script", required=True, type=Path, help="path to install.sh"
    )
    parser.add_argument(
        "--install-ps1",
        type=Path,
        default=None,
        help="path to install.ps1; skipped when absent",
    )
    args = parser.parse_args()

    if not args.workdir.is_dir():
        fail(f"{args.workdir} is not a directory.")
    if not args.install_script.is_file():
        fail(f"{args.install_script} not found.")

    sh_text = args.install_script.read_text()
    fetches = release_fetches(sh_text, args.install_script)
    artifacts = artifact_names_sh(sh_text, args.install_script)

    if args.install_ps1 and args.install_ps1.is_file():
        artifacts |= artifact_names_ps1(args.install_ps1.read_text(), args.install_ps1)
    elif args.install_ps1:
        print(
            f"::notice::verify-release-assets: {args.install_ps1} not found, skipped."
        )

    required = expand(fetches, artifacts)

    missing = sorted(n for n in required if not (args.workdir / n).is_file())
    if missing:
        listed = "\n".join(f"  - {n}" for n in missing)
        fail(
            "the install scripts fetch files this release does not stage:\n"
            f"{listed}\n"
            f"staged in {args.workdir}: {sorted(p.name for p in args.workdir.iterdir())}\n"
            "Add them to the step that builds the release, not to the workflow's "
            "asset declarations; this check reads the directory, not the YAML."
        )

    print(f"OK: all {len(required)} files the install scripts fetch are staged.")


if __name__ == "__main__":
    main()
