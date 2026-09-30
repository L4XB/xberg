//! Regressions for external content-inspection findings (GH#1941).

#![cfg(all(feature = "redaction", feature = "tokio-runtime"))]

use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use xberg::engine::Engine;
use xberg::engine::seams::CacheBackend;
use xberg::text::redaction::parse_external_findings_bounded;
use xberg::{
    ExternalRedactionFinding, ExtractInput, ExtractedDocument, ExtractionConfig, RedactionConfig,
    RedactionOffsetEncoding, extract, extract_with_external_redaction, redact_external,
};

const MASK: &str = "[REDACTED]";

fn run_extraction_test<T, F>(future: F) -> T
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    std::thread::Builder::new()
        .name("external-redaction-test".to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(future)
        })
        .expect("test thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn document(content: &str) -> ExtractedDocument {
    let mut document = ExtractedDocument::default();
    document.content = content.to_string();
    document.mime_type = Cow::Borrowed("text/plain");
    document
}

fn text_finding(label: &str, text: &str) -> ExternalRedactionFinding {
    ExternalRedactionFinding {
        label: label.to_string(),
        text: Some(text.to_string()),
        ..Default::default()
    }
}

fn span_finding(label: &str, start: u32, end: u32) -> ExternalRedactionFinding {
    ExternalRedactionFinding {
        label: label.to_string(),
        start: Some(start),
        end: Some(end),
        ..Default::default()
    }
}

#[test]
fn should_preserve_the_exhaustive_redaction_config_struct_literal() {
    let _config = RedactionConfig {
        categories: Default::default(),
        strategy: Default::default(),
        ner: None,
        preserve_offsets: true,
        custom_terms: Vec::new(),
        custom_patterns: Vec::new(),
    };
}

#[tokio::test]
async fn should_redact_presidio_offsets_without_changing_core_config_shape() {
    let findings = parse_external_findings_bounded(r#"[{"entity_type":"PERSON","start":0,"end":11,"score":0.85}]"#, 10)
        .expect("Presidio output must parse");

    let redacted = redact_external(
        document("Zoë Quorlim called alice@example.com."),
        RedactionConfig::default(),
        findings,
        Some("unicode_code_points"),
        Some(10),
    )
    .await
    .expect("external redaction must succeed");

    assert_eq!(redacted.content, format!("{MASK} called {MASK}."));
}

#[tokio::test]
async fn should_default_the_limit_and_enforce_explicit_zero() {
    let redacted = redact_external(
        document("Zarnak"),
        RedactionConfig::default(),
        vec![text_finding("PERSON", "Zarnak")],
        None,
        None,
    )
    .await
    .expect("the documented default must accept one finding");
    assert_eq!(redacted.content, MASK);

    let error = redact_external(
        document("Zarnak"),
        RedactionConfig::default(),
        vec![text_finding("PERSON", "Zarnak")],
        Some("unicode_code_points"),
        Some(0),
    )
    .await
    .expect_err("an explicit zero limit must reject one finding");
    assert!(error.to_string().contains("maximum of 0"), "{error}");
}

#[tokio::test]
async fn should_default_to_utf8_offsets_and_reject_unknown_encodings() {
    let redacted = redact_external(
        document("Zoë Quorlim"),
        RedactionConfig::default(),
        vec![span_finding("PERSON", 0, 4)],
        None,
        Some(10),
    )
    .await
    .expect("omitted encoding must use UTF-8 byte offsets");
    assert_eq!(redacted.content, format!("{MASK} Quorlim"));

    let error = redact_external(
        document("Zarnak"),
        RedactionConfig::default(),
        vec![text_finding("PERSON", "Zarnak")],
        Some("bytes-ish"),
        Some(10),
    )
    .await
    .expect_err("unknown offset encodings must fail validation");
    assert!(
        error.to_string().contains("unsupported redaction offset encoding"),
        "{error}"
    );
}

#[test]
fn should_stop_parsing_at_the_finding_limit_before_a_later_malformed_entry() {
    let error = parse_external_findings_bounded(
        r#"[
            {"entity_type":"PERSON","text":"Zarnak"},
            {"entity_type":"CITY","text":"Quorlim"},
            not-json
        ]"#,
        1,
    )
    .expect_err("the second finding must exceed the limit before the third is parsed");

    assert!(error.to_string().contains("exceed"), "{error}");
    assert!(error.to_string().contains("1"), "{error}");
}

#[tokio::test]
async fn should_reject_an_unresolvable_span() {
    let error = redact_external(
        document("Zoë Quorlim called."),
        RedactionConfig::default(),
        vec![span_finding("PERSON", 0, 11)],
        Some("utf8_bytes"),
        Some(10),
    )
    .await
    .expect_err("a byte span cutting a word must fail");

    assert!(error.to_string().contains("offset_encoding"), "{error}");
    assert!(
        !error.to_string().contains("Quorl"),
        "document text leaked in error: {error}"
    );
}

