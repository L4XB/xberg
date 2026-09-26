use super::super::error::OcrError;
use super::super::utils::{TSV_MIN_FIELDS, TSV_WORD_LEVEL};
use crate::table_core::HocrWord;

/// Extract words from Tesseract TSV output and convert to HocrWord format.
///
/// This parses Tesseract's TSV format (level, page_num, block_num, ...) and
/// converts it to the HocrWord format used for table reconstruction.
pub(crate) fn extract_words_from_tsv(tsv_data: &str, min_confidence: f64) -> Result<Vec<HocrWord>, OcrError> {
    let mut words = Vec::new();

    for (line_num, line) in tsv_data.lines().enumerate() {
        if line_num == 0 {
            continue;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < TSV_MIN_FIELDS {
            continue;
        }

        let level = fields[0].parse::<u32>().unwrap_or(0);
        if level != TSV_WORD_LEVEL {
            continue;
        }

        let conf = fields[10].parse::<f64>().unwrap_or(-1.0);
        if conf < min_confidence {
            continue;
        }

        let text = fields[11].trim();
        if text.is_empty() {
            continue;
        }

        let word = HocrWord {
            text: text.to_string(),
            left: fields[6].parse().unwrap_or(0),
            top: fields[7].parse().unwrap_or(0),
            width: fields[8].parse().unwrap_or(0),
            height: fields[9].parse().unwrap_or(0),
            confidence: conf,
        };

        words.push(word);
    }

    Ok(words)
}

/// Extract the words table reconstruction reads: [`extract_words_from_tsv`], with underscore
/// marks removed from each word (xberg-io/xberg#1833).
///
/// Tesseract reads the edge of a shaded row as runs of underscores and fuses them onto the
/// values around it (`___7,073_`) or into one word spanning two values (`(2,100)__(2,163)`).
/// The fused word's box then covers the gap between two columns, so the cell merge joins two
/// values into one cell. A leading or trailing run, or an interior run of two or more, is a
/// mark: the word is cut there and each piece keeps the share of the box its characters span.
/// A single interior underscore (`snake_case`) is text and stays. ~keep
pub(crate) fn extract_table_words_from_tsv(tsv_data: &str, min_confidence: f64) -> Result<Vec<HocrWord>, OcrError> {
    Ok(extract_words_from_tsv(tsv_data, min_confidence)?
        .into_iter()
        .flat_map(split_at_underscore_marks)
        .collect())
}

fn split_at_underscore_marks(word: HocrWord) -> Vec<HocrWord> {
    if !word.text.contains('_') {
        return vec![word];
    }
    let chars: Vec<char> = word.text.chars().collect();
    let mut pieces = Vec::new();
    let mut piece_start = None;
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '_' {
            piece_start.get_or_insert(index);
            index += 1;
            continue;
        }
        let run_end = chars[index..]
            .iter()
            .position(|&ch| ch != '_')
            .map_or(chars.len(), |offset| index + offset);
        let is_text = run_end - index == 1 && run_end < chars.len();
        if !is_text && let Some(start) = piece_start.take() {
            pieces.push(word_piece(&word, &chars, start, index));
        }
        index = run_end;
    }
    if let Some(start) = piece_start {
        pieces.push(word_piece(&word, &chars, start, chars.len()));
    }
    pieces
}

