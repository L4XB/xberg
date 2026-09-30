//! Findings supplied by an external content-inspection engine.

use std::collections::{HashMap, HashSet};
#[cfg(feature = "tokio-runtime")]
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserializer;
use serde::de::{IgnoredAny, SeqAccess, Visitor};

use crate::core::config::redaction::{ExternalRedactionFinding, RedactionOffsetEncoding};
use crate::types::redaction::PiiCategory;
use crate::{Result, XbergError};

use super::engine::literal_regex;

pub(crate) const DEFAULT_MAX_FINDINGS: u32 = 10_000;

#[derive(Clone)]
pub(crate) struct ExternalRedactionRequest {
    pub(crate) findings: Arc<[ExternalRedactionFinding]>,
    pub(crate) offset_encoding: RedactionOffsetEncoding,
    pub(crate) max_findings: u32,
    pub(crate) include_configured_sources: bool,
    consumed: Arc<AtomicBool>,
}

impl ExternalRedactionRequest {
    #[cfg(feature = "tokio-runtime")]
    pub(crate) fn new(
        findings: Vec<ExternalRedactionFinding>,
        offset_encoding: RedactionOffsetEncoding,
        max_findings: u32,
        include_configured_sources: bool,
    ) -> Self {
        Self {
            findings: findings.into(),
            offset_encoding,
            max_findings,
            include_configured_sources,
            consumed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn mark_consumed(&self) {
        self.consumed.store(true, Ordering::Release);
    }

    #[cfg(feature = "tokio-runtime")]
    pub(crate) fn was_consumed(&self) -> bool {
        self.consumed.load(Ordering::Acquire)
    }
}

#[cfg(feature = "tokio-runtime")]
tokio::task_local! {
    static EXTERNAL_REDACTION_REQUEST: ExternalRedactionRequest;
}

#[cfg(feature = "tokio-runtime")]
pub(crate) async fn scope_external_redaction<F, T>(request: ExternalRedactionRequest, future: F) -> T
where
    F: Future<Output = T>,
{
    EXTERNAL_REDACTION_REQUEST.scope(request, future).await
}

#[cfg(feature = "tokio-runtime")]
pub(crate) fn current_external_redaction() -> Option<ExternalRedactionRequest> {
    EXTERNAL_REDACTION_REQUEST.try_with(Clone::clone).ok()
}

#[cfg(not(feature = "tokio-runtime"))]
pub(crate) fn current_external_redaction() -> Option<ExternalRedactionRequest> {
    None
}

#[cfg(feature = "tokio-runtime")]
pub(crate) fn external_redaction_is_scoped() -> bool {
    EXTERNAL_REDACTION_REQUEST.try_with(|_| ()).is_ok()
}

#[cfg(not(feature = "tokio-runtime"))]
pub(crate) fn external_redaction_is_scoped() -> bool {
    false
}

/// Parse at most `max_findings` findings from a JSON array or JSON Lines.
#[cfg_attr(alef, alef(skip))]
pub fn parse_external_findings_bounded(text: &str, max_findings: u32) -> Result<Vec<ExternalRedactionFinding>> {
    let max_findings = max_findings as usize;
    if text.trim_start().starts_with('[') {
        let mut deserializer = serde_json::Deserializer::from_str(text);
        let findings = deserializer
            .deserialize_seq(BoundedFindingsVisitor { max_findings })
            .map_err(|error| XbergError::validation(format!("redaction findings: invalid JSON array: {error}")))?;
        deserializer
            .end()
            .map_err(|error| XbergError::validation(format!("redaction findings: invalid JSON array: {error}")))?;
        return Ok(findings);
    }

    let mut findings = Vec::new();
    for (index, line) in text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty()) {
        if findings.len() == max_findings {
            return Err(finding_limit_error(max_findings));
        }
        findings.push(serde_json::from_str(line).map_err(|error| {
            XbergError::validation(format!(
                "redaction findings: invalid JSON on line {}: {error}",
                index + 1
            ))
        })?);
    }
    Ok(findings)
}

struct BoundedFindingsVisitor {
    max_findings: usize,
}

impl<'de> Visitor<'de> for BoundedFindingsVisitor {
    type Value = Vec<ExternalRedactionFinding>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "a JSON array with at most {} findings", self.max_findings)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let capacity = sequence.size_hint().unwrap_or(0).min(self.max_findings);
        let mut findings = Vec::with_capacity(capacity);
        loop {
            if findings.len() == self.max_findings {
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "findings exceed maximum of {}",
                        self.max_findings
                    )));
                }
                break;
            }
            match sequence.next_element()? {
                Some(finding) => findings.push(finding),
                None => break,
            }
        }
        Ok(findings)
    }
}

fn finding_limit_error(max_findings: usize) -> XbergError {
    XbergError::validation(format!("redaction findings exceed maximum of {max_findings}"))
}

