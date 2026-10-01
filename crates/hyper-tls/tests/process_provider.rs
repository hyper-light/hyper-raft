//! The process-default provider is the crate's one provider, aws-lc-rs. The default test runner
//! builds each test file into its own executable, so this test sees a fresh process.

use hyper_tls::crypto::CryptoProvider;
use hyper_tls::ClientConfig;

mod common;
use crate::common::*;

#[test]
fn test_aws_lc_rs_used_as_implicit_provider() {
    assert!(CryptoProvider::get_default().is_none());

    // implicitly installs aws-lc-rs provider
    finish_client_config(KeyType::Rsa2048, ClientConfig::builder());

    let default = CryptoProvider::get_default().expect("provider missing");
    let debug = format!("{default:?}");
    assert!(debug.contains("secure_random: AwsLcRs"));

    let builder = ClientConfig::builder();
    assert_eq!(format!("{:?}", builder.crypto_provider()), debug);
}
