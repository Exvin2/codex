
#[cfg(test)]
mod more_tests {
    // Framework note:
    // - Using Rust's built-in test harness (cargo test) with #[test].
    // - Using tempfile::TempDir for isolated filesystem state (already used elsewhere).
    // - No new dependencies introduced.

    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_add(path: &Path) -> ApplyPatchAction {
        // Mirrors existing test helper usage: new_add_for_test
        ApplyPatchAction::new_add_for_test(path, "".to_string())
    }

    #[test]
    fn assess_patch_safety_constrained_with_sandbox_autoapprove() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();

        let add_inside = make_add(&cwd.join("ok.txt"));

        let policy = AskForApproval::OnRequest;
        let sandbox_policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec\![],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let result = assess_patch_safety(&add_inside, policy, &sandbox_policy, &cwd);
        let expected = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AskUser,
        };
        assert_eq\!(result, expected);
    }

    #[test]
    fn assess_patch_safety_constrained_no_sandbox_but_danger_allows_autoapprove() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let add_inside = make_add(&cwd.join("ok.txt"));

        let policy = AskForApproval::OnRequest;
        let sandbox_policy = SandboxPolicy::DangerFullAccess;

        let result = assess_patch_safety(&add_inside, policy, &sandbox_policy, &cwd);
        let expected = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AutoApprove { sandbox_type: SandboxType::None },
        };
        assert_eq\!(result, expected);
    }

    #[test]
    fn assess_patch_safety_unconstrained_never_rejects() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let parent = cwd.parent().unwrap().to_path_buf();

        let add_outside = make_add(&parent.join("outside.txt"));

        let policy_never = AskForApproval::Never;
        let workspace_only = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec\![],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let result = assess_patch_safety(&add_outside, policy_never, &workspace_only, &cwd);
        assert_eq\!(
            result,
            SafetyCheck::Reject {
                reason: "writing outside of the project; rejected by user approval settings".to_string()
            }
        );
    }

    #[test]
    fn is_write_patch_constrained_handles_relative_and_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();

        // Relative paths that normalize into the workspace
        let p1 = PathBuf::from("./rel.txt");
        let p2 = PathBuf::from("a/../b/./../in.txt");
        let p3 = PathBuf::from("./dir/.././ok.md");

        let a1 = make_add(&p1);
        let a2 = make_add(&p2);
        let a3 = make_add(&p3);

        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec\![],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        assert\!(is_write_patch_constrained_to_writable_paths(&a1, &policy, &cwd));
        assert\!(is_write_patch_constrained_to_writable_paths(&a2, &policy, &cwd));
        assert\!(is_write_patch_constrained_to_writable_paths(&a3, &policy, &cwd));
    }

    #[test]
    fn is_write_patch_constrained_readonly_is_false_and_danger_is_true() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let inside = make_add(&cwd.join("x"));

        let readonly = SandboxPolicy::ReadOnly;
        assert\!(\!is_write_patch_constrained_to_writable_paths(&inside, &readonly, &cwd));

        let danger = SandboxPolicy::DangerFullAccess;
        assert\!(is_write_patch_constrained_to_writable_paths(&inside, &danger, &cwd));
    }

    #[test]
    fn assess_command_safety_user_approved_bypasses_sandbox() {
        let cmd = vec\!["custom".into(), "op".into()];
        let approval_policy = AskForApproval::OnRequest;
        let sandbox_policy = SandboxPolicy::ReadOnly;

        let mut approved = HashSet::new();
        approved.insert(cmd.clone());

        let res = assess_command_safety(&cmd, approval_policy, &sandbox_policy, &approved, false);
        assert_eq\!(
            res,
            SafetyCheck::AutoApprove { sandbox_type: SandboxType::None }
        );
    }

    #[test]
    fn assess_command_safety_untrusted_dangerfullaccess_combinations() {
        let cmd = vec\!["some".into(), "tool".into()];
        let approved = HashSet::new();

        for approval_policy in [
            AskForApproval::OnFailure,
            AskForApproval::Never,
            AskForApproval::OnRequest,
        ] {
            let res = assess_command_safety(
                &cmd,
                approval_policy,
                &SandboxPolicy::DangerFullAccess,
                &approved,
                false,
            );
            assert_eq\!(
                res,
                SafetyCheck::AutoApprove { sandbox_type: SandboxType::None },
                "Expected AutoApprove(None) for {:?} + DangerFullAccess",
                approval_policy
            );
        }
    }

    #[test]
    fn assess_command_safety_escalated_requires_prompt() {
        let cmd = vec\!["needs".into(), "sudo?".into()];
        let approved = HashSet::new();

        // With escalated permissions requested, OnRequest + WorkspaceWrite should AskUser
        let res = assess_command_safety(
            &cmd,
            AskForApproval::OnRequest,
            &SandboxPolicy::WorkspaceWrite {
                writable_roots: vec\![],
                network_access: false,
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            },
            &approved,
            true,
        );
        assert_eq\!(res, SafetyCheck::AskUser);
    }

    #[test]
    fn assess_command_safety_untrusted_onfailure_without_sandbox_asks_user() {
        let cmd = vec\!["untrusted".into()];
        let approved = HashSet::new();

        let res = assess_command_safety(
            &cmd,
            AskForApproval::OnFailure,
            &SandboxPolicy::ReadOnly,
            &approved,
            false,
        );
        let expected = match get_platform_sandbox() {
            Some(s) => SafetyCheck::AutoApprove { sandbox_type: s },
            None => SafetyCheck::AskUser,
        };
        assert_eq\!(res, expected);
    }

    #[test]
    fn assess_command_safety_untrusted_never_without_sandbox_rejects() {
        let cmd = vec\!["untrusted".into()];
        let approved = HashSet::new();

        let res = assess_command_safety(
            &cmd,
            AskForApproval::Never,
            &SandboxPolicy::ReadOnly,
            &approved,
            false,
        );

        match get_platform_sandbox() {
            Some(s) => assert_eq\!(res, SafetyCheck::AutoApprove { sandbox_type: s }),
            None => assert_eq\!(
                res,
                SafetyCheck::Reject {
                    reason: "auto-rejected because command is not on trusted list".to_string()
                }
            ),
        }
    }

    #[test]
    fn assess_command_safety_unless_trusted_always_asks_for_untrusted() {
        let cmd = vec\!["not-approved".into()];
        let approved = HashSet::new();

        let res = assess_command_safety(
            &cmd,
            AskForApproval::UnlessTrusted,
            &SandboxPolicy::DangerFullAccess,
            &approved,
            false,
        );
        assert_eq\!(res, SafetyCheck::AskUser);
    }
}