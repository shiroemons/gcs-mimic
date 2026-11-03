use std::io::{self, Write};

pub(crate) struct Interrupt {
    pub(crate) remaining: usize,
}

impl Write for Interrupt {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert_ne!(self.remaining, 0, "write after interrupted report");
        if bytes.contains(&b'\n') {
            self.remaining = self.remaining.saturating_sub(1);
            if self.remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "interrupted report",
                ));
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
