//! Load and search the bundled AutoEq headphone catalog (gzipped JSON).

use crate::profile::{sane_preamp, AudioProfile, BandType, FilterBand, MAX_BANDS};
use flate2::read::GzDecoder;
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Serde default for a band's `q` when the catalog JSON omits it.
fn default_q() -> f64 {
    1.0
}

/// One band as it appears in the catalog JSON, before validation.
#[derive(Deserialize)]
struct RawBand {
    #[serde(rename = "type")]
    kind: String,
    frequency: f64,
    gain: f64,
    #[serde(default = "default_q")]
    q: f64,
}

/// One profile entry as it appears in the catalog JSON. Optional fields default
/// so older/partial catalog files still deserialize.
#[derive(Deserialize)]
struct RawProfile {
    key: String,
    name: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    form: String,
    #[serde(default)]
    preamp: f64,
    bands: Vec<RawBand>,
}

/// The top-level catalog JSON document: a list of raw profiles.
#[derive(Deserialize)]
struct RawCatalog {
    #[serde(default)]
    profiles: Vec<RawProfile>,
}

/// Validate one raw catalog entry into a usable [`AudioProfile`], or `None`
/// if it carries no parseable band (an entry with no valid EQ is not useful).
fn to_profile(raw: RawProfile) -> Option<AudioProfile> {
    // Cap and sanitize the bands; drop entries whose type strings are all
    // unsupported shapes.
    let bands: Vec<FilterBand> = raw
        .bands
        .iter()
        .take(MAX_BANDS)
        .filter_map(|b| {
            BandType::parse(&b.kind).map(|k| FilterBand::new(k, b.frequency, b.gain, b.q))
        })
        .collect();
    if bands.is_empty() {
        return None;
    }
    // Compose the one-line `detail` from whatever metadata is present (form factor
    // and/or measurement source), falling back to a generic label.
    let mut detail_bits = Vec::new();
    if !raw.form.is_empty() {
        detail_bits.push(raw.form.clone());
    }
    if !raw.source.is_empty() {
        detail_bits.push(format!("measured by {}", raw.source));
    }
    Some(AudioProfile {
        key: raw.key,
        name: raw.name,
        detail: if detail_bits.is_empty() {
            "AutoEq correction".into()
        } else {
            detail_bits.join(" · ")
        },
        source: raw.source,
        form: raw.form,
        preamp: sane_preamp(raw.preamp),
        bands,
    })
}

/// The bundled, read-only library of measured headphone corrections.
pub struct Catalog {
    entries: Vec<AudioProfile>,
}

/// Cap on the *decompressed* catalog size (the bundled catalog is ~6.5 MB). A
/// gzip stream can expand ~1000x, so an untrusted file pointed at by
/// `$ADTUNE_CATALOG` could otherwise decompress to gigabytes and exhaust memory.
const MAX_CATALOG_BYTES: u64 = 64 << 20;

impl Catalog {
    /// Decompress and deserialize a gzipped-JSON catalog from any reader, keeping
    /// only the entries that validate. Shared by [`Catalog::load`] (a file) and
    /// [`Catalog::bundled`] (the embedded bytes).
    fn parse<R: Read>(reader: R) -> std::io::Result<Catalog> {
        let mut text = String::new();
        // `take` bounds the decompression: a bomb truncates and fails the JSON
        // parse rather than exhausting memory.
        GzDecoder::new(reader)
            .take(MAX_CATALOG_BYTES)
            .read_to_string(&mut text)?;
        let raw: RawCatalog = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Catalog {
            entries: raw.profiles.into_iter().filter_map(to_profile).collect(),
        })
    }

    /// Load and decode the gzipped JSON catalog at `path`.
    pub fn load(path: &Path) -> std::io::Result<Catalog> {
        Self::parse(File::open(path)?)
    }

    /// The catalog compiled into the binary at build time — no external file
    /// needed, so installed apps are self-contained on every platform.
    pub fn bundled() -> Catalog {
        const BYTES: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/autoeq-catalog.json.gz"
        ));
        Self::parse(BYTES).unwrap_or_else(|_| Catalog {
            entries: Vec::new(),
        })
    }

    /// Number of profiles in the catalog.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalog holds no profiles (e.g. a failed bundled load).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a profile by its exact catalog key.
    pub fn get(&self, key: &str) -> Option<&AudioProfile> {
        self.entries.iter().find(|p| p.key == key)
    }

    /// Rank catalog entries by how well they match a free-text query. An empty
    /// `form` matches all form factors.
    pub fn search(&self, query: &str, form: &str, limit: usize) -> Vec<&AudioProfile> {
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let joined = terms.join(" ");
        let mut scored: Vec<(f64, &AudioProfile)> = self
            .entries
            .iter()
            .filter_map(|e| {
                // Form-factor filter, then require every query term to appear in the
                // name-or-source haystack (an AND match, not OR).
                if !form.is_empty() && e.form != form {
                    return None;
                }
                let name = e.name.to_lowercase();
                let hay = format!("{} {}", name, e.source.to_lowercase());
                if !terms.iter().all(|t| hay.contains(t.as_str())) {
                    return None;
                }
                // Rank by how tightly the query matches the name: exact > prefix >
                // substring > term-only match. A small length penalty breaks ties
                // toward shorter (more specific) names.
                let mut score = 0.0;
                if !terms.is_empty() {
                    score = if name == joined {
                        100.0
                    } else if name.starts_with(&joined) {
                        70.0
                    } else if name.contains(&joined) {
                        45.0
                    } else {
                        20.0
                    };
                    score -= (name.len().min(40) as f64) * 0.05;
                }
                Some((score, e))
            })
            .collect();
        // Highest score first; ties resolved by name then source for a stable order.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
                .then_with(|| a.1.source.to_lowercase().cmp(&b.1.source.to_lowercase()))
        });
        scored.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}
