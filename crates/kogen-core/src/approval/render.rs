use super::manifest::baseline_warning_lines;
use super::model::{BaselineRow, ShapeWarning};
use crate::intent::Intent;

pub(super) struct ApprovalCard<'a> {
    pub intent: &'a Intent,
    pub hash: &'a str,
    pub approver: &'a str,
    pub base: &'a str,
    pub base_sha: &'a str,
    pub feasibility: &'a str,
    pub warnings: &'a [ShapeWarning],
    pub baseline_warning: bool,
    pub baseline: &'a [BaselineRow],
}

pub(super) fn render_card(card: ApprovalCard<'_>) -> String {
    let ApprovalCard {
        intent,
        hash,
        approver,
        base,
        base_sha,
        feasibility,
        warnings,
        baseline_warning,
        baseline,
    } = card;
    let mut out = format!(
        "Intent: {} — {}\nSHA-256: {hash}\nApprover: {approver}\nBase: {base} at {base_sha}\nFeasibility: {feasibility}\n\nBrief\n",
        intent.slug, intent.frontmatter.title
    );
    let brief = intent
        .brief_lines
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>();
    for line in trim_blank_edges(&brief) {
        let line = line.trim_end();
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("\nAcceptance\n");
    for item in &intent.acceptance {
        let kind = intent
            .verify_for(&item.id)
            .map(|verify| {
                if verify.is_keep() {
                    "test keep"
                } else {
                    "test"
                }
            })
            .unwrap_or("test");
        out.push_str(&format!("  - [{}] {} ({kind})\n", item.id, item.text));
    }
    let warning_prefix = render_warning_prefix(baseline_warning, baseline, warnings);
    if !warning_prefix.is_empty() {
        out.push('\n');
        out.push_str(&warning_prefix);
        out.push('\n');
    } else {
        out.push('\n');
    }
    out.push_str("Approve with:\n");
    out.push_str(&format!(
        "  kogen intent approve {} {}\n",
        intent.slug,
        &hash[..8]
    ));
    out
}

fn trim_blank_edges<'a>(lines: &[&'a str]) -> impl Iterator<Item = &'a str> {
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(start, |index| index + 1);
    lines[start..end].iter().copied()
}

pub(super) fn render_warning_prefix(
    bwarn: bool,
    baseline: &[BaselineRow],
    warnings: &[ShapeWarning],
) -> String {
    let other_warnings = baseline
        .iter()
        .filter(|row| matches!(row.status.as_str(), "unavailable" | "timeout" | "mutating"))
        .collect::<Vec<_>>();
    if !bwarn && warnings.is_empty() && other_warnings.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if !warnings.is_empty() || !other_warnings.is_empty() {
        out.push_str("Warnings\n");
        for warning in warnings {
            let ids = if warning.item_ids.is_empty() {
                "-".to_owned()
            } else {
                warning.item_ids.join(", ")
            };
            out.push_str(&format!(
                "  - {}: {} — {}\n",
                warning.code, ids, warning.message
            ));
        }
        for row in other_warnings {
            out.push_str(&format!(
                "  - baseline_{}: {} — configured check did not pass on the base\n",
                row.status, row.name
            ));
        }
    }
    if bwarn {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("Warning: configured checks are already red on the base:\n");
        for line in baseline_warning_lines(baseline) {
            out.push_str(&line);
            out.push('\n');
        }
        out.push_str(
            "Hint: fix the base first, or scope the check, e.g. a changed-files format argv.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{ApprovalCard, render_card};
    use crate::intent::Intent;

    #[test]
    fn approval_card_keeps_paragraph_breaks_without_trailing_spaces() {
        let source = b"---\ntitle: Paragraphs\nsize: small\ndomains: [app]\n---\nFirst paragraph.  \n\nSecond paragraph.\n\n## Acceptance\n- A1: update the greeting in lib/greet.txt\n\n## Verify\n- A1: test\n";
        let intent = Intent::parse("tiers", source).expect("valid Intent");
        let hash = "a".repeat(64);
        let base_sha = "b".repeat(40);
        let rendered = render_card(ApprovalCard {
            intent: &intent,
            hash: &hash,
            approver: "Kogen Test <test@kogen.invalid>",
            base: "main",
            base_sha: &base_sha,
            feasibility: "not checked",
            warnings: &[],
            baseline_warning: false,
            baseline: &[],
        });

        assert!(rendered.contains("  First paragraph.\n\n  Second paragraph.\n"));
        assert!(
            rendered.lines().all(|line| !line.ends_with(' ')),
            "approval card contains a trailing space: {rendered:?}"
        );
    }
}
