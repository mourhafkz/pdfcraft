//! A reference library in the spirit of Mendeley: every paper's metadata (title, authors, year,
//! venue, DOI, arXiv id, abstract, keywords), the user's collections and tags, free-form notes,
//! reading state, identifier detection in PDF text, metadata from CrossRef and arXiv, citation
//! export (BibTeX, RIS, APA) and search across metadata, notes and full text.
//!
//! The crate knows nothing about PDFs or the interface. The caller hands it page text (so it can
//! find a DOI or guess a title) and file paths; it keeps a JSON file per library. Everything
//! read from disk or the network is untrusted: parsing is lenient, sizes are capped, and nothing
//! here panics.

pub mod ask;
pub mod chats;
pub mod cite;
#[cfg(feature = "fetch")]
pub mod fetch;
pub mod ident;
pub mod lookup;
pub mod model;
pub mod search;
pub mod store;

pub use model::{Author, Collection, CollectionId, Metadata, Paper, PaperId, ReadState};
pub use search::{Filter, SortKey};
pub use store::Library;

/// What can go wrong with a library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("couldn't read or write the library: {0}")]
    Io(#[from] std::io::Error),
    #[error("the library file is damaged: {0}")]
    Format(String),
    #[error("{0}")]
    Lookup(String),
}

pub type Result<T> = std::result::Result<T, Error>;
