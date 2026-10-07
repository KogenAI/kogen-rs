use super::{TempProject, safe_slug};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(in crate::xspec) struct ApprovalSummary {
    pub n: u64,
    pub sha: String,
    pub commit: String,
    pub base: String,
    pub by: String,
    pub feasibility: String,
}

impl TempProject {
    pub(in crate::xspec) fn approval_summaries(
        &self,
        slugs: &[&str],
    ) -> Result<BTreeMap<String, ApprovalSummary>, String> {
        let refs = self
            .origin
            .output(&[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)",
                "refs/kogen/intents",
            ])
            .map_err(|error| error.to_string())?;
        let wanted = slugs.iter().copied().collect::<BTreeSet<_>>();
        let mut summaries = BTreeMap::new();
        for row in refs
            .split(|byte| *byte == b'\n')
            .filter(|row| !row.is_empty())
        {
            let separator = row
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(|| "approval ref listing row has no object separator".to_owned())?;
            let ref_name = std::str::from_utf8(&row[..separator])
                .map_err(|error| format!("approval ref name is not UTF-8: {error}"))?;
            let Some(slug) = ref_name.strip_prefix("refs/kogen/intents/") else {
                continue;
            };
            if !wanted.contains(slug) || safe_slug(slug).is_err() {
                continue;
            }
            let tip = std::str::from_utf8(&row[separator + 1..])
                .map_err(|error| format!("approval ref target is not UTF-8: {error}"))?;
            if let Some(summary) = self.approval_summary_at(slug, tip)? {
                summaries.insert(slug.to_owned(), summary);
            }
        }
        Ok(summaries)
    }

    fn approval_summary_at(
        &self,
        slug: &str,
        tip: &str,
    ) -> Result<Option<ApprovalSummary>, String> {
        let messages = self
            .origin
            .output(&["log", "--format=%B%x00", tip])
            .map_err(|error| error.to_string())?;
        let marker = format!("Kogen-Approval: {slug}");
        let count = messages
            .split(|byte| *byte == 0)
            .flat_map(|message| message.split(|byte| *byte == b'\n'))
            .filter(|line| *line == marker.as_bytes())
            .count() as u64;
        let object = format!("{tip}:.kogen/intents/{slug}/approval.json");
        let bytes = match self.origin.output(&["show", &object]) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(None),
        };
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("approval blob is not JSON: {error}"))?;
        let documented_slug = required_string(&document, "slug")?;
        let sha = required_string(&document, "approval_sha256")?;
        let by = required_string(&document, "by")?;
        let base = required_string(&document, "base_sha")?;
        if documented_slug != slug {
            return Err(format!(
                "approval ref {slug} contains document for {documented_slug}"
            ));
        }
        if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("approval ref {slug} contains an invalid digest"));
        }
        if count == 0 {
            return Err(format!("approval ref {slug} has no Kogen-Approval commit"));
        }
        let feasibility = document
            .get("witness")
            .and_then(|witness| witness.get("verdict"))
            .and_then(serde_json::Value::as_str)
            .map(|value| match value {
                "PROVEN_WITH_CONCERNS" => "PROVEN with concerns".to_owned(),
                other => other.to_owned(),
            })
            .unwrap_or_else(|| "not checked".to_owned());
        Ok(Some(ApprovalSummary {
            n: count,
            sha: sha.to_owned(),
            commit: tip.to_owned(),
            base: base.to_owned(),
            by: by.to_owned(),
            feasibility,
        }))
    }
}

fn required_string<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("approval document is missing string field `{key}`"))
}
