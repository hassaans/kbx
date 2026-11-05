//! The local embedding model of the built-in search engine, and where it comes
//! from. See `docs/search-engine.md`.

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

/// Names a folder holding the model's files, to use instead of downloading
/// them: for a machine with no network, and for the tests.
pub const MODEL_DIR_VAR: &str = "KBX_MODEL_DIR";

/// Names a mirror to download the model's files from, in place of Hugging Face.
pub const MODEL_URL_VAR: &str = "KBX_MODEL_URL";

/// Longer than this says nothing more: the model reads [`MAX_TOKENS`] of it.
const MAX_CHARS: usize = 8000;

/// The most a BERT model can read.
const MAX_TOKENS: usize = 512;

/// Texts per forward pass.
const BATCH: usize = 16;

/// How long a silent connection is given, so that no network fails quickly.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One file of a [`Model`].
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    /// The name, in the repository and on disk.
    pub name: &'static str,
    /// Its SHA-256, so that a download is the file that was tested.
    pub sha256: &'static str,
}

/// How a model's token vectors become one vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// The first token's.
    Cls,
}

/// A model on Hugging Face, pinned to a revision.
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// The repository.
    pub repo: &'static str,
    /// The commit.
    pub revision: &'static str,
    /// What its files come to, for saying so before a download.
    pub megabytes: u32,
    /// How it pools.
    pub pooling: Pooling,
    /// Put before a free-text query, as the model was trained.
    pub query_prefix: &'static str,
    /// What to fetch.
    pub files: [ModelFile; 3],
}

impl Model {
    /// What says which vectors these are: vectors of another model are useless.
    pub fn id(&self) -> String {
        format!("{}@{}", self.repo, self.revision)
    }

    /// Where the files are or would go: [`MODEL_DIR_VAR`], or else the user's
    /// cache folder. `None` if this machine has neither.
    pub fn folder(&self) -> Option<PathBuf> {
        if let Some(dir) = env::var_os(MODEL_DIR_VAR) {
            return Some(PathBuf::from(dir));
        }
        let name = self.repo.replace('/', "--");
        let cache = dirs::cache_dir()?;
        Some(
            cache
                .join("kbx")
                .join("models")
                .join(name)
                .join(self.revision),
        )
    }

    /// Whether every file is there, so that loading needs no network.
    pub fn is_present(&self) -> bool {
        self.folder()
            .is_some_and(|dir| self.files.iter().all(|file| dir.join(file.name).is_file()))
    }

    /// Fetches what's missing. A folder the user named is left as it is.
    pub fn fetch(&self) -> Result<PathBuf, EmbedError> {
        let dir = self.folder().ok_or(EmbedError::NoCacheFolder)?;
        if env::var_os(MODEL_DIR_VAR).is_some() || self.is_present() {
            return Ok(dir);
        }
        let base = env::var(MODEL_URL_VAR).unwrap_or_else(|_| {
            format!(
                "https://huggingface.co/{}/resolve/{}",
                self.repo, self.revision
            )
        });
        fs::create_dir_all(&dir).map_err(|error| EmbedError::Io(dir.clone(), error))?;
        for file in &self.files {
            let path = dir.join(file.name);
            if !path.is_file() {
                download(&format!("{base}/{}", file.name), file.sha256, &path)?;
            }
        }
        Ok(dir)
    }
}

/// The model the built-in engine uses. See `docs/search-engine.md` for why.
pub const MODEL: Model = Model {
    repo: "BAAI/bge-small-en-v1.5",
    revision: "5c38ec7c405ec4b44b94cc5a9bb96e735b38267a",
    megabytes: 134,
    pooling: Pooling::Cls,
    query_prefix: "Represent this sentence for searching relevant passages: ",
    files: [
        ModelFile {
            name: "config.json",
            sha256: "094f8e891b932f2000c92cfc663bac4c62069f5d8af5b5278c4306aef3084750",
        },
        ModelFile {
            name: "tokenizer.json",
            sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
        },
        ModelFile {
            name: "model.safetensors",
            sha256: "3c9f31665447c8911517620762200d2245a2518d6e7208acc78cd9db317e21ad",
        },
    ],
};

