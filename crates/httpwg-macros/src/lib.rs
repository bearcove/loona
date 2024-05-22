// TODO: generate this macro from httpwg sources.
// The macros will be split in a separte `httpwg-macros` crate.
// It can probably be generated using the "types as JSON" output
// used by cargo-semver-checks?
//
// Right now it's too fragile, too easy to slip up and forget
// a test case / have a test name-test case mismatch. Also it
// cannot be used as a standalone CLI.
//
// Example invocation: cargo +nightly rustdoc -Z unstable-options --output-format json -p httpwg --target-dir /tmp/json

#[macro_export]
macro_rules! gen_tests {
    ($body: tt) => {
        #[cfg(test)]
        mod rfc9113 {
            use ::httpwg::rfc9113 as __rfc;

            mod _3_starting_http2 {
                use super::__rfc::_3_starting_http2 as __suite;

                #[test]
                fn sends_client_connection_preface() {
                    use __suite::sends_client_connection_preface as test;
                    $body
                }

                #[test]
                fn sends_invalid_connection_preface() {
                    use __suite::sends_invalid_connection_preface as test;
                    $body
                }
            }
        }
    };
}
