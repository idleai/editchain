//! HTTP-based embedding backend — calls an OpenAI-compatible `/v1/embeddings` endpoint.
//!
//! Uses curl subprocess with temp files for reliable HTTP.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::{EmbedError, Embedder, EmbeddingManifest};

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

pub struct HttpEmbedder {
    manifest: EmbeddingManifest,
    endpoint: String,
    model: String,
}

#[derive(serde::Serialize)]
struct EmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(serde::Deserialize)]
struct EmbedResponse {
    data: Vec<EmbeddingData>,
}

#[derive(serde::Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

impl HttpEmbedder {
    pub fn new(manifest: EmbeddingManifest, endpoint: String) -> Self {
        let model = manifest.model_id.clone();
        Self { manifest, endpoint, model }
    }

    /// Maximum retries per batch on transient failures.
    const MAX_RETRIES: u32 = 3;

    /// Maximum characters per text — SGLang hangs on certain long sequences
    /// (~6.8K+ tokens with dense tokenization like ANSI codes or JSON arrays).
    const MAX_TEXT_CHARS: usize = 8000;

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        // Truncate texts to avoid SGLang HTTP response hang on long sequences.
        let truncated: Vec<String> = texts.iter()
            .map(|t| if t.len() > Self::MAX_TEXT_CHARS {
                let mut s = t[..Self::MAX_TEXT_CHARS].to_string();
                s.push_str(" [truncated]");
                s
            } else {
                t.clone()
            })
            .collect();

        let body = serde_json::to_string(&EmbedRequest {
            model: self.model.clone(),
            input: truncated,
        })
        .map_err(|e| EmbedError::Backend(e.to_string()))?;

        let url = format!("{}/v1/embeddings", self.endpoint);

        // Write body to a temp file to avoid stdin pipe issues with Rust subprocess.
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_path = format!("/tmp/editchain_embed_{}.json", counter);
        let mut tmp_file = std::fs::File::create(&tmp_path)
            .map_err(|e| EmbedError::Backend(format!("tmpfile create: {}", e)))?;
        tmp_file.write_all(body.as_bytes())
            .map_err(|e| EmbedError::Backend(format!("tmpfile write: {}", e)))?;
        drop(tmp_file);

        // Retry loop for transient failures (timeouts, server overload).
        let mut last_err = None;
        for attempt in 0..Self::MAX_RETRIES {
            if attempt > 0 {
                let delay_ms = 500 * (1 << attempt); // 1s, 2s, 4s
                eprintln!("embed: retry {} after {}ms", attempt, delay_ms);
                std::thread::sleep(Duration::from_millis(delay_ms));
            }

            // Use sh -c to avoid any Rust subprocess pipe lifecycle issues.
            let curl_cmd = format!(
                "/usr/bin/curl -s --connect-timeout 10 --max-time 180 -H 'Content-Type: application/json' -d '@{}' '{}'",
                tmp_path, url
            );
            let output = match Command::new("/bin/sh")
                .arg("-c")
                .arg(&curl_cmd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
            {
                Ok(o) => o,
                Err(e) => {
                    last_err = Some(EmbedError::Backend(format!("curl exec: {}", e)));
                    continue;
                }
            };

            if !output.status.success() {
                let code = output.status.code().unwrap_or(-1);
                if code == 28 && attempt + 1 < Self::MAX_RETRIES {
                    last_err = Some(EmbedError::Backend(format!("timeout (exit=28)")));
                    continue;
                }
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Clean up temp file on failure.
                let _ = std::fs::remove_file(&tmp_path);
                return Err(EmbedError::Backend(format!(
                    "curl failed (exit={}): {}",
                    code,
                    if stderr.len() > 200 { &stderr[..200] } else { &stderr },
                )));
            }

            // Empty response — SGLang sometimes hangs on long sequences without
            // sending a response. Retryable.
            if output.stdout.is_empty() && attempt + 1 < Self::MAX_RETRIES {
                last_err = Some(EmbedError::Backend("empty response".to_string()));
                continue;
            }

            let parsed: EmbedResponse = match serde_json::from_slice(&output.stdout) {
                Ok(p) => p,
                Err(e) => {
                    last_err = Some(EmbedError::Backend(format!("json parse: {}", e)));
                    continue;
                }
            };

            let mut sorted = parsed.data;
            sorted.sort_by_key(|d| d.index);

            let mut results = Vec::with_capacity(sorted.len());
            for data in sorted {
                let mut vec = data.embedding;
                if self.manifest.normalize {
                    let norm_sq: f32 = vec.iter().map(|x| x * x).sum();
                    if norm_sq > 0.0 {
                        let norm = norm_sq.sqrt();
                        for x in &mut vec { *x /= norm; }
                    }
                }
                results.push(vec);
            }

            // Clean up temp file.
            let _ = std::fs::remove_file(&tmp_path);
            return Ok(results);
        }

        // Clean up temp file on failure.
        let _ = std::fs::remove_file(&tmp_path);
        Err(last_err.unwrap_or_else(|| EmbedError::Backend("max retries exceeded".to_string())))
    }
}

impl Embedder for HttpEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.embed_batch(texts)
    }

    fn embed_batches(&self, batches: &[Vec<String>]) -> Result<Vec<Vec<Vec<f32>>>, EmbedError> {
        const PARALLELISM: usize = 64;
        let total = batches.len();
        let results = std::sync::Mutex::new(Vec::with_capacity(total));
        let next_idx = std::sync::atomic::AtomicUsize::new(0);

        // Use scoped threads so we can borrow &self.
        std::thread::scope(|s| {
            for _ in 0..PARALLELISM {
                s.spawn(|| {
                    loop {
                        let i = next_idx.fetch_add(1, Ordering::Relaxed);
                        if i >= total {
                            break;
                        }
                        eprintln!("embed: batch {}/{} ({} texts)", i + 1, total, batches[i].len());
                        match self.embed_batch(&batches[i]) {
                            Ok(vecs) => {
                                results.lock().unwrap().push((i, Ok(vecs)));
                            }
                            Err(e) => {
                                results.lock().unwrap().push((i, Err(e)));
                                // Signal others to stop by advancing next_idx past total.
                                next_idx.store(total, Ordering::Relaxed);
                                break;
                            }
                        }
                    }
                });
            }
        });

        // Collect results in order.
        let mut unordered = results.into_inner().unwrap();
        unordered.sort_by_key(|(i, _)| *i);
        let mut out = Vec::with_capacity(total);
        for (_, result) in unordered {
            match result {
                Ok(vecs) => out.push(vecs),
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn manifest(&self) -> &EmbeddingManifest {
        &self.manifest
    }
}