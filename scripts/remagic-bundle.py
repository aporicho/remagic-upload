#!/usr/bin/env python3
"""Build or verify the canonical ReMagic bundle inventory."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile

ALLOWED_MODES = {0o644, 0o755}
DOMAIN = b"remagic-bundle-content-v1\0"


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def collect(root):
    if not (root / "manifest.toml").is_file() or not (root / "payload").is_dir():
        raise ValueError("bundle needs manifest.toml and payload/")
    files = []
    for path in root.rglob("*"):
        relative = path.relative_to(root).as_posix()
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode) or relative == "bundle.json":
            continue
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise ValueError(f"unsafe bundle object: {relative}")
        if relative != "manifest.toml" and not relative.startswith("payload/"):
            raise ValueError(f"unexpected bundle path: {relative}")
        mode = stat.S_IMODE(info.st_mode)
        if mode not in ALLOWED_MODES:
            raise ValueError(f"unsupported mode {mode:o}: {relative}")
        files.append({
            "path": relative,
            "sha256": sha256(path),
            "size": info.st_size,
            "mode": f"{mode:04o}",
        })
    files.sort(key=lambda entry: entry["path"].encode())
    if not any(entry["path"].startswith("payload/") for entry in files):
        raise ValueError("bundle payload is empty")
    return files


def record(entry):
    mode = format(int(entry["mode"], 8), "o")
    return (
        f'{entry["path"]}\0{mode}\0{entry["size"]}\0{entry["sha256"]}\n'
        .encode()
    )


def document(root, app_id, package, version):
    files = collect(root)
    payload = hashlib.sha256()
    content = hashlib.sha256()
    content.update(DOMAIN)
    for value in (app_id, package, version):
        content.update(value.encode())
        content.update(b"\0")
    for entry in files:
        encoded = record(entry)
        content.update(encoded)
        if entry["path"].startswith("payload/"):
            payload.update(encoded)
    return {
        "schema": 1,
        "app_id": app_id,
        "package": package,
        "version": version,
        "content_id": content.hexdigest(),
        "manifest_path": "manifest.toml",
        "payload_sha256": payload.hexdigest(),
        "files": files,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("create", "verify"))
    parser.add_argument("root", type=Path)
    parser.add_argument("--app-id", required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    expected = document(args.root, args.app_id, args.package, args.version)
    output = args.root / "bundle.json"
    if args.action == "verify":
        if json.loads(output.read_text(encoding="utf-8")) != expected:
            raise ValueError("bundle.json does not match package contents")
        return
    descriptor, temporary = tempfile.mkstemp(prefix=".bundle.", dir=args.root)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(expected, stream, ensure_ascii=False, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, output)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    main()
