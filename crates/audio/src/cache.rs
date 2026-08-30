//! Content-addressed on-disk cache of already synthesized speech phrases.
//!
//! Speech synthesis is the slowest step of a spoken turn, and short phrases
//! (acknowledgements, repeated clauses) are synthesized over and over with the
//! same engine, voice, and rate. This cache turns those repeats into a file
//! read so the first audio frame is available immediately.
//!
//! Privacy rules that the rest of the voice stack depends on:
//!
//! - the cached text is user or model content, so only the hex digest is ever
//!   written to a file name, the index, or an error string — never the phrase;
//! - entries and the index are mode 0600 and are replaced atomically through a
//!   unique temporary name, like every other file that persists local content;
//! - a corrupt, truncated, or missing entry is never fatal. The entry is
//!   forgotten and the caller re-synthesizes.

use crate::AudioError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Locked phrase-cache budget: 64 MiB inside the application data directory.
pub const PHRASE_CACHE_CAPACITY_BYTES: u64 = 64 * 1024 * 1024;

const ENTRY_MAGIC: &[u8; 8] = b"PAPHRAS1";
const ENTRY_HEADER_BYTES: usize = 56;
const ENTRY_SUFFIX: &str = ".pcm";
const INDEX_FILE: &str = "index.json";

/// Cache identity of one synthesized phrase.
///
/// The digest is `SHA-256(engine|voice|rate|text)`. Every field is length
/// framed before hashing, so a separator inside the spoken text cannot forge
/// another key's identity, and any engine, voice, or rate change produces a
/// different digest — which is exactly how stale audio is invalidated.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhraseKey {
    digest: String,
}

impl PhraseKey {
    /// Derive the cache identity of one phrase.
    #[must_use]
    pub fn new(engine: &str, voice: &str, rate: &str, text: &str) -> Self {
        let mut hasher = Sha256::new();
        for (position, field) in [engine, voice, rate, text].into_iter().enumerate() {
            if position > 0 {
                hasher.update(b"|");
            }
            hasher.update(field.len().to_le_bytes());
            hasher.update(b":");
            hasher.update(field.as_bytes());
        }
        Self {
            digest: hex_digest(hasher.finalize().as_slice()),
        }
    }

    /// The hex digest used as the on-disk name. It never reveals the phrase.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// One decoded cache hit: interleaved signed 16-bit PCM plus its format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedPhrase {
    pub samples: Vec<i16>,
    pub sample_rate_hz: u32,
    pub channels: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct PhraseRecord {
    bytes: u64,
    used_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PhraseIndex {
    #[serde(default)]
    entries: BTreeMap<String, PhraseRecord>,
    #[serde(default)]
    next_use: u64,
}

impl PhraseIndex {
    fn total_bytes(&self) -> u64 {
        self.entries
            .values()
            .fold(0_u64, |total, record| total.saturating_add(record.bytes))
    }

    fn touch(&mut self, digest: &str) {
        self.next_use = self.next_use.saturating_add(1);
        let next_use = self.next_use;
        if let Some(record) = self.entries.get_mut(digest) {
            record.used_at = next_use;
        }
    }

    /// The least recently used digest, with a deterministic digest tiebreak so
    /// eviction order never depends on map iteration luck.
    fn least_recently_used(&self) -> Option<String> {
        self.entries
            .iter()
            .min_by_key(|(digest, record)| (record.used_at, (*digest).clone()))
            .map(|(digest, _)| digest.clone())
    }
}

/// LRU disk cache of synthesized phrases bounded by an explicit byte budget.
pub struct PhraseCache {
    root: PathBuf,
    capacity_bytes: u64,
    index: Mutex<PhraseIndex>,
}

impl PhraseCache {
    /// Open the cache at `root` with the locked 64 MiB budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, AudioError> {
        Self::with_capacity_bytes(root, PHRASE_CACHE_CAPACITY_BYTES)
    }

    /// Open the cache with an explicit budget so eviction stays testable
    /// without writing the production 64 MiB.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero budget or when the directory cannot be
    /// created.
    pub fn with_capacity_bytes(
        root: impl Into<PathBuf>,
        capacity_bytes: u64,
    ) -> Result<Self, AudioError> {
        if capacity_bytes == 0 {
            return Err(AudioError::Processing(
                "phrase cache capacity must be positive".into(),
            ));
        }
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| AudioError::Processing(error.to_string()))?;
        let index = load_index(&root);
        let cache = Self {
            root,
            capacity_bytes,
            index: Mutex::new(index),
        };
        cache.with_index(|index, root| {
            evict_to_capacity(index, root, capacity_bytes);
            Ok(())
        })?;
        Ok(cache)
    }

