use std::collections::HashSet;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use codex_apply_patch::ApplyPatchAction;
use codex_apply_patch::ApplyPatchFileChange;

use crate::exec::SandboxType;
use crate::is_safe_command::is_known_safe_command;
use crate::protocol::AskForApproval;
use crate::protocol::SandboxPolicy;

#[derive(Debug, PartialEq)]
pub enum SafetyCheck {
    AutoApprove { sandbox_type: SandboxType },
    AskUser,
    Reject { reason: String },
}

pub fn assess_patch_safety(
    action: &ApplyPatchAction,
    policy: AskForApproval,
    sandbox_policy: &SandboxPolicy,
    cwd: &Path,
) -> SafetyCheck {
    if action.is_empty() {
        return SafetyCheck::Reject {
            reason: "empty patch".to_string(),
        };
    }

    // Continue to see if this can be auto-approved for all policies.

    // Even though the patch *appears* to be constrained to writable paths, it
    // is possible that paths in the patch are hard links to files outside the
    // writable roots, so we should still run `apply_patch` in a sandbox in that
    // case.
    if is_write_patch_constrained_to_writable_paths(action, sandbox_policy, cwd)
        || policy == AskForApproval::OnFailure
    {
        // Only auto‑approve when we can actually enforce a sandbox. Otherwise
        // fall back to asking the user because the patch may touch arbitrary
        // paths outside the project.
        match get_platform_sandbox() {
            Some(sandbox_type) => SafetyCheck::AutoApprove { sandbox_type },
            None if sandbox_policy == &SandboxPolicy::DangerFullAccess => {
                // If the user has explicitly requested DangerFullAccess, then
                // we can auto-approve even without a sandbox.
                SafetyCheck::AutoApprove {
                    sandbox_type: SandboxType::None,
                }
            }
            None => SafetyCheck::AskUser,
        }
    } else if policy == AskForApproval::Never {
        SafetyCheck::Reject {
            reason: "writing outside of the project; rejected by user approval settings"
                .to_string(),
        }
    } else {
        SafetyCheck::AskUser
    }
}

/// For a command to be run _without_ a sandbox, one of the following must be
/// true:
///
/// - the user has explicitly approved the command
/// - the command is on the "known safe" list
/// - `DangerFullAccess` was specified and `UnlessTrusted` was not
pub fn assess_command_safety(
    command: &[String],
    approval_policy: AskForApproval,
    sandbox_policy: &SandboxPolicy,
    approved: &HashSet<Vec<String>>,
    with_escalated_permissions: bool,
) -> SafetyCheck {
    // A command is "trusted" because either:
    // - it belongs to a set of commands we consider "safe" by default, or
    // - the user has explicitly approved the command for this session
    //
    // Currently, whether a command is "trusted" is a simple boolean, but we
    // should include more metadata on this command test to indicate whether it
    // should be run inside a sandbox or not. (This could be something the user
    // defines as part of `execpolicy`.)
    //
    // For example, when `is_known_safe_command(command)` returns `true`, it
    // would probably be fine to run the command in a sandbox, but when
    // `approved.contains(command)` is `true`, the user may have approved it for
    // the session _because_ they know it needs to run outside a sandbox.
    if is_known_safe_command(command) || approved.contains(command) {
        return SafetyCheck::AutoApprove {
            sandbox_type: SandboxType::None,
        };
    }

    assess_safety_for_untrusted_command(approval_policy, sandbox_policy, with_escalated_permissions)
}

pub(crate) fn assess_safety_for_untrusted_command(
    approval_policy: AskForApproval,
    sandbox_policy: &SandboxPolicy,
    with_escalated_permissions: bool,
) -> SafetyCheck {
    use AskForApproval::*;
    use SandboxPolicy::*;

    match (approval_policy, sandbox_policy) {
        (UnlessTrusted, _) => {
            // Even though the user may have opted into DangerFullAccess,
            // they also requested that we ask for approval for untrusted
            // commands.
            SafetyCheck::AskUser
        }
        (OnFailure, DangerFullAccess)
        | (Never, DangerFullAccess)
        | (OnRequest, DangerFullAccess) => SafetyCheck::AutoApprove {
            sandbox_type: SandboxType::None,
        },
        (OnRequest, ReadOnly) | (OnRequest, WorkspaceWrite { .. }) => {
            if with_escalated_permissions {
                SafetyCheck::AskUser
            } else {
                match get_platform_sandbox() {
                    Some(sandbox_type) => SafetyCheck::AutoApprove { sandbox_type },
                    // Fall back to asking since the command is untrusted and
                    // we do not have a sandbox available
                    None => SafetyCheck::AskUser,
                }
            }
        }
        (Never, ReadOnly)
        | (Never, WorkspaceWrite { .. })
        | (OnFailure, ReadOnly)
        | (OnFailure, WorkspaceWrite { .. }) => {
            match get_platform_sandbox() {
                Some(sandbox_type) => SafetyCheck::AutoApprove { sandbox_type },
                None => {
                    if matches!(approval_policy, OnFailure) {
                        // Since the command is not trusted, even though the
                        // user has requested to only ask for approval on
                        // failure, we will ask the user because no sandbox is
                        // available.
                        SafetyCheck::AskUser
                    } else {
                        // We are in non-interactive mode and lack approval, so
                        // all we can do is reject the command.
                        SafetyCheck::Reject {
                            reason: "auto-rejected because command is not on trusted list"
                                .to_string(),
                        }
                    }
                }
            }
        }
    }
}

