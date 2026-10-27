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
    ("xref-before-stat", "cmd/blocks/xref/load.rs", "            if crate::file_access::check_read(&file.to_string_lossy()).is_err() {\n                continue;\n            }", "", CLI, "xattach_rejects_existing_and_missing_outside_candidates_equally"),
    ("xref-abort-instead-of-fallback", "cmd/blocks/xref/load.rs", "            if crate::file_access::check_read(&file.to_string_lossy()).is_err() {\n                continue;\n            }", "            crate::file_access::check_read(&file.to_string_lossy()).map_err(Missing::Automation)?;", ENGINE, "confined_xrefs_fall_back_beside_host_and_hide_missing_candidates"),
    ("device-no-disk-spelling", "file_access.rs", "&& !disk_spelling", "&& true", ENGINE, "verbatim_disk_paths_inside_roots_are_accepted"),
    ("prefix-before-absolute", "file_access.rs", "if !is_disk_path(&absolute) {", "if !is_disk_path(Path::new(path)) {", ENGINE, "file_access::tests::normalized_"),
    ("allow-unc", "file_access.rs", "std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)", "std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_) | std::path::Prefix::UNC(_, _)", ENGINE, "normalized_namespaces_are_rejected_before_resolution"),
    ("allow-verbatim-unc", "file_access.rs", "std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)", "std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_) | std::path::Prefix::VerbatimUNC(_, _)", ENGINE, "normalized_namespaces_are_rejected_before_resolution"),
    ("allow-device-ns", "file_access.rs", "std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)", "std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_) | std::path::Prefix::DeviceNS(_)", ENGINE, "normalized_prefix_policy_allows_only_disks"),
    ("allow-other-verbatim", "file_access.rs", "std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)", "std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_) | std::path::Prefix::Verbatim(_)", ENGINE, "normalized_prefix_policy_allows_only_disks"),
    ("skip-normalized-gate", "file_access.rs", "if !is_disk_path(&absolute) {", "if false && !is_disk_path(&absolute) {", ENGINE, "slash_namespaces_are_blocked_at_file_boundaries"),
    ("lisp-findfile", "lisp/builtins.rs", "crate::file_access::check_read(path).is_ok() && std::path::Path::new(path).is_file()", "std::path::Path::new(path).is_file()", ENGINE, "confined_lisp_file_functions_check_paths_at_use"),
    ("lisp-read-load", "lisp/builtins.rs", "    crate::file_access::check_read(path).ok()?;", "", ENGINE, "confined_lisp_file_functions_check_paths_at_use"),
    ("lisp-open-write", "lisp/builtins.rs", "crate::file_access::check_write(&path).is_err() || !can_write(&path)", "!can_write(&path)", ENGINE, "confined_lisp_file_functions_check_paths_at_use"),
    ("lisp-close-write", "lisp/builtins.rs", "    if crate::file_access::check_write(path).is_err() {\n        return;\n    }", "", ENGINE, "confined_lisp_file_functions_check_paths_at_use"),
]

source = Path("crates/engine/src/file_access.rs").read_text(encoding="utf-8")
predicate = re.search(r"fn is_disk_path\(path: &Path\) -> bool \{.*?\n\}", source, re.S).group()
CASES.append(("allow-missing-prefix", "file_access.rs", predicate, predicate.replace("_ => false", "_ => true"), ENGINE, "normalized_prefix_policy_allows_only_disks"))
start = source.index("        // Reject remote/device namespaces")
resolution = "        let target = resolve_path(&absolute)?;"
end = source.index(resolution, start)
block = source[start:end + len(resolution)]
CASES.append(("network-resolution-order", "file_access.rs", block, "        let target = resolve_path(Path::new(path))?;\n" + source[start:end], ENGINE, "normalized_namespaces_are_rejected_before_resolution"))
CASES.append(("resolver-original-spelling", "file_access.rs", resolution, "        let target = resolve_path(Path::new(path))?;", ENGINE, "resolver_receives_the_checked_absolute_spelling"))
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
