#!/usr/bin/env python3
"""Generate checksum-pinned manifests for VSH HerDL releases."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

TARGETS = (
    "linux-x86_64",
    "linux-aarch64",
    "macos-aarch64",
)
REPOSITORY = "victor-software-house/herdr"
RELEASE_PREFIX = f"https://github.com/{REPOSITORY}/releases/download/"


def load_optional_json(path: Path | None) -> dict[str, Any]:
    if path is None or not path.exists():
        return {}
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def artifact_metadata(artifacts_dir: Path, tag: str) -> dict[str, dict[str, str]]:
    assets: dict[str, dict[str, str]] = {}
    for target in TARGETS:
        filename = f"herdl-{target}"
        artifact = artifacts_dir / filename / filename
        checksum_file = artifacts_dir / filename / f"{filename}.sha256"
        if not artifact.is_file() or not checksum_file.is_file():
            raise ValueError(f"missing artifact or checksum for {target}")
        fields = checksum_file.read_text(encoding="utf-8").split()
        if len(fields) < 2 or fields[1].lstrip("*") != filename:
            raise ValueError(f"invalid checksum file for {target}")
        digest = fields[0].lower()
        actual = hashlib.sha256(artifact.read_bytes()).hexdigest()
        if digest != actual:
            raise ValueError(f"checksum mismatch for {target}")
        assets[target] = {
            "url": f"{RELEASE_PREFIX}{tag}/{filename}",
            "sha256": digest,
        }
    return assets


def validate_identity(base_version: str, build_id: str, commit: str) -> str:
    if len(build_id) != 12 or not commit.startswith(build_id):
        raise ValueError("build id must be the commit's 12-character prefix")
    if not all(character in "0123456789abcdef" for character in commit.lower()):
        raise ValueError("commit must be hexadecimal")
    return f"{base_version.removeprefix('v')}-vsh.{build_id}"


def validate_assets(assets: dict[str, Any], tag: str) -> None:
    if set(assets) != set(TARGETS):
        raise ValueError(f"manifest targets must be exactly {', '.join(TARGETS)}")
    for target, asset in assets.items():
        if not isinstance(asset, dict):
            raise ValueError(f"asset {target} must be an object")
        expected = f"{RELEASE_PREFIX}{tag}/herdl-{target}"
        if asset.get("url") != expected:
            raise ValueError(f"asset {target} must use {expected}")
        digest = asset.get("sha256")
        if not isinstance(digest, str) or len(digest) != 64:
            raise ValueError(f"asset {target} must have a SHA-256 digest")


def retained(mapping: Any, key: str, value: dict[str, Any], limit: int) -> dict[str, Any]:
    result = dict(mapping) if isinstance(mapping, dict) else {}
    result[key] = value
    keys = list(result)
    for stale in keys[:-limit]:
        result.pop(stale, None)
    return result


def generate_manifests(args: argparse.Namespace) -> tuple[dict[str, Any], dict[str, Any]]:
    identity = validate_identity(args.base_version, args.build_id, args.commit)
    tag = f"v{identity}"
    assets = artifact_metadata(args.artifacts_dir, tag)
    validate_assets(assets, tag)
    notes = args.notes.read_text(encoding="utf-8").strip()
    if not notes:
        raise ValueError("release notes must not be empty")

    current_release = {
        "base_version": args.base_version.removeprefix("v"),
        "identity": identity,
        "build_id": args.build_id,
        "commit": args.commit,
        "built_at": args.built_at,
        "protocol": args.protocol,
        "endpoint_generation": args.endpoint_generation,
        "notes": notes,
        "assets": assets,
    }

    previous_latest = load_optional_json(args.previous_latest)
    previous_releases = previous_latest.get("releases", {})
    previous_identity = previous_latest.get("identity")
    if isinstance(previous_identity, str) and previous_identity:
        archived = {
            key: previous_latest[key]
            for key in current_release
            if key in previous_latest
        }
        previous_releases = retained(previous_releases, previous_identity, archived, args.retain)
    releases = retained(previous_releases, identity, current_release, args.retain)
    latest = {
        "schema_version": 1,
        "channel": "stable",
        "version": args.base_version.removeprefix("v"),
        **current_release,
        "releases": releases,
    }

    previous_preview = load_optional_json(args.previous_preview)
    build = {
        key: current_release[key]
        for key in (
            "base_version",
            "identity",
            "commit",
            "built_at",
            "protocol",
            "endpoint_generation",
            "assets",
        )
    }
    builds = retained(previous_preview.get("builds", {}), args.build_id, build, args.retain)
    preview = {
        "schema_version": 1,
        "channel": "preview",
        **current_release,
        "builds": builds,
    }
    return latest, preview


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=False) + "\n", encoding="utf-8")


def manifest_command(args: argparse.Namespace) -> None:
    latest, preview = generate_manifests(args)
    write_json(args.latest_output, latest)
    write_json(args.preview_output, preview)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    subcommands = result.add_subparsers(dest="command", required=True)
    manifest = subcommands.add_parser("manifest")
    manifest.add_argument("--base-version", required=True)
    manifest.add_argument("--build-id", required=True)
    manifest.add_argument("--commit", required=True)
    manifest.add_argument("--built-at", required=True)
    manifest.add_argument("--protocol", required=True, type=int)
    manifest.add_argument("--endpoint-generation", required=True, type=int)
    manifest.add_argument("--notes", required=True, type=Path)
    manifest.add_argument("--artifacts-dir", required=True, type=Path)
    manifest.add_argument("--previous-latest", type=Path)
    manifest.add_argument("--previous-preview", type=Path)
    manifest.add_argument("--latest-output", required=True, type=Path)
    manifest.add_argument("--preview-output", required=True, type=Path)
    manifest.add_argument("--retain", type=int, default=30)
    manifest.set_defaults(run=manifest_command)
    return result


def main() -> None:
    args = parser().parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
