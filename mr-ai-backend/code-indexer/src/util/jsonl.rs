//! JSONL writer utility.

use crate::errors::{Error, Result};
use serde::Serialize;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

pub struct JsonlWriter {
    w: BufWriter<File>,
}

impl JsonlWriter {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let f = File::create(path).map_err(Error::from)?;
        Ok(Self {
            w: BufWriter::new(f),
        })
    }
    pub fn write_obj<T: Serialize>(&mut self, obj: &T) -> Result<()> {
        serde_json::to_writer(&mut self.w, obj).map_err(Error::from)?;
        self.w.write_all(b"\n").map_err(Error::from)?;
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        self.w.flush().map_err(Error::from)
    }
}
