//! FTPS implementation of [`RemoteClient`] — FTP over TLS, with trust-on-first-use
//! certificate pinning.
//!
//! The command logic is shared verbatim with plain FTP (the `op_*` helpers in
//! [`crate::ftp`]); only the connection setup differs: this client wraps the
//! transport in TLS (explicit `AUTH TLS` by default, implicit on connect as an
//! option) and **encrypts the data channel too** (`PBSZ 0` + `PROT P`).
//!
//! ## Certificate trust — the FTPS analogue of SSH host-key TOFU
//!
//! A CA-valid chain is accepted silently. Anything else (self-signed, private CA,
//! pinned) is gated by the SHA-256 fingerprint of the leaf certificate against a
//! [`KnownHosts`]-backed `known_certs` store: a recorded match is trusted, an
//! unknown/changed fingerprint prompts the user (the same
//! [`ServerTrustPrompt`] the SSH path uses, with
//! [`ServerTrustKind::Certificate`]). Because rustls' verifier is synchronous, we
//! cannot await the prompt mid-handshake: the verifier instead *captures* the
//! untrusted fingerprint and fails the handshake; [`connect`] then prompts, and on
//! acceptance persists the fingerprint and retries (the verifier now finds it
//! trusted).
//!
//! [`connect`]: RemoteClient::connect

use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use nyx_core::{
    EntryKind, FtpsMode, NyxError, RemoteEntry, RemotePath, Result, ServerTrustKind,
    TransferProgress,
};
use sha2::{Digest, Sha256};
use suppaftp::tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream};
use suppaftp::tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use suppaftp::tokio_rustls::rustls::client::WebPkiServerVerifier;
use suppaftp::tokio_rustls::rustls::crypto::{ring, CryptoProvider};
use suppaftp::tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use suppaftp::tokio_rustls::rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme,
};
use suppaftp::tokio_rustls::TlsConnector;
use suppaftp::Status;
use tokio::sync::Mutex;

use crate::ftp::{
    map_connect_err, map_ftp_err, op_default_dir, op_download, op_exists, op_list_dir,
    op_remote_size, op_remove, op_setup, op_target_kind, op_upload, op_walk_dir,
};
use crate::host_key::ServerTrustPrompt;
use crate::known_hosts::{KnownHostStatus, KnownHosts};
use crate::util::reject_offset;
use crate::{DirWalk, RemoteClient};

/// An FTPS client (FTP over TLS).
///
/// Construct with [`FtpsClient::new`], then drive via the [`RemoteClient`] trait.
pub struct FtpsClient {
    host: String,
    port: u16,
    username: String,
    /// The login password. Held only until [`RemoteClient::connect`] consumes it,
    /// then cleared. Never logged.
    password: String,
    mode: FtpsMode,
    /// The TOFU store of accepted (self-signed / pinned) certificate fingerprints,
    /// keyed by host — the certificate parallel to `known_hosts`.
    known_certs: KnownHosts,
    prompt: Arc<dyn ServerTrustPrompt>,
    stream: Mutex<Option<AsyncRustlsFtpStream>>,
}

