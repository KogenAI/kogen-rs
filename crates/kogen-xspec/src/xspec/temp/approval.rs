use super::{TempProject, safe_slug};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(in crate::xspec) struct ApprovalSummary {
    pub n: u64,
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
            let Some(separator) = row.iter().position(|byte| *byte == 0) else {
                continue;
            };
            let Ok(ref_name) = std::str::from_utf8(&row[..separator]) else {
                continue;
            };
            let Some(slug) = ref_name.strip_prefix("refs/kogen/intents/") else {
                continue;
            };
            if !wanted.contains(slug) || safe_slug(slug).is_err() {
                continue;
            }
            let Ok(tip) = std::str::from_utf8(&row[separator + 1..]) else {
                continue;
            };
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
        let Ok(bytes) = self.origin.output(&["show", &object]) else {
            return Ok(None);
        };
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("approval blob is not JSON: {error}"))?;
        Ok(Some(ApprovalSummary {
            n: count,
            by: get_string(&document, "by").unwrap_or_default().to_owned(),
            feasibility: get_string(&document, "feasibility")
                .unwrap_or("not checked")
                .to_owned(),
        }))
    }
}

fn get_string<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}
