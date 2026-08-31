//! The envelope a value is stored in, and the shape a caller holds it by.

use std::ops::{Deref, DerefMut};

use doclite_core::{CodecError, FromStorage, ObjectId, ToStorage};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::clock::Timestamp;
use crate::identity::{DocumentId, Ref, RevisionId};
use crate::record::Amendable;

/// The framing this envelope writes, so a later one can be told apart from it.
const VERSION: u8 = 2;
const HAS_PREV: u8 = 0b0000_0001;
const HEADER: usize = 18;

/// One revision of a value: what it was, when it was written, which record it
/// belongs to, and what it superseded.
///
/// Revisions are never rewritten, so an update files a new one whose `prev`
/// names the old. Following that chain is the whole of history. The
/// [`DocumentId`] is written down rather than worked out, because decoding sees
/// bytes and nothing else, and every revision of a record has to agree on it.
/// Its own [`RevisionId`] is the key it is stored under rather than a field, so
/// `prev` is the only one written here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision<T> {
    document: DocumentId,
    prev: Option<RevisionId>,
    inner: T,
    created_at: Timestamp,
}

impl<T> Revision<T> {
    /// Identity is written into the envelope rather than worked out later,
    /// which is ADR 0010.
    pub fn new(
        inner: T,
        document: DocumentId,
        created_at: Timestamp,
        prev: Option<RevisionId>,
    ) -> Self {
        Self {
            document,
            prev,
            inner,
            created_at,
        }
    }

    /// The document, which is the id that never moves.
    pub fn id(&self) -> DocumentId {
        self.document
    }

    /// The revision this one supersedes, and `None` for the first.
    pub fn prev(&self) -> Option<RevisionId> {
        self.prev
    }

    /// When this revision was written, read once while the operation was built
    /// so a re-walk does not change it.
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// The record itself, with the envelope dropped.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T> Deref for Revision<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.inner
    }
}

/// The envelope frames its own header and hands the body to `T`, so a `T` with
/// a codec of its own is stored under one and never has to be serde.
/// The envelope a text record gets, so the whole object is one JSON value.
///
/// A binary header in front of a JSON body would make the object neither, which
/// is the whole point of choosing JSON: under `doclite-sqlite` the body is then
/// a text value that `sqlite3` prints and `json_extract` reads.
///
/// The record nests under `body` rather than being flattened beside the header,
/// so no field a record declares can ever collide with a field this writes.
///
/// `doc` and `prev` are numbers, and a reader whose integers are floats (a
/// browser's `JSON.parse`, say) loses precision above 2^53 where these ids sit.
/// SQLite's own JSON functions read them exactly.
#[derive(Serialize)]
struct TextEnvelope<'a> {
    v: u8,
    at: u64,
    doc: u64,
    prev: Option<u64>,
    body: &'a RawValue,
}

/// The same envelope coming back, holding the record's bytes unparsed.
#[derive(Deserialize)]
struct ReadEnvelope {
    v: u8,
    at: u64,
    doc: u64,
    #[serde(default)]
    prev: Option<u64>,
    body: Box<RawValue>,
}

impl<T: ToStorage> ToStorage for Revision<T> {
    const TEXT: bool = T::TEXT;

    fn to_storage(&self) -> Result<Vec<u8>, CodecError> {
        let body = self.inner.to_storage()?;
        if T::TEXT {
            return self.as_text(&body);
        }
        let mut bytes = Vec::with_capacity(HEADER + 8 + body.len());
        bytes.push(VERSION);
        bytes.push(if self.prev.is_some() { HAS_PREV } else { 0 });
        bytes.extend(self.created_at.0.to_be_bytes());
        bytes.extend(self.document.0 .0.to_be_bytes());
        if let Some(RevisionId(ObjectId(prev))) = self.prev {
            bytes.extend(prev.to_be_bytes());
        }
        bytes.extend(body);
        Ok(bytes)
    }
}

impl<T: ToStorage> Revision<T> {
    /// The whole revision as one JSON value, with the record spliced in rather
    /// than parsed and written out again.
    fn as_text(&self, body: &[u8]) -> Result<Vec<u8>, CodecError> {
        let body = std::str::from_utf8(body).map_err(|error| {
            CodecError::encoding(format!("a text record that is not utf-8: {error}"))
        })?;
        let body = RawValue::from_string(body.to_owned()).map_err(CodecError::encoding)?;
        serde_json::to_vec(&TextEnvelope {
            v: VERSION,
            at: self.created_at.0,
            doc: self.document.0 .0,
            prev: self.prev.map(|RevisionId(ObjectId(id))| id),
            body: &body,
        })
        .map_err(CodecError::encoding)
    }
}

impl<T: FromStorage> FromStorage for Revision<T> {
    const TEXT: bool = T::TEXT;

