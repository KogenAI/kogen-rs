use crate::ExitCode;
use crate::error::{CoreError, ErrorClass};
use crate::git::GitRepo;
use crate::intent::{Intent, approval_sha256, intent_sha256};
use crate::project::{ProjectResolution, valid_slug};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(super) struct ApprovedBuild {
    pub slug: String,
    pub commit: String,
    pub approval_sha256: String,
    pub target_branch: String,
    pub base_sha: String,
    pub intent_bytes: Vec<u8>,
    pub acceptance_path: String,
    pub acceptance_bytes: Vec<u8>,
    pub approval: Value,
    pub intent: Intent,
    pub approval_time: i64,
}

impl ApprovedBuild {
    pub fn load(project: &ProjectResolution, slug: &str) -> Result<Self, CoreError> {
        if !valid_slug(slug) {
            return Err(invalid_approval(slug, "invalid slug"));
        }
        let origin = GitRepo::new(&project.origin);
        let reference = format!("refs/kogen/intents/{slug}");
        let commit = origin
            .ref_target(&reference)
            .map_err(|error| invalid_approval(slug, error))?
            .ok_or_else(|| invalid_approval(slug, "approval ref is missing"))?;
        let root = format!(".kogen/intents/{slug}");
        let approval_bytes = origin
            .blob_at(&commit, &format!("{root}/approval.json"))
            .map_err(|error| invalid_approval(slug, error))?
            .ok_or_else(|| invalid_approval(slug, "approval document is missing"))?;
        let approval: Value = serde_json::from_slice(&approval_bytes)
            .map_err(|error| invalid_approval(slug, error))?;
        let intent_bytes = origin
            .blob_at(&commit, &format!("{root}/intent.md"))
            .map_err(|error| invalid_approval(slug, error))?
            .ok_or_else(|| invalid_approval(slug, "approved Intent is missing"))?;
        let acceptance_path = approval
            .get("acceptance_paths")
            .and_then(Value::as_array)
            .and_then(|paths| paths.first())
            .and_then(Value::as_str)
            .filter(|path| path.starts_with(".kogen/acceptance/") && !path.contains(".."))
            .ok_or_else(|| invalid_approval(slug, "acceptance path is invalid"))?
            .to_owned();
        let acceptance_bytes = origin
            .blob_at(&commit, &acceptance_path)
            .map_err(|error| invalid_approval(slug, error))?
            .ok_or_else(|| invalid_approval(slug, "approved acceptance test is missing"))?;
        let expected_hash = approval
            .get("approval_sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_approval(slug, "approval hash is missing"))?;
        if approval.get("schema").and_then(Value::as_u64) != Some(2)
            || approval.get("slug").and_then(Value::as_str) != Some(slug)
            || approval.get("intent_sha256").and_then(Value::as_str)
                != Some(intent_sha256(&intent_bytes).as_str())
            || approval_sha256(&intent_bytes, &acceptance_bytes) != expected_hash
        {
            return Err(invalid_approval(
                slug,
                "approval bytes do not match the document",
            ));
        }
        let intent =
            Intent::parse(slug, &intent_bytes).map_err(|error| invalid_approval(slug, error))?;
        if intent.acceptance.is_empty()
            || intent.acceptance.len() != intent.verify.len()
            || intent.verify.iter().any(|item| item.kind() != Some("test"))
        {
            return Err(invalid_approval(
                slug,
                "approved acceptance contract is invalid",
            ));
        }
        let target_branch = approval
            .get("target_branch")
            .and_then(Value::as_str)
            .filter(|branch| !branch.is_empty())
            .ok_or_else(|| invalid_approval(slug, "target branch is missing"))?
            .to_owned();
        let base_sha = approval
            .get("base_sha")
            .and_then(Value::as_str)
            .filter(|base| !base.is_empty())
            .ok_or_else(|| invalid_approval(slug, "approval base is missing"))?
            .to_owned();
        let approval_time = origin
            .text(&["show", "-s", "--format=%ct", &commit])
            .map_err(|error| invalid_approval(slug, error))?
            .parse()
            .unwrap_or_default();
        Ok(Self {
            slug: slug.to_owned(),
            commit,
            approval_sha256: expected_hash.to_owned(),
            target_branch,
            base_sha,
            intent_bytes,
            acceptance_path,
            acceptance_bytes,
            approval,
            intent,
            approval_time,
        })
    }

    pub fn protected_hashes(&self) -> Result<BTreeMap<String, String>, CoreError> {
        self.approval
            .get("protected_manifest")
            .and_then(Value::as_object)
            .map(|manifest| {
                manifest
                    .iter()
                    .map(|(path, hash)| {
                        hash.as_str()
                            .map(|hash| (path.clone(), hash.to_owned()))
                            .ok_or_else(|| {
                                invalid_approval(&self.slug, "protected manifest is invalid")
                            })
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                Err(invalid_approval(
                    &self.slug,
                    "protected manifest is missing",
                ))
            })
    }
}

fn invalid_approval(slug: &str, detail: impl std::fmt::Display) -> CoreError {
    CoreError::new(
        ErrorClass::Controller,
        "approval_invalid",
        format!("{slug}: {detail}"),
        ExitCode::Bug,
    )
}
