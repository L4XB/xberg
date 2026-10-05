//! An XLSX date, time or duration cell must read as the sheet shows it.
//! calamine reports all three as `DateTime`, and rendering every one as a
//! timestamp put a time of day on 1899-12-31, added `00:00:00` to a date, and
//! turned a 36-hour `[h]:mm` duration into `1900-01-01 12:00:00`.

#![allow(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)] // ~keep: test/bench binaries print by design; org logging policy exempts tests
#![cfg(feature = "excel")]

mod helpers;
use helpers::extract_bytes_document_blocking;

use xberg::core::config::ExtractionConfig;

const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

#[test]
fn date_time_and_duration_cells_read_as_the_sheet_shows_them() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/office/date_time_cells.xlsx");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipping: fixture not present at {path:?}");
        return;
    };
    let doc = extract_bytes_document_blocking(&bytes, XLSX_MIME, &ExtractionConfig::default())
        .expect("extraction must succeed");
    let table = doc.tables.first().expect("a table must be extracted");
    let value = |kind: &str| -> String {
        table
            .cells
            .iter()
            .find(|row| row.first().map(String::as_str) == Some(kind))
            .and_then(|row| row.get(1).cloned())
            .unwrap_or_else(|| panic!("row {kind:?} missing from {:?}", table.cells))
    };

    // Each row's number format: yyyy-mm-dd, hh:mm, h:mm AM/PM, yyyy-mm-dd hh:mm:ss, [h]:mm, [mm]:ss.
    assert_eq!(value("date"), "2024-09-30");
    assert_eq!(value("time"), "13:45:00");
    assert_eq!(value("time 12h"), "13:45:00");
    assert_eq!(value("datetime"), "2024-09-30 13:45:07");
    assert_eq!(value("duration hours"), "36:00:00");
    assert_eq!(value("duration minutes"), "1:30:00");
}
