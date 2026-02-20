//! Core library for `xet-ai`.
//!
//! This crate provides the orchestration and storage primitives used by the CLI:
//! configuration loading, git integration, pointer indexing, reachability planning,
//! remote storage abstraction, and sync/publish flows.

pub mod commands;
pub mod config;
pub mod git;
pub mod pointers;
pub mod reachability;
pub mod remote;
pub mod repo;
pub mod sync;
