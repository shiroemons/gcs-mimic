//! Gzip members read the way Cloud Storage's front end reads a transport-compressed request
//! (observed 2026-11-25, see the transport gzip provider observations): every member needs its
//! header, its complete deflate data and its 8-byte trailer, and concatenated members decode as
//! one body, but the trailer's CRC-32 and size are not compared with the data. A wrong CRC-32 or
//! size therefore decodes, while a body that ends inside a member, or that carries anything after
//! its last member, fails as a whole.

use flate2::{Decompress, FlushDecompress, Status};
use std::io::{self, BufRead, Read};

const ID1: u8 = 0x1f;
const ID2: u8 = 0x8b;
const DEFLATE: u8 = 8;
const FHCRC: u8 = 0x02;
const FEXTRA: u8 = 0x04;
const FNAME: u8 = 0x08;
const FCOMMENT: u8 = 0x10;
const RESERVED: u8 = 0xe0;
/// ID1, ID2, CM, FLG, MTIME (4), XFL and OS.
const FIXED_HEADER_BYTES: usize = 10;
/// CRC-32 and ISIZE, both read and ignored.
const TRAILER_BYTES: usize = 8;

/// Why a request body is not a sequence of gzip members.
#[derive(Debug, thiserror::Error)]
pub(super) enum GzipError {
    #[error("The body does not start a gzip member")]
    NotGzip,
    /// Bytes follow the last complete member.
    #[error("Bytes follow the last gzip member")]
    TrailingData,
    #[error("Gzip compression method {0} is not deflate")]
    UnsupportedMethod(u8),
    #[error("Gzip header flags {0:#04x} are reserved")]
    ReservedFlags(u8),
    /// The body ends inside a member.
    #[error("The body ends inside a gzip member")]
    Truncated,
    #[error("Invalid deflate data in a gzip member")]
    Deflate(#[source] flate2::DecompressError),
}

/// A decode failure leaves [`Members`] as [`io::ErrorKind::InvalidData`]; every other error it
/// returns comes from its input.
impl From<GzipError> for io::Error {
    fn from(error: GzipError) -> Self {
        Self::new(io::ErrorKind::InvalidData, error)
    }
}

#[derive(Clone, Copy)]
enum HeaderStep {
    /// The number of fixed header bytes read so far.
    Fixed(usize),
    /// FEXTRA's 2-byte length, with its low byte once read.
    ExtraLength(Option<u8>),
    Skip(usize),
    /// FNAME or FCOMMENT, up to and including its NUL.
    Text,
}

/// Where in the body a member may begin, which names what other bytes there are.
#[derive(Clone, Copy)]
enum Place {
    /// The start of the body, where other bytes are not gzip.
    Start,
    /// After a complete member, where other bytes are trailing data.
    AfterMember,
}

/// One member header, fed a byte at a time so that a read can stop at any input boundary.
struct Header {
    step: HeaderStep,
    /// The optional fields the flags announce that are still ahead.
    pending: u8,
    place: Place,
}

impl Header {
    fn new(place: Place) -> Self {
        Self {
            step: HeaderStep::Fixed(0),
            pending: 0,
            place,
        }
    }

    /// Takes header bytes from `input`: how many it used, and whether the header is complete.
    fn take(&mut self, input: &[u8]) -> Result<(usize, bool), GzipError> {
        for (index, &byte) in input.iter().enumerate() {
            if self.feed(byte)? {
                return Ok((index + 1, true));
            }
        }
        Ok((input.len(), false))
    }

    /// `true` once `byte` completes the header.
    fn feed(&mut self, byte: u8) -> Result<bool, GzipError> {
        match self.step {
            HeaderStep::Fixed(read) => {
                match (read, byte) {
                    (0, ID1) | (1, ID2) | (2, DEFLATE) => {}
                    (0 | 1, _) => {
                        return Err(match self.place {
                            Place::Start => GzipError::NotGzip,
                            Place::AfterMember => GzipError::TrailingData,
                        });
                    }
                    (2, method) => return Err(GzipError::UnsupportedMethod(method)),
                    (3, flags) if flags & RESERVED != 0 => {
                        return Err(GzipError::ReservedFlags(flags));
                    }
                    (3, flags) => self.pending = flags & (FEXTRA | FNAME | FCOMMENT | FHCRC),
                    _ => {}
                }
                if read + 1 == FIXED_HEADER_BYTES {
                    return Ok(self.next_field());
                }
                self.step = HeaderStep::Fixed(read + 1);
                Ok(false)
            }
            HeaderStep::ExtraLength(None) => {
                self.step = HeaderStep::ExtraLength(Some(byte));
                Ok(false)
            }
            HeaderStep::ExtraLength(Some(low)) => match u16::from_le_bytes([low, byte]) {
                0 => Ok(self.next_field()),
                length => {
                    self.step = HeaderStep::Skip(usize::from(length));
                    Ok(false)
                }
            },
            HeaderStep::Skip(1) => Ok(self.next_field()),
            HeaderStep::Skip(left) => {
                self.step = HeaderStep::Skip(left - 1);
                Ok(false)
            }
            HeaderStep::Text if byte == 0 => Ok(self.next_field()),
            HeaderStep::Text => Ok(false),
        }
    }

