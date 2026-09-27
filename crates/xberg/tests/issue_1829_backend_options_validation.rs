//! Regression test for https://github.com/xberg-io/xberg/issues/1829
//!
//! A candle backend's `backend_options` and a `paddle_ocr_config` override are parsed by the
//! backend on each page. The automatic OCR route keeps native text when a page fails, so an
//! invalid value there used to surface only as a warning. Configuration validation now runs the
//! same check before any page, so these cases extract a plain-text document: it needs no OCR,
//! and only the up-front check can reject it. ~keep

#![allow(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)] // ~keep: test/bench binaries print by design; org logging policy exempts tests
#![cfg(all(
    feature = "ocr",
    feature = "pdf",
    feature = "candle-trocr",
    feature = "candle-paddleocr-vl",
    paddle_ocr
))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use xberg::XbergError;
use xberg::core::config::{ExtractInput, ExtractionConfig, OcrConfig, OcrPipelineConfig, OcrPipelineStage};
use xberg::plugins::{OcrBackend, OcrBackendType, Plugin, register_ocr_backend, unregister_ocr_backend};

const PLAIN_TEXT: &str = "Plain text that needs no OCR.";

async fn extract_plain_text(ocr: OcrConfig) -> xberg::Result<xberg::ExtractionResult> {
    let config = ExtractionConfig {
        ocr: Some(ocr),
        ..Default::default()
    };
    xberg::extract(
        ExtractInput::from_bytes(
            PLAIN_TEXT.as_bytes().to_vec(),
            "text/plain",
            Some("gh1829.txt".to_string()),
        ),
        &config,
    )
    .await
}

fn expect_validation_error(result: xberg::Result<xberg::ExtractionResult>, needle: &str) {
    match result {
        Err(XbergError::Validation { message, .. }) => assert!(
            message.contains(needle),
            "the validation error must name the rejected setting ({needle}): {message}"
        ),
        Err(other) => panic!("expected a validation error naming {needle}, got {other:?}"),
        Ok(_) => panic!("an invalid {needle} must fail the extraction before any page runs"),
    }
}

fn pipeline_with_stage(stage: OcrPipelineStage) -> OcrPipelineConfig {
    OcrPipelineConfig {
        stages: vec![stage],
        quality_thresholds: Default::default(),
    }
}

fn stage(backend: &str) -> OcrPipelineStage {
    OcrPipelineStage {
        backend: backend.to_string(),
        priority: 100,
        language: None,
        tesseract_config: None,
        paddle_ocr_config: None,
        vlm_config: None,
        backend_options: None,
    }
}

#[tokio::test]
async fn should_fail_before_any_page_when_candle_backend_options_are_invalid() {
    let result = extract_plain_text(OcrConfig {
        backend: "candle-trocr".to_string(),
        backend_options: Some(serde_json::json!({"variant": "no-such-variant"})),
        ..Default::default()
    })
    .await;

    expect_validation_error(result, "candle-trocr backend_options");
}

#[tokio::test]
async fn should_fail_before_any_page_when_trocr_hf_revision_is_blank() {
    let result = extract_plain_text(OcrConfig {
        backend: "candle-trocr".to_string(),
        backend_options: Some(serde_json::json!({"hf_revision": "  "})),
        ..Default::default()
    })
    .await;

    expect_validation_error(result, "candle-trocr backend_options.hf_revision");
}

#[tokio::test]
async fn should_fail_before_any_page_when_a_pipeline_stage_has_invalid_candle_backend_options() {
    let result = extract_plain_text(OcrConfig {
        pipeline: Some(pipeline_with_stage(OcrPipelineStage {
            backend_options: Some(serde_json::json!({"model_id": "  "})),
            ..stage("candle-paddleocr-vl")
        })),
        ..Default::default()
    })
    .await;

    expect_validation_error(result, "candle-paddleocr-vl backend_options.model_id");
}

#[cfg(feature = "candle-glm-ocr")]
#[tokio::test]
async fn should_fail_before_any_page_when_glm_ocr_backend_options_are_invalid() {
    let result = extract_plain_text(OcrConfig {
        backend: "candle-glm-ocr".to_string(),
        backend_options: Some(serde_json::json!({"cache_dir": "  "})),
        ..Default::default()
    })
    .await;

    expect_validation_error(result, "candle-glm-ocr backend_options.cache_dir");
}

#[cfg(feature = "candle-deepseek-ocr")]
#[tokio::test]
async fn should_fail_before_any_page_when_deepseek_ocr_backend_options_are_invalid() {
    let result = extract_plain_text(OcrConfig {
        backend: "candle-deepseek-ocr".to_string(),
        backend_options: Some(serde_json::json!({"version": 3})),
        ..Default::default()
    })
    .await;

    expect_validation_error(result, "candle-deepseek-ocr backend_options.version");
}

#[tokio::test]
async fn should_fail_before_any_page_when_paddle_ocr_config_is_invalid() {
    let result = extract_plain_text(OcrConfig {
        paddle_ocr_config: Some(serde_json::json!({"det_db_thresh": "not a number"})),
        ..Default::default()
    })
    .await;

    expect_validation_error(result, "paddle_ocr_config");
}

