//! Core library for the Relational Database MCP (Model Context Protocol) server.
//!
//! Provides the database abstraction layer, application error types, and the
//! [`ServerHandler`](rmcp::ServerHandler) implementation exposing relational
//! databases (MySQL, PostgreSQL, SQLite) as MCP tools and resources.
//!
//! Transport selection and process startup are the responsibility of the binary
//! crates that depend on this library.

pub mod db;
pub mod error;
pub mod server;