    /// The enforced byte budget of this cache.
    #[must_use]
    pub fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }

    /// Bytes currently accounted to cached entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the index lock is poisoned.
    pub fn total_bytes(&self) -> Result<u64, AudioError> {
        self.with_index(|index, _| Ok(index.total_bytes()))
    }

    /// Digests currently retained, in stable order.
    ///
    /// # Errors
    ///
    /// Returns an error when the index lock is poisoned.
    pub fn digests(&self) -> Result<Vec<String>, AudioError> {
        self.with_index(|index, _| Ok(index.entries.keys().cloned().collect()))
    }

    /// Whether a phrase is retained without decoding or promoting it.
    ///
    /// # Errors
    ///
    /// Returns an error when the index lock is poisoned.
    pub fn contains(&self, key: &PhraseKey) -> Result<bool, AudioError> {
        self.with_index(|index, _| Ok(index.entries.contains_key(key.digest())))
    }

    /// Read one cached phrase and mark it most recently used.
    ///
    /// A miss, a truncated file, or a payload that fails its digest check all
    /// return `None` after forgetting the entry: playback then re-synthesizes
    /// instead of failing.
    #[must_use]
    pub fn get(&self, key: &PhraseKey) -> Option<CachedPhrase> {
        let path = self.entry_path(key);
        let mut index = self.index.lock().ok()?;
        if !index.entries.contains_key(key.digest()) {
            return None;
        }
        let decoded = fs::read(&path).ok().and_then(|bytes| decode_entry(&bytes));
        if let Some(phrase) = decoded {
            index.touch(key.digest());
            let snapshot = index.clone();
            drop(index);
            let _ = persist_index(&self.root, &snapshot);
            Some(phrase)
        } else {
            index.entries.remove(key.digest());
            let snapshot = index.clone();
            drop(index);
            let _ = fs::remove_file(&path);
            let _ = persist_index(&self.root, &snapshot);
            None
        }
    }

    /// Store one synthesized phrase, evicting least recently used entries
    /// until the budget holds again.
    ///
    /// # Errors
    ///
    /// Rejects invalid PCM, phrases larger than the whole budget, and
    /// file-system failures.
    pub fn put(
        &self,
        key: &PhraseKey,
        samples: &[i16],
        sample_rate_hz: u32,
        channels: u16,
    ) -> Result<(), AudioError> {
        let encoded = encode_entry(samples, sample_rate_hz, channels)?;
        let bytes = u64::try_from(encoded.len())
            .map_err(|_| AudioError::Processing("cached phrase size does not fit".into()))?;
        if bytes > self.capacity_bytes {
            return Err(AudioError::Processing(
                "phrase is larger than the whole phrase-cache budget".into(),
            ));
        }
        let path = self.entry_path(key);
        write_private_atomic(&path, &encoded)?;
        let capacity_bytes = self.capacity_bytes;
        let digest = key.digest().to_owned();
        self.with_index(|index, root| {
            index.next_use = index.next_use.saturating_add(1);
            let used_at = index.next_use;
            index
                .entries
                .insert(digest, PhraseRecord { bytes, used_at });
            evict_to_capacity(index, root, capacity_bytes);
            Ok(())
        })
        .inspect_err(|_| {
            let _ = fs::remove_file(&path);
        })
    }

    fn entry_path(&self, key: &PhraseKey) -> PathBuf {
        self.root.join(format!("{}{ENTRY_SUFFIX}", key.digest()))
    }

    fn with_index<T>(
        &self,
        operation: impl FnOnce(&mut PhraseIndex, &Path) -> Result<T, AudioError>,
    ) -> Result<T, AudioError> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| AudioError::Processing("phrase cache index lock is poisoned".into()))?;
        let value = operation(&mut index, &self.root)?;
        let snapshot = index.clone();
        drop(index);
        persist_index(&self.root, &snapshot)?;
        Ok(value)
    }
}

