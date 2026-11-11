//! Create PDF ▸ From an Office file: Word, Excel, PowerPoint, OpenDocument and RTF files become
//! PDF through LibreOffice, run as a separate program (a "sidecar", docs/gaps.md gap 8).
//!
//! LibreOffice (MPL-2.0) is never linked or bundled: the user installs it, PdfCraft finds it
//! and runs `soffice --headless --convert-to pdf` on a private copy of the file, with a profile
//! of its own so an open LibreOffice window is neither used nor disturbed. Nothing goes over the
//! network. Macros do not run: headless conversion keeps LibreOffice's macro security.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::CreateError;

/// The extensions LibreOffice converts, lowercase.
pub const OFFICE_EXTENSIONS: &[&str] =
    &["doc", "docx", "docm", "dot", "dotx", "odt", "ott", "rtf", "wpd", "xls", "xlsx", "xlsm", "ods", "csv", "ppt", "pptx", "pptm", "odp", "odg"];
/// How long one conversion may take. The first run builds LibreOffice's profile and is slow.
pub const CONVERT_LIMIT: Duration = Duration::from_secs(240);
/// The largest input converted, in bytes.
pub const MAX_INPUT: u64 = 512 * 1024 * 1024;
/// The largest PDF read back, in bytes.
pub const MAX_OUTPUT: u64 = 1024 * 1024 * 1024;

/// Whether `path` names a file [`office_to_pdf`] can convert, by its extension.
pub fn is_office_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| OFFICE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The first LibreOffice found: `$PDFCRAFT_SOFFICE`, then `soffice` or `libreoffice` on the
/// `PATH`, then the usual install locations.
pub fn find_office() -> Option<PathBuf> {
    office_candidates().into_iter().find(|p| p.is_file())
}

fn office_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("PDFCRAFT_SOFFICE") {
        out.push(PathBuf::from(p));
    }
    let names: &[&str] = if cfg!(windows) { &["soffice.exe", "soffice.com"] } else { &["soffice", "libreoffice"] };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            out.extend(names.iter().map(|n| dir.join(n)));
        }
    }
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(d) = std::env::var_os(var) {
                out.push(PathBuf::from(d).join("LibreOffice").join("program").join("soffice.exe"));
            }
        }
    } else if cfg!(target_os = "macos") {
        out.push(PathBuf::from("/Applications/LibreOffice.app/Contents/MacOS/soffice"));
    } else {
        out.extend(
            ["/usr/bin/soffice", "/usr/lib/libreoffice/program/soffice", "/opt/libreoffice/program/soffice", "/snap/bin/libreoffice"]
                .map(PathBuf::from),
        );
    }
    out
}

/// A `file://` URL for a local path, as LibreOffice's `-env:UserInstallation` wants it.
fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            url.push(char::from(b));
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