    fn from_storage(bytes: &[u8]) -> Result<Self, CodecError> {
        if T::TEXT {
            return Self::from_text(bytes);
        }
        let header = bytes
            .get(..HEADER)
            .ok_or_else(|| CodecError::decoding("a revision shorter than its own header"))?;
        if header[0] != VERSION {
            return Err(CodecError::decoding(format!(
                "a revision written by envelope version {}, this is {VERSION}",
                header[0]
            )));
        }

        let created_at = Timestamp(u64::from_be_bytes(
            header[2..10]
                .try_into()
                .map_err(|_| CodecError::decoding("a truncated timestamp"))?,
        ));

        let document = DocumentId(ObjectId(u64::from_be_bytes(
            header[10..HEADER]
                .try_into()
                .map_err(|_| CodecError::decoding("a truncated document id"))?,
        )));

        let (prev, body) = if header[1] & HAS_PREV == 0 {
            (None, &bytes[HEADER..])
        } else {
            let raw = bytes.get(HEADER..HEADER + 8).ok_or_else(|| {
                CodecError::decoding("a revision claiming a prev it does not carry")
            })?;
            let prev = u64::from_be_bytes(
                raw.try_into()
                    .map_err(|_| CodecError::decoding("a truncated prev"))?,
            );
            (Some(RevisionId(ObjectId(prev))), &bytes[HEADER + 8..])
        };

        Ok(Self {
            document,
            prev,
            inner: T::from_storage(body)?,
            created_at,
        })
    }
}

impl<T: FromStorage> Revision<T> {
    /// The text envelope back. The framing is checked the same way the binary
    /// one's is, because a version is what tells one envelope from a later one.
    fn from_text(bytes: &[u8]) -> Result<Self, CodecError> {
        let read: ReadEnvelope = serde_json::from_slice(bytes).map_err(CodecError::decoding)?;
        if read.v != VERSION {
            return Err(CodecError::decoding(format!(
                "a revision written by envelope version {}, this is {VERSION}",
                read.v
            )));
        }
        Ok(Self {
            document: DocumentId(ObjectId(read.doc)),
            prev: read.prev.map(|id| RevisionId(ObjectId(id))),
            inner: T::from_storage(read.body.get().as_bytes())?,
            created_at: Timestamp(read.at),
        })
    }
}

/// A value as it came back from the store, and the only thing a caller edits.
///
/// Derefs to the value, so a field reads the way it was written; the envelope
/// is read through `id`, `revision`, `prev` and `created_at` rather than
/// reached into. Reaching for `&mut` copies the value aside first: the index
/// rows to retire are keyed on what the value *was*, and once an edit has
/// happened nothing else remembers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored<T> {
    revision: RevisionId,
    envelope: Revision<T>,
    was: Option<T>,
}

impl<T> Stored<T> {
    pub(crate) fn new(revision: RevisionId, envelope: Revision<T>) -> Self {
        Self {
            revision,
            envelope,
            was: None,
        }
    }

    /// The record, stable across every revision of it.
    ///
    /// This is the one to store as a foreign key. An update files a new
    /// revision but leaves this alone, so a reference by it does not go stale.
    pub fn id(&self) -> DocumentId {
        self.envelope.document
    }

    /// A reference another record can hold to this one.
    ///
    /// The same id [`Stored::id`] returns, carrying what it points at, so a
    /// field that stores it says so and following it needs no turbofish.
    pub fn reference(&self) -> Ref<T> {
        Ref::new(self.id())
    }

    /// This particular revision.
    pub fn revision(&self) -> RevisionId {
        self.revision
    }

    /// When this revision was written, not when the document was first filed.
    pub fn created_at(&self) -> Timestamp {
        self.envelope.created_at
    }

    /// The revision this one supersedes, which is what `history` walks back
    /// along.
    pub fn prev(&self) -> Option<RevisionId> {
        self.envelope.prev
    }

    /// The record itself, with both ids dropped.
    pub fn into_inner(self) -> T {
        self.envelope.inner
    }

    /// The record, this revision, the value as it was read if an edit has
    /// copied it aside, and the value as it now is.
    ///
    /// [`Connection::update`] needs all four by value: the pre-edit copy names
    /// the index rows to retire, and the edited one goes into the revision that
    /// supersedes them.
    ///
    /// [`Connection::update`]: crate::Connection::update
    pub(crate) fn into_parts(self) -> (DocumentId, RevisionId, Option<T>, T) {
        (
            self.envelope.document,
            self.revision,
            self.was,
            self.envelope.inner,
        )
    }
}

impl<T> Deref for Stored<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.envelope.inner
    }
}