#[tokio::test]
async fn should_apply_only_external_matchers_when_base_redaction_is_absent() {
    let output = extract_with_external_redaction(
        ExtractInput::from_bytes(
            b"Zarnak Quorlim emailed alice@example.com.".to_vec(),
            "text/plain",
            None,
        ),
        &ExtractionConfig::default(),
        vec![text_finding("PERSON", "Zarnak Quorlim")],
        Some("unicode_code_points"),
        Some(10),
    )
    .await
    .expect("extraction must succeed");

    assert_eq!(output.results[0].content, format!("{MASK} emailed alice@example.com."));
}

#[tokio::test]
async fn should_combine_external_findings_with_per_input_redaction() {
    let mut input = ExtractInput::from_bytes(b"Zarnak Quorlim met Blorp Nazzle.".to_vec(), "text/plain", None);
    input.config = Some(xberg::FileExtractionConfig {
        redaction: Some(RedactionConfig {
            custom_terms: vec![xberg::RedactionTerm::labeled("inspection_term", "Blorp Nazzle")],
            ..Default::default()
        }),
        ..Default::default()
    });

    let output = extract_with_external_redaction(
        input,
        &ExtractionConfig::default(),
        vec![text_finding("PERSON", "Zarnak Quorlim")],
        Some("unicode_code_points"),
        Some(10),
    )
    .await
    .expect("extraction must succeed");

    assert_eq!(output.results[0].content, format!("{MASK} met {MASK}."));
}

#[test]
fn should_isolate_concurrent_external_redaction_scopes() {
    run_extraction_test(async {
        let first = tokio::spawn(async {
            extract_with_external_redaction(
                ExtractInput::from_bytes(b"Zarnak Quorlim met Blorp Nazzle.".to_vec(), "text/plain", None),
                &ExtractionConfig::default(),
                vec![text_finding("PERSON", "Zarnak Quorlim")],
                Some("unicode_code_points"),
                Some(10),
            )
            .await
        });
        let second = tokio::spawn(async {
            extract_with_external_redaction(
                ExtractInput::from_bytes(b"Zarnak Quorlim met Blorp Nazzle.".to_vec(), "text/plain", None),
                &ExtractionConfig::default(),
                vec![text_finding("PERSON", "Blorp Nazzle")],
                Some("unicode_code_points"),
                Some(10),
            )
            .await
        });

        let (first, second) = tokio::join!(first, second);
        assert_eq!(
            first.expect("first task").expect("first extraction").results[0].content,
            format!("{MASK} met Blorp Nazzle.")
        );
        assert_eq!(
            second.expect("second task").expect("second extraction").results[0].content,
            format!("Zarnak Quorlim met {MASK}.")
        );

        let ordinary = extract(
            ExtractInput::from_bytes(b"Zarnak Quorlim met Blorp Nazzle.".to_vec(), "text/plain", None),
            &ExtractionConfig::default(),
        )
        .await
        .expect("ordinary extraction");
        assert_eq!(ordinary.results[0].content, "Zarnak Quorlim met Blorp Nazzle.");
    });
}

#[derive(Default)]
struct CountingCache {
    gets: AtomicUsize,
    puts: AtomicUsize,
    cached: Option<Vec<u8>>,
}