fn evict_to_capacity(index: &mut PhraseIndex, root: &Path, capacity_bytes: u64) {
    while index.total_bytes() > capacity_bytes {
        let Some(digest) = index.least_recently_used() else {
            break;
        };
        index.entries.remove(&digest);
        let _ = fs::remove_file(root.join(format!("{digest}{ENTRY_SUFFIX}")));
    }
}

/// Load the index, rebuilding it from the directory when it is missing or
/// unreadable so a partial write can never inflate the accounted budget.
fn load_index(root: &Path) -> PhraseIndex {
    fs::read(root.join(INDEX_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PhraseIndex>(&bytes).ok())
        .map_or_else(|| rebuild_index(root), |index| retain_present(root, index))
}

fn retain_present(root: &Path, mut index: PhraseIndex) -> PhraseIndex {
    index.entries.retain(|digest, record| {
        fs::metadata(root.join(format!("{digest}{ENTRY_SUFFIX}")))
            .is_ok_and(|metadata| metadata.len() == record.bytes)
    });
    remove_unaccounted_entries(root, &index);
    index
}

/// Delete entry files the index does not account for. A crash between the
/// entry write and the index write would otherwise leave bytes on disk that
/// the 64 MiB budget never sees again.
fn remove_unaccounted_entries(root: &Path, index: &PhraseIndex) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(digest) = name.strip_suffix(ENTRY_SUFFIX) else {
            continue;
        };
        if is_hex_digest(digest) && !index.entries.contains_key(digest) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn rebuild_index(root: &Path) -> PhraseIndex {
    let mut index = PhraseIndex::default();
    let Ok(entries) = fs::read_dir(root) else {
        return index;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(digest) = name.strip_suffix(ENTRY_SUFFIX) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || !is_hex_digest(digest) {
            continue;
        }
        index.next_use = index.next_use.saturating_add(1);
        index.entries.insert(
            digest.to_owned(),
            PhraseRecord {
                bytes: metadata.len(),
                used_at: index.next_use,
            },
        );
    }
    index
}

fn persist_index(root: &Path, index: &PhraseIndex) -> Result<(), AudioError> {
    let encoded =
        serde_json::to_vec(index).map_err(|error| AudioError::Processing(error.to_string()))?;
    write_private_atomic(&root.join(INDEX_FILE), &encoded)
}

fn encode_entry(
    samples: &[i16],
    sample_rate_hz: u32,
    channels: u16,
) -> Result<Vec<u8>, AudioError> {
    if samples.is_empty()
        || !(8_000..=192_000).contains(&sample_rate_hz)
        || channels == 0
        || !samples.len().is_multiple_of(usize::from(channels))
    {
        return Err(AudioError::Processing(
            "phrase cache received an invalid PCM phrase".into(),
        ));
    }
    let sample_count = u64::try_from(samples.len())
        .map_err(|_| AudioError::Processing("cached phrase length does not fit".into()))?;
    let mut payload = Vec::with_capacity(samples.len().saturating_mul(2));
    for sample in samples {
        payload.extend_from_slice(&sample.to_le_bytes());
    }
    let mut encoded = Vec::with_capacity(ENTRY_HEADER_BYTES.saturating_add(payload.len()));
    encoded.extend_from_slice(ENTRY_MAGIC);
    encoded.extend_from_slice(&sample_rate_hz.to_le_bytes());
    encoded.extend_from_slice(&channels.to_le_bytes());
    encoded.extend_from_slice(&0_u16.to_le_bytes());
    encoded.extend_from_slice(&sample_count.to_le_bytes());
    encoded.extend_from_slice(Sha256::digest(&payload).as_slice());
    encoded.extend_from_slice(&payload);
    Ok(encoded)
}

fn decode_entry(bytes: &[u8]) -> Option<CachedPhrase> {
    let header = bytes.get(..ENTRY_HEADER_BYTES)?;
    if header.get(..8)? != ENTRY_MAGIC.as_slice() {
        return None;
    }
    let sample_rate_hz = u32::from_le_bytes(header.get(8..12)?.try_into().ok()?);
    let channels = u16::from_le_bytes(header.get(12..14)?.try_into().ok()?);
    let sample_count =
        usize::try_from(u64::from_le_bytes(header.get(16..24)?.try_into().ok()?)).ok()?;
    let expected = header.get(24..ENTRY_HEADER_BYTES)?;
    let payload = bytes.get(ENTRY_HEADER_BYTES..)?;
    if sample_count == 0
        || channels == 0
        || !(8_000..=192_000).contains(&sample_rate_hz)
        || payload.len() != sample_count.checked_mul(2)?
        || !sample_count.is_multiple_of(usize::from(channels))
        || Sha256::digest(payload).as_slice() != expected
    {
        return None;
    }
    let samples = payload
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect::<Vec<_>>();
    Some(CachedPhrase {
        samples,
        sample_rate_hz,
        channels,
    })
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), AudioError> {
    let parent = path
        .parent()
        .ok_or_else(|| AudioError::Processing("phrase cache path has no parent".into()))?;
    let temporary = parent.join(unique_temporary_name());
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| AudioError::Processing(error.to_string()))
}

