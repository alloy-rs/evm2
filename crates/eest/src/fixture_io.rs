use flate2::read::GzDecoder;
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

/// Reads a plain JSON fixture or gzip-compressed JSON fixture.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader: Box<dyn Read> =
        if is_gzip_path(path) { Box::new(GzDecoder::new(file)) } else { Box::new(file) };
    let mut input = String::new();
    reader.read_to_string(&mut input)?;
    Ok(input)
}

fn is_gzip_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "gz")
}
