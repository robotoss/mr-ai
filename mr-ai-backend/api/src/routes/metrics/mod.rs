//! Prometheus `/metrics` endpoint. Open route (no auth) so a scraper
//! can poll without sharing the admin token. Network-level isolation
//! (firewall / k8s NetworkPolicy / VPC) is expected to keep this
//! endpoint inaccessible from outside.

pub mod metrics_route;