pub(super) fn compile_external_findings(
    content: &str,
    findings: &[ExternalRedactionFinding],
    offset_encoding: RedactionOffsetEncoding,
    max_findings: u32,
) -> Result<Vec<(PiiCategory, regex::Regex)>> {
    if findings.len() > max_findings as usize {
        return Err(finding_limit_error(max_findings as usize));
    }
    for (index, finding) in findings.iter().enumerate() {
        finding.validate(&format!("external findings[{index}]"))?;
    }

    let mut wanted: Vec<usize> = findings
        .iter()
        .filter(|finding| finding.text.is_none())
        .flat_map(|finding| [finding.start, finding.end])
        .flatten()
        .map(|offset| offset as usize)
        .collect();
    let byte_offsets = byte_offsets(content, offset_encoding, &mut wanted);

    let mut compiled: HashMap<&str, regex::Regex> = HashMap::new();
    let mut seen: HashSet<(PiiCategory, &str)> = HashSet::new();
    let mut output = Vec::new();
    for (index, finding) in findings.iter().enumerate() {
        let location = format!("external findings[{index}]");
        let (literal, anchor) = resolve_literal(content, finding, &byte_offsets, offset_encoding, &location)?;
        let regex = match compiled.get(literal) {
            Some(regex) => regex.clone(),
            None => {
                let Some(regex) = literal_regex(literal) else {
                    return Err(XbergError::validation(format!(
                        "{location}: text cannot be compiled into a matcher"
                    )));
                };
                compiled.insert(literal, regex.clone());
                regex
            }
        };
        if let Some(anchor) = anchor
            && regex.find_at(content, anchor).map(|found| found.start()) != Some(anchor)
        {
            return Err(XbergError::validation(format!(
                "{location}: span {}..{} cuts a word under {}; check offset_encoding",
                finding.start.unwrap_or_default(),
                finding.end.unwrap_or_default(),
                encoding_name(offset_encoding)
            )));
        }
        let category = PiiCategory::Custom(finding.label.trim().to_string());
        if seen.insert((category.clone(), literal)) {
            output.push((category, regex));
        }
    }
    Ok(output)
}

fn resolve_literal<'a>(
    content: &'a str,
    finding: &'a ExternalRedactionFinding,
    byte_offsets: &HashMap<usize, usize>,
    offset_encoding: RedactionOffsetEncoding,
    location: &str,
) -> Result<(&'a str, Option<usize>)> {
    let (literal, anchor) = match (&finding.text, finding.start, finding.end) {
        (Some(text), _, _) => (text.trim(), None),
        (None, Some(start), Some(end)) => {
            let (Some(&byte_start), Some(&byte_end)) =
                (byte_offsets.get(&(start as usize)), byte_offsets.get(&(end as usize)))
            else {
                return Err(XbergError::validation(format!(
                    "{location}: span {start}..{end} does not fall on {} boundaries within content",
                    encoding_name(offset_encoding)
                )));
            };
            let span = &content[byte_start..byte_end];
            let leading = span.len() - span.trim_start().len();
            (span.trim(), Some(byte_start + leading))
        }
        _ => {
            return Err(XbergError::validation(format!(
                "{location}: needs either text or both start and end"
            )));
        }
    };
    if literal.is_empty() {
        return Err(XbergError::validation(format!("{location}: resolves to blank text")));
    }
    Ok((literal, anchor))
}

fn encoding_name(encoding: RedactionOffsetEncoding) -> &'static str {
    match encoding {
        RedactionOffsetEncoding::Utf8Bytes => "utf8_bytes",
        RedactionOffsetEncoding::UnicodeCodePoints => "unicode_code_points",
        RedactionOffsetEncoding::Utf16CodeUnits => "utf16_code_units",
    }
}

fn byte_offsets(content: &str, encoding: RedactionOffsetEncoding, wanted: &mut Vec<usize>) -> HashMap<usize, usize> {
    wanted.sort_unstable();
    wanted.dedup();
    let unit_len: fn(char) -> usize = match encoding {
        RedactionOffsetEncoding::Utf8Bytes => char::len_utf8,
        RedactionOffsetEncoding::UnicodeCodePoints => |_| 1,
        RedactionOffsetEncoding::Utf16CodeUnits => char::len_utf16,
    };

    let mut map = HashMap::with_capacity(wanted.len());
    let mut pending = wanted.iter().copied().peekable();
    let mut units = 0;
    let positions = content.char_indices().map(Some).chain([None]);
    for position in positions {
        let byte = position.map_or(content.len(), |(byte, _)| byte);
        while let Some(&offset) = pending.peek() {
            if offset > units {
                break;
            }
            if offset == units {
                map.insert(offset, byte);
            }
            pending.next();
        }
        match (position, pending.peek()) {
            (Some((_, character)), Some(_)) => units += unit_len(character),
            _ => break,
        }
    }
    map
}

#[cfg(all(test, feature = "tokio-runtime"))]
mod tests {
    use super::*;

    fn request(label: &str) -> ExternalRedactionRequest {
        ExternalRedactionRequest::new(
            vec![ExternalRedactionFinding {
                label: label.to_string(),
                text: Some(label.to_string()),
                ..Default::default()
            }],
            RedactionOffsetEncoding::UnicodeCodePoints,
            1,
            false,
        )
    }

    #[tokio::test]
    async fn nested_scopes_shadow_then_restore_the_outer_request() {
        let outer = request("outer");
        scope_external_redaction(outer, async {
            assert_eq!(current_external_redaction().unwrap().findings[0].label, "outer");
            scope_external_redaction(request("inner"), async {
                assert_eq!(current_external_redaction().unwrap().findings[0].label, "inner");
            })
            .await;
            assert_eq!(current_external_redaction().unwrap().findings[0].label, "outer");
        })
        .await;
        assert!(current_external_redaction().is_none());
    }

    #[tokio::test]
    async fn spawned_tasks_do_not_inherit_the_scope() {
        scope_external_redaction(request("outer"), async {
            let inherited = tokio::spawn(async { current_external_redaction().is_some() })
                .await
                .expect("spawned task");
            assert!(!inherited);
        })
        .await;
    }
}
