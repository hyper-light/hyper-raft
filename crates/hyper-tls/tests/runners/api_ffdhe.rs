#[macro_use]
mod macros;

#[path = "."]
mod tests_with_aws_lc_rs {
    provider_aws_lc_rs!();

    #[path = "../api_ffdhe.rs"]
    mod tests;
}
