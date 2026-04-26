//! Network security foundation: endpoint validation and controlled HTTP client management.
//! This module provides a security boundary for all network interactions,
//! restricting inference endpoints to localhost/loopback addresses only.

use anyhow::{anyhow, Result};
use std::time::Duration;

/// Validated inference endpoint wrapper.
/// Restricts network access to localhost-only endpoints (127.0.0.1, localhost, [::1], or 127.x.x.x).
/// This prevents accidental or malicious access to arbitrary network addresses.
#[derive(Clone)]
pub struct InferenceEndpoint {
    endpoint: String,
    client: reqwest::Client,
}

impl InferenceEndpoint {
    /// Create a new InferenceEndpoint with validation.
    /// Only allows 127.0.0.1, localhost, [::1], or 127.x.x.x addresses.
    pub fn new(endpoint: &str) -> Result<Self> {
        validate_endpoint(endpoint)?;
        let client = Self::new_client();
        Ok(Self {
            endpoint: endpoint.to_string(),
            client,
        })
    }

    /// Get the endpoint as a string reference
    pub fn as_string(&self) -> &str {
        &self.endpoint
    }

    /// Get a reference to the HTTP client
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Create a new reqwest::Client with appropriate timeouts for inference
    fn new_client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(3600))
            .pool_idle_timeout(Duration::from_secs(3600))
            .build()
            .expect("Failed to build HTTP client")
    }
}

