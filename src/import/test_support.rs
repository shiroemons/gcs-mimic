use std::io::{self, Write};

pub(super) struct StopAfterLines(pub usize);

impl Write for StopAfterLines {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.contains(&b'\n') {
            self.0 = self.0.saturating_sub(1);
            if self.0 == 0 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
