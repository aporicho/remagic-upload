#!/usr/bin/env python3
from pathlib import Path
import tomllib

manifest = tomllib.loads(Path("manifests/upload.toml").read_text())
assert manifest["id"] == "upload"
assert manifest["name"] == "文件上传"
assert manifest["package"] == "remagic-upload"
assert manifest["required_remagic_api"] == 4
assert set(manifest["supported_devices"]) == {"paper_pro", "paper_pro_move"}
assert manifest["runtime"]["network"] == {"mode": "inbound", "listen_port": 8787}
assert manifest["runtime"]["background_execution"] == "freeze"
assert "network:listen-v1" in manifest["capabilities"]
assert "storage:books-write-v1" in manifest["capabilities"]
assert "storage:wallpapers-write-v1" in manifest["capabilities"]
print("manifest contract ok")
