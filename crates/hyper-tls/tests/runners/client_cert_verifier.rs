#[macro_use]
mod macros;

#[path = "."]
mod tests_with_aws_lc_rs {
    provider_aws_lc_rs!();

    #[path = "../client_cert_verifier.rs"]
    mod tests;
}
