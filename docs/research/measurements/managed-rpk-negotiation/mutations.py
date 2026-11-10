#!/usr/bin/env python3
"""Remove each approved condition independently, restore source even on failure.

Run only in the private admitted Linux fixture checkout, with CARGO_TARGET_DIR
pointing at its owned compiler cache. Each cargo process is bounded by timeout.
"""
from pathlib import Path
import hashlib
import os
import subprocess

ROOT = Path.cwd()
EVIDENCE = Path(os.environ["MANAGED_RPK_EVIDENCE"])
EVIDENCE.mkdir(parents=True, exist_ok=True)
SERVER = Path("crates/hyper-tls/src/server/tls13.rs")
CLIENT = Path("crates/hyper-tls/src/client/tls13.rs")
HELLO = Path("crates/hyper-tls/src/client/hs.rs")
MUTATIONS = [
    ("server-sender", SERVER,
     "                .filter(|_| !cx.config.cert_resolver.only_raw_public_keys())\n", ""),
    ("client-sender", CLIENT,
     "            .filter(|_| !cx.config.client_auth_cert_resolver.only_raw_public_keys())\n", ""),
    ("server-receiver", SERVER,
     "                    _ if config.verifier.requires_raw_public_keys() => None,\n", ""),
    ("client-receiver", HELLO,
     "if !config.verifier.requires_raw_public_keys() && !config.cert_decompressors.is_empty() {",
     "if !config.cert_decompressors.is_empty() {"),
]
ORIGINALS = {path: path.read_bytes() for _, path, _, _ in MUTATIONS}
COMMAND = ["timeout", "120", "cargo", "test", "--locked", "-j4", "--release", "-p", "hyper-tls",
           "--test", "rpk_compression_policy", "rpk_senders_avoid_compression_with_older_offering_peers",
           "--", "--exact", "--test-threads=1", "--nocapture"]
(EVIDENCE / "source.txt").write_bytes(subprocess.check_output(["git", "rev-parse", "HEAD"]))
(EVIDENCE / "command.txt").write_text(" ".join(COMMAND) + "\n")
try:
    for name, path, before, after in MUTATIONS:
        for original_path, content in ORIGINALS.items():
            original_path.write_bytes(content)
        content = path.read_text()
        assert content.count(before) == 1, (name, "guard not unique")
        path.write_text(content.replace(before, after, 1))
        (EVIDENCE / (name + ".patch")).write_bytes(subprocess.check_output(["git", "diff", "--", str(path)]))
        (EVIDENCE / (name + ".sha256")).write_text(hashlib.sha256(path.read_bytes()).hexdigest() + "\n")
        with (EVIDENCE / (name + ".log")).open("wb") as log:
            result = subprocess.run(COMMAND, stdout=log, stderr=subprocess.STDOUT, timeout=180)
        (EVIDENCE / (name + ".exit")).write_text(str(result.returncode) + "\n")
        text = (EVIDENCE / (name + ".log")).read_text()
        assert result.returncode == 101 and "assertion `left == right` failed" in text, (name, result.returncode)
        assert "left: (1, 0)" in text or "left: (0, 1)" in text, (name, "wrong failure")
        print(name, "expected compression regression", flush=True)
finally:
    for path, content in ORIGINALS.items():
        path.write_bytes(content)
        assert path.read_bytes() == content
