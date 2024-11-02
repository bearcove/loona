//! Exposes the struct `Decoder` that allows for HPACK-encoded header blocks to
//! be decoded into a header list.
//!
//! The decoder only follows HPACK rules, without performing any additional
//! (semantic) checks on the header name/value pairs, i.e. it considers the
//! headers as opaque octets.
//!
//! # Example
//!
//! A simple example of using the decoder that demonstrates its API:
//!
//! ```rust
//! use loona_hpack::Decoder;
//! let mut decoder = Decoder::new();
//!
//! let header_list = decoder.decode(&[0x82, 0x84]).unwrap();
//!
//! assert_eq!(header_list, [
//!     (b":method".to_vec(), b"GET".to_vec()),
//!     (b":path".to_vec(), b"/".to_vec()),
//! ]);
//! ```
//!
//! A more complex example where the callback API is used, providing the client
//! a borrowed representation of each header, rather than an owned
//! representation.
//!
//! ```rust
//! use loona_hpack::Decoder;
//! let mut decoder = Decoder::new();
//!
//! let mut count = 0;
//! let header_list = decoder.decode_with_cb(&[0x82, 0x84], |name, value| {
//!     count += 1;
//!     match count {
//!         1 => {
//!             assert_eq!(&name[..], &b":method"[..]);
//!             assert_eq!(&value[..], &b"GET"[..]);
//!         },
//!         2 => {
//!             assert_eq!(&name[..], &b":path"[..]);
//!             assert_eq!(&value[..], &b"/"[..]);
//!         },
//!         _ => panic!("Did not expect more than two headers!"),
//!     };
//! });
//! ```

use std::borrow::Cow;
use std::num::Wrapping;

use tracing::trace;

use super::huffman::HuffmanDecoder;
use super::huffman::HuffmanDecoderError;

use super::STATIC_TABLE;
use super::{HeaderTable, StaticTable};

/// Decodes an integer encoded with a given prefix size (in bits).
/// Assumes that the buffer `buf` contains the integer to be decoded,
/// with the first byte representing the octet that contains the
/// prefix.
///
/// Returns a tuple representing the decoded integer and the number
/// of bytes from the buffer that were used.
fn decode_integer(buf: &[u8], prefix_size: u8) -> Result<(usize, usize), DecoderError> {
    if !(1..=8).contains(&prefix_size) {
        return Err(DecoderError::IntegerDecodingError(
            IntegerDecodingError::InvalidPrefix,
        ));
    }
    if buf.is_empty() {
        return Err(DecoderError::IntegerDecodingError(
            IntegerDecodingError::NotEnoughOctets,
        ));
    }

    // Make sure there's no overflow in the shift operation
    let Wrapping(mask) = if prefix_size == 8 {
        Wrapping(0xFF)
    } else {
        Wrapping(1u8 << prefix_size) - Wrapping(1)
    };
    let mut value = (buf[0] & mask) as usize;
    if value < (mask as usize) {
        // Value fits in the prefix bits.
        return Ok((value, 1));
    }

    // The value does not fit into the prefix bits, so we read as many following
    // bytes as necessary to decode the integer.
    // Already one byte used (the prefix)
    let mut total = 1;
    let mut m = 0;
    // The octet limit is chosen such that the maximum allowed *value* can
    // never overflow an unsigned 32-bit integer. The maximum value of any
    // integer that can be encoded with 5 octets is ~2^28
    let octet_limit = 5;

    for &b in buf[1..].iter() {
        total += 1;
        value += ((b & 127) as usize) * (1 << m);
        m += 7;

        if b & 128 != 128 {
            // Most significant bit is not set => no more continuation bytes
            return Ok((value, total));
        }

        if total == octet_limit {
            // The spec tells us that we MUST treat situations where the
            // encoded representation is too long (in octets) as an error.
            return Err(DecoderError::IntegerDecodingError(
                IntegerDecodingError::TooManyOctets,
            ));
        }
    }

    // If we have reached here, it means the buffer has been exhausted without
    // hitting the termination condition.
    Err(DecoderError::IntegerDecodingError(
        IntegerDecodingError::NotEnoughOctets,
    ))
}

/// Decodes an octet string under HPACK rules of encoding found in the given
/// buffer `buf`.
///
/// It is assumed that the first byte in the buffer represents the start of the
/// encoded octet string.
///
/// Returns the decoded string in a newly allocated `Vec` and the number of
/// bytes consumed from the given buffer.
fn decode_string(buf: &[u8]) -> Result<(Cow<'_, [u8]>, usize), DecoderError> {
    let (len, consumed) = decode_integer(buf, 7)?;
    trace!("decode_string: Consumed = {}, len = {}", consumed, len);
    if consumed + len > buf.len() {
        return Err(DecoderError::StringDecodingError(
            StringDecodingError::NotEnoughOctets,
        ));
    }
    let raw_string = &buf[consumed..consumed + len];
    if buf[0] & 128 == 128 {
        trace!("decode_string: Using the Huffman code");
        // Huffman coding used: pass the raw octets to the Huffman decoder
        // and return its result.
        let mut decoder = HuffmanDecoder::new();
        let decoded = match decoder.decode(raw_string) {
            Err(e) => {
                return Err(DecoderError::StringDecodingError(
                    StringDecodingError::HuffmanDecoderError(e),
                ));
            }
            Ok(res) => res,
        };
        Ok((Cow::Owned(decoded), consumed + len))
    } else {
        // The octets were transmitted raw
        trace!("decode_string: Raw octet string received");
        Ok((Cow::Borrowed(raw_string), consumed + len))
    }
}

/// Different variants of how a particular header field can be represented in
/// an HPACK encoding.
enum FieldRepresentation {
    Indexed,
    LiteralWithIncrementalIndexing,
    SizeUpdate,
    LiteralNeverIndexed,
    LiteralWithoutIndexing,
}

impl FieldRepresentation {
    /// Based on the given octet, returns the type of the field representation.
    ///
    /// The given octet should be the top-order byte of the header field that
    /// is about to be decoded.
    fn new(octet: u8) -> FieldRepresentation {
        if octet & 128 == 128 {
            // High-order bit set
            FieldRepresentation::Indexed
        } else if octet & 64 == 64 {
            // Bit pattern `01`
            FieldRepresentation::LiteralWithIncrementalIndexing
        } else if octet & 32 == 32 {
            // Bit pattern `001`
            FieldRepresentation::SizeUpdate
        } else if octet & 16 == 16 {
            // Bit pattern `0001`
            FieldRepresentation::LiteralNeverIndexed
        } else {
            // None of the top 4 bits is set => bit pattern `0000xxxx`
            FieldRepresentation::LiteralWithoutIndexing
        }
    }
}

/// Represents all errors that can be encountered while decoding an
/// integer.
#[derive(PartialEq, Copy, Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IntegerDecodingError {
    /// 5.1. specifies that "excessively large integer decodings" MUST be
    /// considered an error (whether the size is the number of octets or
    /// value). This variant corresponds to the encoding containing too many
    /// octets.
    #[error("Too many octets in the integer encoding")]
    TooManyOctets,
    /// The variant corresponds to the case where the value of the integer
    /// being decoded exceeds a certain threshold.
    #[error("Integer value too large")]
    ValueTooLarge,
    /// When a buffer from which an integer was supposed to be encoded does
    /// not contain enough octets to complete the decoding.
    #[error("Not enough octets in the buffer")]
    NotEnoughOctets,
    /// Only valid prefixes are [1, 8]
    #[error("Invalid prefix")]
    InvalidPrefix,
}

