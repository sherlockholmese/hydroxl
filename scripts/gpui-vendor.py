#!/usr/bin/env python3
"""Reproduce the local GPUI patch set from checksum-pinned crates (Python 3.11+).

check: verify the entire vendor trees match published sources plus patches.
refresh: regenerate minimal patches after editing vendor sources, then verify.
rebuild: stage and validate all patches before replacing vendor trees.
Sources come from Cargo's archive cache; --download permits fetching missing ones.
"""
import argparse
import difflib
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
PATCHES = ROOT / "patches/gpui"
IGNORED = {".cargo-ok", ".cargo_vcs_info.json"}


def files(folder):
    result = {}
    for path in folder.rglob("*"):
        if path.is_symlink():
            raise ValueError(f"Unexpected symlink: {path}")
        if path.is_file() and path.name not in IGNORED:
            result[path.relative_to(folder).as_posix()] = path.read_bytes()
    return result


def archive(spec, scratch, download):
    filename = f"{spec['name']}-{spec['version']}.crate"
    cargo = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    matches = sorted((cargo / "registry/cache").glob(f"*/{filename}"))
    data = None
    for candidate in matches:
        candidate_data = candidate.read_bytes()
        if hashlib.sha256(candidate_data).hexdigest() == spec["sha256"]:
            data = candidate_data
            break
    if data is None:
        if not download:
            raise ValueError(f"Missing verified {filename}; run cargo fetch or pass --download")
        url = f"https://static.crates.io/crates/{spec['name']}/{filename}"
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read()
    if hashlib.sha256(data).hexdigest() != spec["sha256"]:
        raise ValueError(f"Checksum mismatch: {filename}")
    target = scratch / filename
    target.write_bytes(data)
    return target


def extract(spec, package, destination):
    prefix = f"{spec['name']}-{spec['version']}"
    destination.mkdir()
    with tarfile.open(package) as source:
        for member in source.getmembers():
            relative = Path(member.name)
            if not relative.parts or relative.parts[0] != prefix or ".." in relative.parts:
                raise ValueError(f"Unsafe archive member: {member.name}")
            if member.isdir():
                continue
            if not member.isfile():
                raise ValueError(f"Unsupported archive member: {member.name}")
            relative = Path(*relative.parts[1:])
            if relative.name in IGNORED:
                continue
            path = destination / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(source.extractfile(member).read())


def refresh(spec, pristine):
    original, current = files(pristine), files(ROOT / "vendor" / spec["name"])
    chunks = []
    for name in sorted(original.keys() | current.keys()):
        before, after = original.get(name, b""), current.get(name, b"")
        if before == after:
            continue
        try:
            before, after = before.decode("utf-8"), after.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ValueError(f"Binary patch unsupported: {spec['name']}/{name}") from error
        if (before and not before.endswith("\n")) or (after and not after.endswith("\n")):
            raise ValueError(f"Patch files must end in a newline: {name}")
        chunks.extend(difflib.unified_diff(
            before.splitlines(keepends=True), after.splitlines(keepends=True),
            fromfile=f"a/{name}" if name in original else "/dev/null",
            tofile=f"b/{name}" if name in current else "/dev/null",
        ))
    (PATCHES / spec["patch"]).write_text("".join(chunks))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "refresh", "rebuild"))
    parser.add_argument("--download", action="store_true")
    args = parser.parse_args()
    specs = tomllib.loads((PATCHES / "upstream.toml").read_text())["crate"]
    locked = {p["name"]: p for p in tomllib.loads((ROOT / "Cargo.lock").read_text())["package"] if p["name"] in {s["name"] for s in specs}}
    for spec in specs:
        if locked.get(spec["name"], {}).get("version") != spec["version"]:
            raise ValueError(f"Lockfile version differs from patch pin: {spec['name']}")
    with tempfile.TemporaryDirectory(prefix="hydroxl-gpui-") as temporary:
        scratch = Path(temporary)
        staged = []
        for spec in specs:
            pristine = scratch / spec["name"]
            extract(spec, archive(spec, scratch, args.download), pristine)
            if args.command == "refresh":
                refresh(spec, pristine)
            result = subprocess.run(
                ["patch", "--batch", "--forward", "--fuzz=0", "-p1", "-i", str(PATCHES / spec["patch"])],
                cwd=pristine, capture_output=True, text=True,
            )
            if result.returncode or "offset" in result.stdout.lower():
                raise ValueError(f"Patch did not apply exactly: {spec['patch']}\n{result.stdout}{result.stderr}")
            if args.command != "rebuild":
                expected, actual = files(pristine), files(ROOT / "vendor" / spec["name"])
                differences = [name for name in sorted(expected.keys() | actual.keys()) if expected.get(name) != actual.get(name)]
                if differences:
                    raise ValueError(f"Vendor drift in {spec['name']}: {', '.join(differences)}")
            staged.append((spec, pristine))
            print(f"Verified {spec['name']} {spec['version']}")
        if args.command == "rebuild":
            # Validate every crate before changing any source tree. Keep backups until
            # the whole replacement succeeds; rollback if any copy/rename fails.
            installed, backups = [], []
            try:
                for spec, source in staged:
                    destination = ROOT / "vendor" / spec["name"]
                    backup = scratch / f"{spec['name']}.backup"
                    if destination.exists():
                        shutil.copytree(destination, backup)
                        backups.append((destination, backup))
                        shutil.rmtree(destination)
                    installed.append(destination)
                    shutil.copytree(source, destination)
            except BaseException:
                for destination in installed:
                    if destination.exists():
                        shutil.rmtree(destination)
                for destination, backup in backups:
                    shutil.copytree(backup, destination)
                raise


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, tarfile.TarError) as error:
        raise SystemExit(str(error))
