# PdfCraft Library — a Mendeley-style reference manager

This fork adds a **Library** to PdfCraft: your papers with their metadata, collections, tags,
notes and highlights, in one place, with citation export. Open it with the **book** button next to
Home in the tab strip.

## What it does

| | |
|---|---|
| **Library** | Every PDF you add, as a sortable table (title, authors, year, venue, date added), with *All papers*, *Recently added*, *Favorites*, *Needs review*, *Unfiled* and *Unread / Reading / Read*. |
| **Metadata** | Read from each PDF when it's added: DOI, arXiv id, ISBN, title, authors and keywords. When the paper has a DOI or arXiv id, the rest is filled from **CrossRef** or **arXiv** (only the identifier is sent; turn it off in the sidebar). Every field stays editable: type, title, authors, year, journal, volume, issue, pages, publisher, DOI, arXiv, ISBN, URL, abstract, keywords, citation key. *Look up* fills empty fields; **Shift**-click it to replace them all. |
| **Notes** | A notes page per paper (Markdown), saved automatically. While reading a paper, the **sticky-note** button in the tab strip opens its notes in a window beside the PDF. |
| **Highlights** | Highlight in any colour with PdfCraft's Highlight tool and write a note on a highlight; the paper's **Highlights** tab lists every highlight with its colour, page, the highlighted sentence and your note. Filter by colour, click one to open the PDF at that page, or add them all to your notes as Markdown (grouped by colour). Highlights live in the PDF itself as standard annotations, so other readers (Preview, Acrobat, Zotero) see them too. |
| **Organise** | Collections (one level of sub-collections; drag papers onto a collection), tags, favourites, reading state, duplicate warning for a DOI that is already in the library. |
| **Watched folders** | PDFs saved into a watched folder join the library when you open it. |
| **Search** | One box searches titles, authors, abstracts, keywords, tags, your notes — and the **full text** of every paper, with the matching passage shown under the title. |
| **Ask this paper** | The **sparkles** button over any open PDF opens a chat with Claude about that paper. Answers come from the paper's text and cite the pages they rely on (click a page chip to go there; hover it for the quoted passage). For a paper in the Library, your highlights, their comments and your notes can be sent along ("go through my highlights…"). Save any answer to the paper's notes. Conversations are kept per paper: reopening the window continues the latest one, **History** switches to an earlier one, **Export** saves one as Markdown, and the paper's **Chats** tab in the Library lists them all (continue, export, copy, delete). Uses your own Anthropic API key (kept in the macOS Keychain, or `ANTHROPIC_API_KEY`); the paper's text is sent to Anthropic only when you ask. Choose Sonnet, Opus or Haiku in the window. |
| **Study (languages)** | The **languages** button opens Study. Select a paragraph in the PDF and click **Study selection**: a literal translation sentence by sentence, the grammar structures worth learning at your level (each with where it occurs, an explanation and new example sentences), and the vocabulary worth keeping (nouns with article and plural, verbs with their forms and case). **+ Deck** saves a word with the sentence and page it came from; the **Vocabulary** tab reviews the deck as flashcards with spaced repetition (Enter shows the answer, 1–4 grade it) and exports it for Anki. Level is set in the window (A2–B1 … C1+). Uses the same Claude API key as Ask. |
| **Citations** | Copy an APA 7 reference, or export the selection (or everything shown) as **BibTeX** or **RIS** for LaTeX, Word, Zotero or EndNote. |

The library is kept in PdfCraft's settings folder (`~/Library/Application Support/PdfCraft/library/`
on macOS): `library.json` plus the extracted text in `fulltext/`. Set `PDFCRAFT_LIBRARY_DIR` to keep
it elsewhere (a synced folder, say). Removing a paper from the library never deletes its PDF.

## Getting the Mac app

Every push to `feat/research-library` builds an Apple-silicon DMG on GitHub
Actions: **Actions ▸ macOS build (Library fork) ▸ latest run ▸ Artifacts**. It can also be run
by hand (**Run workflow**) with `universal` or `x86_64` for Intel Macs.

The app is ad-hoc signed (no Apple Developer certificate), so macOS blocks the download at first.
After dragging PdfCraft.app to Applications, run once in Terminal:

```sh
xattr -dr com.apple.quarantine /Applications/PdfCraft.app
```

### Building on your Mac instead

```sh
xcode-select --install                     # once: Apple's command-line tools
curl https://sh.rustup.rs -sSf | sh        # once: Rust
git clone https://github.com/mourhafkz/pdfcraft && cd pdfcraft
git checkout feat/research-library
cargo run --release -p pdfcraft            # run it directly, or:
packaging/macos/package.sh --arch aarch64  # build dist/release/pdfcraft-*.dmg
```

## Where the code is

- `crates/library` — the reference library: records, storage, identifier detection, CrossRef and
  arXiv parsing (the HTTPS request is behind the `fetch` feature), BibTeX / RIS / APA, search.
  No interface code; `cargo test -p pdfcraft-library --features fetch`.
- `crates/ui-egui/src/library_ui.rs` — the Library tab and the notes window.
- `crates/ui-egui/src/library_marks.rs` — reading a PDF for the library: identifiers, text, and
  highlights with the text under them.
