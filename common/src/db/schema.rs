//! Schema module.
//!
//! The actual database schema is shipped as plain SQL (`schema.sql`) and applied
//! automatically when the database is opened; this file simply re-exports it so
//! existing references inside the crate still compile.