/// Validate that an endpoint is a localhost/loopback address.
/// Allowed patterns:
/// - http://127.0.0.1:port
/// - http://127.x.x.x:port (any 127.* IPv4)
/// - http://localhost:port
/// - http://[::1]:port (IPv6 loopback)
fn validate_endpoint(endpoint: &str) -> Result<()> {
    // Parse the URL
    let url = url::Url::parse(endpoint)
        .map_err(|_| anyhow!("Invalid endpoint URL: {}", endpoint))?;

    // Only allow http and https schemes
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(anyhow!(
            "Endpoint must use http:// or https:// scheme, got: {}",
            scheme
        ));
    }

    // Extract host
    let host = url
        .host()
        .ok_or_else(|| anyhow!("Endpoint has no host: {}", endpoint))?;

    // Check if host is localhost or loopback
    match host {
        url::Host::Ipv4(ip) => {
            // Allow 127.x.x.x (loopback range)
            if ip.octets()[0] == 127 {
                Ok(())
            } else {
                Err(anyhow!(
                    "Endpoint must be a localhost address (127.x.x.x), got: {}",
                    ip
                ))
            }
        }
        url::Host::Ipv6(ip) => {
            // Allow ::1 (IPv6 loopback)
            if ip.is_loopback() {
                Ok(())
            } else {
                Err(anyhow!(
                    "Endpoint must be localhost (::1), got: {}",
                    ip
                ))
            }
        }
        url::Host::Domain(domain) => {
            let lower = domain.to_lowercase();
            // Allow "localhost" and local domain variants
            if lower == "localhost" || lower == "localhost.localdomain" {
                Ok(())
            } else {
                Err(anyhow!(
                    "Endpoint domain must be 'localhost', got: {}",
                    domain
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_ipv4_loopback() {
        assert!(validate_endpoint("http://127.0.0.1:11434").is_ok());
        assert!(validate_endpoint("https://127.0.0.1:11434").is_ok());
        assert!(validate_endpoint("http://127.1.2.3:8000").is_ok());
        assert!(validate_endpoint("http://127.255.255.255:9999").is_ok());
    }

    #[test]
    fn test_valid_ipv6_loopback() {
        assert!(validate_endpoint("http://[::1]:11434").is_ok());
        assert!(validate_endpoint("https://[::1]:8000").is_ok());
    }

    #[test]
    fn test_valid_localhost_domain() {
        assert!(validate_endpoint("http://localhost:11434").is_ok());
        assert!(validate_endpoint("https://localhost:8000").is_ok());
        assert!(validate_endpoint("http://localhost.localdomain:3000").is_ok());
    }

    #[test]
    fn test_invalid_remote_ipv4() {
        assert!(validate_endpoint("http://192.168.1.1:11434").is_err());
        assert!(validate_endpoint("http://8.8.8.8:53").is_err());
        assert!(validate_endpoint("http://10.0.0.1:8000").is_err());
    }

    #[test]
    fn test_invalid_remote_domain() {
        assert!(validate_endpoint("http://google.com:80").is_err());
        assert!(validate_endpoint("https://example.com:443").is_err());
        assert!(validate_endpoint("http://ollama.example.com:11434").is_err());
    }

    #[test]
    fn test_invalid_scheme() {
        assert!(validate_endpoint("ftp://127.0.0.1:11434").is_err());
        assert!(validate_endpoint("tcp://localhost:8000").is_err());
    }

    #[test]
    fn test_malformed_url() {
        assert!(validate_endpoint("not-a-url").is_err());
        assert!(validate_endpoint("http://[invalid").is_err());
    }

    #[test]
    fn test_inference_endpoint_new() {
        let ep = InferenceEndpoint::new("http://localhost:11434");
        assert!(ep.is_ok());

        let ep = ep.unwrap();
        assert_eq!(ep.as_string(), "http://localhost:11434");
    }

    #[test]
    fn test_inference_endpoint_rejects_invalid() {
        let ep = InferenceEndpoint::new("http://google.com:80");
        assert!(ep.is_err());
    }

    #[test]
    fn test_inference_endpoint_clone() {
        let ep1 = InferenceEndpoint::new("http://127.0.0.1:11434").unwrap();
        let ep2 = ep1.clone();
        assert_eq!(ep1.as_string(), ep2.as_string());
    }

    // ===== URL with path component =====

    #[test]
    fn test_valid_localhost_with_path() {
        assert!(validate_endpoint("http://localhost:11434/api/generate").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:11434/v1/chat/completions").is_ok());
    }

    #[test]
    fn test_valid_localhost_no_port() {
        // No port is fine — default 80/443
        assert!(validate_endpoint("http://localhost/api").is_ok());
    }

    #[test]
    fn test_valid_ipv4_port_zero() {
        // Port 0 is unusual but syntactically valid
        assert!(validate_endpoint("http://127.0.0.1:0").is_ok());
    }

    #[test]
    fn test_valid_ipv4_max_port() {
        assert!(validate_endpoint("http://127.0.0.1:65535").is_ok());
    }

    #[test]
    fn test_valid_ipv4_full_loopback_range() {
        // 127.0.0.1 through 127.255.255.255 should all be valid
        assert!(validate_endpoint("http://127.0.0.2:8080").is_ok());
        assert!(validate_endpoint("http://127.128.0.1:9000").is_ok());
        assert!(validate_endpoint("http://127.255.255.254:1234").is_ok());
    }

    #[test]
    fn test_invalid_ipv4_zero_address() {
        assert!(validate_endpoint("http://0.0.0.0:8080").is_err());
    }

    #[test]
    fn test_invalid_ipv4_broadcast() {
        assert!(validate_endpoint("http://255.255.255.255:80").is_err());
    }

    #[test]
    fn test_invalid_ipv4_private_class_a() {
        assert!(validate_endpoint("http://10.0.0.1:8080").is_err());
    }

    #[test]
    fn test_invalid_ipv4_private_class_b() {
        assert!(validate_endpoint("http://172.16.0.1:8080").is_err());
    }

    #[test]
    fn test_invalid_ipv4_private_class_c() {
        assert!(validate_endpoint("http://192.168.0.1:8080").is_err());
    }

    #[test]
    fn test_invalid_ipv6_non_loopback() {
        assert!(validate_endpoint("http://[::2]:8080").is_err());
        assert!(validate_endpoint("http://[fe80::1]:8080").is_err());
        assert!(validate_endpoint("http://[2001:db8::1]:8080").is_err());
    }

    #[test]
    fn test_invalid_domain_subdomain_of_localhost() {
        // "api.localhost" is NOT the same as "localhost"
        assert!(validate_endpoint("http://api.localhost:8080").is_err());
    }

    #[test]
    fn test_invalid_domain_with_localhost_substring() {
        // "notlocalhost.com" must not be accepted
        assert!(validate_endpoint("http://notlocalhost.com:80").is_err());
        assert!(validate_endpoint("http://localhost.example.com:80").is_err());
    }

    #[test]
    fn test_invalid_empty_string() {
        assert!(validate_endpoint("").is_err());
    }

    #[test]
    fn test_invalid_just_scheme() {
        assert!(validate_endpoint("http://").is_err());
    }

    #[test]
    fn test_invalid_no_scheme() {
        assert!(validate_endpoint("localhost:11434").is_err());
    }

    #[test]
    fn test_invalid_file_scheme() {
        assert!(validate_endpoint("file:///etc/passwd").is_err());
    }

    #[test]
    fn test_invalid_data_uri() {
        assert!(validate_endpoint("data:text/plain,hello").is_err());
    }

    #[test]
    fn test_valid_https_localhost() {
        assert!(validate_endpoint("https://localhost:8443").is_ok());
        assert!(validate_endpoint("https://127.0.0.1:8443").is_ok());
    }

    #[test]
    fn test_valid_https_ipv6_loopback() {
        assert!(validate_endpoint("https://[::1]:8443").is_ok());
    }

    #[test]
    fn test_localhost_localdomain_valid() {
        assert!(validate_endpoint("http://localhost.localdomain:8080").is_ok());
    }

    #[test]
    fn test_inference_endpoint_exposes_client() {
        let ep = InferenceEndpoint::new("http://127.0.0.1:11434").unwrap();
        // client() must return a valid client reference without panicking
        let _client = ep.client();
    }

    #[test]
    fn test_inference_endpoint_as_string_roundtrip() {
        let url = "http://localhost:11434";
        let ep = InferenceEndpoint::new(url).unwrap();
        assert_eq!(ep.as_string(), url);
    }
}
