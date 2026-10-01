#[macro_use]
mod macros;

#[path = "."]
mod tests_with_aws_lc_rs {
    provider_aws_lc_rs!();

    #[path = "../server_cert_verifier.rs"]
    mod tests;
}
