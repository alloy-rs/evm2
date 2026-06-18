use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};
use zstd::stream::read::Decoder as ZstdDecoder;

/// Reads a plain JSON fixture or zstd-compressed JSON fixture.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader: Box<dyn Read> =
        if is_zstd_path(path) { Box::new(ZstdDecoder::new(file)?) } else { Box::new(file) };
    let mut input = String::new();
    reader.read_to_string(&mut input)?;
    Ok(input)
}

fn is_zstd_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "zst")
}
