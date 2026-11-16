//! Bounded decoding with progress for large portable replay bundles.
use super::*;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

const MAX_DECODED_BYTES: usize = 512 * 1024 * 1024;
const READ_CHUNK_BYTES: usize = 1024 * 1024;

fn read_bytes(path: &Path, mut decoded: impl FnMut(usize)) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);
    let gzip = reader
        .fill_buf()
        .map_err(|e| e.to_string())?
        .starts_with(&[0x1f, 0x8b]);
    let mut input: Box<dyn Read> = if gzip {
        Box::new(flate2::read::GzDecoder::new(reader))
    } else {
        Box::new(reader)
    };
    let mut bytes = Vec::new();
    let mut chunk = vec![0; READ_CHUNK_BYTES];
    loop {
        let n = input.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if bytes.len() + n > MAX_DECODED_BYTES {
            return Err("Replay exceeds the 512 MB decoded limit".into());
        }
        bytes.extend_from_slice(&chunk[..n]);
        decoded(bytes.len());
    }
    Ok(bytes)
}

pub(super) fn read_json(path: &Path) -> Result<serde_json::Value> {
    serde_json::from_slice(&read_bytes(path, |_| {})?).map_err(|e| e.to_string())
}

/// Publish only a finished gzip, without replacing an existing output. Keeping
/// the staging file beside the destination makes the hard-link publish atomic.
pub(super) fn write_new_gzip(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut part = path.as_os_str().to_os_string();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .map_err(|e| format!("{}: {e}", part.display()))?;
    let result = (|| {
        let mut gzip = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        serde_json::to_writer(&mut gzip, value).map_err(|e| e.to_string())?;
        gzip.finish()
            .map_err(|e| e.to_string())?
            .sync_all()
            .map_err(|e| e.to_string())?;
        std::fs::hard_link(&part, path).map_err(|e| format!("{}: {e}", path.display()))
    })();
    let cleanup = std::fs::remove_file(&part).map_err(|e| e.to_string());
    result.and(cleanup)
}

impl Replay {
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_with_progress(path, |_, _| {})
    }
    pub fn load_with_progress(path: &Path, mut progress: impl FnMut(f32, String)) -> Result<Self> {
        progress(0.0, "Reading schedule file…".into());
        let bytes = read_bytes(path, |decoded| {
            progress(
                0.08,
                format!(
                    "Decoding schedule · {:.1} MiB decoded",
                    decoded as f64 / 1048576.0
                ),
            );
        })?;
        progress(0.15, "Parsing AOIs, jobs and satellite paths…".into());
        // Deserialize directly; avoid constructing and then copying a second full JSON tree.
        let document = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        drop(bytes);
        progress(0.22, "Validating saved timing and deadline results…".into());
        let replay = Self::new(document)?;
        progress(
            0.28,
            format!(
                "Validated {} jobs and {} scheduled sweeps",
                replay.jobs.len(),
                replay.capture_ends.len()
            ),
        );
        Ok(replay)
    }
}
