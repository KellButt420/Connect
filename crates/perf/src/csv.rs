//! Plain CSV: a header row, then one row per record. Numbers only (plus a few fixed words), so
//! nothing needs quoting; empty cells mean "not measured".

use std::{fmt::Write as _, io::Write as _, path::Path};

/// A trace record that can be written as a CSV row.
pub trait CsvRecord {
    /// The column names, comma-separated, in row order.
    const HEADER: &'static str;
    /// Appends this record's cells, comma-separated, without the line break.
    fn write_row(&self, out: &mut String);
}

/// Writes `records` to `path` as CSV, replacing the file. Builds the text first, so this is
/// for the end of a session or an explicit export, not the video threads.
pub fn write_csv<T: CsvRecord>(path: &Path, records: &[T]) -> std::io::Result<()> {
    let mut text = String::with_capacity(64 + records.len() * 160);
    text.push_str(T::HEADER);
    text.push('\n');
    for record in records {
        record.write_row(&mut text);
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(text.as_bytes())
}

/// Appends `value` and a comma; an empty cell for `None`.
pub fn cell<T: std::fmt::Display>(out: &mut String, value: Option<T>) {
    if let Some(value) = value {
        let _ = write!(out, "{value}");
    }
    out.push(',');
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Row(u32, Option<i64>, &'static str);

    impl CsvRecord for Row {
        const HEADER: &'static str = "seq,value_us,status";
        fn write_row(&self, out: &mut String) {
            cell(out, Some(self.0));
            cell(out, self.1);
            out.push_str(self.2);
        }
    }

    #[test]
    fn writes_a_header_and_one_row_per_record() {
        let dir = std::env::temp_dir().join(format!("connect-perf-csv-{}", std::process::id()));
        let path = dir.join("nested").join("trace.csv");
        write_csv(&path, &[Row(1, Some(-5), "shown"), Row(2, None, "superseded")]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "seq,value_us,status\n1,-5,shown\n2,,superseded\n");
        // Every row has as many cells as the header.
        let columns = text.lines().next().unwrap().split(',').count();
        assert!(text.lines().all(|line| line.split(',').count() == columns));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