#[tokio::test]
async fn should_fail_before_any_page_when_a_pipeline_stage_has_an_invalid_paddle_ocr_config() {
    let result = extract_plain_text(OcrConfig {
        pipeline: Some(pipeline_with_stage(OcrPipelineStage {
            paddle_ocr_config: Some(serde_json::json!({"use_angle_cls": "yes"})),
            ..stage("paddle-ocr")
        })),
        ..Default::default()
    })
    .await;

    expect_validation_error(result, "paddle_ocr_config");
}

#[tokio::test]
async fn should_extract_when_backend_options_and_paddle_ocr_config_are_valid() {
    let result = extract_plain_text(OcrConfig {
        backend: "candle-trocr".to_string(),
        backend_options: Some(serde_json::json!({"variant": "large-printed", "cache_dir": "/tmp/models"})),
        paddle_ocr_config: Some(serde_json::json!({"det_db_thresh": 0.4, "use_angle_cls": false})),
        ..Default::default()
    })
    .await
    .expect("valid backend options must not fail validation");

    assert!(
        result.results.iter().any(|doc| doc.content.contains(PLAIN_TEXT)),
        "the document must still be extracted: {:?}",
        result.results.first().map(|doc| doc.content.clone())
    );
}

/// A page the OCR decode rejects under a security limit is a per-page failure, not a
/// configuration error: the automatic route must still return the native text with a warning.
/// The limit is set on `OcrConfig` only, so the render runs under the default limit and only
/// the OCR decode of the scanned page can reject it. ~keep
#[tokio::test]
async fn should_keep_native_text_when_a_security_limit_rejects_a_scanned_page() {
    const REJECTING_MAX_CONTENT_SIZE: usize = 5_000;
    let config = ExtractionConfig {
        ocr: Some(OcrConfig {
            backend: "tesseract".to_string(),
            security_limits: Some(xberg::extractors::security::SecurityLimits {
                max_content_size: REJECTING_MAX_CONTENT_SIZE,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = extract_mixed_native_scanned_pdf(&config)
        .await
        .expect("a per-page security-limit rejection must not fail the extraction");

    expect_native_text_with_ocr_warning(&result, &REJECTING_MAX_CONTENT_SIZE.to_string());
}

const REJECTING_BACKEND: &str = "gh1829-rejecting-backend";
const REJECTING_BACKEND_MESSAGE: &str = "gh1829 stub rejects this page";

/// A backend whose every page call fails with a validation error, as a custom plugin backend
/// does when it checks its own options on each page.
struct RejectingOcrBackend {
    called: Arc<AtomicBool>,
}

impl Plugin for RejectingOcrBackend {
    fn name(&self) -> &str {
        REJECTING_BACKEND
    }

    fn version(&self) -> String {
        "1.0.0".to_string()
    }

    fn initialize(&self) -> xberg::Result<()> {
        Ok(())
    }

    fn shutdown(&self) -> xberg::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl OcrBackend for RejectingOcrBackend {
    async fn process_image(&self, _image_bytes: &[u8], _config: &OcrConfig) -> xberg::Result<xberg::ExtractedDocument> {
        self.called.store(true, Ordering::SeqCst);
        Err(XbergError::Validation {
            message: REJECTING_BACKEND_MESSAGE.to_string(),
            source: None,
        })
    }

    fn supports_language(&self, _language: &str) -> bool {
        true
    }

    fn backend_type(&self) -> OcrBackendType {
        OcrBackendType::Custom
    }
}

/// A page error of the validation kind is still a per-page failure on the automatic route:
/// the native text comes back with a warning, whatever kind of error the backend returns. ~keep
#[tokio::test]
async fn should_keep_native_text_when_a_backend_rejects_a_page_with_a_validation_error() {
    let called = Arc::new(AtomicBool::new(false));
    let _ = unregister_ocr_backend(REJECTING_BACKEND);
    register_ocr_backend(Arc::new(RejectingOcrBackend {
        called: Arc::clone(&called),
    }))
    .expect("the stub backend must register");
    let config = ExtractionConfig {
        ocr: Some(OcrConfig {
            backend: REJECTING_BACKEND.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = extract_mixed_native_scanned_pdf(&config).await;
    let _ = unregister_ocr_backend(REJECTING_BACKEND);

    assert!(called.load(Ordering::SeqCst), "the scanned page must reach the backend");
    let result = result.expect("a validation error on one page must not fail the extraction");
    expect_native_text_with_ocr_warning(&result, REJECTING_BACKEND_MESSAGE);
}

async fn extract_mixed_native_scanned_pdf(config: &ExtractionConfig) -> xberg::Result<xberg::ExtractionResult> {
    let name = "mixed_native_scanned.pdf";
    let bytes = std::fs::read(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ocr")
            .join(name),
    )
    .expect("fixture must exist");
    xberg::extract(
        ExtractInput::from_bytes(bytes, "application/pdf", Some(name.to_string())),
        config,
    )
    .await
}

fn expect_native_text_with_ocr_warning(result: &xberg::ExtractionResult, needle: &str) {
    let doc = result.results.first().expect("one document");
    assert!(
        !doc.content.trim().is_empty(),
        "the native text must be returned when OCR of a page fails"
    );
    assert!(
        doc.processing_warnings
            .iter()
            .any(|warning| warning.message.contains(needle)),
        "the page failure must surface as a warning citing {needle}: {:?}",
        doc.processing_warnings
    );
}
