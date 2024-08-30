use std::{net::SocketAddr, str::FromStr, sync::Arc};

use rustls::{pki_types::PrivatePkcs8KeyDer, KeyLogFile, ServerConfig};

#[derive(Debug, Clone, Copy)]
#[allow(clippy::upper_case_acronyms)]
pub enum Proto {
    H1,
    H2C,
    TLS,
}

pub struct Settings {
    pub listen_addr: SocketAddr,
    pub proto: Proto,
}

impl Settings {
    pub fn from_env() -> eyre::Result<Self> {
        let port = std::env::var("PORT").unwrap_or("0".to_string());
        let addr = std::env::var("ADDR").unwrap_or("127.0.0.1".to_string());
        let listen_addr = SocketAddr::from_str(&format!("{}:{}", addr, port))?;

        let proto = std::env::var("PROTO").unwrap_or("h2c".to_string());
        let proto = match proto.as_str() {
            // plaintext HTTP/1.1
            "h1" => Proto::H1,
            // HTTP/2 with prior knowledge
            "h2c" => Proto::H2C,
            // TLS with ALPN
            "tls" => Proto::TLS,
            _ => panic!("PROTO must be one of 'h1', 'h2c', or 'tls'"),
        };
        Ok(Self { listen_addr, proto })
    }

    pub const LISTEN_LINE_PREFIX: &'static str = "🌎🦊👉";

    pub fn print_listen_line(&self, addr: SocketAddr) {
        println!("🌎🦊👉 {addr} ({:?})", self.proto)
    }

    pub fn decode_listen_line(&self, line: &str) -> eyre::Result<Option<SocketAddr>> {
        let line = match line.strip_prefix(Self::LISTEN_LINE_PREFIX) {
            Some(l) => l,
            None => return Ok(None),
        };
        let addr_token = line
            .split_whitespace()
            .next()
            .ok_or_else(|| eyre::eyre!("No address token found"))?;
        let addr = addr_token
            .parse::<SocketAddr>()
            .map_err(|e| eyre::eyre!("Failed to parse SocketAddr: {}", e))?;
        Ok(Some(addr))
    }

    pub fn gen_rustls_server_config() -> eyre::Result<ServerConfig> {
        let certified_key =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let crt = certified_key.cert.der();
        let key = certified_key.key_pair.serialize_der();

        let mut server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![crt.clone()],
                PrivatePkcs8KeyDer::from(key.clone()).into(),
            )
            .unwrap();
        server_config.key_log = Arc::new(KeyLogFile::new());
        server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(server_config)
    }

    pub fn message_for_404() -> &'static str {
        r#"404 Not Found

This server serves the following routes:

/echo-body — Echoes back the request body.
/status/{code} — Returns a response with the specified status code.
/repeat-4k-blocks/{repeat} — Streams the specified number of 4KB blocks (from memory)
/stream-file/{name} — Streams the contents of a file from `/tmp/stream-file/{name}` — see `scripts/mkfiles.sh`
/"#
    }
}

/// A sample block of 4KiB of data.
pub const SAMPLE_4K_BLOCK: &[u8] = b"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";