#[async_trait]
impl CacheBackend for CountingCache {
    async fn get(&self, _key: &str) -> Option<Vec<u8>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.cached.clone()
    }

    async fn put(&self, _key: &str, _value: Vec<u8>, _ttl: Option<std::time::Duration>) {
        self.puts.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn should_bypass_engine_and_extraction_caches_for_scoped_findings() {
    let cached = xberg::ExtractionResult::single(document("CACHED UNREDACTED RESULT"));
    let cache = Arc::new(CountingCache {
        cached: Some(serde_json::to_vec(&cached).expect("cached result JSON")),
        ..Default::default()
    });
    let engine = Engine::builder().with_cache_backend(cache.clone()).build();

    let output = engine
        .extract_with_external_redaction(
            ExtractInput::from_bytes(b"Zarnak Quorlim".to_vec(), "text/plain", None),
            &ExtractionConfig::default(),
            vec![text_finding("PERSON", "Zarnak Quorlim")],
            RedactionOffsetEncoding::UnicodeCodePoints,
            Some(10),
        )
        .await
        .expect("extraction must succeed");

    assert_eq!(output.results[0].content, MASK);
    assert_eq!(cache.gets.load(Ordering::SeqCst), 0);
    assert_eq!(cache.puts.load(Ordering::SeqCst), 0);

    let second = engine
        .extract_with_external_redaction(
            ExtractInput::from_bytes(b"Zarnak Quorlim".to_vec(), "text/plain", None),
            &ExtractionConfig::default(),
            vec![text_finding("PERSON", "Quorlim")],
            RedactionOffsetEncoding::UnicodeCodePoints,
            Some(10),
        )
        .await
        .expect("second extraction must succeed");
    assert_eq!(second.results[0].content, "Zarnak [REDACTED]");
    assert_eq!(cache.gets.load(Ordering::SeqCst), 0);
    assert_eq!(cache.puts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn should_fail_closed_when_the_late_processor_is_disabled() {
    let config = ExtractionConfig {
        postprocessor: Some(xberg::PostProcessorConfig {
            enabled: false,
            ..Default::default()
        }),
        ..Default::default()
    };
    let error = extract_with_external_redaction(
        ExtractInput::from_bytes(b"Zarnak Quorlim".to_vec(), "text/plain", None),
        &config,
        vec![text_finding("PERSON", "Zarnak Quorlim")],
        Some("unicode_code_points"),
        Some(10),
    )
    .await
    .expect_err("an unconsumed request must fail");

    assert!(
        error.to_string().contains("did not reach the Late processor"),
        "{error}"
    );
}

#[tokio::test]
async fn should_reject_uri_input_and_leave_no_scope_after_an_error() {
    let error = extract_with_external_redaction(
        ExtractInput::from_uri("document.txt"),
        &ExtractionConfig::default(),
        vec![text_finding("PERSON", "Zarnak")],
        Some("unicode_code_points"),
        Some(10),
    )
    .await
    .expect_err("URI input must be rejected");
    assert!(error.to_string().contains("one bytes input"), "{error}");

    let ordinary = extract(
        ExtractInput::from_bytes(b"Zarnak".to_vec(), "text/plain", None),
        &ExtractionConfig::default(),
    )
    .await
    .expect("ordinary extraction after error");
    assert_eq!(ordinary.results[0].content, "Zarnak");
}

#[tokio::test]
async fn should_leave_no_scope_after_cancellation() {
    let token = xberg::cancellation::CancellationToken::new();
    token.cancel();
    let config = ExtractionConfig {
        cancel_token: Some(token),
        ..Default::default()
    };
    extract_with_external_redaction(
        ExtractInput::from_bytes(b"Zarnak".to_vec(), "text/plain", None),
        &config,
        vec![text_finding("PERSON", "Zarnak")],
        Some("unicode_code_points"),
        Some(10),
    )
    .await
    .expect_err("cancelled extraction must fail");

    let ordinary = extract(
        ExtractInput::from_bytes(b"Zarnak".to_vec(), "text/plain", None),
        &ExtractionConfig::default(),
    )
    .await
    .expect("ordinary extraction after cancellation");
    assert_eq!(ordinary.results[0].content, "Zarnak");
}

#[test]
fn should_expose_send_futures_and_send_sync_request_types() {
    fn assert_send<T: Send>(_value: T) {}
    fn assert_send_sync<T: Send + Sync>() {}

    let config = ExtractionConfig::default();
    assert_send(extract_with_external_redaction(
        ExtractInput::from_bytes(b"Zarnak".to_vec(), "text/plain", None),
        &config,
        vec![text_finding("PERSON", "Zarnak")],
        Some("unicode_code_points"),
        Some(10),
    ));
    assert_send_sync::<ExternalRedactionFinding>();
    assert_send_sync::<RedactionOffsetEncoding>();
}

#[cfg(feature = "office")]
#[test]
fn should_redact_before_docx_output_is_encoded() {
    use base64::Engine as _;
    use std::io::Read;
    use xberg::OutputFormat;

    let config = ExtractionConfig {
        output_format: OutputFormat::Custom("docx".to_string()),
        ..Default::default()
    };
    let output = run_extraction_test(async move {
        extract_with_external_redaction(
            ExtractInput::from_bytes(b"Zarnak Quorlim signed.".to_vec(), "text/plain", None),
            &config,
            vec![text_finding("PERSON", "Zarnak Quorlim")],
            Some("unicode_code_points"),
            Some(10),
        )
        .await
        .expect("DOCX extraction must succeed")
    });
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&output.results[0].content)
        .expect("base64 DOCX");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("DOCX archive");
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .expect("document XML")
        .read_to_string(&mut xml)
        .expect("read document XML");

    assert!(!xml.contains("Zarnak Quorlim"), "secret remained in DOCX XML");
    assert!(xml.contains(MASK), "redaction token missing from DOCX XML");
}

#[cfg(feature = "pdf")]
#[test]
fn should_redact_before_pdf_output_is_encoded() {
    use base64::Engine as _;
    use xberg::OutputFormat;

    let extracted = run_extraction_test(async {
        let config = ExtractionConfig {
            output_format: OutputFormat::Custom("pdf".to_string()),
            ..Default::default()
        };
        let rendered = extract_with_external_redaction(
            ExtractInput::from_bytes(b"Zarnak Quorlim signed.".to_vec(), "text/plain", None),
            &config,
            vec![text_finding("PERSON", "Zarnak Quorlim")],
            Some("unicode_code_points"),
            Some(10),
        )
        .await
        .expect("PDF rendering");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&rendered.results[0].content)
            .expect("base64 PDF");
        extract(
            ExtractInput::from_bytes(bytes, "application/pdf", None),
            &ExtractionConfig::default(),
        )
        .await
        .expect("PDF render and extraction")
    });

    assert!(!extracted.results[0].content.contains("Zarnak Quorlim"));
    assert!(extracted.results[0].content.contains(MASK));
}
