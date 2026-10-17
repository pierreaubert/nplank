//! Local answer drafts are scoped to a provider's exact pending request.

// Rust guideline compliant 2026-02-21
use crate::model::PendingApproval;
use sha2::{Digest, Sha256};

pub(crate) fn selection_key(session: &str, generation: u64, approval: &PendingApproval) -> String {
    let revision = Sha256::digest(approval.requirement.to_string().as_bytes());
    let revision: String = revision.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("question-{session}-{generation}-{}-{revision}", approval.id)
}

pub(crate) fn toggle(current: &str, choice: &str) -> String {
    if choice == "0" {
        return if current == "0" {
            String::new()
        } else {
            "0".into()
        };
    }
    let mut selected: Vec<_> = current
        .split(',')
        .filter(|id| !id.is_empty() && *id != "0")
        .collect();
    if selected.contains(&choice) {
        selected.retain(|id| *id != choice);
    } else {
        selected.push(choice);
    }
    selected.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decline_is_exclusive_and_options_can_be_toggled_off() {
        assert_eq!(toggle("1,2", "0"), "0");
        assert_eq!(toggle("0", "2"), "2");
        assert_eq!(toggle("1,2", "1"), "2");
        assert_eq!(toggle("0", "0"), "");
    }

    #[test]
    fn revised_question_and_new_generation_do_not_reuse_a_draft() {
        let mut approval = PendingApproval {
            id: "question".into(),
            requirement: serde_json::json!({"revision": "a"}),
            tool: String::new(),
            arguments: String::new(),
            document: None,
            choices: vec![],
        };
        let original = selection_key("session", 1, &approval);
        assert_ne!(original, selection_key("session", 2, &approval));
        approval.requirement["revision"] = "b".into();
        assert_ne!(original, selection_key("session", 1, &approval));
    }
}
