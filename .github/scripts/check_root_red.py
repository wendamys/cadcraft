"""Prove the new regressions fail against b4fbd85 production code before green tests."""
from pathlib import Path
import subprocess

cases = [
    ("file_access.rs", "extended_devices_are_rejected_before_resolution"),
    ("cmd/blocks/xref/load.rs", "confined_xrefs_fall_back_beside_host_and_hide_missing_candidates"),
]
for source, test in cases:
    path = Path("crates/engine/src") / source
    original = path.read_text(encoding="utf-8")
    old = subprocess.check_output(["git", "show", f"b4fbd85:{path.as_posix()}"], text=True, encoding="utf-8")
    marker = "#[cfg(test)]\nmod tests {"
    try:
        path.write_text(old.split(marker)[0] + marker + original.split(marker)[1], encoding="utf-8")
        result = subprocess.run(["cargo", "test", "--locked", "-p", "cadcraft-engine", "--lib", test], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
        print(f"=== RED {test} ===\n{result.stdout}", flush=True)
        if result.returncode == 0 or "test result: FAILED." not in result.stdout:
            raise RuntimeError(f"Expected assertion failure on old production code: {test}")
    finally:
        path.write_text(original, encoding="utf-8")
print("Proved 2/2 new regressions fail on b4fbd85", flush=True)
