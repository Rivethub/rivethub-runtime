use std::fs::File;
use std::io::BufRead as _;
use std::io::BufReader;
use std::path::Path;

use kernel::transcript::HarnessToolOutput;
use kernel::transcript::TranscriptReaderError;

use super::super::super::claude::transcript_file;
use super::super::super::field::Field;
use super::super::super::field::object_line;
use super::super::super::files::read_failure;
use super::Block;
use super::Kind;
use super::Record;
use super::flagged;
use super::result_text;

const OUTPUT_BYTES: usize = 64 * 1024;

pub(crate) fn tool_output(
    root: &Path,
    id: &str,
    call: &str,
) -> Result<Option<HarnessToolOutput>, TranscriptReaderError> {
    let Some(path) = transcript_file(root, id)? else {
        return Ok(None);
    };
    let file = File::open(&path).map_err(|error| read_failure(&path, error))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| read_failure(&path, error))?;
        if read == 0 {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&line);
        if !text.contains(call) {
            continue;
        }
        if let Some(output) = object_line::<Record>(&text)
            .filter(|record| record.kind() == Kind::User)
            .and_then(|record| record_output(&record, call))
        {
            return Ok(Some(output));
        }
    }
}

fn record_output(record: &Record, call: &str) -> Option<HarnessToolOutput> {
    let block = record
        .body()?
        .content?
        .items()?
        .iter()
        .filter_map(Field::object::<Block>)
        .find(|block| {
            block.is("tool_result")
                && block
                    .tool_use_id
                    .as_ref()
                    .is_some_and(|id| id.is_text(call))
        })?;
    let text = result_text(record, &block).unwrap_or_default();
    let truncated = text.len() > OUTPUT_BYTES;
    Some(HarnessToolOutput {
        text: if truncated { tail_bytes(&text) } else { text },
        is_error: flagged(block.is_error.as_ref()),
        truncated,
    })
}

fn tail_bytes(text: &str) -> String {
    let start = text.ceil_char_boundary(text.len().saturating_sub(OUTPUT_BYTES));
    text.get(start..).unwrap_or_default().to_owned()
}
