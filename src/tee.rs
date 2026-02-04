use std::io::{Read, Result, Write};

pub struct Reader<R, W> {
    read: R,
    output: W,
    force_flush: bool,
}

impl<R, W> Reader<R, W> {
    pub fn new(read: R, output: W, force_flush: bool) -> Self {
        Self {
            read,
            output,
            force_flush,
        }
    }
}

impl<R: Read, W: Write> Read for Reader<R, W> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = self.read.read(buf)?;
        let out = buf.get(..n).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "reader reported more bytes read than buffer length",
            )
        })?;
        self.output.write_all(out)?;
        if self.force_flush {
            self.output.flush()?;
        }
        Ok(n)
    }
}

impl<R: Read + Clone, W: Write + Clone> Clone for Reader<R, W> {
    fn clone(&self) -> Self {
        Self {
            read: self.read.clone(),
            output: self.output.clone(),
            force_flush: self.force_flush,
        }
    }
}