fn unique_temporary_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(
        ".phrase-{}-{nanos}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_root(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        std::env::temp_dir().join(format!(
            "personal-agent-phrase-cache-{label}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn ack_key() -> PhraseKey {
        PhraseKey::new("qwen3-tts", "Ryan", "24000@100", "On it.")
    }

    fn tone(samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|index| i16::try_from(index % 4_096).unwrap_or(i16::MAX))
            .collect()
    }

    #[test]
    fn locked_budget_is_sixty_four_mebibytes() {
        assert_eq!(PHRASE_CACHE_CAPACITY_BYTES, 67_108_864);
        let root = cache_root("budget");
        let cache = PhraseCache::open(&root).expect("open");
        assert_eq!(cache.capacity_bytes(), PHRASE_CACHE_CAPACITY_BYTES);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn second_request_for_the_same_phrase_hits_the_disk_cache() {
        let root = cache_root("hit");
        let key = ack_key();
        let cache = PhraseCache::open(&root).expect("open");
        assert!(cache.get(&key).is_none(), "first request must miss");
        cache.put(&key, &tone(2_400), 24_000, 1).expect("store");

        let reopened = PhraseCache::open(&root).expect("reopen");
        let hit = reopened.get(&key).expect("second request hits the cache");
        assert_eq!(hit.samples, tone(2_400));
        assert_eq!(hit.sample_rate_hz, 24_000);
        assert_eq!(hit.channels, 1);
        assert!(
            !root
                .join(format!("{}{ENTRY_SUFFIX}", key.digest()))
                .is_dir()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn engine_voice_and_rate_changes_invalidate_the_cached_phrase() {
        let root = cache_root("invalidate");
        let cache = PhraseCache::open(&root).expect("open");
        cache.put(&ack_key(), &tone(480), 24_000, 1).expect("store");
        for changed in [
            PhraseKey::new("kokoro", "Ryan", "24000@100", "On it."),
            PhraseKey::new("qwen3-tts", "af_heart", "24000@100", "On it."),
            PhraseKey::new("qwen3-tts", "Ryan", "24000@120", "On it."),
            PhraseKey::new("qwen3-tts", "Ryan", "16000@100", "On it."),
            PhraseKey::new("qwen3-tts", "Ryan", "24000@100", "On it!"),
        ] {
            assert_ne!(changed.digest(), ack_key().digest());
            assert!(cache.get(&changed).is_none());
        }
        assert!(cache.get(&ack_key()).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn field_framing_prevents_separator_confusion_between_keys() {
        assert_ne!(
            PhraseKey::new("qwen3-tts", "Ryan", "24000@100", "On it.").digest(),
            PhraseKey::new("qwen3-tts|Ryan", "", "24000@100", "On it.").digest()
        );
    }

    #[test]
    fn eviction_drops_the_least_recently_used_phrase_within_the_budget() {
        let root = cache_root("evict");
        // Three 400-sample entries (856 bytes each) fit; the fourth cannot.
        let cache = PhraseCache::with_capacity_bytes(&root, 3_000).expect("open");
        let keys = ["one", "two", "three", "four"]
            .map(|text| PhraseKey::new("qwen3-tts", "Ryan", "24000@100", text));
        for key in &keys[..3] {
            cache.put(key, &tone(400), 24_000, 1).expect("store");
        }
        assert!(cache.get(&keys[0]).is_some(), "promote the oldest phrase");
        cache.put(&keys[3], &tone(400), 24_000, 1).expect("store");

        assert!(cache.total_bytes().expect("total") <= cache.capacity_bytes());
        assert!(cache.contains(&keys[0]).expect("retained"));
        assert!(!cache.contains(&keys[1]).expect("evicted"));
        assert!(
            !root
                .join(format!("{}{ENTRY_SUFFIX}", keys[1].digest()))
                .exists()
        );
        assert!(cache.contains(&keys[3]).expect("stored"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_phrase_larger_than_the_budget_is_refused_without_evicting_everything() {
        let root = cache_root("oversize");
        let cache = PhraseCache::with_capacity_bytes(&root, 4_000).expect("open");
        let key = ack_key();
        cache.put(&key, &tone(400), 24_000, 1).expect("store");
        let oversized = PhraseKey::new("qwen3-tts", "Ryan", "24000@100", "long");
        assert!(cache.put(&oversized, &tone(8_000), 24_000, 1).is_err());
        assert!(cache.contains(&key).expect("retained"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn truncated_entries_fail_soft_and_are_forgotten() {
        let root = cache_root("truncated");
        let cache = PhraseCache::open(&root).expect("open");
        let key = ack_key();
        cache.put(&key, &tone(2_400), 24_000, 1).expect("store");
        let path = root.join(format!("{}{ENTRY_SUFFIX}", key.digest()));
        let truncated = fs::read(&path).expect("entry")[..1_000].to_vec();
        fs::write(&path, truncated).expect("truncate");

        assert!(cache.get(&key).is_none(), "truncation must not be fatal");
        assert!(!cache.contains(&key).expect("forgotten"));
        assert!(!path.exists());
        cache
            .put(&key, &tone(2_400), 24_000, 1)
            .expect("re-synthesize");
        assert!(cache.get(&key).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_payloads_and_headers_fail_soft() {
        let root = cache_root("corrupt");
        let cache = PhraseCache::open(&root).expect("open");
        let key = ack_key();
        cache.put(&key, &tone(2_400), 24_000, 1).expect("store");
        let path = root.join(format!("{}{ENTRY_SUFFIX}", key.digest()));
        let mut bytes = fs::read(&path).expect("entry");
        let last = bytes.len().saturating_sub(1);
        bytes[last] ^= 0xff;
        fs::write(&path, &bytes).expect("corrupt");

        assert!(
            cache.get(&key).is_none(),
            "digest mismatch must not be fatal"
        );
        assert!(!cache.contains(&key).expect("forgotten"));
        assert!(decode_entry(b"not an entry").is_none());
        assert!(decode_entry(&[]).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_deleted_entry_and_an_unreadable_index_never_inflate_the_budget() {
        let root = cache_root("reconcile");
        let key = ack_key();
        let cache = PhraseCache::open(&root).expect("open");
        cache.put(&key, &tone(2_400), 24_000, 1).expect("store");
        fs::remove_file(root.join(format!("{}{ENTRY_SUFFIX}", key.digest()))).expect("remove");
        let reopened = PhraseCache::open(&root).expect("reopen");
        assert_eq!(reopened.total_bytes().expect("total"), 0);
        assert!(reopened.digests().expect("digests").is_empty());

        let unaccounted = root.join(format!("{}{ENTRY_SUFFIX}", "0".repeat(64)));
        fs::write(&unaccounted, vec![0_u8; 4_096]).expect("leak an entry");
        let swept = PhraseCache::open(&root).expect("sweep");
        assert_eq!(swept.total_bytes().expect("total"), 0);
        assert!(!unaccounted.exists(), "unaccounted bytes must not survive");

        cache.put(&key, &tone(2_400), 24_000, 1).expect("store");
        fs::write(root.join(INDEX_FILE), b"{ not json").expect("corrupt index");
        let rebuilt = PhraseCache::open(&root).expect("rebuild");
        assert_eq!(rebuilt.digests().expect("digests"), vec![key.digest()]);
        assert!(rebuilt.get(&key).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_pcm_and_budgets_are_rejected_explicitly() {
        let root = cache_root("invalid");
        let cache = PhraseCache::open(&root).expect("open");
        let key = ack_key();
        assert!(cache.put(&key, &[], 24_000, 1).is_err());
        assert!(cache.put(&key, &tone(480), 100, 1).is_err());
        assert!(cache.put(&key, &tone(480), 24_000, 0).is_err());
        assert!(cache.put(&key, &tone(481), 24_000, 2).is_err());
        assert!(PhraseCache::with_capacity_bytes(&root, 0).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