/// Fetches `url` to `path`, which appears only once it's whole and is the file
/// `sha256` says it should be.
pub fn download(url: &str, sha256: &str, path: &Path) -> Result<(), EmbedError> {
    let failed = |error: String| EmbedError::Download(url.to_string(), error);
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .build()
        .new_agent();
    let response = agent.get(url).call().map_err(|e| failed(e.to_string()))?;
    let mut body = response.into_body().into_reader();

    let dir = path.parent().unwrap_or(Path::new("."));
    let io_error = |error| EmbedError::Io(path.to_path_buf(), error);
    let mut staged = NamedTempFile::new_in(dir).map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = body.read(&mut buffer).map_err(|e| failed(e.to_string()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        staged.write_all(&buffer[..read]).map_err(io_error)?;
    }
    let found: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if found != sha256 {
        return Err(failed(format!("expected SHA-256 {sha256}, got {found}")));
    }
    staged.persist(path).map_err(|e| io_error(e.error))?;
    Ok(())
}

/// Why there are no vectors.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// This machine has no cache folder to put a model in.
    #[error("no cache folder to keep the model in; set {MODEL_DIR_VAR}")]
    NoCacheFolder,
    /// A file couldn't be downloaded.
    #[error("could not download {0}: {1}")]
    Download(String, String),
    /// A file couldn't be read or written.
    #[error("{0}: {1}")]
    Io(PathBuf, io::Error),
    /// The model's files make no sense to it.
    #[error("the embedding model at {0}: {1}")]
    Load(PathBuf, String),
    /// The model failed on some text.
    #[error("the embedding model: {0}")]
    Run(String),
}

/// Turns text into vectors of unit length, so that a dot product is a cosine.
pub trait Embedder {
    /// What says which vectors these are.
    fn id(&self) -> &str;
    /// A vector per text, for text to be found.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
    /// The vector of a free-text query.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError>;
}

/// A BERT model on the CPU.
pub struct BertEmbedder {
    id: String,
    model: BertModel,
    tokenizer: Tokenizer,
    pooling: Pooling,
    query_prefix: &'static str,
}

impl BertEmbedder {
    /// Loads `model`'s files from `dir`.
    pub fn load(model: &Model, dir: &Path) -> Result<BertEmbedder, EmbedError> {
        let failed = |error: String| EmbedError::Load(dir.to_path_buf(), error);
        let file = dir.join("config.json");
        let config = fs::read_to_string(&file).map_err(|error| EmbedError::Io(file, error))?;
        let config: Config = serde_json::from_str(&config).map_err(|e| failed(e.to_string()))?;

        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|error| failed(error.to_string()))?;
        tokenizer.with_padding(Some(PaddingParams::default()));
        let truncation = TruncationParams {
            max_length: MAX_TOKENS,
            ..TruncationParams::default()
        };
        tokenizer
            .with_truncation(Some(truncation))
            .map_err(|error| failed(error.to_string()))?;

        let weights = [dir.join("model.safetensors")];
        // Safe as long as nothing rewrites the file while it's mapped, and a
        // download replaces a file rather than writing into it.
        let weights =
            unsafe { VarBuilder::from_mmaped_safetensors(&weights, DType::F32, &Device::Cpu) };
        let weights = weights.map_err(|error| failed(error.to_string()))?;
        let bert = BertModel::load(weights, &config).map_err(|e| failed(e.to_string()))?;
        Ok(BertEmbedder {
            id: model.id(),
            model: bert,
            tokenizer,
            pooling: model.pooling,
            query_prefix: model.query_prefix,
        })
    }

    fn batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let run = |error: String| EmbedError::Run(error);
        let cut: Vec<&str> = texts.iter().map(|text| clipped(text)).collect();
        let encodings = self
            .tokenizer
            .encode_batch(cut, true)
            .map_err(|error| run(error.to_string()))?;
        let tensors = || -> candle_core::Result<Vec<Vec<f32>>> {
            let rows = |ids: &dyn Fn(usize) -> Vec<u32>| {
                let rows: Vec<Tensor> = (0..encodings.len())
                    .map(|row| Tensor::new(ids(row), &Device::Cpu))
                    .collect::<candle_core::Result<_>>()?;
                Tensor::stack(&rows, 0)
            };
            let ids = rows(&|row| encodings[row].get_ids().to_vec())?;
            let mask = rows(&|row| encodings[row].get_attention_mask().to_vec())?;
            let types = ids.zeros_like()?;
            let states = self.model.forward(&ids, &types, Some(&mask))?;
            let pooled = match self.pooling {
                Pooling::Cls => states.i((.., 0))?,
            };
            let norm = pooled.sqr()?.sum_keepdim(1)?.sqrt()?;
            pooled.broadcast_div(&norm)?.to_vec2()
        };
        tensors().map_err(|error| run(error.to_string()))
    }
}

impl Embedder for BertEmbedder {
    fn id(&self) -> &str {
        &self.id
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            vectors.extend(self.batch(batch)?);
        }
        Ok(vectors)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let query = format!("{}{text}", self.query_prefix);
        let vectors = self.batch(&[&query])?;
        vectors
            .into_iter()
            .next()
            .ok_or(EmbedError::Run("no vector".to_string()))
    }
}

/// `text` up to [`MAX_CHARS`], on a character boundary.
fn clipped(text: &str) -> &str {
    match text.char_indices().nth(MAX_CHARS) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}
