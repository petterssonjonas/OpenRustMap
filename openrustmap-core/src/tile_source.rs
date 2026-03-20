use crate::config::TileSourceConfig;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct TileSource {
    base_url: String,
    cache_dir: Option<PathBuf>,
    client: Client,
    mem_cache: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    mem_cache_max: usize,
    pending: Arc<Mutex<HashSet<String>>>,
    /// Fired when a background fetch completes (for waking the UI loop).
    notify_tx: Option<Sender<()>>,
}

impl TileSource {
    fn maybe_decompress_gzip(bytes: Vec<u8>) -> Result<Vec<u8>> {
        if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
            let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).context("decompress gzip tile")?;
            Ok(out)
        } else {
            Ok(bytes)
        }
    }

    pub fn new(config: &TileSourceConfig) -> Result<Self> {
        Self::with_notify(config, None)
    }

    pub fn with_notify(config: &TileSourceConfig, notify_tx: Option<Sender<()>>) -> Result<Self> {
        let base_url = config.base_url.clone();
        let base_url = if base_url.ends_with('/') {
            base_url
        } else {
            format!("{base_url}/")
        };

        let cache_dir = config.resolved_cache_dir().map(|d| d.join("tiles"));

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        Ok(Self {
            base_url,
            cache_dir,
            client,
            mem_cache: Arc::new(Mutex::new(HashMap::new())),
            mem_cache_max: 256,
            pending: Arc::new(Mutex::new(HashSet::new())),
            notify_tx,
        })
    }

    fn tile_url2(&self, z: u32, x: u32, y: u32) -> String {
        format!("{}{}/{}/{}.pbf", self.base_url, z, x, y)
    }

    fn cache_path(&self, z: u32, x: u32, y: u32) -> Option<PathBuf> {
        let cache_dir = self.cache_dir.as_ref()?;
        let dir = cache_dir.join(z.to_string());
        Some(dir.join(format!("{}-{}.pbf", x, y)))
    }

    fn cache_key(z: u32, x: u32, y: u32) -> String {
        format!("{}-{}-{}", z, x, y)
    }

    /// Memory + on-disk cache only; never blocks on the network.
    pub fn try_get_tile_bytes(&self, z: u32, x: u32, y: u32) -> Option<Vec<u8>> {
        let key = Self::cache_key(z, x, y);
        if let Ok(mem) = self.mem_cache.lock() {
            if let Some(v) = mem.get(&key) {
                return Some(v.clone());
            }
        }
        if let Some(path) = self.cache_path(z, x, y) {
            if let Ok(bytes) = fs::read(&path) {
                if let Ok(out) = Self::maybe_decompress_gzip(bytes) {
                    if let Ok(mut mem) = self.mem_cache.lock() {
                        Self::evict_if_needed(&mut mem, self.mem_cache_max);
                        mem.insert(key, out.clone());
                    }
                    return Some(out);
                }
            }
        }
        None
    }

    fn evict_if_needed(mem: &mut HashMap<String, Vec<u8>>, max: usize) {
        if mem.len() >= max {
            if let Some(k) = mem.keys().next().cloned() {
                mem.remove(&k);
            }
        }
    }

    /// Full fetch (HTTP); used by tests and synchronous paths.
    pub fn get_tile_blocking(&self, z: u32, x: u32, y: u32) -> Result<Vec<u8>> {
        if let Some(b) = self.try_get_tile_bytes(z, x, y) {
            return Ok(b);
        }
        let key = Self::cache_key(z, x, y);
        let url = self.tile_url2(z, x, y);
        let bytes = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("fetching tile: {url}"))?
            .bytes()
            .with_context(|| format!("reading tile body: {url}"))?;

        let out = Self::maybe_decompress_gzip(bytes.to_vec())?;

        if let Some(path) = self.cache_path(z, x, y) {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&path, &out);
        }

        if let Ok(mut mem) = self.mem_cache.lock() {
            Self::evict_if_needed(&mut mem, self.mem_cache_max);
            mem.insert(key, out.clone());
        }

        Ok(out)
    }

    /// If the tile is not cached, start a background download. UI should call
    /// [`Self::try_get_tile_bytes`] on later frames.
    pub fn request_tile(&self, z: u32, x: u32, y: u32) {
        if self.try_get_tile_bytes(z, x, y).is_some() {
            return;
        }
        let key = Self::cache_key(z, x, y);
        {
            let mut pending = match self.pending.lock() {
                Ok(p) => p,
                Err(_) => return,
            };
            if pending.contains(&key) {
                return;
            }
            pending.insert(key.clone());
        }

        let base_url = self.base_url.clone();
        let cache_dir = self.cache_dir.clone();
        let client = self.client.clone();
        let mem_cache = Arc::clone(&self.mem_cache);
        let pending = Arc::clone(&self.pending);
        let notify_tx = self.notify_tx.clone();
        let max = self.mem_cache_max;

        thread::spawn(move || {
            let _guard = PendingGuard {
                pending: Arc::clone(&pending),
                key: key.clone(),
            };

            let url = format!("{}{}/{}/{}.pbf", base_url, z, x, y);
            let result = client
                .get(&url)
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.bytes())
                .map(|b| b.to_vec());

            let Ok(raw) = result else {
                return;
            };

            let Ok(out) = Self::maybe_decompress_gzip(raw) else {
                return;
            };

            if let Some(path) = cache_dir.as_ref().map(|d| {
                let dir = d.join(z.to_string());
                dir.join(format!("{}-{}.pbf", x, y))
            }) {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(&path, &out);
            }

            if let Ok(mut mem) = mem_cache.lock() {
                Self::evict_if_needed(&mut mem, max);
                mem.insert(key, out);
            }

            if let Some(tx) = notify_tx {
                let _ = tx.send(());
            }
        });
    }
}

/// Remove `key` from pending if the thread panics before completion.
struct PendingGuard {
    pending: Arc<Mutex<HashSet<String>>>,
    key: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&self.key);
        }
    }
}

// Optional helper: keep the compiler happy about unused imports during early scaffolding.
#[allow(dead_code)]
fn _touch(path: &Path) {
    let _ = fs::metadata(path).and_then(|_| Ok(()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_vector_tile::VectorTile;

    #[test]
    fn sample_tile_is_parseable_after_decompression() {
        let cfg = TileSourceConfig {
            base_url: crate::config::DEFAULT_TILE_BASE_URL.to_string(),
            cache_dir: None,
        };
        let ts = TileSource::new(&cfg).expect("init tile source");
        let bytes = ts.get_tile_blocking(4, 8, 5).expect("fetch tile");

        let result = std::panic::catch_unwind(|| {
            let mut vt = VectorTile::new(bytes, None);
            vt.read_layers();
            vt
        });

        assert!(result.is_ok(), "vector tile parser panicked");
    }
}