#[derive(PartialEq, Copy, Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StringDecodingError {
    #[error("Not enough octets in the buffer")]
    NotEnoughOctets,
    #[error("Huffman decoder error: {0}")]
    HuffmanDecoderError(HuffmanDecoderError),
}

/// Represents all errors that can be encountered while performing the decoding
/// of an HPACK header set.
#[derive(PartialEq, Copy, Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecoderError {
    #[error("Header index out of bounds")]
    HeaderIndexOutOfBounds,
    #[error("Integer decoding error: {0}")]
    IntegerDecodingError(IntegerDecodingError),
    #[error("String decoding error: {0}")]
    StringDecodingError(StringDecodingError),
    /// The size of the dynamic table can never be allowed to exceed the max
    /// size mandated to the decoder by the protocol. (by performing changes
    /// made by SizeUpdate blocks).
    #[error("Dynamic table size exceeds the maximum size")]
    InvalidMaxDynamicSize,
    /// A header block cannot end with a dynamic table size update, it
    /// must be treating as a decoding error.
    #[error("Dynamic table size update at the end of a header block")]
    SizeUpdateAtEnd,
}

/// Represents all errors that can be encountered while performing the decoding
/// of an HPACK header set, or while invoking the callback.
pub enum DecoderOrCallbackError<E> {
    DecoderError(DecoderError),
    CallbackError(E),
}

impl<E> From<DecoderError> for DecoderOrCallbackError<E> {
    fn from(err: DecoderError) -> Self {
        Self::DecoderError(err)
    }
}

/// The result returned by the `decode` method of the `Decoder`.
pub type DecoderResult = Result<Vec<(Vec<u8>, Vec<u8>)>, DecoderError>;

/// Decodes headers encoded using HPACK.
///
/// For now, incremental decoding is not supported, i.e. it is necessary
/// to pass in the entire encoded representation of all headers to the
/// decoder, rather than processing it piece-by-piece.
pub struct Decoder<'a> {
    // The dynamic table will own its own copy of headers
    header_table: HeaderTable<'a>,

    max_allowed_table_size: Option<usize>,

    // Allow trailing size updates (used by tests)
    #[cfg(test)]
    pub(crate) allow_trailing_size_updates: bool,
}

impl Default for Decoder<'_> {
    fn default() -> Decoder<'static> {
        Decoder::with_static_table(STATIC_TABLE)
    }
}

type DecodedLiteralCow<'a> = ((Cow<'a, [u8]>, Cow<'a, [u8]>), usize);
type DecodedLiteralSlice<'a> = ((&'a [u8], &'a [u8]), usize);

