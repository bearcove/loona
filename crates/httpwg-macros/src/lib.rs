//! Macros to help generate code for all suites/groups/tests of the httpwg crate

// This file is automatically @generated by httpwg-gen
// It is not intended for manual editing

/// This generates a module tree with some #[test] functions.
/// The `$body` argument is pasted inside those unit test, and
/// in that scope, `test` is the `httpwg` function you can use
/// to run the test (that takes a `mut conn: Conn<IO>`)
#[macro_export]
macro_rules! tests {
    ($body: tt) => {
        #[cfg(test)]
        mod rfc9113 {
            use httpwg::rfc9113 as __suite;

            mod _3_starting_http2 {
                use httpwg::rfc9113 as __suite;

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

            mod _4_2_frame_size {
                use httpwg::rfc9113 as __suite;

                #[test]
                fn frame_exceeding_max_size() {
                    use __suite::frame_exceeding_max_size as test;
                    $body
                }
            }
        }
    };
}