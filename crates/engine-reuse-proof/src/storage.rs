//! In-memory storage archive and chunk-codec proof.
//!
//! Every call here is CPU-side byte framing over the neutral storage crate.
//! The proof encodes one canonical chunk section, wraps it in a deliberately
//! out-of-order two-section archive, re-parses the image and reads back only
//! section 3 before decoding it. Nothing here opens a file, reads a clock,
//! spawns a thread, inspects the environment or creates a GPU object, so the
//! proof stays device-free and its report stays byte-deterministic.

use opencraft_storage::archive::{encode_archive, SaveImage, SectionId};
use opencraft_storage::chunk_section::{encode_chunk_section, ChunkSection};

/// The observable result of one device-free storage proof run.
#[derive(Clone, Copy, Debug)]
pub struct StorageProof {
    /// Byte length of the encoded two-section archive image.
    pub archive_bytes: usize,
    /// Sections the parsed archive reports.
    pub section_count: usize,
    /// Edits decoded back from the archived chunk section.
    pub chunk_edits: usize,
}

/// Run the archive and chunk-codec proofs deterministically.
///
/// The archive is built in memory only: no path, file handle or temporary
/// directory is created, so the proof runs in a sandbox with no writable disk.
pub fn run() -> StorageProof {
    // Canonical chunk section over three raw content edits. The codec derives
    // the palette, so duplicates in content indices are expected and fine.
    let mut chunk = Vec::new();
    encode_chunk_section(&mut chunk, &[(0, 5), (2, 5), (4, 7)])
        .expect("the proof's fixed edits are encodable");

    // Deliberately out of id order: encode_archive canonicalizes to ascending
    // sections, which the parse below relies on.
    let sections = [
        (SectionId(9), &b"opaque"[..]),
        (SectionId(3), chunk.as_slice()),
    ];
    let image = encode_archive(10, 0, &sections).expect("the proof's fixed sections encode");
    let archive_bytes = image.len();

    let image = SaveImage::parse(image).expect("a freshly encoded archive parses");
    assert_eq!(
        image.section_count(),
        2,
        "proof archive carries two sections"
    );

    // Read only the chunk section body; the opaque section is never touched.
    let body = image
        .section(SectionId(3))
        .expect("section 3 is present in the proof archive");
    let decoded = ChunkSection::decode(body).expect("the archived chunk section decodes");
    assert_eq!(decoded.len(), 3, "proof chunk section carries three edits");

    StorageProof {
        archive_bytes,
        section_count: image.section_count(),
        chunk_edits: decoded.len(),
    }
}
