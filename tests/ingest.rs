//! Source ingestion: PDF text extraction, URL retrieval, and honest access
//! levels (SPEC §4.2, §4.3, §12, §13 "Literature-only task").

mod common;

use common::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::tools::ingest_input;

fn repo_docs() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs")
}

async fn read_artifact(f: &Fixture, source: &Source) -> String {
    let id = source.text_artifact.as_ref().expect("text artifact");
    let artifact = f.store.get_artifact(id).await.unwrap();
    tokio::fs::read_to_string(f.store.resolve_path(&artifact.storage_path))
        .await
        .unwrap()
}

/// A local PDF is extracted, hashed, and recorded as full text with the
/// paper's own title, using the repository's foundation preprint.
#[tokio::test]
async fn pdf_text_is_extracted_and_recorded_as_full_text() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let pdf = repo_docs().join("pre_coscientist.pdf");
    let permissions = Permissions {
        read_paths: vec![repo_docs().to_string_lossy().into_owned()],
        ..Default::default()
    };

    let ingested = ingest_input(
        &f.store,
        &f.run_id,
        &InputRef {
            locator: pdf.to_string_lossy().into_owned(),
            note: None,
        },
        &permissions,
    )
    .await
    .unwrap();

    assert_eq!(
        ingested.source.access,
        AccessLevel::FullText,
        "{:?}",
        ingested.source
    );
    assert_eq!(
        ingested.source.content_hash,
        ContentHash::of_bytes(&std::fs::read(&pdf).unwrap()),
        "the hash pins the exact bytes read"
    );
    let text = read_artifact(&f, &ingested.source).await;
    assert!(
        text.to_ascii_lowercase().contains("co-scientist"),
        "the extracted text must contain the paper's subject"
    );
    assert!(
        text.len() > 100_000,
        "an 80-page paper yields substantial text: {}",
        text.len()
    );
    let title = ingested.source.title.clone().unwrap_or_default();
    assert!(
        title.to_ascii_lowercase().contains("co-scientist"),
        "the title comes from the document, not the file name: {title:?}"
    );
}

/// A minimal valid PDF with one blank page.
fn blank_pdf() -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>",
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for o in offsets {
        out.push_str(&format!("{o:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

/// A PDF without a text layer is metadata-only, with the limitation stated,
/// rather than presented as read (SPEC §4.3).
#[tokio::test]
async fn pdf_without_text_is_metadata_only() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let path = f.dir.path().join("scan.pdf");
    tokio::fs::write(&path, blank_pdf()).await.unwrap();

    let ingested = ingest_input(
        &f.store,
        &f.run_id,
        &InputRef {
            locator: path.to_string_lossy().into_owned(),
            note: None,
        },
        &f.goal.permissions,
    )
    .await
    .unwrap();

    assert_eq!(
        ingested.source.access,
        AccessLevel::MetadataOnly,
        "{:?}",
        ingested.source
    );
    assert!(ingested.artifact.is_none(), "no text, no text artifact");
    let limitation = ingested
        .source
        .access_limitations
        .clone()
        .unwrap_or_default();
    assert!(limitation.contains("no text"), "{limitation}");
}

/// Serve one HTTP response per connection from a local listener.
async fn serve(responses: Vec<(u16, &'static str, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        for (status, content_type, body) in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut total = 0;
            loop {
                let n = sock.read(&mut buf[total..]).await.unwrap();
                total += n;
                if n == 0 || buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&body).await.unwrap();
            sock.shutdown().await.ok();
        }
    });
    base
}

const ARTICLE: &str = r#"<html><head><title>Study A</title><meta name="author" content="Alice Example">
<meta property="article:published_time" content="2025-03-01"></head>
<body><nav>home | about | contact</nav>
<article><h1>Study A results</h1>
<p>We observed a 23% increase in the treated group (p=0.03) across forty samples, which we interpret as a real effect of the intervention.</p>
<p>A second paragraph gives the readability heuristics enough material to keep the article body and drop the navigation.</p>
</article><footer>copyright</footer></body></html>"#;

/// A URL is retrieved only with the network permission; the article text,
/// title, byline, and date are recorded, and the bytes are hashed.
#[tokio::test]
async fn url_retrieval_is_gated_and_extracts_the_article() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let base = serve(vec![
        (200, "text/html; charset=utf-8", ARTICLE.as_bytes().to_vec()),
        (404, "text/html", b"<html>gone</html>".to_vec()),
        (
            200,
            "application/pdf",
            std::fs::read(repo_docs().join("pre_coscientist.pdf")).unwrap(),
        ),
    ])
    .await;
    let url = format!("{base}/study-a");

    // Without the permission, nothing is fetched and the source says so.
    let denied = ingest_input(
        &f.store,
        &f.run_id,
        &InputRef {
            locator: url.clone(),
            note: None,
        },
        &Permissions::default(),
    )
    .await
    .unwrap();
    assert_eq!(denied.source.access, AccessLevel::Unavailable);
    assert!(
        denied
            .source
            .access_limitations
            .as_deref()
            .unwrap_or("")
            .contains("not permitted")
    );

    let permissions = Permissions {
        network: true,
        ..Default::default()
    };
    let ingested = ingest_input(
        &f.store,
        &f.run_id,
        &InputRef {
            locator: url.clone(),
            note: None,
        },
        &permissions,
    )
    .await
    .unwrap();
    assert_eq!(
        ingested.source.access,
        AccessLevel::FullText,
        "{:?}",
        ingested.source
    );
    assert_eq!(ingested.source.title.as_deref(), Some("Study A"));
    assert_eq!(ingested.source.authors.as_deref(), Some("Alice Example"));
    assert_eq!(ingested.source.date.as_deref(), Some("2025-03-01"));
    assert_eq!(
        ingested.source.content_hash,
        ContentHash::of_bytes(ARTICLE.as_bytes())
    );
    let text = read_artifact(&f, &ingested.source).await;
    assert!(text.contains("23% increase"), "{text}");
    assert!(
        !text.contains("home | about"),
        "navigation is not article text: {text}"
    );

    // A failed retrieval is recorded as unavailable with the status.
    let missing = ingest_input(
        &f.store,
        &f.run_id,
        &InputRef {
            locator: format!("{base}/missing"),
            note: None,
        },
        &permissions,
    )
    .await
    .unwrap();
    assert_eq!(missing.source.access, AccessLevel::Unavailable);
    assert!(
        missing
            .source
            .access_limitations
            .as_deref()
            .unwrap_or("")
            .contains("HTTP 404")
    );

    // A PDF served over HTTP goes through the same extractor.
    let paper = ingest_input(
        &f.store,
        &f.run_id,
        &InputRef {
            locator: format!("{base}/paper.pdf"),
            note: None,
        },
        &permissions,
    )
    .await
    .unwrap();
    assert_eq!(
        paper.source.access,
        AccessLevel::FullText,
        "{:?}",
        paper.source
    );
    assert!(
        read_artifact(&f, &paper.source)
            .await
            .to_ascii_lowercase()
            .contains("co-scientist")
    );

    assert_eq!(f.store.list_sources(&f.run_id).await.unwrap().len(), 4);
}
