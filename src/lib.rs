//! Rust client implementation for backpage of DarkOrbit's `*.darkorbit.com`
//! AJAX endpoints driven by a `dosid` session cookie.
//!
//! - [`login`] — username/password or server+sid login
//! - [`launch`] — preloader embed and authenticated Flash contract
//! - [`client::BackpageClient`] — rate-limited session HTTP client
//! - [`ops`] — hangar, auction, captain (P.E.T.), shop and user endpoints

pub mod base62;
pub mod client;
pub mod error;
pub mod launch;
pub mod login;
pub mod ops;
pub mod proxy;
pub mod session;

pub use client::{BackpageClient, Param, SidStatus, USER_AGENT};
pub use error::{Error, Result};
pub use proxy::{ProxyAuth, ProxyConfig};
pub use session::Session;