/// Represents a decoder of HPACK encoded headers. Maintains the state
/// necessary to correctly decode subsequent HPACK blocks.
impl<'a> Decoder<'a> {
    /// Creates a new `Decoder` with all settings set to default values.
    pub fn new() -> Decoder<'a> {
        Default::default()
    }

    /// Creates a new `Decoder` with the given slice serving as its static
    /// table.
    ///
    /// The slice should contain tuples where the tuple coordinates represent
    /// the header name and value, respectively.
    ///
    /// Note: in order for the final decoded content to match the encoding
    ///       (according to the standard, at least), this static table must be
    ///       the one defined in the HPACK spec.
    fn with_static_table(static_table: StaticTable<'a>) -> Decoder<'a> {
        Decoder {
            header_table: HeaderTable::with_static_table(static_table),
            max_allowed_table_size: None,
            #[cfg(test)]
            allow_trailing_size_updates: false,
        }
    }

    /// Sets a new maximum dynamic table size for the decoder.
    ///
    /// If `max_allowed_table_size` is set, `new_max_size` must be <= to it.
    pub fn set_max_table_size(&mut self, new_max_size: usize) {
        if let Some(max_allowed_size) = self.max_allowed_table_size {
            // this lives as an assert for now, since if you're calling this
            // explicitly so you can check preconditions first.
            assert!(
                new_max_size <= max_allowed_size,
                "new_max_size ({}) > max_allowed_size ({})",
                new_max_size,
                max_allowed_size
            );
        }

        self.header_table
            .dynamic_table
            .set_max_table_size(new_max_size);
    }

    /// Sets max allowed table size: any "dynamic table size updates" that try
    /// to bring the table size over that value will error out with
    /// [DecoderError::InvalidMaxDynamicSize]
    pub fn set_max_allowed_table_size(&mut self, max_allowed_size: usize) {
        self.max_allowed_table_size = Some(max_allowed_size);
    }

    /// Decodes the headers found in the given buffer `buf`. Invokes the
    /// callback `cb` for each decoded header in turn, by providing it the
    /// header name and value as `Cow` byte array slices.
    ///
    /// The callback is free to decide how to handle the emitted header, however
    /// the `Cow` cannot outlive the closure body without assuming ownership
    /// or otherwise copying the contents.
    ///
    /// This is due to the fact that the header might be found (fully or
    /// partially) in the header table of the decoder, in which case the
    /// callback will have received a borrow of its contents. However, when
    /// one of the following headers is decoded, it is possible that the
    /// header table might have to be modified; so the borrow is only valid
    /// until the next header decoding begins, meaning until the end of the
    /// callback's body.
    ///
    /// If an error is encountered during the decoding of any header, decoding
    /// halts and the appropriate error is returned as the `Err` variant of
    /// the `Result`.
    pub fn decode_with_cb(
        &mut self,
        buf: &[u8],
        mut cb: impl FnMut(Cow<[u8]>, Cow<[u8]>),
    ) -> Result<(), DecoderError> {
        let mut current_octet_index = 0;

        let mut last_was_size_update = false;
        while current_octet_index < buf.len() {
            // At this point we are always at the beginning of the next block
            // within the HPACK data.
            // The type of the block can always be determined from the first
            // byte.
            let initial_octet = buf[current_octet_index];
            let buffer_leftover = &buf[current_octet_index..];
            let field_representation = FieldRepresentation::new(initial_octet);
            last_was_size_update = matches!(field_representation, FieldRepresentation::SizeUpdate);

            let consumed = match field_representation {
                FieldRepresentation::Indexed => {
                    let ((name, value), consumed) = self.decode_indexed(buffer_leftover)?;
                    cb(Cow::Borrowed(name), Cow::Borrowed(value));

                    consumed
                }
                FieldRepresentation::LiteralWithIncrementalIndexing => {
                    let ((name, value), consumed) = {
                        let ((name, value), consumed) =
                            self.decode_literal(buffer_leftover, true)?;
                        cb(Cow::Borrowed(&name), Cow::Borrowed(&value));

                        // Since we are to add the decoded header to the header table, we need to
                        // convert them into owned buffers that the decoder can keep internally.
                        let name = name.into_owned();
                        let value = value.into_owned();

                        ((name, value), consumed)
                    };
                    // This cannot be done in the same scope as the `decode_literal` call, since
                    // Rust cannot figure out that the `into_owned` calls effectively drop the
                    // borrow on `self` that the `decode_literal` return value had. Since adding
                    // a header to the table requires a `&mut self`, it fails to compile.
                    // Manually separating it out here works around it...
                    self.header_table.add_header(name, value);

                    consumed
                }
                FieldRepresentation::LiteralWithoutIndexing => {
                    let ((name, value), consumed) = self.decode_literal(buffer_leftover, false)?;
                    cb(name, value);

                    consumed
                }
                FieldRepresentation::LiteralNeverIndexed => {
                    // Same as the previous one, except if we were also a proxy
                    // we would need to make sure not to change the
                    // representation received here. We don't care about this
                    // for now.
                    let ((name, value), consumed) = self.decode_literal(buffer_leftover, false)?;
                    cb(name, value);

                    consumed
                }
                FieldRepresentation::SizeUpdate => {
                    // Handle the dynamic table size update...
                    self.update_max_dynamic_size(buffer_leftover)?
                }
            };

            current_octet_index += consumed;
        }

        if last_was_size_update {
            #[cfg(test)]
            if self.allow_trailing_size_updates {
                return Ok(());
            }

            return Err(DecoderError::SizeUpdateAtEnd);
        }

        Ok(())
    }

    /// Decode the header block found in the given buffer.
    ///
    /// The decoded representation is returned as a sequence of headers, where
    /// both the name and value of each header is represented by an owned
    /// byte sequence (i.e. `Vec<u8>`).
    ///
    /// The buffer should represent the entire block that should be decoded.
    /// For example, in HTTP/2, all continuation frames need to be concatenated
    /// to a single buffer before passing them to the decoder.
    pub fn decode(&mut self, buf: &[u8]) -> DecoderResult {
        let mut header_list = Vec::new();

        self.decode_with_cb(buf, |n, v| {
            header_list.push((n.into_owned(), v.into_owned()))
        })?;

        Ok(header_list)
    }

    /// Decodes an indexed header representation.
    fn decode_indexed(&self, buf: &[u8]) -> Result<DecodedLiteralSlice<'_>, DecoderError> {
        let (index, consumed) = decode_integer(buf, 7)?;
        trace!(
            "Decoding indexed: index = {}, consumed = {}",
            index,
            consumed
        );

        let (name, value) = self.get_from_table(index)?;

        Ok(((name, value), consumed))
    }

    /// Gets the header (name, value) pair with the given index from the table.
    ///
    /// In this context, the "table" references the definition of the table
    /// where the static table is concatenated with the dynamic table and is
    /// 1-indexed.
    fn get_from_table(&self, index: usize) -> Result<(&[u8], &[u8]), DecoderError> {
        self.header_table
            .get_from_table(index)
            .ok_or(DecoderError::HeaderIndexOutOfBounds)
    }

    /// Decodes a literal header representation from the given buffer.
    ///
    /// # Parameters
    ///
    /// - index: whether or not the decoded value should be indexed (i.e.
    ///   included in the dynamic table).
    fn decode_literal<'b>(
        &'b self,
        buf: &'b [u8],
        index: bool,
    ) -> Result<DecodedLiteralCow<'b>, DecoderError> {
        let prefix = if index { 6 } else { 4 };
        let (table_index, mut consumed) = decode_integer(buf, prefix)?;

        // First read the name appropriately
        let name = if table_index == 0 {
            // Read name string as literal
            let (name, name_len) = decode_string(&buf[consumed..])?;
            consumed += name_len;
            name
        } else {
            // Read name indexed from the table
            let (name, _) = self.get_from_table(table_index)?;
            Cow::Borrowed(name)
        };

        // Now read the value as a literal...
        let (value, value_len) = decode_string(&buf[consumed..])?;
        consumed += value_len;

        Ok(((name, value), consumed))
    }

    /// Handles processing the `SizeUpdate` HPACK block: updates the maximum
    /// size of the underlying dynamic table, possibly causing a number of
    /// headers to be evicted from it.
    ///
    /// Assumes that the first byte in the given buffer `buf` is the first
    /// octet in the `SizeUpdate` block.
    ///
    /// Returns the number of octets consumed from the given buffer.
    fn update_max_dynamic_size(&mut self, buf: &[u8]) -> Result<usize, DecoderError> {
        let (new_size, consumed) = decode_integer(buf, 5)?;
        if let Some(max_size) = self.max_allowed_table_size {
            if new_size > max_size {
                return Err(DecoderError::InvalidMaxDynamicSize);
            }
        }
        self.header_table.dynamic_table.set_max_table_size(new_size);

        trace!(
            "Decoder changed max table size from {} to {}",
            self.header_table.dynamic_table.get_size(),
            new_size
        );

        Ok(consumed)
    }
}

#[cfg(test)]
mod tests {
    use super::decode_integer;

    use std::borrow::Cow;

    use super::super::encoder::encode_integer;
    use super::super::huffman::HuffmanDecoderError;
    use super::decode_string;
    use super::Decoder;
    use super::FieldRepresentation;
    use super::{DecoderError, DecoderResult};
    use super::{IntegerDecodingError, StringDecodingError};