fn word_piece(word: &HocrWord, chars: &[char], start: usize, end: usize) -> HocrWord {
    let span = |offset: usize| (u64::from(word.width) * offset as u64 / chars.len() as u64) as u32;
    HocrWord {
        text: chars[start..end].iter().collect(),
        left: word.left + span(start),
        top: word.top,
        width: span(end) - span(start),
        height: word.height,
        confidence: word.confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_word_texts(tsv_rows: &str) -> Vec<(String, u32, u32)> {
        let tsv = format!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n{tsv_rows}"
        );
        extract_table_words_from_tsv(&tsv, 0.0)
            .unwrap()
            .into_iter()
            .map(|word| (word.text, word.left, word.width))
            .collect()
    }

    #[test]
    fn table_words_trim_leading_and_trailing_underscore_marks_and_narrow_the_box() {
        let words = table_word_texts(
            "5\t1\t0\t0\t0\t0\t2000\t100\t90\t60\t8\t___7,073_\n5\t1\t0\t0\t0\t1\t2200\t100\t60\t30\t8\t_5,017\n",
        );
        assert_eq!(
            words,
            vec![("7,073".to_string(), 2030, 50), ("5,017".to_string(), 2210, 50)]
        );
    }

    #[test]
    fn table_words_split_one_word_fused_across_an_underscore_run() {
        let words = table_word_texts("5\t1\t0\t0\t0\t0\t1000\t100\t160\t40\t0\t(2,100)__(2,163)\n");
        assert_eq!(
            words,
            vec![("(2,100)".to_string(), 1000, 70), ("(2,163)".to_string(), 1090, 70)]
        );
    }

    #[test]
    fn table_words_drop_a_word_of_only_underscores_and_keep_its_neighbours() {
        let words = table_word_texts(
            "5\t1\t0\t0\t0\t0\t100\t100\t60\t30\t90\tItem\n5\t1\t0\t0\t0\t1\t200\t100\t40\t4\t50\t___\n",
        );
        assert_eq!(words, vec![("Item".to_string(), 100, 60)]);
    }

    #[test]
    fn table_words_keep_a_single_interior_underscore_and_plain_words_whole() {
        let words = table_word_texts(
            "5\t1\t0\t0\t0\t0\t100\t100\t90\t30\t90\tfile_name\n5\t1\t0\t0\t0\t1\t300\t100\t50\t30\t90\t4,871\n",
        );
        assert_eq!(
            words,
            vec![("file_name".to_string(), 100, 90), ("4,871".to_string(), 300, 50)]
        );
    }

    /// Two values with a shading mark fused onto the second: the mark's box closes the gap
    /// between the columns, so without the trim the cell merge joins both values into one cell.
    #[test]
    fn underscore_marks_between_two_values_do_not_glue_them_into_one_cell() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t0\t0\t0\t0\t100\t100\t60\t30\t90\tYear\n\
5\t1\t0\t0\t0\t1\t300\t100\t60\t30\t90\tYear\n\
5\t1\t0\t0\t0\t2\t100\t200\t90\t30\t60\t6,867\n\
5\t1\t0\t0\t0\t3\t200\t200\t190\t30\t8\t_____7,073__\n";
        let words = extract_table_words_from_tsv(tsv, 0.0).unwrap();
        let table = crate::table_core::reconstruct_table(&words, 20, 0.5);
        assert!(
            table.iter().flatten().all(|cell| !cell.contains('_')),
            "no cell may keep an underscore mark: {table:?}"
        );
        let value_row = table.iter().find(|row| row.iter().any(|cell| cell == "6,867")).unwrap();
        assert!(
            value_row.iter().any(|cell| cell == "7,073"),
            "the second value must sit in a cell of its own: {table:?}"
        );
    }

    #[test]
    fn test_extract_words_basic() {
        let tsv = r#"level	page_num	block_num	par_num	line_num	word_num	left	top	width	height	conf	text
5	1	0	0	0	0	100	50	80	30	95.5	Hello
5	1	0	0	0	1	190	50	70	30	92.3	World"#;

        let words = extract_words_from_tsv(tsv, 0.0).unwrap();
        assert_eq!(words.len(), 2);

        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[0].left, 100);
        assert_eq!(words[0].top, 50);
        assert_eq!(words[0].confidence, 95.5);

        assert_eq!(words[1].text, "World");
        assert_eq!(words[1].left, 190);
    }

    #[test]
    fn test_extract_words_confidence_filter() {
        let tsv = r#"level	page_num	block_num	par_num	line_num	word_num	left	top	width	height	conf	text
5	1	0	0	0	0	100	50	80	30	95.5	Hello
5	1	0	0	0	1	190	50	70	30	50.0	World
5	1	0	0	0	2	270	50	60	30	92.3	Test"#;

        let words = extract_words_from_tsv(tsv, 90.0).unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[1].text, "Test");
    }

    #[test]
    fn test_extract_words_level_filter() {
        let tsv = r#"level	page_num	block_num	par_num	line_num	word_num	left	top	width	height	conf	text
3	1	0	0	0	0	100	50	80	30	95.5	Paragraph
5	1	0	0	0	0	100	50	80	30	95.5	Hello
4	1	0	0	0	1	190	50	70	30	92.3	Line"#;

        let words = extract_words_from_tsv(tsv, 0.0).unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "Hello");
    }

    #[test]
    fn test_hocr_word_methods() {
        let word = HocrWord {
            text: "Hello".to_string(),
            left: 100,
            top: 50,
            width: 80,
            height: 30,
            confidence: 95.5,
        };

        assert_eq!(word.right(), 180);
        assert_eq!(word.bottom(), 80);
        assert_eq!(word.y_center(), 65.0);
        assert_eq!(word.x_center(), 140.0);
    }

    #[test]
    fn test_extract_words_empty_text() {
        let tsv = r#"level	page_num	block_num	par_num	line_num	word_num	left	top	width	height	conf	text
5	1	0	0	0	0	100	50	80	30	95.5
5	1	0	0	0	1	190	50	70	30	92.3	World"#;

        let words = extract_words_from_tsv(tsv, 0.0).unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "World");
    }

    #[test]
    fn test_extract_words_malformed() {
        let tsv = r#"level	page_num	block_num
5	1	0	0	0	0	100	50	80	30	95.5	Hello
invalid line
5	1	0	0	0	1	190	50	70	30	92.3	World"#;

        let words = extract_words_from_tsv(tsv, 0.0).unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[1].text, "World");
    }
}