    /// Moves to the next optional field in the order RFC 1952 lays them out; `true` when none
    /// is left.
    fn next_field(&mut self) -> bool {
        let Some(field) = [FEXTRA, FNAME, FCOMMENT, FHCRC]
            .into_iter()
            .find(|field| self.pending & field != 0)
        else {
            return true;
        };
        self.pending &= !field;
        self.step = match field {
            FEXTRA => HeaderStep::ExtraLength(None),
            FHCRC => HeaderStep::Skip(2),
            _ => HeaderStep::Text,
        };
        false
    }
}

/// Whether `prefix`, the first bytes of a body, can begin a gzip member. A streamed body that
/// cannot is refused before the rest of it arrives.
pub(super) fn may_start_member(prefix: &[u8]) -> bool {
    Header::new(Place::Start).take(prefix).is_ok()
}

enum Stage {
    /// Between members: the end of the input ends the body here.
    Boundary(Place),
    Header(Header),
    Deflate,
    /// Trailer bytes still to skip.
    Trailer(usize),
}

/// The decoded bytes of the gzip members in `input`. An input that is empty decodes to an empty
/// body. A read that returns an error from the input, such as [`io::ErrorKind::WouldBlock`],
/// keeps its place, so the next read continues where it stopped.
pub(super) struct Members<R> {
    input: R,
    inflate: Decompress,
    stage: Stage,
}

impl<R: BufRead> Members<R> {
    pub(super) fn new(input: R) -> Self {
        Self {
            input,
            inflate: Decompress::new(false),
            stage: Stage::Boundary(Place::Start),
        }
    }

    pub(super) fn get_mut(&mut self) -> &mut R {
        &mut self.input
    }

    /// Inflates into `output`: the bytes it wrote, or `None` when the member's deflate data
    /// needs more input to produce any.
    fn inflate(&mut self, output: &mut [u8]) -> io::Result<Option<usize>> {
        let input = self.input.fill_buf()?;
        let available = input.len();
        let (read_before, written_before) = (self.inflate.total_in(), self.inflate.total_out());
        let status = self
            .inflate
            .decompress(input, output, FlushDecompress::None)
            .map_err(GzipError::Deflate)?;
        let consumed = usize::try_from(self.inflate.total_in() - read_before).unwrap_or(available);
        let written = usize::try_from(self.inflate.total_out() - written_before).unwrap_or(0);
        self.input.consume(consumed);
        if status == Status::StreamEnd {
            self.stage = Stage::Trailer(TRAILER_BYTES);
        } else if written == 0 && available == 0 {
            return Err(GzipError::Truncated.into());
        }
        Ok((written > 0).then_some(written))
    }
}

impl<R: BufRead> Read for Members<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            match &mut self.stage {
                Stage::Boundary(place) => {
                    let place = *place;
                    if self.input.fill_buf()?.is_empty() {
                        return Ok(0);
                    }
                    self.stage = Stage::Header(Header::new(place));
                }
                Stage::Header(header) => {
                    let input = self.input.fill_buf()?;
                    if input.is_empty() {
                        return Err(GzipError::Truncated.into());
                    }
                    let (used, complete) = header.take(input)?;
                    self.input.consume(used);
                    if complete {
                        self.inflate.reset(false);
                        self.stage = Stage::Deflate;
                    }
                }
                Stage::Deflate => {
                    if let Some(written) = self.inflate(output)? {
                        return Ok(written);
                    }
                }
                Stage::Trailer(left) => {
                    let input = self.input.fill_buf()?;
                    if input.is_empty() {
                        return Err(GzipError::Truncated.into());
                    }
                    let used = (*left).min(input.len());
                    self.input.consume(used);
                    *left -= used;
                    if *left == 0 {
                        self.stage = Stage::Boundary(Place::AfterMember);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn decode(input: &[u8]) -> io::Result<Vec<u8>> {
        let mut decoded = Vec::new();
        Members::new(input).read_to_end(&mut decoded)?;
        Ok(decoded)
    }

    /// Asserts that `input` fails to decode with `expected`.
    fn fails(input: &[u8], expected: &GzipError) {
        let error = decode(input).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), expected.to_string());
    }

    /// Hands out its input one byte at a time and answers `WouldBlock` before each byte, the
    /// way an upload's input runs dry between frames.
    struct Trickle {
        bytes: Vec<u8>,
        offset: usize,
        ready: bool,
    }