    /// Tests that valid integer encodings are properly decoded.
    #[test]
    fn test_decode_integer() {
        assert_eq!((10, 1), decode_integer(&[10], 5).ok().unwrap());
        assert_eq!((1337, 3), decode_integer(&[31, 154, 10], 5).ok().unwrap());
        assert_eq!(
            (1337, 3),
            decode_integer(&[31 + 32, 154, 10], 5).ok().unwrap()
        );
        assert_eq!(
            (1337, 3),
            decode_integer(&[31 + 64, 154, 10], 5).ok().unwrap()
        );
        assert_eq!(
            (1337, 3),
            decode_integer(&[31, 154, 10, 111, 22], 5).ok().unwrap()
        );

        assert_eq!((127, 2), decode_integer(&[255, 0], 7).ok().unwrap());
        assert_eq!((127, 2), decode_integer(&[127, 0], 7).ok().unwrap());
        assert_eq!((255, 3), decode_integer(&[127, 128, 1], 7).ok().unwrap());
        assert_eq!((255, 2), decode_integer(&[255, 0], 8).unwrap());
        assert_eq!((254, 1), decode_integer(&[254], 8).unwrap());
        assert_eq!((1, 1), decode_integer(&[1], 8).unwrap());
        assert_eq!((0, 1), decode_integer(&[0], 8).unwrap());
        // The largest allowed integer correctly gets decoded...
        assert_eq!(
            (268435710, 5),
            decode_integer(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF - 128], 8)
                .ok()
                .unwrap()
        );
    }

    /// A helper macro that asserts that a given `DecoderResult` represents
    /// the given `IntegerDecodingError`.
    macro_rules! assert_integer_err (
        ($err_type:expr, $decoder_result:expr) => (
            assert_eq!($err_type, match $decoder_result {
                Err(DecoderError::IntegerDecodingError(e)) => e,
                _ => panic!("Expected a decoding error"),
            });
        );
    );

    /// Tests that some invalid integer encodings are detected and signalled as
    /// errors.
    #[test]
    fn test_decode_integer_errors() {
        assert_integer_err!(
            IntegerDecodingError::NotEnoughOctets,
            decode_integer(&[], 5)
        );
        assert_integer_err!(
            IntegerDecodingError::NotEnoughOctets,
            decode_integer(&[0xFF, 0xFF], 5)
        );
        assert_integer_err!(
            IntegerDecodingError::TooManyOctets,
            decode_integer(
                &[0xFF, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80],
                1
            )
        );
        assert_integer_err!(
            IntegerDecodingError::TooManyOctets,
            decode_integer(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x0], 8)
        );
        assert_integer_err!(
            IntegerDecodingError::InvalidPrefix,
            decode_integer(&[10], 0)
        );
        assert_integer_err!(
            IntegerDecodingError::InvalidPrefix,
            decode_integer(&[10], 9)
        );
    }

    #[test]
    fn test_detect_literal_without_indexing() {
        assert!(matches!(
            FieldRepresentation::new(0),
            FieldRepresentation::LiteralWithoutIndexing
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 4) - 1),
            FieldRepresentation::LiteralWithoutIndexing
        ));
        assert!(matches!(
            FieldRepresentation::new(2),
            FieldRepresentation::LiteralWithoutIndexing
        ));
    }

    #[test]
    fn test_detect_literal_never_indexed() {
        assert!(matches!(
            FieldRepresentation::new(1 << 4),
            FieldRepresentation::LiteralNeverIndexed
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 4) + 15),
            FieldRepresentation::LiteralNeverIndexed
        ));
    }

    #[test]
    fn test_detect_literal_incremental_indexing() {
        assert!(matches!(
            FieldRepresentation::new(1 << 6),
            FieldRepresentation::LiteralWithIncrementalIndexing
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 6) + (1 << 4)),
            FieldRepresentation::LiteralWithIncrementalIndexing
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 7) - 1),
            FieldRepresentation::LiteralWithIncrementalIndexing
        ));
    }

    #[test]
    fn test_detect_indexed() {
        assert!(matches!(
            FieldRepresentation::new(1 << 7),
            FieldRepresentation::Indexed
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 7) + (1 << 4)),
            FieldRepresentation::Indexed
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 7) + (1 << 5)),
            FieldRepresentation::Indexed
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 7) + (1 << 6)),
            FieldRepresentation::Indexed
        ));
        assert!(matches!(
            FieldRepresentation::new(255),
            FieldRepresentation::Indexed
        ));
    }

    #[test]
    fn test_detect_dynamic_table_size_update() {
        assert!(matches!(
            FieldRepresentation::new(1 << 5),
            FieldRepresentation::SizeUpdate
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 5) + (1 << 4)),
            FieldRepresentation::SizeUpdate
        ));
        assert!(matches!(
            FieldRepresentation::new((1 << 6) - 1),
            FieldRepresentation::SizeUpdate
        ));
    }

    #[test]
    fn test_decode_string_no_huffman() {
        /// Checks that the result matches the expectation, but also that the
        /// `Cow` is borrowed!
        fn assert_borrowed_eq(expected: (&[u8], usize), result: (Cow<'_, [u8]>, usize)) {
            let (expected_str, expected_len) = expected;
            let (actual_str, actual_len) = result;
            assert_eq!(expected_len, actual_len);
            match actual_str {
                Cow::Borrowed(actual) => assert_eq!(actual, expected_str),
                _ => panic!("Expected the result to be borrowed!"),
            };
        }

        assert_eq!(
            (Cow::Borrowed(&b"abc"[..]), 4),
            decode_string(&[3, b'a', b'b', b'c']).ok().unwrap()
        );
        assert_eq!(
            (Cow::Borrowed(&b"a"[..]), 2),
            decode_string(&[1, b'a']).ok().unwrap()
        );
        assert_eq!(
            (Cow::Borrowed(&b""[..]), 1),
            decode_string(&[0, b'a']).ok().unwrap()
        );

        assert_borrowed_eq(
            (&b"abc"[..], 4),
            decode_string(&[3, b'a', b'b', b'c']).ok().unwrap(),
        );
        assert_borrowed_eq((&b"a"[..], 2), decode_string(&[1, b'a']).ok().unwrap());
        assert_borrowed_eq((&b""[..], 1), decode_string(&[0, b'a']).ok().unwrap());

        // Buffer smaller than advertised string length
        assert_eq!(
            StringDecodingError::NotEnoughOctets,
            match decode_string(&[3, b'a', b'b']) {
                Err(DecoderError::StringDecodingError(e)) => e,
                _ => panic!("Expected NotEnoughOctets error!"),
            }
        );
    }

    /// Tests that an octet string is correctly decoded when it's length
    /// is longer than what can fit into the 7-bit prefix.
    #[test]
    fn test_decode_string_no_huffman_long() {
        {
            let full_string: Vec<u8> = (0u8..200).collect();
            let mut encoded = encode_integer(full_string.len(), 7);
            encoded.extend(full_string.clone());

            assert_eq!(
                (Cow::Owned(full_string), encoded.len()),
                decode_string(&encoded).ok().unwrap()
            );
        }
        {
            let full_string: Vec<u8> = (0u8..127).collect();
            let mut encoded = encode_integer(full_string.len(), 7);
            encoded.extend(full_string.clone());

            assert_eq!(
                (Cow::Owned(full_string), encoded.len()),
                decode_string(&encoded).ok().unwrap()
            );
        }
    }

    /// Tests that a header list with only a single header found fully in the
    /// static header table is correctly decoded.
    /// (example from: HPACK-draft-10, C.2.4.)
    #[test]
    fn test_decode_fully_in_static_table() {
        let mut decoder = Decoder::new();

        let header_list = decoder.decode(&[0x82]).ok().unwrap();

        assert_eq!(vec![(b":method".to_vec(), b"GET".to_vec())], header_list);
    }

    #[test]
    fn test_decode_multiple_fully_in_static_table() {
        let mut decoder = Decoder::new();

        let header_list = decoder.decode(&[0x82, 0x86, 0x84]).ok().unwrap();

        assert_eq!(
            header_list,
            [
                (b":method".to_vec(), b"GET".to_vec()),
                (b":scheme".to_vec(), b"http".to_vec()),
                (b":path".to_vec(), b"/".to_vec()),
            ]
        );
    }

    /// Tests that a literal with an indexed name and literal value is correctly
    /// decoded.
    /// (example from: HPACK-draft-10, C.2.2.)
    #[test]
    fn test_decode_literal_indexed_name() {
        let mut decoder = Decoder::new();
        let hex_dump = [
            0x04, 0x0c, 0x2f, 0x73, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x2f, 0x70, 0x61, 0x74, 0x68,
        ];

        let header_list = decoder.decode(&hex_dump).ok().unwrap();

        assert_eq!(
            header_list,
            [(b":path".to_vec(), b"/sample/path".to_vec()),]
        );
        // Nothing was added to the dynamic table
        assert_eq!(decoder.header_table.dynamic_table.len(), 0);
    }

    /// Tests that a header with both a literal name and value is correctly
    /// decoded.
    /// (example from: HPACK-draft-10, C.2.1.)
    #[test]
    fn test_decode_literal_both() {
        let mut decoder = Decoder::new();
        let hex_dump = [
            0x40, 0x0a, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x6b, 0x65, 0x79, 0x0d, 0x63,
            0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x68, 0x65, 0x61, 0x64, 0x65, 0x72,
        ];

        let header_list = decoder.decode(&hex_dump).ok().unwrap();

        assert_eq!(
            header_list,
            [(b"custom-key".to_vec(), b"custom-header".to_vec()),]
        );
        // The entry got added to the dynamic table?
        assert_eq!(decoder.header_table.dynamic_table.len(), 1);
        let expected_table = vec![(b"custom-key".to_vec(), b"custom-header".to_vec())];
        let actual = decoder.header_table.dynamic_table.to_vec();
        assert_eq!(actual, expected_table);
    }

    /// Tests that a header with a name indexed from the dynamic table and a
    /// literal value is correctly decoded.
    #[test]
    fn test_decode_literal_name_in_dynamic() {
        let mut decoder = Decoder::new();
        {
            // Prepares the context: the dynamic table contains a custom-key.
            let hex_dump = [
                0x40, 0x0a, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x6b, 0x65, 0x79, 0x0d, 0x63,
                0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x68, 0x65, 0x61, 0x64, 0x65, 0x72,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [(b"custom-key".to_vec(), b"custom-header".to_vec()),]
            );
            // The entry got added to the dynamic table?
            assert_eq!(decoder.header_table.dynamic_table.len(), 1);
            let expected_table = vec![(b"custom-key".to_vec(), b"custom-header".to_vec())];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            let hex_dump = [
                0x40 + 62, // Index 62 in the table => 1st in dynamic table
                0x0e,
                0x63,
                0x75,
                0x73,
                0x74,
                0x6f,
                0x6d,
                0x2d,
                0x68,
                0x65,
                0x61,
                0x64,
                0x65,
                0x72,
                0x2d,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [(b"custom-key".to_vec(), b"custom-header-".to_vec()),]
            );
            // The entry got added to the dynamic table, so now we have two?
            assert_eq!(decoder.header_table.dynamic_table.len(), 2);
            let expected_table = vec![
                (b"custom-key".to_vec(), b"custom-header-".to_vec()),
                (b"custom-key".to_vec(), b"custom-header".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
    }

    /// Tests that a header with a "never indexed" type is correctly
    /// decoded.
    /// (example from: HPACK-draft-10, C.2.3.)
    #[test]
    fn test_decode_literal_field_never_indexed() {
        let mut decoder = Decoder::new();
        let hex_dump = [
            0x10, 0x08, 0x70, 0x61, 0x73, 0x73, 0x77, 0x6f, 0x72, 0x64, 0x06, 0x73, 0x65, 0x63,
            0x72, 0x65, 0x74,
        ];

        let header_list = decoder.decode(&hex_dump).ok().unwrap();

        assert_eq!(header_list, [(b"password".to_vec(), b"secret".to_vec()),]);
        // Nothing was added to the dynamic table
        assert_eq!(decoder.header_table.dynamic_table.len(), 0);
    }

    /// Tests that a each header list from a sequence of requests is correctly
    /// decoded.
    /// (example from: HPACK-draft-10, C.3.*)
    #[test]
    fn test_request_sequence_no_huffman() {
        let mut decoder = Decoder::new();
        {
            // First Request (C.3.1.)
            let hex_dump = [
                0x82, 0x86, 0x84, 0x41, 0x0f, 0x77, 0x77, 0x77, 0x2e, 0x65, 0x78, 0x61, 0x6d, 0x70,
                0x6c, 0x65, 0x2e, 0x63, 0x6f, 0x6d,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":method".to_vec(), b"GET".to_vec()),
                    (b":scheme".to_vec(), b"http".to_vec()),
                    (b":path".to_vec(), b"/".to_vec()),
                    (b":authority".to_vec(), b"www.example.com".to_vec()),
                ]
            );
            // Only one entry got added to the dynamic table?
            assert_eq!(decoder.header_table.dynamic_table.len(), 1);
            let expected_table = vec![(b":authority".to_vec(), b"www.example.com".to_vec())];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            // Second Request (C.3.2.)
            let hex_dump = [
                0x82, 0x86, 0x84, 0xbe, 0x58, 0x08, 0x6e, 0x6f, 0x2d, 0x63, 0x61, 0x63, 0x68, 0x65,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":method".to_vec(), b"GET".to_vec()),
                    (b":scheme".to_vec(), b"http".to_vec()),
                    (b":path".to_vec(), b"/".to_vec()),
                    (b":authority".to_vec(), b"www.example.com".to_vec()),
                    (b"cache-control".to_vec(), b"no-cache".to_vec()),
                ]
            );
            // One entry got added to the dynamic table, so we have two?
            let expected_table = vec![
                (b"cache-control".to_vec(), b"no-cache".to_vec()),
                (b":authority".to_vec(), b"www.example.com".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            // Third Request (C.3.3.)
            let hex_dump = [
                0x82, 0x87, 0x85, 0xbf, 0x40, 0x0a, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x6b,
                0x65, 0x79, 0x0c, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x76, 0x61, 0x6c, 0x75,
                0x65,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":method".to_vec(), b"GET".to_vec()),
                    (b":scheme".to_vec(), b"https".to_vec()),
                    (b":path".to_vec(), b"/index.html".to_vec()),
                    (b":authority".to_vec(), b"www.example.com".to_vec()),
                    (b"custom-key".to_vec(), b"custom-value".to_vec()),
                ]
            );
            // One entry got added to the dynamic table, so we have three at
            // this point...?
            let expected_table = vec![
                (b"custom-key".to_vec(), b"custom-value".to_vec()),
                (b"cache-control".to_vec(), b"no-cache".to_vec()),
                (b":authority".to_vec(), b"www.example.com".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
    }

    /// Tests that a each header list from a sequence of responses is correctly
    /// decoded.
    /// (example from: HPACK-draft-10, C.5.*)
    #[test]
    fn response_sequence_no_huffman() {
        let mut decoder = Decoder::new();
        // The example sets the max table size to 256 octets.
        decoder.set_max_table_size(256);
        {
            // First Response (C.5.1.)
            let hex_dump = [
                0x48, 0x03, 0x33, 0x30, 0x32, 0x58, 0x07, 0x70, 0x72, 0x69, 0x76, 0x61, 0x74, 0x65,
                0x61, 0x1d, 0x4d, 0x6f, 0x6e, 0x2c, 0x20, 0x32, 0x31, 0x20, 0x4f, 0x63, 0x74, 0x20,
                0x32, 0x30, 0x31, 0x33, 0x20, 0x32, 0x30, 0x3a, 0x31, 0x33, 0x3a, 0x32, 0x31, 0x20,
                0x47, 0x4d, 0x54, 0x6e, 0x17, 0x68, 0x74, 0x74, 0x70, 0x73, 0x3a, 0x2f, 0x2f, 0x77,
                0x77, 0x77, 0x2e, 0x65, 0x78, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x2e, 0x63, 0x6f, 0x6d,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":status".to_vec(), b"302".to_vec()),
                    (b"cache-control".to_vec(), b"private".to_vec()),
                    (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                    (b"location".to_vec(), b"https://www.example.com".to_vec()),
                ]
            );
            // All entries in the dynamic table too?
            let expected_table = vec![
                (b"location".to_vec(), b"https://www.example.com".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                (b"cache-control".to_vec(), b"private".to_vec()),
                (b":status".to_vec(), b"302".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            // Second Response (C.5.2.)
            let hex_dump = [0x48, 0x03, 0x33, 0x30, 0x37, 0xc1, 0xc0, 0xbf];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":status".to_vec(), b"307".to_vec()),
                    (b"cache-control".to_vec(), b"private".to_vec()),
                    (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                    (b"location".to_vec(), b"https://www.example.com".to_vec()),
                ]
            );
            // The new status replaces the old status in the table, since it
            // cannot fit without evicting something from the table.
            let expected_table = vec![
                (b":status".to_vec(), b"307".to_vec()),
                (b"location".to_vec(), b"https://www.example.com".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                (b"cache-control".to_vec(), b"private".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            // Third Response (C.5.3.)
            let hex_dump = [
                0x88, 0xc1, 0x61, 0x1d, 0x4d, 0x6f, 0x6e, 0x2c, 0x20, 0x32, 0x31, 0x20, 0x4f, 0x63,
                0x74, 0x20, 0x32, 0x30, 0x31, 0x33, 0x20, 0x32, 0x30, 0x3a, 0x31, 0x33, 0x3a, 0x32,
                0x32, 0x20, 0x47, 0x4d, 0x54, 0xc0, 0x5a, 0x04, 0x67, 0x7a, 0x69, 0x70, 0x77, 0x38,
                0x66, 0x6f, 0x6f, 0x3d, 0x41, 0x53, 0x44, 0x4a, 0x4b, 0x48, 0x51, 0x4b, 0x42, 0x5a,
                0x58, 0x4f, 0x51, 0x57, 0x45, 0x4f, 0x50, 0x49, 0x55, 0x41, 0x58, 0x51, 0x57, 0x45,
                0x4f, 0x49, 0x55, 0x3b, 0x20, 0x6d, 0x61, 0x78, 0x2d, 0x61, 0x67, 0x65, 0x3d, 0x33,
                0x36, 0x30, 0x30, 0x3b, 0x20, 0x76, 0x65, 0x72, 0x73, 0x69, 0x6f, 0x6e, 0x3d, 0x31,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            let expected_header_list = [
                (b":status".to_vec(), b"200".to_vec()),
                (b"cache-control".to_vec(), b"private".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:22 GMT".to_vec()),
                (b"location".to_vec(), b"https://www.example.com".to_vec()),
                (b"content-encoding".to_vec(), b"gzip".to_vec()),
                (
                    b"set-cookie".to_vec(),
                    b"foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1".to_vec(),
                ),
            ];
            assert_eq!(header_list, expected_header_list);
            // The new status replaces the old status in the table, since it
            // cannot fit without evicting something from the table.
            let expected_table = vec![
                (
                    b"set-cookie".to_vec(),
                    b"foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1".to_vec(),
                ),
                (b"content-encoding".to_vec(), b"gzip".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:22 GMT".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
    }

    /// Tests that when the decoder receives an update of the max dynamic table
    /// size as 0, all entries are cleared from the dynamic table.
    #[test]
    fn test_decoder_clear_dynamic_table() {
        let mut decoder = Decoder::new();
        decoder.allow_trailing_size_updates = true;

        {
            let hex_dump = [
                0x48, 0x03, 0x33, 0x30, 0x32, 0x58, 0x07, 0x70, 0x72, 0x69, 0x76, 0x61, 0x74, 0x65,
                0x61, 0x1d, 0x4d, 0x6f, 0x6e, 0x2c, 0x20, 0x32, 0x31, 0x20, 0x4f, 0x63, 0x74, 0x20,
                0x32, 0x30, 0x31, 0x33, 0x20, 0x32, 0x30, 0x3a, 0x31, 0x33, 0x3a, 0x32, 0x31, 0x20,
                0x47, 0x4d, 0x54, 0x6e, 0x17, 0x68, 0x74, 0x74, 0x70, 0x73, 0x3a, 0x2f, 0x2f, 0x77,
                0x77, 0x77, 0x2e, 0x65, 0x78, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x2e, 0x63, 0x6f, 0x6d,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":status".to_vec(), b"302".to_vec()),
                    (b"cache-control".to_vec(), b"private".to_vec()),
                    (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                    (b"location".to_vec(), b"https://www.example.com".to_vec()),
                ]
            );
            // All entries in the dynamic table too?
            let expected_table = vec![
                (b"location".to_vec(), b"https://www.example.com".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                (b"cache-control".to_vec(), b"private".to_vec()),
                (b":status".to_vec(), b"302".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            let hex_dump = [
                0x48, 0x03, 0x33, 0x30, 0x37, 0xc1, 0xc0, 0xbf,
                // This instructs the decoder to clear the list
                // (it's doubtful that it would ever be found there in a real
                // response, though...)
                0x20,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            // Headers have been correctly decoded...
            assert_eq!(
                header_list,
                [
                    (b":status".to_vec(), b"307".to_vec()),
                    (b"cache-control".to_vec(), b"private".to_vec()),
                    (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                    (b"location".to_vec(), b"https://www.example.com".to_vec()),
                ]
            );
            // Expect an empty table!
            let expected_table = vec![];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
            assert_eq!(0, decoder.header_table.dynamic_table.get_max_table_size());
        }
    }

    /// Tests that a each header list from a sequence of requests is correctly
    /// decoded, when Huffman coding is used
    /// (example from: HPACK-draft-10, C.4.*)
    #[test]
    fn request_sequence_huffman() {
        let mut decoder = Decoder::new();
        {
            // First Request (B.4.1.)
            let hex_dump = [
                0x82, 0x86, 0x84, 0x41, 0x8c, 0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab,
                0x90, 0xf4, 0xff,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":method".to_vec(), b"GET".to_vec()),
                    (b":scheme".to_vec(), b"http".to_vec()),
                    (b":path".to_vec(), b"/".to_vec()),
                    (b":authority".to_vec(), b"www.example.com".to_vec()),
                ]
            );
            // Only one entry got added to the dynamic table?
            assert_eq!(decoder.header_table.dynamic_table.len(), 1);
            let expected_table = vec![(b":authority".to_vec(), b"www.example.com".to_vec())];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            // Second Request (C.4.2.)
            let hex_dump = [
                0x82, 0x86, 0x84, 0xbe, 0x58, 0x86, 0xa8, 0xeb, 0x10, 0x64, 0x9c, 0xbf,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":method".to_vec(), b"GET".to_vec()),
                    (b":scheme".to_vec(), b"http".to_vec()),
                    (b":path".to_vec(), b"/".to_vec()),
                    (b":authority".to_vec(), b"www.example.com".to_vec()),
                    (b"cache-control".to_vec(), b"no-cache".to_vec()),
                ]
            );
            // One entry got added to the dynamic table, so we have two?
            let expected_table = vec![
                (b"cache-control".to_vec(), b"no-cache".to_vec()),
                (b":authority".to_vec(), b"www.example.com".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            // Third Request (C.4.3.)
            let hex_dump = [
                0x82, 0x87, 0x85, 0xbf, 0x40, 0x88, 0x25, 0xa8, 0x49, 0xe9, 0x5b, 0xa9, 0x7d, 0x7f,
                0x89, 0x25, 0xa8, 0x49, 0xe9, 0x5b, 0xb8, 0xe8, 0xb4, 0xbf,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":method".to_vec(), b"GET".to_vec()),
                    (b":scheme".to_vec(), b"https".to_vec()),
                    (b":path".to_vec(), b"/index.html".to_vec()),
                    (b":authority".to_vec(), b"www.example.com".to_vec()),
                    (b"custom-key".to_vec(), b"custom-value".to_vec()),
                ]
            );
            // One entry got added to the dynamic table, so we have three at
            // this point...?
            let expected_table = vec![
                (b"custom-key".to_vec(), b"custom-value".to_vec()),
                (b"cache-control".to_vec(), b"no-cache".to_vec()),
                (b":authority".to_vec(), b"www.example.com".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
    }

    /// Tests that a each header list from a sequence of responses is correctly
    /// decoded, when Huffman encoding is used
    /// (example from: HPACK-draft-10, C.6.*)
    #[test]
    fn response_sequence_huffman() {
        let mut decoder = Decoder::new();
        // The example sets the max table size to 256 octets.
        decoder.set_max_table_size(256);
        {
            // First Response (C.6.1.)
            let hex_dump = [
                0x48, 0x82, 0x64, 0x02, 0x58, 0x85, 0xae, 0xc3, 0x77, 0x1a, 0x4b, 0x61, 0x96, 0xd0,
                0x7a, 0xbe, 0x94, 0x10, 0x54, 0xd4, 0x44, 0xa8, 0x20, 0x05, 0x95, 0x04, 0x0b, 0x81,
                0x66, 0xe0, 0x82, 0xa6, 0x2d, 0x1b, 0xff, 0x6e, 0x91, 0x9d, 0x29, 0xad, 0x17, 0x18,
                0x63, 0xc7, 0x8f, 0x0b, 0x97, 0xc8, 0xe9, 0xae, 0x82, 0xae, 0x43, 0xd3,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":status".to_vec(), b"302".to_vec()),
                    (b"cache-control".to_vec(), b"private".to_vec()),
                    (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                    (b"location".to_vec(), b"https://www.example.com".to_vec()),
                ]
            );
            // All entries in the dynamic table too?
            let expected_table = vec![
                (b"location".to_vec(), b"https://www.example.com".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                (b"cache-control".to_vec(), b"private".to_vec()),
                (b":status".to_vec(), b"302".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            // Second Response (C.6.2.)
            let hex_dump = [0x48, 0x83, 0x64, 0x0e, 0xff, 0xc1, 0xc0, 0xbf];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            assert_eq!(
                header_list,
                [
                    (b":status".to_vec(), b"307".to_vec()),
                    (b"cache-control".to_vec(), b"private".to_vec()),
                    (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                    (b"location".to_vec(), b"https://www.example.com".to_vec()),
                ]
            );
            // The new status replaces the old status in the table, since it
            // cannot fit without evicting something from the table.
            let expected_table = vec![
                (b":status".to_vec(), b"307".to_vec()),
                (b"location".to_vec(), b"https://www.example.com".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:21 GMT".to_vec()),
                (b"cache-control".to_vec(), b"private".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
        {
            // Third Response (C.6.3.)
            let hex_dump = [
                0x88, 0xc1, 0x61, 0x96, 0xd0, 0x7a, 0xbe, 0x94, 0x10, 0x54, 0xd4, 0x44, 0xa8, 0x20,
                0x05, 0x95, 0x04, 0x0b, 0x81, 0x66, 0xe0, 0x84, 0xa6, 0x2d, 0x1b, 0xff, 0xc0, 0x5a,
                0x83, 0x9b, 0xd9, 0xab, 0x77, 0xad, 0x94, 0xe7, 0x82, 0x1d, 0xd7, 0xf2, 0xe6, 0xc7,
                0xb3, 0x35, 0xdf, 0xdf, 0xcd, 0x5b, 0x39, 0x60, 0xd5, 0xaf, 0x27, 0x08, 0x7f, 0x36,
                0x72, 0xc1, 0xab, 0x27, 0x0f, 0xb5, 0x29, 0x1f, 0x95, 0x87, 0x31, 0x60, 0x65, 0xc0,
                0x03, 0xed, 0x4e, 0xe5, 0xb1, 0x06, 0x3d, 0x50, 0x07,
            ];

            let header_list = decoder.decode(&hex_dump).ok().unwrap();

            let expected_header_list = [
                (b":status".to_vec(), b"200".to_vec()),
                (b"cache-control".to_vec(), b"private".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:22 GMT".to_vec()),
                (b"location".to_vec(), b"https://www.example.com".to_vec()),
                (b"content-encoding".to_vec(), b"gzip".to_vec()),
                (
                    b"set-cookie".to_vec(),
                    b"foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1".to_vec(),
                ),
            ];
            assert_eq!(header_list, expected_header_list);
            // The new status replaces the old status in the table, since it
            // cannot fit without evicting something from the table.
            let expected_table = vec![
                (
                    b"set-cookie".to_vec(),
                    b"foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1".to_vec(),
                ),
                (b"content-encoding".to_vec(), b"gzip".to_vec()),
                (b"date".to_vec(), b"Mon, 21 Oct 2013 20:13:22 GMT".to_vec()),
            ];
            let actual = decoder.header_table.dynamic_table.to_vec();
            assert_eq!(actual, expected_table);
        }
    }

    /// Helper function that verifies whether the given `DecoderResult`
    /// indicates the given `DecoderError`
    fn is_decoder_error(err: &DecoderError, result: &DecoderResult) -> bool {
        match *result {
            Err(ref e) => e == err,
            _ => false,
        }
    }

    /// Tests that when a header representation indicates an indexed header
    /// encoding, but the index is out of valid bounds, the appropriate error
    /// is returned by the decoder.
    #[test]
    fn test_index_out_of_bounds() {
        let mut decoder = Decoder::new();
        // Some fixtures of raw messages which definitely need to cause an
        // index out of bounds error.
        let raw_messages = [
            // This indicates that the index of the header is 0, which is
            // invalid...
            vec![0x80],
            // This indicates that the index of the header is 62, which is out
            // of the bounds of the header table, given that there are no
            // entries in the dynamic table and the static table contains 61
            // elements.
            vec![0xbe],
            // Literal encoded with an indexed name where the index is out of
            // bounds.
            vec![126, 1, 65],
        ];

        // Check them all...
        for raw_message in raw_messages.iter() {
            assert!(
                is_decoder_error(
                    &DecoderError::HeaderIndexOutOfBounds,
                    &decoder.decode(raw_message)
                ),
                "Expected index out of bounds"
            );
        }
    }

    /// Tests that if a header encoded using a literal string representation
    /// (using Huffman encoding) contains an invalid string encoding, an error
    /// is returned.
    #[test]
    fn test_invalid_literal_huffman_string() {
        let mut decoder = Decoder::new();
        // Invalid padding introduced into the message
        let hex_dump = [
            0x82, 0x86, 0x84, 0x41, 0x8c, 0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab,
            0x90, 0xf4, 0xfe,
        ];

        assert!(matches!(
            decoder.decode(&hex_dump),
            Err(DecoderError::StringDecodingError(
                StringDecodingError::HuffmanDecoderError(HuffmanDecoderError::InvalidPadding,)
            ))
        ));
    }

    /// Tests that if the message cuts short before the header key is decoded,
    /// we get an appropriate error.
    #[test]
    fn test_literal_header_key_incomplete() {
        let mut decoder = Decoder::new();
        // The message does not have the length specifier of the header value
        // (cuts short after the header key is complete)
        let hex_dump = [
            0x40, 0x0a, b'c', b'u', b's', b't', b'o', b'm', b'-', b'k', b'e',
        ];

        let result = decoder.decode(&hex_dump);

        assert!(matches!(
            result,
            Err(DecoderError::StringDecodingError(
                StringDecodingError::NotEnoughOctets
            ))
        ));
    }

    /// Tests that when a header is encoded as a literal with both a name and
    /// a value, if the value is missing, we get an error.
    #[test]
    fn test_literal_header_missing_value() {
        let mut decoder = Decoder::new();
        // The message does not have the length specifier of the header value
        // (cuts short after the header key is complete)
        let hex_dump = [
            0x40, 0x0a, b'c', b'u', b's', b't', b'o', b'm', b'-', b'k', b'e', b'y',
        ];

        let result = decoder.decode(&hex_dump);

        assert!(matches!(
            result,
            Err(DecoderError::IntegerDecodingError(
                IntegerDecodingError::NotEnoughOctets
            ))
        ));
    }
}

/// The module defines interop tests between this HPACK decoder
/// and some other encoder implementations, based on their results
/// published at
/// [http2jp/hpack-test-case](https://github.com/http2jp/hpack-test-case)
#[cfg(test)]
#[cfg(feature = "interop-tests")]
mod interop_tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::{borrow::Cow, collections::HashMap};

    use serde::Deserialize;
    use tracing::trace;

    use super::Decoder;

    #[derive(Deserialize)]
    struct RawTestStory<'a> {
        cases: Vec<RawTestFixture<'a>>,
    }

    #[derive(Deserialize)]
    struct RawTestFixture<'a> {
        // The raw bytes of the message, as a hex-encoded string.
        wire: Cow<'a, str>,

        // The headers of the message: each HashMap has a single key-value pair
        headers: Vec<HashMap<Cow<'a, str>, Cow<'a, str>>>,
    }

    /// Defines the structure of a single part of a story file. We only care
    /// about the bytes and corresponding headers and ignore the rest.
    struct TestFixture {
        wire_bytes: Vec<u8>,
        headers: Vec<(Vec<u8>, Vec<u8>)>,
    }

    /// Defines the structure corresponding to a full story file. We only
    /// care about the cases for now.
    struct TestStory {
        cases: Vec<TestFixture>,
    }

    #[derive(Debug, thiserror::Error)]
    enum DecodeError {
        #[error("Failed to parse hex-encoded bytes: {0}")]
        FromHex(#[from] hex::FromHexError),
    }

    impl TryFrom<RawTestStory<'_>> for TestStory {
        type Error = DecodeError;

        fn try_from(raw_story: RawTestStory) -> Result<Self, Self::Error> {
            let mut cases = Vec::with_capacity(raw_story.cases.len());
            for raw_case in raw_story.cases {
                let wire_bytes = hex::decode(raw_case.wire.as_bytes())?;

                let headers: Vec<_> = raw_case
                    .headers
                    .into_iter()
                    .flat_map(|h| {
                        h.into_iter().map(|(k, v)| {
                            (k.into_owned().into_bytes(), v.into_owned().into_bytes())
                        })
                    })
                    .collect();

                cases.push(TestFixture {
                    wire_bytes,
                    headers,
                });
            }

            Ok(TestStory { cases })
        }
    }

    /// Tests that the `TestStory` can be properly read out of a JSON encoded
    /// string. Sanity check for the `Decodable` implementation.
    #[test]
    fn test_story_parser_sanity_check() {
        let raw_json = r#"
            {
              "cases": [
                {
                  "seqno": 0,
                  "wire": "82864188f439ce75c875fa5784",
                  "headers": [
                    {
                      ":method": "GET"
                    },
                    {
                      ":scheme": "http"
                    },
                    {
                      ":authority": "yahoo.co.jp"
                    },
                    {
                      ":path": "/"
                    }
                  ]
                },
                {
                  "seqno": 1,
                  "wire": "8286418cf1e3c2fe8739ceb90ebf4aff84",
                  "headers": [
                    {
                      ":method": "GET"
                    },
                    {
                      ":scheme": "http"
                    },
                    {
                      ":authority": "www.yahoo.co.jp"
                    },
                    {
                      ":path": "/"
                    }
                  ]
                }
              ],
              "draft": 9
            }
        "#;

        let decoded: RawTestStory = serde_json::from_str(raw_json).unwrap();
        let decoded = TestStory::try_from(decoded).unwrap();

        assert_eq!(decoded.cases.len(), 2);
        assert_eq!(
            decoded.cases[0].wire_bytes,
            vec![0x82, 0x86, 0x41, 0x88, 0xf4, 0x39, 0xce, 0x75, 0xc8, 0x75, 0xfa, 0x57, 0x84]
        );
        assert_eq!(
            decoded.cases[0].headers,
            vec![
                (b":method".to_vec(), b"GET".to_vec()),
                (b":scheme".to_vec(), b"http".to_vec()),
                (b":authority".to_vec(), b"yahoo.co.jp".to_vec()),
                (b":path".to_vec(), b"/".to_vec()),
            ]
        );
    }

    /// A helper function that performs an interop test for a given story file.
    ///
    /// It does so by first decoding the JSON representation of the story into
    /// a `TestStory` struct. After this, each subsequent block of headers is
    /// passed to the same decoder instance (since each story represents one
    /// coder context). The result returned by the decoder is compared to the
    /// headers stored for that particular block within the story file.
    fn test_story(story_file_name: PathBuf) {
        // Set up the story by parsing the given file
        let story: TestStory = {
            let buf = std::fs::read_to_string(story_file_name);
            let raw_story: RawTestStory = serde_json::from_str(&buf.unwrap()).unwrap();
            raw_story.try_into().unwrap()
        };
        // Set up the decoder
        let mut decoder = Decoder::new();
        // decoder.allow_trailing_size_updates = true;

        // Now check whether we correctly decode each case
        for case in story.cases.iter() {
            let decoded = decoder.decode(&case.wire_bytes).unwrap();
            assert_eq!(decoded, case.headers);
        }
    }

    /// Tests a full fixture set, provided a path to a directory containing a
    /// number of story files (and no other file types).
    ///
    /// It calls the `test_story` function for each file found in the given
    /// directory.
    fn test_fixture_set(fixture_dir: &str) {
        let files = fs::read_dir(Path::new(fixture_dir)).unwrap();

        for fixture in files {
            let file_name = fixture.unwrap().path();
            trace!("Testing fixture: {:?}", file_name);
            test_story(file_name);
        }
    }

    #[test]
    fn test_nghttp2_interop() {
        test_fixture_set("fixtures/hpack/interop/nghttp2");
    }

    #[test]
    fn test_nghttp2_change_table_size_interop() {
        test_fixture_set("fixtures/hpack/interop/nghttp2-change-table-size");
    }

    #[test]
    fn test_go_hpack_interop() {
        test_fixture_set("fixtures/hpack/interop/go-hpack");
    }

    #[test]
    fn test_node_http2_hpack_interop() {
        test_fixture_set("fixtures/hpack/interop/node-http2-hpack");
    }

    #[test]
    fn test_haskell_http2_linear_huffman() {
        test_fixture_set("fixtures/hpack/interop/haskell-http2-linear-huffman");
    }
}
