//! `POST /retrieve` — semantic + graph-aware retrieval (S8).
//!
//! Replaces the legacy `/search_vector_base` route. Body shape and
//! response contract are documented in
//! [`docs/reference/retrieve-api.md`](../../../../docs/reference/retrieve-api.md).

pub mod request;
pub mod response;
pub mod retrieve_route;
