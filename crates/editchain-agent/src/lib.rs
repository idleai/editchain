#![doc = "Editchain agent crate for SWE-bench evaluation."]

// Suppress warnings for unused crate dependencies that are pulled in for
// side-effect initialization or are re-exports used by other crates.
use chrono as _;
use clap as _;
use editchain_codec as _;
use editchain_core as _;
use minijinja as _;
use reqwest as _;
use serde as _;
use serde_yaml as _;
use tracing_subscriber as _;
use uuid as _;

/// Agent loop -- parallels mini-swe-agent's `DefaultAgent`.
pub mod agent;

/// Configuration types matching mini-swe-agent's YAML structure.
pub mod config;

/// Docker-based execution environment.
pub mod docker_env;

/// OpenAI-compatible model client and response types.
pub mod model;

/// SWE-bench instance runner.
pub mod swebench;

/// Editchain-op-based trajectory recorder.
pub mod trajectory;
