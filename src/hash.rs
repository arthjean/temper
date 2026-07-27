use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Result, TemperError};

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(|error| {
        TemperError::new(format!(
            "Could not open {} for hashing: {error}",
            path.display()
        ))
    })?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let bytes_read = reader.read(&mut buffer).map_err(|error| {
            TemperError::new(format!("Could not hash {}: {error}", path.display()))
        })?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}
