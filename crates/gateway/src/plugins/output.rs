use tokio::io::{AsyncBufRead, AsyncBufReadExt as _};

pub(super) const MAX_LINE_BYTES: usize = 16 * 1024;

pub(super) async fn next_line<R>(reader: &mut R, max: usize) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut kept: Vec<u8> = Vec::new();
    let mut dropped = 0_usize;
    let mut read_any = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(read_any.then(|| finish(&kept, dropped)));
        }
        read_any = true;
        let line = available
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or(available);
        let ended = line.len() < available.len();
        let space_left = max.saturating_sub(kept.len());
        kept.extend(line.iter().take(space_left));
        dropped += line.len().saturating_sub(space_left);
        let used = if ended { line.len() + 1 } else { line.len() };
        reader.consume(used);
        if ended {
            return Ok(Some(finish(&kept, dropped)));
        }
    }
}

fn finish(kept: &[u8], dropped: usize) -> String {
    let text = String::from_utf8_lossy(kept);
    let text = text.strip_suffix('\r').unwrap_or(&text);
    if dropped == 0 {
        text.to_owned()
    } else {
        format!("{text} … [{dropped} more bytes of this line dropped]")
    }
}

#[cfg(test)]
mod tests {
    use super::next_line;

    #[tokio::test]
    async fn a_long_line_is_cut_and_the_next_line_still_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut input = vec![b'x'; 100_000];
        input.extend_from_slice(b"\nnext\n");
        let mut reader = tokio::io::BufReader::with_capacity(4096, input.as_slice());
        let first = next_line(&mut reader, 1024).await?.ok_or("a first line")?;
        assert!(first.starts_with(&"x".repeat(1024)));
        assert!(
            first.ends_with("… [98976 more bytes of this line dropped]"),
            "{first}"
        );
        assert_eq!(next_line(&mut reader, 1024).await?.as_deref(), Some("next"));
        assert_eq!(next_line(&mut reader, 1024).await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn bytes_that_are_not_text_do_not_stop_the_reader() -> Result<(), std::io::Error> {
        let input: &[u8] = b"bad \xff byte\r\nlast without newline";
        let mut reader = tokio::io::BufReader::new(input);
        assert_eq!(
            next_line(&mut reader, 1024).await?.as_deref(),
            Some("bad \u{fffd} byte")
        );
        assert_eq!(
            next_line(&mut reader, 1024).await?.as_deref(),
            Some("last without newline")
        );
        assert_eq!(next_line(&mut reader, 1024).await?, None);
        Ok(())
    }
}