impl FtpsClient {
    /// Create a new, unconnected FTPS client.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        password: impl Into<String>,
        mode: FtpsMode,
        known_certs: KnownHosts,
        prompt: Arc<dyn ServerTrustPrompt>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            password: password.into(),
            mode,
            known_certs,
            prompt,
            stream: Mutex::new(None),
        }
    }

    /// Build a rustls TLS connector whose verifier accepts CA-valid chains and,
    /// failing that, captures the leaf fingerprint into `captured` for TOFU.
    fn connector(&self, captured: Arc<StdMutex<Option<String>>>) -> Result<AsyncRustlsConnector> {
        // Explicit ring provider so we never depend on a process-wide default
        // being installed (none is, in this app).
        let provider = Arc::new(ring::default_provider());
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(|e| NyxError::Other(format!("tls verifier: {e}")))?;
        let verifier = Arc::new(TofuVerifier {
            webpki,
            provider: provider.clone(),
            known_certs: self.known_certs.clone(),
            host: self.host.clone(),
            captured,
        });
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| NyxError::Other(format!("tls config: {e}")))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Ok(AsyncRustlsConnector::from(TlsConnector::from(Arc::new(
            config,
        ))))
    }

    /// One TLS handshake + login attempt with the given connector.
    async fn handshake(&self, connector: AsyncRustlsConnector) -> Result<AsyncRustlsFtpStream> {
        let addr = format!("{}:{}", self.host, self.port);
        let mut stream = match self.mode {
            FtpsMode::Explicit => {
                let plain = AsyncRustlsFtpStream::connect(&addr)
                    .await
                    .map_err(map_connect_err)?;
                // `into_secure` performs AUTH TLS then PBSZ 0 + PROT P, so the data
                // channel is encrypted; a server that refuses PROT P fails here.
                plain
                    .into_secure(connector, &self.host)
                    .await
                    .map_err(map_connect_err)?
            }
            FtpsMode::Implicit => {
                let mut stream =
                    AsyncRustlsFtpStream::connect_secure_implicit(&addr, connector, &self.host)
                        .await
                        .map_err(map_connect_err)?;
                // Implicit connect does not negotiate data-channel protection, so
                // assert it explicitly: a clear data channel is a silent downgrade.
                assert_data_protection(&mut stream).await?;
                stream
            }
        };
        stream
            .login(self.username.as_str(), self.password.as_str())
            .await
            .map_err(map_ftp_err)?;
        op_setup(&mut stream).await?;
        Ok(stream)
    }
}

#[async_trait]
impl RemoteClient for FtpsClient {
    async fn connect(&mut self) -> Result<()> {
        // Two attempts at most: the first may capture an untrusted leaf cert; if
        // the user trusts it we persist the fingerprint and retry (the verifier
        // then finds it trusted). A second untrusted capture is a hard failure.
        for attempt in 0..2 {
            let captured: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
            let connector = self.connector(captured.clone())?;
            match self.handshake(connector).await {
                Ok(stream) => {
                    self.password.clear();
                    *self.stream.lock().await = Some(stream);
                    return Ok(());
                }
                Err(err) => {
                    let fingerprint = captured.lock().unwrap().take();
                    match (attempt, fingerprint) {
                        (0, Some(fingerprint)) => {
                            if self
                                .prompt
                                .confirm_unknown(
                                    &self.host,
                                    &fingerprint,
                                    ServerTrustKind::Certificate,
                                )
                                .await
                            {
                                self.known_certs
                                    .trust(&self.host, &fingerprint)
                                    .map_err(|e| NyxError::Io(e.to_string()))?;
                                continue;
                            }
                            return Err(NyxError::HostKey(
                                "server certificate rejected".to_string(),
                            ));
                        }
                        _ => return Err(err),
                    }
                }
            }
        }
        unreachable!("the connect loop returns on both attempts")
    }

    async fn default_dir(&self) -> Result<RemotePath> {
        let mut guard = self.stream.lock().await;
        op_default_dir(connected(&mut guard)?).await
    }

    async fn list_dir(&self, path: &RemotePath) -> Result<Vec<RemoteEntry>> {
        let mut guard = self.stream.lock().await;
        op_list_dir(connected(&mut guard)?, path).await
    }

    async fn walk_dir(&self, root: &RemotePath) -> Result<DirWalk> {
        let mut guard = self.stream.lock().await;
        op_walk_dir(connected(&mut guard)?, root).await
    }

    async fn target_kind(&self, path: &RemotePath) -> Result<EntryKind> {
        let mut guard = self.stream.lock().await;
        op_target_kind(connected(&mut guard)?, path).await
    }

    async fn exists(&self, path: &RemotePath) -> Result<bool> {
        let mut guard = self.stream.lock().await;
        op_exists(connected(&mut guard)?, path).await
    }

    async fn remote_size(&self, path: &RemotePath) -> Option<u64> {
        let mut guard = self.stream.lock().await;
        op_remote_size(guard.as_mut()?, path).await
    }

    async fn download(
        &self,
        remote: &RemotePath,
        local: &Path,
        progress: &TransferProgress,
        offset: u64,
    ) -> Result<()> {
        // FTPS resume (REST) is a follow-up; `supports_resume` is false, so a
        // non-zero offset never reaches here — reject it defensively.
        reject_offset(offset)?;
        let mut guard = self.stream.lock().await;
        op_download(connected(&mut guard)?, remote, local, progress).await
    }

