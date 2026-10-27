"""Prove structural namespace regressions fail on eac0b41 before green tests."""
from pathlib import Path
import subprocess

cases = [
    ("file_access.rs", "normalized_namespaces_are_rejected_before_resolution"),
    ("file_access.rs", "slash_namespaces_are_blocked_at_file_boundaries"),
]
for source, test in cases:
    path = Path("crates/engine/src") / source
    original = path.read_text(encoding="utf-8")
    old = subprocess.check_output(["git", "show", f"eac0b41:{path.as_posix()}"], text=True, encoding="utf-8")
    # Keep the test-only observer at the first filesystem call in the old resolver too.
    old = old.replace("        match std::fs::canonicalize(at) {", "        #[cfg(test)]\n        tests::before_filesystem_resolution();\n        match std::fs::canonicalize(at) {")
    marker = "#[cfg(test)]\nmod tests {"
    try:
        path.write_text(old.split(marker)[0] + marker + original.split(marker)[1], encoding="utf-8")
        result = subprocess.run(["cargo", "test", "--locked", "-p", "cadcraft-engine", "--lib", test], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
        print(f"=== RED {test} ===\n{result.stdout}", flush=True)
        if result.returncode == 0 or "test result: FAILED." not in result.stdout:
            raise RuntimeError(f"Expected assertion failure on old production code: {test}")
    finally:
        path.write_text(original, encoding="utf-8")
print("Proved 2/2 new regressions fail on eac0b41", flush=True)