/// Convert the Office file at `input` to PDF bytes with the LibreOffice at `soffice`.
pub fn office_to_pdf(soffice: &Path, input: &Path, limit: Duration) -> Result<Vec<u8>, CreateError> {
    let invalid = |m: String| CreateError::Invalid(m);
    let ext = input.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if !OFFICE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(invalid(format!("{} is not an Office file this can convert ({})", input.display(), OFFICE_EXTENSIONS.join(", "))));
    }
    let size = std::fs::metadata(input).map_err(|e| invalid(format!("{}: {e}", input.display())))?.len();
    if size > MAX_INPUT {
        return Err(invalid(format!("{} is larger than {} MB", input.display(), MAX_INPUT / (1024 * 1024))));
    }
    let scratch = Scratch::new().map_err(|e| invalid(format!("no temporary folder: {e}")))?;
    // A private copy with a plain name: LibreOffice names its output after the input, and the
    // user's file name may hold characters its command line treats specially.
    let source = scratch.0.join(format!("source.{ext}"));
    std::fs::copy(input, &source).map_err(|e| invalid(format!("{}: {e}", input.display())))?;
    let out_dir = scratch.0.join("out");
    let profile = scratch.0.join("profile");
    std::fs::create_dir(&out_dir).map_err(|e| invalid(e.to_string()))?;
    let mut cmd = Command::new(soffice);
    // CREATE_NO_WINDOW: `soffice.com` is a console program, which would flash a console window
    // when started from the windowed app on Windows.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.arg(format!("-env:UserInstallation={}", file_url(&profile)))
        .args(["--headless", "--invisible", "--nodefault", "--nolockcheck", "--nologo", "--norestore", "--convert-to", "pdf", "--outdir"])
        .arg(&out_dir)
        .arg(&source)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| invalid(format!("LibreOffice could not be started ({e})")))?;
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(invalid(format!("LibreOffice did not finish within {} s", limit.as_secs())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(invalid(e.to_string()));
            }
        }
    };
    let pdf = out_dir.join("source.pdf");
    let len = std::fs::metadata(&pdf).map(|m| m.len()).map_err(|_| {
        invalid(format!("LibreOffice made no PDF ({status}); the file may be damaged, password-protected or of a kind it cannot read"))
    })?;
    if len > MAX_OUTPUT {
        return Err(invalid(format!("the converted PDF is larger than {} MB", MAX_OUTPUT / (1024 * 1024))));
    }
    let bytes = std::fs::read(&pdf).map_err(|e| invalid(e.to_string()))?;
    if !bytes.starts_with(b"%PDF-") {
        return Err(invalid("LibreOffice wrote something that is not a PDF".into()));
    }
    Ok(bytes)
}

/// A private temporary folder, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Scratch> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("pdfcraft-office-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        // `create_dir` fails if the name exists, so a planted folder is never used.
        std::fs::create_dir(&path)?;
        Ok(Scratch(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_files_are_known_by_extension_in_any_case() {
        assert!(is_office_file(Path::new("Lasku.DOCX")) && is_office_file(Path::new("a/b.xlsx")) && is_office_file(Path::new("x.odt")));
        assert!(!is_office_file(Path::new("x.pdf")) && !is_office_file(Path::new("docx")) && !is_office_file(Path::new("")));
    }

    #[test]
    fn profile_urls_are_absolute_file_urls_with_unsafe_bytes_escaped() {
        assert_eq!(file_url(Path::new("/tmp/a b/ä")), "file:///tmp/a%20b/%C3%A4");
        assert_eq!(file_url(Path::new(r"C:\Users\Aapo Ä\p")), "file:///C:/Users/Aapo%20%C3%84/p");
    }

    #[test]
    fn refusals_come_before_starting_anything() {
        let missing = Path::new("/nonexistent/soffice");
        let err = |p: &Path| office_to_pdf(missing, p, CONVERT_LIMIT).unwrap_err().to_string();
        assert!(err(Path::new("x.pdf")).contains("not an Office file"));
        assert!(err(Path::new("/nonexistent/x.docx")).contains("/nonexistent/x.docx"));
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("a.docx");
        std::fs::write(&doc, b"x").unwrap();
        assert!(err(&doc).contains("could not be started"));
    }

    /// End to end with a real LibreOffice, when one is installed.
    #[test]
    fn converts_an_rtf_with_an_installed_libreoffice() {
        let Some(soffice) = find_office() else {
            eprintln!("skipped: LibreOffice not installed");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let rtf = dir.path().join("Lasku (1) ä.rtf");
        std::fs::write(&rtf, br"{\rtf1\ansi\deff0{\fonttbl{\f0 Helvetica;}}\f0\fs24 Lasku 42: P\u228?iv\u228?m\u228?\u228?r\u228? 1.12.2026\par}")
            .unwrap();
        let pdf = office_to_pdf(&soffice, &rtf, CONVERT_LIMIT).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        let doc = pdfcraft_cos::Document::open(std::sync::Arc::new(pdf)).unwrap();
        assert!(doc.root().is_some());
    }
}