    async fn upload(
        &self,
        local: &Path,
        remote: &RemotePath,
        progress: &TransferProgress,
        offset: u64,
    ) -> Result<()> {
        reject_offset(offset)?;
        let mut guard = self.stream.lock().await;
        op_upload(connected(&mut guard)?, local, remote, progress).await
    }

    async fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        let mut guard = self.stream.lock().await;
        connected(&mut guard)?
            .rename(from.as_str(), to.as_str())
            .await
            .map_err(map_ftp_err)
    }

    async fn remove(&self, path: &RemotePath) -> Result<()> {
        let mut guard = self.stream.lock().await;
        op_remove(connected(&mut guard)?, path).await
    }

    async fn mkdir(&self, path: &RemotePath) -> Result<()> {
        let mut guard = self.stream.lock().await;
        connected(&mut guard)?
            .mkdir(path.as_str())
            .await
            .map_err(map_ftp_err)
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.lock().await.take() {
            let _ = stream.quit().await;
        }
        Ok(())
    }
}

/// The live TLS stream out of a held lock guard, or a connection error.
fn connected(guard: &mut Option<AsyncRustlsFtpStream>) -> Result<&mut AsyncRustlsFtpStream> {
    guard
        .as_mut()
        .ok_or_else(|| NyxError::Connection("not connected".into()))
}

/// Send `PBSZ 0` + `PROT P` so the data channel is encrypted (used after an
/// implicit connect, which doesn't negotiate it). A server that refuses `PROT P`
/// is a hard error — we never silently fall back to a clear data channel.
async fn assert_data_protection(stream: &mut AsyncRustlsFtpStream) -> Result<()> {
    stream
        .custom_command("PBSZ 0", &[Status::CommandOk])
        .await
        .map_err(map_ftp_err)?;
    stream
        .custom_command("PROT P", &[Status::CommandOk])
        .await
        .map_err(map_ftp_err)?;
    Ok(())
}

/// A rustls certificate verifier that accepts a CA-valid chain and otherwise
/// falls back to trust-on-first-use on the leaf fingerprint.
#[derive(Debug)]
struct TofuVerifier {
    webpki: Arc<WebPkiServerVerifier>,
    provider: Arc<CryptoProvider>,
    known_certs: KnownHosts,
    host: String,
    /// Set to the leaf fingerprint when an untrusted cert is seen, so `connect`
    /// can prompt the user out-of-band (the verifier itself can't await).
    captured: Arc<StdMutex<Option<String>>>,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        // A normal, CA-valid chain needs no prompt.
        if self
            .webpki
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .is_ok()
        {
            return Ok(ServerCertVerified::assertion());
        }
        // Otherwise gate on the pinned leaf fingerprint (TOFU).
        let fingerprint = fingerprint(end_entity);
        match self.known_certs.check(&self.host, &fingerprint) {
            KnownHostStatus::Match => Ok(ServerCertVerified::assertion()),
            KnownHostStatus::Unknown | KnownHostStatus::Mismatch => {
                *self.captured.lock().unwrap() = Some(fingerprint);
                Err(TlsError::General("untrusted server certificate".into()))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        // Signature checks only need the leaf's public key, not chain trust — so
        // delegating to the provider's algorithms is correct even for a pinned
        // self-signed cert.
        suppaftp::tokio_rustls::rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        suppaftp::tokio_rustls::rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// The `SHA256:<hex>` fingerprint of a leaf certificate (DER), the pinned
/// identity stored in `known_certs`. A certificate is public, so this is not a
/// secret.
fn fingerprint(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let mut out = String::with_capacity(7 + digest.len() * 2);
    out.push_str("SHA256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_sha256_hex() {
        let cert = CertificateDer::from(vec![1u8, 2, 3, 4]);
        let fp = fingerprint(&cert);
        assert!(fp.starts_with("SHA256:"));
        // SHA-256 of the 4 bytes above, hex-encoded.
        assert_eq!(
            fp,
            "SHA256:9f64a747e1b97f131fabb6b447296c9b6f0201e79fb3c5356e6c77e89b6a806a"
        );
    }
}