pub fn get_platform_sandbox() -> Option<SandboxType> {
    if cfg!(target_os = "macos") {
        Some(SandboxType::MacosSeatbelt)
    } else if cfg!(target_os = "linux") {
        Some(SandboxType::LinuxSeccomp)
    } else {
        None
    }
}

fn is_write_patch_constrained_to_writable_paths(
    action: &ApplyPatchAction,
    sandbox_policy: &SandboxPolicy,
    cwd: &Path,
) -> bool {
    // Early‑exit if there are no declared writable roots.
    let writable_roots = match sandbox_policy {
        SandboxPolicy::ReadOnly => {
            return false;
        }
        SandboxPolicy::DangerFullAccess => {
            return true;
        }
        SandboxPolicy::WorkspaceWrite { .. } => sandbox_policy.get_writable_roots_with_cwd(cwd),
    };

    // Normalize a path by removing `.` and resolving `..` without touching the
    // filesystem (works even if the file does not exist).
    fn normalize(path: &Path) -> Option<PathBuf> {
        let mut out = PathBuf::new();
        for comp in path.components() {
            match comp {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => { /* skip */ }
                other => out.push(other.as_os_str()),
            }
        }
        Some(out)
    }

    // Determine whether `path` is inside **any** writable root. Both `path`
    // and roots are converted to absolute, normalized forms before the
    // prefix check.
    let is_path_writable = |p: &PathBuf| {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            cwd.join(p)
        };
        let abs = match normalize(&abs) {
            Some(v) => v,
            None => return false,
        };

        writable_roots
            .iter()
            .any(|writable_root| writable_root.is_path_writable(&abs))
    };

    for (path, change) in action.changes() {
        match change {
            ApplyPatchFileChange::Add { .. } | ApplyPatchFileChange::Delete { .. } => {
                if !is_path_writable(path) {
                    return false;
                }
            }
            ApplyPatchFileChange::Update { move_path, .. } => {
                if !is_path_writable(path) {
                    return false;
                }
                if let Some(dest) = move_path
                    && !is_path_writable(dest)
                {
                    return false;
                }
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_writable_roots_constraint() {
        // Use a temporary directory as our workspace to avoid touching
        // the real current working directory.
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let parent = cwd.parent().unwrap().to_path_buf();

        // Helper to build a single‑entry patch that adds a file at `p`.
        let make_add_change = |p: PathBuf| ApplyPatchAction::new_add_for_test(&p, "".to_string());

        let add_inside = make_add_change(cwd.join("inner.txt"));
        let add_outside = make_add_change(parent.join("outside.txt"));

        // Policy limited to the workspace only; exclude system temp roots so
        // only `cwd` is writable by default.
        let policy_workspace_only = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        assert!(is_write_patch_constrained_to_writable_paths(
            &add_inside,
            &policy_workspace_only,
            &cwd,
        ));

        assert!(!is_write_patch_constrained_to_writable_paths(
            &add_outside,
            &policy_workspace_only,
            &cwd,
        ));

        // With the parent dir explicitly added as a writable root, the
        // outside write should be permitted.
        let policy_with_parent = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![parent.clone()],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };
        assert!(is_write_patch_constrained_to_writable_paths(
            &add_outside,
            &policy_with_parent,
            &cwd,
        ));
    }

    #[test]
    fn test_request_escalated_privileges() {
        // Should not be a trusted command
        let command = vec!["git commit".to_string()];
        let approval_policy = AskForApproval::OnRequest;
        let sandbox_policy = SandboxPolicy::ReadOnly;
        let approved: HashSet<Vec<String>> = HashSet::new();
        let request_escalated_privileges = true;

        let safety_check = assess_command_safety(
            &command,
            approval_policy,
            &sandbox_policy,
            &approved,
            request_escalated_privileges,
        );

        assert_eq!(safety_check, SafetyCheck::AskUser);
    }

    #[test]
    fn test_request_escalated_privileges_no_sandbox_fallback() {
        let command = vec!["git".to_string(), "commit".to_string()];
        let approval_policy = AskForApproval::OnRequest;
        let sandbox_policy = SandboxPolicy::ReadOnly;
        let approved: HashSet<Vec<String>> = HashSet::new();
        let request_escalated_privileges = false;

        let safety_check = assess_command_safety(
            &command,
            approval_policy,
            &sandbox_policy,
            &approved,
            request_escalated_privileges,
        );

        let expected = match get_platform_sandbox() {
            Some(sandbox_type) => SafetyCheck::AutoApprove { sandbox_type },
            None => SafetyCheck::AskUser,
        };
        assert_eq!(safety_check, expected);
    }
}

#[cfg(test)]
mod tests_more {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // Helper: build a WorkspaceWrite policy that only allows the workspace itself by default.
    fn workspace_only_policy() -> SandboxPolicy {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec\![],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        }
    }

    // Helper: builds a simple add-file patch for tests (path may be absolute or relative).
    fn make_add(path: PathBuf) -> ApplyPatchAction {
        ApplyPatchAction::new_add_for_test(&path, "".to_string())
    }

    #[test]
    fn assess_patch_safety_rejects_empty_patch() {
        // Empty patches must be rejected irrespective of policies.
        let cwd = Path::new("."); // not used for empty
        let policy = AskForApproval::OnRequest;
        let sandbox_policy = workspace_only_policy();

        // Construct an empty action. Use a minimal sequence: add and then delete to get empty?
        // Prefer the library helper if available; otherwise, rely on default.
        // If ApplyPatchAction implements Default (common), use it. If not, ensure a known empty.
        let empty = ApplyPatchAction::default();
        assert\!(empty.is_empty());

        let res = assess_patch_safety(&empty, policy, &sandbox_policy, cwd);
        assert_eq\!(
            res,
            SafetyCheck::Reject {
                reason: "empty patch".to_string()
            }
        );
    }

    #[test]
    fn assess_patch_safety_outside_workspace_policy_never_rejects() {
        // When a patch attempts to write outside writable roots and approval policy is Never,
        // it should be rejected with a specific message.
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let parent = cwd.parent().unwrap().to_path_buf();

        let outside = make_add(parent.join("outside.txt"));
        let policy_never = AskForApproval::Never;
        let sandbox_policy = workspace_only_policy();

        let res = assess_patch_safety(&outside, policy_never, &sandbox_policy, &cwd);
        assert_eq\!(
            res,
            SafetyCheck::Reject {
                reason: "writing outside of the project; rejected by user approval settings"
                    .to_string()
            }
        );
    }

    #[test]
    fn assess_patch_safety_constrained_patch_autosandbox_or_ask() {
        // For a patch constrained to writable paths (workspace-only), assess_patch_safety
        // should auto-approve with a sandbox when available; otherwise AskUser.
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();

        let inside = make_add(cwd.join("inner.txt"));
        let policy = AskForApproval::OnRequest;
        let sandbox_policy = workspace_only_policy();

        let res = assess_patch_safety(&inside, policy, &sandbox_policy, &cwd);

        let expected = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AskUser,
        };
        assert_eq\!(res, expected);
    }

    #[test]
    fn assess_patch_safety_on_failure_policy_prefers_auto_approve() {
        // With AskForApproval::OnFailure, we should auto-approve if a sandbox exists.
        // If no sandbox exists, we auto-approve for DangerFullAccess or ask user otherwise.
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let inside = make_add(cwd.join("inner.txt"));

        // WorkspaceWrite
        let res_workspace = assess_patch_safety(
            &inside,
            AskForApproval::OnFailure,
            &workspace_only_policy(),
            &cwd,
        );
        let expected_workspace = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AskUser,
        };
        assert_eq\!(res_workspace, expected_workspace);

        // DangerFullAccess has a special case: if no sandbox exists, still auto-approve None.
        let res_danger = assess_patch_safety(
            &inside,
            AskForApproval::OnFailure,
            &SandboxPolicy::DangerFullAccess,
            &cwd,
        );
        let expected_danger = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AutoApprove {
                sandbox_type: SandboxType::None,
            },
        };
        assert_eq\!(res_danger, expected_danger);
    }

    #[test]
    fn is_write_patch_constrained_to_writable_paths_normalizes_paths() {
        // Relative path with ./ and .. should normalize and still be considered inside workspace.
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();

        // Create a nested path: ./a/../b/./c.txt relative to cwd
        let rel = PathBuf::from("./a/../b/./c.txt");
        let add_rel = make_add(rel);

        assert\!(is_write_patch_constrained_to_writable_paths(
            &add_rel,
            &workspace_only_policy(),
            &cwd
        ));
    }

    #[test]
    fn assess_command_safety_trusted_when_approved_explicitly() {
        // Commands explicitly approved should auto-approve without sandbox.
        let command = vec\!["some-binary".to_string(), "--flag".to_string()];
        let mut approved: HashSet<Vec<String>> = HashSet::new();
        approved.insert(command.clone());

        let res = assess_command_safety(
            &command,
            AskForApproval::OnRequest,
            &SandboxPolicy::ReadOnly,
            &approved,
            /*with_escalated_permissions=*/ false,
        );
        assert_eq\!(
            res,
            SafetyCheck::AutoApprove {
                sandbox_type: SandboxType::None
            }
        );
    }

    #[test]
    fn assess_safety_for_untrusted_unless_trusted_always_asks() {
        // Regardless of sandbox or danger policy, UnlessTrusted should ask.
        for sp in [
            SandboxPolicy::ReadOnly,
            SandboxPolicy::DangerFullAccess,
            workspace_only_policy(),
        ] {
            let res = assess_safety_for_untrusted_command(
                AskForApproval::UnlessTrusted,
                &sp,
                /*with_escalated_permissions=*/ false,
            );
            assert_eq\!(res, SafetyCheck::AskUser);
        }
    }

    #[test]
    fn assess_safety_for_untrusted_danger_full_access_autoapprove_none() {
        // DangerFullAccess auto-approves without sandbox for multiple approval policies.
        for ap in [
            AskForApproval::OnFailure,
            AskForApproval::Never,
            AskForApproval::OnRequest,
        ] {
            let res = assess_safety_for_untrusted_command(
                ap,
                &SandboxPolicy::DangerFullAccess,
                /*with_escalated_permissions=*/ false,
            );
            assert_eq\!(
                res,
                SafetyCheck::AutoApprove {
                    sandbox_type: SandboxType::None
                }
            );
        }
    }

    #[test]
    fn assess_safety_for_untrusted_onrequest_readonly_escalated_asks() {
        let res = assess_safety_for_untrusted_command(
            AskForApproval::OnRequest,
            &SandboxPolicy::ReadOnly,
            /*with_escalated_permissions=*/ true,
        );
        assert_eq\!(res, SafetyCheck::AskUser);
    }

    #[test]
    fn assess_safety_for_untrusted_onrequest_readonly_without_escalation_sandbox_or_ask() {
        let res = assess_safety_for_untrusted_command(
            AskForApproval::OnRequest,
            &SandboxPolicy::ReadOnly,
            /*with_escalated_permissions=*/ false,
        );
        let expected = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AskUser,
        };
        assert_eq\!(res, expected);
    }

    #[test]
    fn assess_safety_for_untrusted_never_readonly_sandbox_or_reject() {
        let res = assess_safety_for_untrusted_command(
            AskForApproval::Never,
            &SandboxPolicy::ReadOnly,
            /*with_escalated_permissions=*/ false,
        );
        let expected = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::Reject {
                reason: "auto-rejected because command is not on trusted list".to_string(),
            },
        };
        assert_eq\!(res, expected);
    }

    #[test]
    fn assess_safety_for_untrusted_onfailure_readonly_sandbox_or_ask() {
        let res = assess_safety_for_untrusted_command(
            AskForApproval::OnFailure,
            &SandboxPolicy::ReadOnly,
            /*with_escalated_permissions=*/ false,
        );
        let expected = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AskUser,
        };
        assert_eq\!(res, expected);
    }

    #[test]
    fn assess_command_safety_untrusted_falls_back_to_policy_handling() {
        // A command neither known-safe nor pre-approved should defer to untrusted handling.
        let command = vec\!["definitely-not-safe".to_string()];
        let approved: HashSet<Vec<String>> = HashSet::new();

        let res = assess_command_safety(
            &command,
            AskForApproval::OnRequest,
            &SandboxPolicy::ReadOnly,
            &approved,
            /*with_escalated_permissions=*/ false,
        );
        // Mirrors assess_safety_for_untrusted_command for (OnRequest, ReadOnly, false)
        let expected = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AskUser,
        };
        assert_eq\!(res, expected);
    }
}