    impl Read for Trickle {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let length = self.fill_buf()?.read(output)?;
            self.consume(length);
            Ok(length)
        }
    }

    impl BufRead for Trickle {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if !self.ready && self.offset < self.bytes.len() {
                self.ready = true;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let end = (self.offset + 1).min(self.bytes.len());
            Ok(&self.bytes[self.offset..end])
        }

        fn consume(&mut self, length: usize) {
            self.offset += length;
            if length > 0 {
                self.ready = false;
            }
        }
    }

    #[test]
    fn concatenated_members_with_every_optional_field_decode_as_one_body() {
        let mut named = Vec::new();
        let mut encoder = flate2::GzBuilder::new()
            .extra(vec![1, 2, 3])
            .filename("name")
            .comment("comment")
            .write(&mut named, flate2::Compression::fast());
        encoder.write_all(b"second ").unwrap();
        encoder.finish().unwrap();
        // A header CRC-16 after the fixed header, which no encoder here writes.
        let mut checked = gzip(b"third");
        checked[3] |= FHCRC;
        checked.splice(FIXED_HEADER_BYTES..FIXED_HEADER_BYTES, [0xaa, 0xbb]);
        let body = [gzip(b"first "), named, checked].concat();
        assert_eq!(decode(&body).unwrap(), b"first second third");
        assert_eq!(decode(b"").unwrap(), b"");
    }

    #[test]
    fn a_trailer_that_disagrees_with_the_data_still_decodes() {
        let mut body = gzip(b"payload");
        let crc = body.len() - TRAILER_BYTES;
        body[crc..].fill(0);
        assert_eq!(decode(&body).unwrap(), b"payload");
    }

    #[test]
    fn a_body_that_ends_inside_a_member_or_continues_past_it_fails() {
        let body = gzip(b"payload");
        let header_only = &body[..FIXED_HEADER_BYTES - 1];
        let without_trailer = &body[..body.len() - TRAILER_BYTES];
        let short_trailer = &body[..body.len() - 1];
        for truncated in [
            &body[..1],
            header_only,
            &body[..12],
            without_trailer,
            short_trailer,
        ] {
            fails(truncated, &GzipError::Truncated);
        }
        fails(b"not gzip", &GzipError::NotGzip);
        let garbage = [body.clone(), b"garbage".to_vec()].concat();
        fails(&garbage, &GzipError::TrailingData);
        let mut method = body.clone();
        method[2] = 7;
        fails(&method, &GzipError::UnsupportedMethod(7));
        let mut flags = body.clone();
        flags[3] = 0x20;
        fails(&flags, &GzipError::ReservedFlags(0x20));
        let mut deflate = body;
        deflate[FIXED_HEADER_BYTES] = 0xff;
        let error = decode(&deflate).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "Invalid deflate data in a gzip member");
    }

    #[test]
    fn a_read_that_runs_out_of_input_continues_where_it_stopped() {
        let mut named = Vec::new();
        let mut encoder = flate2::GzBuilder::new()
            .extra(vec![9; 4])
            .filename("name")
            .write(&mut named, flate2::Compression::fast());
        encoder.write_all(&[b'x'; 3000]).unwrap();
        encoder.finish().unwrap();
        let bytes = [gzip(b"head "), named].concat();
        let mut members = Members::new(Trickle {
            bytes,
            offset: 0,
            ready: false,
        });
        let (mut decoded, mut blocked) = (Vec::new(), 0);
        let mut output = [0; 7];
        loop {
            match members.read(&mut output) {
                Ok(0) => break,
                Ok(length) => decoded.extend_from_slice(&output[..length]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => blocked += 1,
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(decoded, [b"head ".as_slice(), &[b'x'; 3000]].concat());
        assert!(blocked > 30, "every input byte arrived after a WouldBlock");
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    for (error, text) in [
        (GzipError::NotGzip, "The body does not start a gzip member"),
        (GzipError::TrailingData, "Bytes follow the last gzip member"),
        (
            GzipError::UnsupportedMethod(7),
            "Gzip compression method 7 is not deflate",
        ),
        (
            GzipError::ReservedFlags(0x20),
            "Gzip header flags 0x20 are reserved",
        ),
        (GzipError::Truncated, "The body ends inside a gzip member"),
    ] {
        assert_eq!(error.to_string(), text);
        assert!(std::error::Error::source(&error).is_none());
    }
    let failure = Decompress::new(false)
        .decompress(&[0xff], &mut [0; 16], FlushDecompress::Finish)
        .unwrap_err();
    let error = GzipError::Deflate(failure);
    assert_eq!(error.to_string(), "Invalid deflate data in a gzip member");
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<flate2::DecompressError>()
    );
    crate::error::assert_distinct_causes(&error);
}
