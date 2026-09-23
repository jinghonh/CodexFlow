use codexflow_domain::{
    EvidenceField, FactKind, FactOutcome, HistoryItem, SourceEvidence, SourceFact,
};
use sha2::{Digest, Sha256};

pub const RULE_VERSION: &str = "structured-facts-v1";

fn stable_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub fn content_digest(items: &[HistoryItem]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(RULE_VERSION);
    for item in items {
        for part in [
            &item.thread_id,
            &item.turn_id,
            &item.id,
            &item.content_version,
        ] {
            hasher.update((part.len() as u64).to_be_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.update(item.ordinal.to_be_bytes());
        hasher.update(item.source_updated_at.to_be_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn outcome(item: &HistoryItem) -> FactOutcome {
    if item.status.as_deref() == Some("failed") {
        return FactOutcome::Failed;
    }
    if item.source_type == "commandExecution" {
        return match item.exit_code {
            Some(0) => FactOutcome::Succeeded,
            Some(_) => FactOutcome::Failed,
            None => FactOutcome::Unknown,
        };
    }
    match item.status.as_deref() {
        Some("completed") => FactOutcome::Succeeded,
        _ => FactOutcome::Unknown,
    }
}

fn branch_operation(command: &str) -> Option<(&'static str, &str)> {
    let words: Vec<&str> = command.split_ascii_whitespace().collect();
    let safe_branch = |branch: &str| {
        !branch.is_empty()
            && !branch.starts_with('-')
            && branch.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/')
            })
    };
    match words.as_slice() {
        ["git", "switch", "-c", branch] | ["git", "checkout", "-b", branch]
            if safe_branch(branch) =>
        {
            Some(("createdAndSwitched", branch))
        }
        ["git", "branch", branch] if safe_branch(branch) => Some(("created", branch)),
        _ => None,
    }
}

fn append(
    facts: &mut Vec<SourceFact>,
    evidence: &mut Vec<SourceEvidence>,
    item: &HistoryItem,
    kind: FactKind,
    subject: &str,
    operation: &str,
    result: FactOutcome,
    field: EvidenceField,
    change_index: Option<u32>,
    excerpt: &str,
) {
    let index = change_index
        .map(|value| value.to_string())
        .unwrap_or_default();
    let category = format!("{kind:?}");
    let id = stable_id(&[
        &item.thread_id,
        &item.turn_id,
        &item.id,
        &category,
        &index,
        operation,
        subject,
    ]);
    let evidence_id = stable_id(&[&id, "evidence"]);
    facts.push(SourceFact {
        id: id.clone(),
        thread_id: item.thread_id.clone(),
        turn_id: item.turn_id.clone(),
        item_id: item.id.clone(),
        kind,
        subject: subject.into(),
        operation: operation.into(),
        outcome: result,
        evidence_id: evidence_id.clone(),
        content_version: item.content_version.clone(),
        rule_version: RULE_VERSION.into(),
    });
    evidence.push(SourceEvidence {
        id: evidence_id,
        fact_id: id,
        thread_id: item.thread_id.clone(),
        turn_id: item.turn_id.clone(),
        item_id: item.id.clone(),
        field,
        change_index,
        excerpt: excerpt.chars().take(240).collect(),
        content_version: item.content_version.clone(),
    });
}

pub fn extract(items: &[HistoryItem]) -> (Vec<SourceFact>, Vec<SourceEvidence>) {
    let mut facts = Vec::new();
    let mut evidence = Vec::new();
    for item in items {
        match item.source_type.as_str() {
            "commandExecution" => {
                let Some(command) = item.command.as_deref().filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let result = outcome(item);
                append(
                    &mut facts,
                    &mut evidence,
                    item,
                    FactKind::Command,
                    command,
                    "executed",
                    result,
                    EvidenceField::Command,
                    None,
                    command,
                );
                if result == FactOutcome::Succeeded {
                    if let Some((operation, branch)) = branch_operation(command) {
                        append(
                            &mut facts,
                            &mut evidence,
                            item,
                            FactKind::Branch,
                            branch,
                            operation,
                            result,
                            EvidenceField::Command,
                            None,
                            command,
                        );
                    }
                }
            }
            "fileChange" => {
                for (index, change) in item.changes.iter().enumerate() {
                    if change.path.is_empty() {
                        continue;
                    }
                    let result = outcome(item);
                    append(
                        &mut facts,
                        &mut evidence,
                        item,
                        FactKind::File,
                        &change.path,
                        &change.kind,
                        result,
                        EvidenceField::ChangePath,
                        Some(index as u32),
                        &change.path,
                    );
                    if result == FactOutcome::Succeeded
                        && matches!(
                            change.kind.to_ascii_lowercase().as_str(),
                            "add" | "added" | "create" | "created"
                        )
                    {
                        append(
                            &mut facts,
                            &mut evidence,
                            item,
                            FactKind::Artifact,
                            &change.path,
                            "created",
                            result,
                            EvidenceField::ChangePath,
                            Some(index as u32),
                            &change.path,
                        );
                    }
                }
            }
            _ => {} // Natural-language plans and unknown source types never create operation facts.
        }
    }
    (facts, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::HistoryFileChange;

    fn item(turn: &str, id: &str, source_type: &str) -> HistoryItem {
        HistoryItem {
            thread_id: "thread".into(),
            turn_id: turn.into(),
            id: id.into(),
            ordinal: 0,
            source_type: source_type.into(),
            supported: true,
            text: None,
            command: None,
            cwd: None,
            output: None,
            exit_code: None,
            status: None,
            changes: vec![],
            source_updated_at: 1,
            content_version: "v1".into(),
        }
    }

    #[test]
    fn only_structured_operations_become_facts_and_unknown_results_stay_unknown() {
        let mut plan = item("turn-1", "plan", "plan");
        plan.text = Some("计划修改 src/lib.rs 并执行 git switch -c topic".into());
        let mut command = item("turn-1", "command", "commandExecution");
        command.command = Some("cargo test".into());
        let mut failed = item("turn-1", "failed", "commandExecution");
        failed.command = Some("git switch -c broken".into());
        failed.exit_code = Some(1);
        failed.status = Some("failed".into());
        let mut branch = item("turn-2", "command", "commandExecution");
        branch.command = Some("git switch -c topic".into());
        branch.exit_code = Some(0);
        branch.status = Some("completed".into());
        let mut file = item("turn-2", "file", "fileChange");
        file.status = Some("completed".into());
        file.changes = vec![HistoryFileChange {
            path: "src/lib.rs".into(),
            kind: "add".into(),
            diff: "+new".into(),
        }];
        let unknown = item("turn-2", "mystery", "newFutureType");
        let (facts, evidence) = extract(&[plan, command, failed, branch, file, unknown]);
        assert_eq!(facts.len(), 6); // unknown command, failed command, successful command + branch, file + artifact
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.kind == FactKind::Branch)
                .count(),
            1
        );
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.kind == FactKind::Artifact)
                .count(),
            1
        );
        assert_eq!(facts[0].outcome, FactOutcome::Unknown);
        assert_eq!(facts[1].outcome, FactOutcome::Failed);
        assert_eq!(facts[2].outcome, FactOutcome::Succeeded);
        assert!(facts
            .iter()
            .all(|fact| fact.item_id != "plan" && fact.item_id != "mystery"));
        assert_eq!(evidence.len(), facts.len());
        assert_ne!(facts[0].id, facts[2].id); // same item ID in another turn remains distinct
        assert_eq!(evidence[4].change_index, Some(0));
    }

    #[test]
    fn identity_is_stable_for_same_source_and_changes_digest_on_new_content() {
        let mut command = item("turn", "item", "commandExecution");
        command.command = Some("cargo test".into());
        command.exit_code = Some(0);
        let (first, _) = extract(&[command.clone()]);
        let (again, _) = extract(&[command.clone()]);
        assert_eq!(first[0].id, again[0].id);
        let old_digest = content_digest(&[command.clone()]);
        command.content_version = "v2".into();
        assert_ne!(old_digest, content_digest(&[command]));
    }
}
