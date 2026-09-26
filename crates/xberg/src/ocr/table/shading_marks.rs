use crate::table_core::{HocrWord, detect_rows, find_row_index, median_height};

/// A word under this share of its row's median height is thin enough to be a mark. ~keep
const THIN_MARK_HEIGHT_RATIO: f64 = 0.25;

/// A word over this share of its row's median height is tall enough to be a mark. ~keep
const TALL_MARK_HEIGHT_RATIO: f64 = 1.8;

/// Tesseract's word confidence (0 to 100) under which a word of mark height is a mark. The marks
/// measured on a shaded table read at 0 to 28; the thin date-range dash of #1649 reads at 46. ~keep
const MARK_MAX_CONFIDENCE: f64 = 35.0;

/// Drop the marks Tesseract reads off the edge of a shaded table row, before table
/// reconstruction (xberg-io/xberg#1858).
///
/// Such a mark (a tall `=`, a thin dash) sits in the gap between two values, and the cell merge
/// then joins both values into one cell. A word is a mark when its height is far from the median
/// height of its row AND its confidence is low. Neither test alone is enough: a real value can
/// read at confidence 0 at normal height, and a real dash between two dates (#1649) is thin but
/// reads at a moderate confidence. Rows are the ones table reconstruction itself detects. ~keep
pub(crate) fn drop_shading_marks(words: Vec<HocrWord>, row_threshold_ratio: f64) -> Vec<HocrWord> {
    let row_positions = detect_rows(&words, row_threshold_ratio);
    let rows: Vec<Option<usize>> = words.iter().map(|word| find_row_index(&row_positions, word)).collect();

    let mut row_heights: Vec<Vec<u32>> = vec![Vec::new(); row_positions.len()];
    for (word, row) in words.iter().zip(&rows) {
        if let Some(row) = *row {
            row_heights[row].push(word.height);
        }
    }
    let row_medians: Vec<u32> = row_heights.into_iter().map(median_height).collect();

    words
        .into_iter()
        .zip(rows)
        .filter(|(word, row)| !row.is_some_and(|row| is_shading_mark(word, row_medians[row])))
        .map(|(word, _)| word)
        .collect()
}

fn is_shading_mark(word: &HocrWord, row_median_height: u32) -> bool {
    if row_median_height == 0 || word.confidence >= MARK_MAX_CONFIDENCE {
        return false;
    }
    let ratio = word.height as f64 / row_median_height as f64;
    !(THIN_MARK_HEIGHT_RATIO..=TALL_MARK_HEIGHT_RATIO).contains(&ratio)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, left: u32, top: u32, height: u32, confidence: f64) -> HocrWord {
        HocrWord {
            text: text.to_string(),
            left,
            top,
            width: 60,
            height,
            confidence,
        }
    }

    /// A row of three values 100 px apart, 30 px high at confidence 90, with `mark` after the first.
    fn row_with(mark: HocrWord) -> Vec<HocrWord> {
        vec![
            word("7,812", 100, 200, 30, 90.0),
            mark,
            word("7,968", 200, 200, 30, 90.0),
            word("8,127", 300, 200, 30, 90.0),
        ]
    }

    fn texts(words: &[HocrWord]) -> Vec<&str> {
        words.iter().map(|word| word.text.as_str()).collect()
    }

    const ROW_THRESHOLD_RATIO: f64 = 0.5;

    #[test]
    fn drops_a_tall_low_confidence_mark() {
        let kept = drop_shading_marks(row_with(word("=", 165, 184, 62, 13.0)), ROW_THRESHOLD_RATIO);
        assert_eq!(texts(&kept), ["7,812", "7,968", "8,127"]);
    }

    #[test]
    fn drops_a_thin_low_confidence_mark() {
        let kept = drop_shading_marks(row_with(word("\u{2014},", 165, 213, 5, 0.0)), ROW_THRESHOLD_RATIO);
        assert_eq!(texts(&kept), ["7,812", "7,968", "8,127"]);
    }

    /// The #1649 shape: a dash 10 px high in a row of 60 px words, read at confidence 46.
    #[test]
    fn keeps_a_thin_dash_read_at_a_moderate_confidence() {
        let row = vec![
            word("Jan 1", 100, 200, 60, 95.0),
            word("-", 165, 225, 10, 46.0),
            word("Jan 31,", 200, 200, 60, 95.0),
            word("2026", 300, 200, 60, 95.0),
        ];
        let kept = drop_shading_marks(row, ROW_THRESHOLD_RATIO);
        assert_eq!(texts(&kept), ["Jan 1", "-", "Jan 31,", "2026"]);
    }

    #[test]
    fn keeps_a_tall_high_confidence_word() {
        let kept = drop_shading_marks(row_with(word("(", 165, 184, 62, 90.0)), ROW_THRESHOLD_RATIO);
        assert_eq!(texts(&kept), ["7,812", "(", "7,968", "8,127"]);
    }

    #[test]
    fn keeps_a_value_at_confidence_zero_and_normal_height() {
        let kept = drop_shading_marks(row_with(word("(2,100)", 165, 200, 30, 0.0)), ROW_THRESHOLD_RATIO);
        assert_eq!(texts(&kept), ["7,812", "(2,100)", "7,968", "8,127"]);
    }

    /// A row whose words mostly report a height of 0 has no median to measure a mark against.
    #[test]
    fn keeps_every_word_of_a_row_whose_median_height_is_zero() {
        let row = vec![
            word("-", 100, 200, 0, 0.0),
            word("=", 200, 200, 0, 0.0),
            word("7,812", 300, 200, 30, 0.0),
        ];
        let kept = drop_shading_marks(row, ROW_THRESHOLD_RATIO);
        assert_eq!(texts(&kept), ["-", "=", "7,812"]);
    }

    #[test]
    fn keeps_every_word_of_an_empty_or_one_word_region() {
        assert!(drop_shading_marks(Vec::new(), ROW_THRESHOLD_RATIO).is_empty());
        let kept = drop_shading_marks(vec![word("=", 100, 200, 5, 0.0)], ROW_THRESHOLD_RATIO);
        assert_eq!(texts(&kept), ["="]);
    }
}