/// Copies the value aside on the first edit and not on later ones, so reading a
/// row costs nothing and editing one costs a single clone.
///
/// A record that cannot be superseded is never handed a `&mut` in the first
/// place, so the edit is refused where it was written rather than at the
/// [`Connection::update`] that could not have filed it.
///
/// [`Connection::update`]: crate::Connection::update
impl<T: Clone + Amendable> DerefMut for Stored<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.was.get_or_insert_with(|| self.envelope.inner.clone());
        &mut self.envelope.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(id: u64) -> DocumentId {
        DocumentId(ObjectId(id))
    }

    fn roundtrip(revision: Revision<Vec<u8>>) {
        let bytes = revision.to_storage().expect("bytes encode");
        assert_eq!(Revision::<Vec<u8>>::from_storage(&bytes), Ok(revision));
    }

    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Note {
        slug: String,
        len: u8,
    }
    doclite_core::json_storage!(Note);

    fn note() -> Note {
        Note {
            slug: "n-1".into(),
            len: 3,
        }
    }

    /// The whole point of a text envelope: the object is one JSON value, so
    /// something that is not this crate can read it without decoding first.
    #[test]
    fn a_text_record_makes_the_whole_envelope_one_json_value() {
        let revision = Revision::new(note(), document(7), Timestamp(1_767_225_600_123), None);
        let bytes = revision.to_storage().expect("a text record encodes");
        assert_eq!(
            std::str::from_utf8(&bytes),
            Ok(concat!(
                r#"{"v":2,"at":1767225600123,"doc":7,"prev":null,"#,
                r#""body":{"slug":"n-1","len":3}}"#
            ))
        );
    }

    /// The record nests rather than flattening, so a field it declares can
    /// never collide with one the envelope writes.
    #[test]
    fn a_record_naming_an_envelope_field_does_not_collide_with_it() {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        struct Awkward {
            v: String,
            doc: String,
        }
        doclite_core::json_storage!(Awkward);

        let inner = Awkward {
            v: "not the version".into(),
            doc: "not the document".into(),
        };
        let revision = Revision::new(inner.clone(), document(7), Timestamp(1), None);
        let bytes = revision.to_storage().expect("it encodes");
        let back = Revision::<Awkward>::from_storage(&bytes).expect("it decodes");
        assert_eq!(*back, inner, "the record came back as itself");
        assert_eq!(back.id(), document(7), "and the envelope kept its own");
    }

    #[test]
    fn a_text_revision_comes_back_whole() {
        for prev in [None, Some(RevisionId(ObjectId(9)))] {
            let revision = Revision::new(note(), document(7), Timestamp(1_767_225_600_123), prev);
            let bytes = revision.to_storage().expect("a text record encodes");
            assert_eq!(Revision::<Note>::from_storage(&bytes), Ok(revision));
        }
    }

    /// The framing byte earns its keep across both envelopes: a text one from a
    /// later version is refused rather than half-read.
    #[test]
    fn a_text_envelope_of_another_version_is_refused() {
        let written = br#"{"v":99,"at":1,"doc":7,"prev":null,"body":{"slug":"n-1","len":3}}"#;
        let Err(why) = Revision::<Note>::from_storage(written) else {
            panic!("a version this does not write is not one it can read");
        };
        assert!(format!("{why}").contains("version 99"), "{why}");
    }

    #[test]
    fn a_first_revision_carries_no_prev() {
        roundtrip(Revision::new(
            b"a blob".to_vec(),
            document(7),
            Timestamp(1),
            None,
        ));
    }

    #[test]
    fn a_later_revision_names_what_it_superseded() {
        roundtrip(Revision::new(
            b"a blob".to_vec(),
            document(7),
            Timestamp(1_767_225_600_123),
            Some(RevisionId(ObjectId(9))),
        ));
    }

    #[test]
    fn an_empty_body_survives_the_framing() {
        roundtrip(Revision::new(Vec::new(), document(1), Timestamp(0), None));
    }

    #[test]
    fn a_revision_keeps_the_document_it_belongs_to() {
        let bytes = Revision::new(b"x".to_vec(), document(42), Timestamp(1), None)
            .to_storage()
            .expect("bytes encode");
        let read = Revision::<Vec<u8>>::from_storage(&bytes).expect("bytes decode");
        assert_eq!(read.id(), document(42));
    }

    #[test]
    fn another_envelope_version_is_refused_rather_than_misread() {
        let mut bytes = Revision::new(b"x".to_vec(), document(1), Timestamp(1), None)
            .to_storage()
            .expect("bytes encode");
        bytes[0] = VERSION + 1;
        assert!(Revision::<Vec<u8>>::from_storage(&bytes).is_err());
    }

    #[test]
    fn a_truncated_revision_is_refused() {
        assert!(Revision::<Vec<u8>>::from_storage(&[VERSION, 0, 0]).is_err());
    }

    /// Stands in for a record, because a `Vec<u8>` is not one and only a record
    /// is ever edited. Written by hand rather than derived, since this crate
    /// cannot depend on the macro that writes the impl.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Body(Vec<u8>);

    impl Amendable for Body {}

    #[test]
    fn an_edit_remembers_what_the_value_was() {
        let mut stored = Stored::new(
            RevisionId(ObjectId(7)),
            Revision::new(Body(vec![1u8]), document(7), Timestamp(1), None),
        );
        assert_eq!(stored.was, None, "reading copies nothing");

        stored.0.push(2);
        assert_eq!(stored.was, Some(Body(vec![1u8])));
        assert_eq!(*stored, Body(vec![1u8, 2]));

        stored.0.push(3);
        assert_eq!(
            stored.was,
            Some(Body(vec![1u8])),
            "only the first edit copies"
        );
    }
}
