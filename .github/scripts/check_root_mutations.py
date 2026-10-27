"""Remove each root boundary independently; only a failed regression kills a mutation."""
from pathlib import Path
import subprocess
import re

ENGINE = ["-p", "cadcraft-engine", "--lib"]
CLI = ["-p", "cadcraft-cli", "--test", "mcp_roots"]
CASES = [
    ("recovery-list", "autosave.rs", "        crate::file_access::check_read(&dir.to_string_lossy()).ok()?;", "", ENGINE, "recovery_listing_checks_the_directory"),
    ("recovery-delete", "autosave.rs", "        if crate::file_access::check_write(path).is_err() {\n            return;\n        }", "", ENGINE, "recovery_discard_checks_each_write"),
    ("recovery-prune", "autosave.rs", "if crate::file_access::check_write(&path.to_string_lossy()).is_ok() {", "if true {", ENGINE, "recovery_prune_checks_each_write"),
    ("recovery-lock", "autosave.rs", "        crate::file_access::check_read(&lock_path(dir, instance).to_string_lossy()).ok()?;", "", ENGINE, "recovery_lock_metadata_does_not_follow_outside_links"),
    ("recovery-command-list", "cmd/audit.rs", '        crate::file_access::check_read(&dir).map_err(|e| bad("drawingrecovery", e))?;', "", ENGINE, "recovery_command_reports_denied_directory_and_discard"),
    ("recovery-command-delete", "cmd/audit.rs", '            crate::file_access::check_write(&f.path).map_err(|e| bad("drawingrecovery", e))?;', "", ENGINE, "recovery_command_reports_denied_directory_and_discard"),
    ("autosave-setting", "sysvars.rs", '            crate::file_access::check_write_directory(path).map_err(|msg| EngineError::BadParams { cmd: "setvar".into(), msg })?;', "", CLI, "savefilepath_outside_the_write_root_is_refused"),
    ("atomic-target", "atomic_write.rs", "    crate::file_access::check_write(&path.to_string_lossy()).map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e))?;", "", ENGINE, "atomic_destination_checks_write_authority"),
    ("atomic-temp", "atomic_write.rs", "        crate::file_access::check_write(&tmp.to_string_lossy()).map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e))?;", "", ENGINE, "atomic_temporary_stays_inside_the_write_root"),
    ("xref-before-stat", "cmd/blocks/xref/load.rs", "            crate::file_access::check_read(&file.to_string_lossy()).map_err(Missing::Automation)?;", "", CLI, "xattach_rejects_existing_and_missing_outside_candidates_equally"),
    ("network-before-resolve", "file_access.rs", "if special || extended_network {", "if false && (special || extended_network) {", ENGINE, "network_paths_are_rejected_before_resolution"),
]

source = Path("crates/engine/src/file_access.rs").read_text(encoding="utf-8")
for kind, test in [("read", "files_open_only_from_inside_the_read_root"), ("write", "writes_go_only_into_the_write_root_and_leave_no_temp_file_outside")]:
    match = re.search(r"pub fn check_" + kind + r"\(path: &str\) -> Result<\(\), String> \{.*?\n\}", source, re.S)
    if match is None:
        raise RuntimeError(f"Missing {kind} gate")
    old = match.group()
    CASES.append((f"original-{kind}", "file_access.rs", old, old.split("\n")[0] + "\n    let _ = path;\n    Ok(())\n}", CLI, test))

for name, source, old, new, args, test in CASES:
    path = Path("crates/engine/src") / source
    original = path.read_text(encoding="utf-8")
    if original.count(old) != 1:
        raise RuntimeError(f"Mutation must match exactly once: {name}")
    try:
        path.write_text(original.replace(old, new), encoding="utf-8")
        result = subprocess.run(["cargo", "test", "--locked", *args, test], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
        print(f"=== {name} ===\n{result.stdout}", flush=True)
        if result.returncode == 0 or "test result: FAILED." not in result.stdout:
            raise RuntimeError(f"Survived or failed to run: {name}")
    finally:
        path.write_text(original, encoding="utf-8")
print(f"Killed {len(CASES)}/{len(CASES)} mutations", flush=True)
